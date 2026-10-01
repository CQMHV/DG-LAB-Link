//! DG-LAB Socket V3. Wire values stay local to this adapter; the Hub uses shared
//! device operations and receives actual APP strength feedback.

use std::sync::atomic::Ordering;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, timeout};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use url::Url;

use super::event_delivery::EventSink;
use super::{
    DeviceCapabilities, DeviceOperation, InitializationState, QueuedOperation, SessionCommand,
    SessionDevice, SessionEvent, SessionHandle, SessionQueues, SessionReply,
    TransportConnectionSnapshot, TransportError, TransportKind, V3_CONNECTION_ID, session_channel,
};
use crate::dglab::v4::V3WaveFrame;
use crate::hub::{ChannelStatus, ConnectionState};
use crate::model::Channel;

pub const DEFAULT_RELAY_ENDPOINT: &str = "wss://ws.dungeon-lab.cn/";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(6);
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);
const STEP_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_MESSAGE_SIZE: usize = 64 * 1024;
const SLOT_ID: &str = "v3";

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireMessage {
    #[serde(rename = "type")]
    kind: String,
    client_id: String,
    target_id: String,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StrengthReport {
    intensity: [u16; 2],
    limit: [u16; 2],
}

fn parse_strength(message: &str) -> Option<StrengthReport> {
    let mut fields = message.strip_prefix("strength-")?.split('+');
    let mut values = [0_u16; 4];
    for value in &mut values {
        *value = fields.next()?.parse().ok()?;
        if *value > 200 {
            return None;
        }
    }
    if fields.next().is_some() {
        return None;
    }
    Some(StrengthReport {
        intensity: [values[0], values[1]],
        limit: [values[2], values[3]],
    })
}

/// V3's APP interprets a relative adjustment as one unit regardless of the last
/// number. Sending larger deltas would silently lose the remaining adjustment.
fn relative_strength_message(channel: Channel, increase: bool) -> String {
    format!(
        "strength-{}+{}+1",
        channel_number(channel),
        u8::from(increase)
    )
}

fn zero_strength_message(channel: Channel) -> String {
    format!("strength-{}+2+0", channel_number(channel))
}

fn clear_message(channel: Channel) -> String {
    format!("clear-{}", channel_number(channel))
}

const fn channel_number(channel: Channel) -> u8 {
    channel.as_v4() + 1
}

fn application_message(controller_id: &str, peer_id: &str, message: String) -> Value {
    json!({
        "type": "msg",
        "clientId": controller_id,
        "targetId": peer_id,
        "message": message,
    })
}

#[derive(Debug)]
struct StrengthSteps {
    current: u16,
    target: u16,
    expected: u16,
}

#[derive(Debug, PartialEq, Eq)]
enum StepFeedback {
    Unchanged,
    Continue,
    Completed,
    Unexpected,
}

impl StrengthSteps {
    fn new(current: u16, limit: u16, delta: i32) -> Option<Self> {
        let target = i32::from(current).checked_add(delta)?;
        if delta == 0 || !(0..=i32::from(limit)).contains(&target) {
            return None;
        }
        let mut steps = Self {
            current,
            target: target as u16,
            expected: current,
        };
        steps.next_expected();
        Some(steps)
    }

    fn increase(&self) -> bool {
        self.target > self.current
    }

    fn next_expected(&mut self) {
        self.expected = if self.increase() {
            self.current + 1
        } else {
            self.current - 1
        };
    }

    fn observe(&mut self, actual: u16) -> StepFeedback {
        if actual == self.current {
            return StepFeedback::Unchanged;
        }
        if actual != self.expected {
            return StepFeedback::Unexpected;
        }
        self.current = actual;
        if self.current == self.target {
            return StepFeedback::Completed;
        }
        self.next_expected();
        StepFeedback::Continue
    }
}

struct PendingAdjustment {
    request_id: String,
    generation: u64,
    steps: StrengthSteps,
    deadline: Instant,
}

#[derive(Default)]
struct Peer {
    controller_id: String,
    client_id: String,
    strength: Option<StrengthReport>,
    pending: [Option<PendingAdjustment>; 2],
    blocked: [bool; 2],
}

impl Peer {
    fn device(&self) -> SessionDevice {
        let strength = self.strength.unwrap_or(StrengthReport {
            intensity: [0; 2],
            limit: [0; 2],
        });
        SessionDevice {
            id: self.client_id.clone(),
            slot_id: SLOT_ID.to_owned(),
            name: "V3 APP 双通道设备".to_owned(),
            device_type: "UNKNOWN".to_owned(),
            power: None,
            intensity_a: strength.intensity[0],
            intensity_b: strength.intensity[1],
            intensity_limit_a: strength.limit[0],
            intensity_limit_b: strength.limit[1],
            // V3 exposes no hardware load state; capability=false tells clients
            // not to treat these idle values as hardware observations.
            channel_a_status: ChannelStatus::Idle,
            channel_b_status: ChannelStatus::Idle,
            initialization: if self.strength.is_some() {
                InitializationState::Ready
            } else {
                InitializationState::Initializing
            },
            capabilities: DeviceCapabilities {
                battery: false,
                load_status: false,
                operation_confirmation: false,
                ..DeviceCapabilities::default()
            },
            ble_parameters: None,
            configuration_status: None,
        }
    }

    fn is_paired(&self) -> bool {
        !self.controller_id.is_empty() && !self.client_id.is_empty()
    }
}

pub fn spawn(events: mpsc::Sender<SessionEvent>) -> (SessionHandle, JoinHandle<()>) {
    let (handle, queues) = session_channel(256);
    let task = tokio::spawn(run(queues, EventSink::new(events)));
    (handle, task)
}

async fn receive_command(queues: &mut SessionQueues) -> Option<SessionCommand> {
    tokio::select! {
        biased;
        command = queues.safety.recv() => command,
        command = queues.commands.recv() => command,
    }
}

async fn emit(events: &EventSink, event: SessionEvent) {
    events.push(event);
}

async fn connection_event(
    events: &EventSink,
    endpoint: &str,
    peer: &Peer,
    state: ConnectionState,
    error: Option<String>,
) {
    let controller_id = (!peer.controller_id.is_empty()).then(|| peer.controller_id.clone());
    let pairing_url = controller_id
        .as_deref()
        .and_then(|id| pairing_url(endpoint, id).ok());
    emit(
        events,
        SessionEvent::Connection(TransportConnectionSnapshot {
            connection_id: V3_CONNECTION_ID.to_owned(),
            transport: TransportKind::WsV3,
            state,
            endpoint: endpoint.to_owned(),
            controller_id,
            pairing_url,
            app_count: usize::from(peer.is_paired()),
            last_error: error,
        }),
    )
    .await;
}

async fn device_event(events: &EventSink, peer: &Peer) {
    emit(
        events,
        SessionEvent::Device {
            connection_id: V3_CONNECTION_ID.to_owned(),
            client_id: peer.client_id.clone(),
            device: peer.device(),
        },
    )
    .await;
}

async fn operation_event(
    events: &EventSink,
    client_id: &str,
    request_id: String,
    result: Result<(), TransportError>,
    confirmed: bool,
) {
    emit(
        events,
        SessionEvent::OperationFinished {
            connection_id: V3_CONNECTION_ID.to_owned(),
            client_id: client_id.to_owned(),
            request_id,
            result,
            confirmed,
        },
    )
    .await;
}

async fn cancel_pending(
    events: &EventSink,
    peer: &mut Peer,
    channel: Option<Channel>,
    error: TransportError,
) {
    for selected in Channel::ALL {
        if channel.is_some_and(|channel| channel != selected) {
            continue;
        }
        if let Some(pending) = peer.pending[selected.as_v4() as usize].take() {
            operation_event(
                events,
                &peer.client_id,
                pending.request_id,
                Err(error.clone()),
                false,
            )
            .await;
        }
    }
}

fn invalidated() -> TransportError {
    TransportError::new("operation_cancelled", "操作已被停止或断开取代")
}

fn not_ready() -> TransportError {
    TransportError::new("device_unavailable", "V3 APP 未配对或尚未上报设备强度")
}

fn validate_endpoint(endpoint: &str) -> Result<String, TransportError> {
    let url = Url::parse(endpoint)
        .map_err(|error| TransportError::new("invalid_endpoint", error.to_string()))?;
    if !matches!(url.scheme(), "ws" | "wss")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(TransportError::new(
            "invalid_endpoint",
            "V3 地址必须为不含凭据和片段的 WS/WSS 地址",
        ));
    }
    Ok(url.into())
}

async fn send(socket: &mut Socket, message: Message) -> Result<(), TransportError> {
    timeout(WRITE_TIMEOUT, socket.send(message))
        .await
        .map_err(|_| TransportError::new("transport_timeout", "写入 V3 Relay 超时"))?
        .map_err(|error| TransportError::new("transport_error", error.to_string()))
}

async fn send_application(
    socket: &mut Socket,
    peer: &Peer,
    message: String,
) -> Result<(), TransportError> {
    if !peer.is_paired() {
        return Err(not_ready());
    }
    send(
        socket,
        Message::Text(
            application_message(&peer.controller_id, &peer.client_id, message)
                .to_string()
                .into(),
        ),
    )
    .await
}

async fn close(socket: &mut Socket) {
    let _ = timeout(WRITE_TIMEOUT, socket.close(None)).await;
}

async fn connect(
    queues: &mut SessionQueues,
    endpoint: &str,
    generation: u64,
) -> Result<Socket, TransportError> {
    if generation < queues.handle.operation_floor.load(Ordering::Acquire) {
        return Err(invalidated());
    }
    // Bound both frame and reassembled message size before receiving any APP
    // data, rather than relying on the much larger WebSocket defaults.
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(MAX_MESSAGE_SIZE);
    config.max_frame_size = Some(MAX_MESSAGE_SIZE);
    let connecting = tokio_tungstenite::connect_async_with_config(endpoint, Some(config), false);
    tokio::select! {
        biased;
        _ = queues.handle.shutdown.cancelled() => Err(TransportError::stopped()),
        safety = queues.safety.recv() => {
            match safety {
                Some(SessionCommand::Stop { reply, .. }) | Some(SessionCommand::Disconnect { reply }) => {
                    let _ = reply.send(Ok(()));
                }
                _ => {}
            }
            Err(invalidated())
        }
        result = timeout(CONNECT_TIMEOUT, connecting) => {
            let (mut socket, _) = result
                .map_err(|_| TransportError::new("connect_timeout", "连接 V3 Relay 超时"))?
                .map_err(|error| TransportError::new("transport_error", error.to_string()))?;
            if generation < queues.handle.operation_floor.load(Ordering::Acquire) {
                close(&mut socket).await;
                return Err(invalidated());
            }
            Ok(socket)
        }
    }
}

async fn run(mut queues: SessionQueues, events: EventSink) {
    let shutdown = queues.handle.shutdown.clone();
    let mut pending_connect: Option<(String, u64, SessionReply)> = None;
    loop {
        let (endpoint, generation, reply) = if let Some(pending) = pending_connect.take() {
            pending
        } else {
            let command = tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                command = receive_command(&mut queues) => command,
            };
            match command {
                Some(SessionCommand::Connect {
                    endpoint,
                    generation,
                    reply,
                    ..
                }) => (endpoint, generation, reply),
                Some(SessionCommand::Disconnect { reply })
                | Some(SessionCommand::Stop { reply, .. }) => {
                    let _ = reply.send(Ok(()));
                    continue;
                }
                Some(SessionCommand::Configure { reply, .. }) => {
                    let _ = reply.send(Err(TransportError::new(
                        "unsupported_operation",
                        "V3 WS 不支持 BLE 参数",
                    )));
                    continue;
                }
                Some(SessionCommand::Operation(operation)) => {
                    operation_event(
                        &events,
                        "",
                        operation.operation.request_id().to_owned(),
                        Err(not_ready()),
                        false,
                    )
                    .await;
                    continue;
                }
                None => break,
            }
        };
        let endpoint = match validate_endpoint(&endpoint) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                let _ = reply.send(Err(error));
                continue;
            }
        };
        let mut peer = Peer::default();
        connection_event(&events, &endpoint, &peer, ConnectionState::Connecting, None).await;
        let mut socket = match connect(&mut queues, &endpoint, generation).await {
            Ok(socket) => socket,
            Err(error) => {
                let stopped = queues.handle.shutdown.is_cancelled();
                connection_event(
                    &events,
                    &endpoint,
                    &peer,
                    ConnectionState::Disconnected,
                    Some(error.message.clone()),
                )
                .await;
                let _ = reply.send(Err(error));
                if stopped {
                    break;
                }
                continue;
            }
        };
        let _ = reply.send(Ok(()));
        connection_event(&events, &endpoint, &peer, ConnectionState::Waiting, None).await;
        let exit = session(&mut socket, &mut queues, &events, &endpoint, &mut peer).await;
        cancel_pending(&events, &mut peer, None, invalidated()).await;
        if !peer.client_id.is_empty() {
            emit(
                &events,
                SessionEvent::Removed {
                    connection_id: V3_CONNECTION_ID.to_owned(),
                    client_id: peer.client_id.clone(),
                },
            )
            .await;
        }
        close(&mut socket).await;
        match exit {
            SessionExit::Reconnect {
                endpoint: next,
                generation,
                reply,
            } => {
                connection_event(
                    &events,
                    &endpoint,
                    &Peer::default(),
                    ConnectionState::Disconnected,
                    None,
                )
                .await;
                pending_connect = Some((next, generation, reply));
            }
            SessionExit::Disconnected(error) => {
                connection_event(
                    &events,
                    &endpoint,
                    &Peer::default(),
                    ConnectionState::Disconnected,
                    error,
                )
                .await;
            }
            SessionExit::Shutdown => break,
        }
    }
}

enum SessionExit {
    Reconnect {
        endpoint: String,
        generation: u64,
        reply: SessionReply,
    },
    Disconnected(Option<String>),
    Shutdown,
}

async fn session(
    socket: &mut Socket,
    queues: &mut SessionQueues,
    events: &EventSink,
    endpoint: &str,
    peer: &mut Peer,
) -> SessionExit {
    let connected_at = Instant::now();
    let mut timer = tokio::time::interval(Duration::from_millis(20));
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        if events.overflowed() {
            // Slow observers cannot hold the socket actor. Completion overflow is
            // fail-closed: clear/zero the paired device and discard this connection.
            if peer.is_paired() {
                let _ = stop(socket, events, peer, SLOT_ID, None, true).await;
            }
            return SessionExit::Disconnected(Some("V3 事件观察队列已满，已停止设备".to_owned()));
        }
        let command = tokio::select! {
            biased;
            _ = queues.handle.shutdown.cancelled() => return SessionExit::Shutdown,
            command = queues.safety.recv() => command,
            incoming = socket.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Err(error) = incoming_text(socket, queues, events, endpoint, peer, text.as_ref()).await {
                            return SessionExit::Disconnected(Some(error.message));
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if let Err(error) = send(socket, Message::Pong(payload)).await {
                            return SessionExit::Disconnected(Some(error.message));
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(frame))) => {
                        return SessionExit::Disconnected(frame.map(|frame|format!("V3 Relay 已关闭：{}", frame.reason)));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return SessionExit::Disconnected(Some(error.to_string())),
                    None => return SessionExit::Disconnected(Some("V3 Relay 连接中断".to_owned())),
                }
                continue;
            }
            _ = timer.tick() => {
                check_pending(queues, events, peer).await;
                continue;
            }
            command = queues.commands.recv() => command,
        };
        match command {
            Some(SessionCommand::Disconnect { reply }) => {
                cancel_pending(events, peer, None, invalidated()).await;
                close(socket).await;
                let _ = reply.send(Ok(()));
                return SessionExit::Disconnected(None);
            }
            Some(SessionCommand::Connect {
                endpoint,
                generation,
                reply,
                ..
            }) => {
                return SessionExit::Reconnect {
                    endpoint,
                    generation,
                    reply,
                };
            }
            Some(SessionCommand::Configure { reply, .. }) => {
                let _ = reply.send(Err(TransportError::new(
                    "unsupported_operation",
                    "V3 WS 不支持 BLE 参数",
                )));
            }
            Some(SessionCommand::Stop {
                slot_id,
                channel,
                zero,
                reply,
                ..
            }) => {
                let result = stop(socket, events, peer, &slot_id, channel, zero).await;
                let error = result.as_ref().err().map(|error| error.message.clone());
                let _ = reply.send(result);
                if let Some(error) = error {
                    return SessionExit::Disconnected(Some(error));
                }
            }
            Some(SessionCommand::Operation(queued)) => {
                if let Err(error) =
                    operation(socket, queues, events, peer, connected_at, queued).await
                {
                    return SessionExit::Disconnected(Some(error.message));
                }
            }
            None => return SessionExit::Shutdown,
        };
    }
}

async fn check_pending(queues: &SessionQueues, events: &EventSink, peer: &mut Peer) {
    let floor = queues.handle.operation_floor.load(Ordering::Acquire);
    for channel in Channel::ALL {
        let index = channel.as_v4() as usize;
        let error = peer.pending[index].as_ref().and_then(|pending| {
            if pending.generation < floor {
                Some(invalidated())
            } else if Instant::now() >= pending.deadline {
                Some(TransportError::new(
                    "intensity_timeout",
                    "V3 强度反馈超时，已取消剩余步进；请重新连接",
                ))
            } else {
                None
            }
        });
        if let Some(error) = error {
            if error.code == "intensity_timeout" {
                peer.blocked[index] = true;
            }
            cancel_pending(events, peer, Some(channel), error).await;
        }
    }
}

async fn stop(
    socket: &mut Socket,
    events: &EventSink,
    peer: &mut Peer,
    slot_id: &str,
    channel: Option<Channel>,
    zero: bool,
) -> Result<(), TransportError> {
    if !slot_id.is_empty() && slot_id != SLOT_ID {
        return Err(TransportError::new(
            "device_unavailable",
            "V3 设备槽位不存在",
        ));
    }
    if channel.is_none() || zero {
        cancel_pending(events, peer, channel, invalidated()).await;
    }
    if !peer.is_paired() {
        return Ok(());
    }
    for selected in Channel::ALL {
        if channel.is_some_and(|channel| channel != selected) {
            continue;
        }
        send_application(socket, peer, clear_message(selected)).await?;
        if zero {
            send_application(socket, peer, zero_strength_message(selected)).await?;
        }
    }
    Ok(())
}

async fn operation(
    socket: &mut Socket,
    queues: &SessionQueues,
    events: &EventSink,
    peer: &mut Peer,
    connected_at: Instant,
    queued: QueuedOperation,
) -> Result<(), TransportError> {
    let request_id = queued.operation.request_id().to_owned();
    let error = if queued.queued_at < connected_at || !queues.handle.is_current(&queued) {
        Some(invalidated())
    } else if queued.operation.scope().0 != SLOT_ID || !peer.is_paired() || peer.strength.is_none()
    {
        Some(not_ready())
    } else {
        None
    };
    if let Some(error) = error {
        operation_event(events, &peer.client_id, request_id, Err(error), false).await;
        return Ok(());
    }
    match queued.operation {
        DeviceOperation::Wave { channel, frame, .. } => {
            let pulse = format!(
                "pulse-{channel}:{}",
                json!([V3WaveFrame::from_wave_frame(&frame).to_hex()])
            );
            let result = send_application(socket, peer, pulse).await;
            operation_event(events, &peer.client_id, request_id, result.clone(), false).await;
            result
        }
        DeviceOperation::AdjustIntensity { channel, delta, .. } => {
            let index = channel.as_v4() as usize;
            let strength = peer.strength.expect("readiness checked");
            let error = if peer.blocked[index] {
                Some(TransportError::new(
                    "intensity_unconfirmed",
                    "V3 强度操作状态不确定，请重新连接",
                ))
            } else if peer.pending[index].is_some() {
                Some(TransportError::busy())
            } else {
                None
            };
            if let Some(error) = error {
                operation_event(events, &peer.client_id, request_id, Err(error), false).await;
                return Ok(());
            }
            let Some(steps) =
                StrengthSteps::new(strength.intensity[index], strength.limit[index], delta)
            else {
                operation_event(
                    events,
                    &peer.client_id,
                    request_id,
                    Err(TransportError::new(
                        "intensity_limit",
                        "V3 强度调整超出设备上限或变化无效",
                    )),
                    false,
                )
                .await;
                return Ok(());
            };
            let result = send_application(
                socket,
                peer,
                relative_strength_message(channel, steps.increase()),
            )
            .await;
            if let Err(error) = result {
                operation_event(
                    events,
                    &peer.client_id,
                    request_id,
                    Err(error.clone()),
                    false,
                )
                .await;
                return Err(error);
            }
            peer.pending[index] = Some(PendingAdjustment {
                request_id,
                generation: queued.generation,
                steps,
                deadline: Instant::now() + STEP_TIMEOUT,
            });
            Ok(())
        }
    }
}

async fn incoming_text(
    socket: &mut Socket,
    queues: &SessionQueues,
    events: &EventSink,
    endpoint: &str,
    peer: &mut Peer,
    text: &str,
) -> Result<(), TransportError> {
    let Ok(frame) = serde_json::from_str::<WireMessage>(text) else {
        return Ok(());
    };
    match frame.kind.as_str() {
        "bind" if frame.target_id.is_empty() && frame.message == "targetId" => {
            if frame.client_id.is_empty() || frame.client_id.len() > 256 {
                return Err(TransportError::new("protocol_error", "V3 控制端标识无效"));
            }
            peer.controller_id = frame.client_id;
            connection_event(events, endpoint, peer, ConnectionState::Waiting, None).await;
        }
        "bind" if frame.message == "200" => {
            if frame.client_id != peer.controller_id
                || frame.target_id.is_empty()
                || frame.target_id.len() > 256
            {
                return Err(TransportError::new("protocol_error", "V3 配对标识不匹配"));
            }
            if peer.is_paired() && peer.client_id != frame.target_id {
                return Err(TransportError::new(
                    "protocol_error",
                    "V3 连接只能绑定一个 APP",
                ));
            }
            peer.client_id = frame.target_id;
            device_event(events, peer).await;
            connection_event(events, endpoint, peer, ConnectionState::Connected, None).await;
        }
        "bind" => {
            emit(
                events,
                SessionEvent::Log {
                    connection_id: V3_CONNECTION_ID.to_owned(),
                    message: format!("V3 配对失败：{}", frame.message),
                    warning: true,
                },
            )
            .await;
        }
        "heartbeat" => {}
        "break" => {
            return Err(TransportError::new(
                "peer_disconnected",
                "V3 APP 已断开配对",
            ));
        }
        "error" => {
            cancel_pending(
                events,
                peer,
                None,
                TransportError::new("relay_error", format!("V3 Relay 错误：{}", frame.message)),
            )
            .await;
            peer.blocked = [true; 2];
            if frame.message == "idle_timeout" {
                return Err(TransportError::new("idle_timeout", "V3 配对等待超时"));
            }
            emit(
                events,
                SessionEvent::Log {
                    connection_id: V3_CONNECTION_ID.to_owned(),
                    message: format!("V3 Relay 错误：{}", frame.message),
                    warning: true,
                },
            )
            .await;
        }
        "msg" if peer.is_paired() => {
            // APP replies use the original controller/APP order. Reject frames
            // from stale peers even if a Relay implementation forwards them.
            if frame.client_id != peer.controller_id || frame.target_id != peer.client_id {
                return Ok(());
            }
            if let Some(strength) = parse_strength(&frame.message) {
                peer.strength = Some(strength);
                device_event(events, peer).await;
                advance_steps(socket, queues, events, peer, strength).await?;
            } else if frame.message.starts_with("feedback-") {
                let feedback = frame
                    .message
                    .strip_prefix("feedback-")
                    .and_then(|value| value.parse::<u8>().ok());
                if feedback.is_some_and(|feedback| feedback <= 9) {
                    emit(
                        events,
                        SessionEvent::Log {
                            connection_id: V3_CONNECTION_ID.to_owned(),
                            message: format!("V3 APP 反馈：{}", feedback.unwrap()),
                            warning: false,
                        },
                    )
                    .await;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

async fn advance_steps(
    socket: &mut Socket,
    queues: &SessionQueues,
    events: &EventSink,
    peer: &mut Peer,
    strength: StrengthReport,
) -> Result<(), TransportError> {
    for channel in Channel::ALL {
        let index = channel.as_v4() as usize;
        let Some(mut pending) = peer.pending[index].take() else {
            continue;
        };
        if pending.generation < queues.handle.operation_floor.load(Ordering::Acquire) {
            operation_event(
                events,
                &peer.client_id,
                pending.request_id,
                Err(invalidated()),
                false,
            )
            .await;
            continue;
        }
        if Instant::now() >= pending.deadline {
            peer.blocked[index] = true;
            operation_event(
                events,
                &peer.client_id,
                pending.request_id,
                Err(TransportError::new(
                    "intensity_timeout",
                    "V3 强度反馈已超时，请重新连接",
                )),
                false,
            )
            .await;
            continue;
        }
        match pending.steps.observe(strength.intensity[index]) {
            StepFeedback::Completed => {
                operation_event(events, &peer.client_id, pending.request_id, Ok(()), true).await;
            }
            StepFeedback::Unexpected => {
                peer.blocked[index] = true;
                operation_event(
                    events,
                    &peer.client_id,
                    pending.request_id,
                    Err(TransportError::new(
                        "intensity_conflict",
                        "V3 强度反馈与预期步进不同，剩余调整已取消；请重新连接",
                    )),
                    false,
                )
                .await;
            }
            StepFeedback::Unchanged => peer.pending[index] = Some(pending),
            StepFeedback::Continue => {
                if pending.steps.target > strength.limit[index] {
                    operation_event(
                        events,
                        &peer.client_id,
                        pending.request_id,
                        Err(TransportError::new(
                            "intensity_limit",
                            "V3 APP 降低了强度上限，剩余调整已取消",
                        )),
                        false,
                    )
                    .await;
                    continue;
                }
                // Safety is observable while feedback is being published too;
                // re-check immediately before every subsequent unit write.
                if pending.generation < queues.handle.operation_floor.load(Ordering::Acquire) {
                    operation_event(
                        events,
                        &peer.client_id,
                        pending.request_id,
                        Err(invalidated()),
                        false,
                    )
                    .await;
                    continue;
                }
                if let Err(error) = send_application(
                    socket,
                    peer,
                    relative_strength_message(channel, pending.steps.increase()),
                )
                .await
                {
                    operation_event(
                        events,
                        &peer.client_id,
                        pending.request_id,
                        Err(error.clone()),
                        false,
                    )
                    .await;
                    return Err(error);
                }
                pending.deadline = Instant::now() + STEP_TIMEOUT;
                peer.pending[index] = Some(pending);
            }
        }
    }
    Ok(())
}

pub fn pairing_url(endpoint: &str, controller_id: &str) -> Result<String, String> {
    let mut socket_url = Url::parse(endpoint).map_err(|error| error.to_string())?;
    if !matches!(socket_url.scheme(), "ws" | "wss") || controller_id.is_empty() {
        return Err("V3 配对地址无效".to_owned());
    }
    socket_url
        .path_segments_mut()
        .map_err(|()| "V3 配对地址不能添加控制端标识".to_owned())?
        .pop_if_empty()
        .push(controller_id);
    let mut link = Url::parse("https://www.dungeon-lab.com/app-download.php")
        .expect("constant APP download URL is valid");
    // encodeURIComponent, as used by the official SDK, also escapes ':', '/',
    // '?', '&' and '='. A URL fragment by itself does not escape those values.
    let encoded = socket_url
        .as_str()
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect::<String>();
    link.set_fragment(Some(&format!("DGLAB-SOCKET#{encoded}")));
    Ok(link.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_tungstenite::accept_async;

    async fn server_message(
        socket: &mut WebSocketStream<TcpStream>,
        kind: &str,
        target: &str,
        message: &str,
    ) {
        socket
            .send(Message::Text(
                json!({
                    "type": kind,
                    "clientId": "controller",
                    "targetId": target,
                    "message": message,
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
    }

    async fn server_pair(socket: &mut WebSocketStream<TcpStream>) {
        server_message(socket, "bind", "", "targetId").await;
        server_message(socket, "bind", "app", "200").await;
        server_message(socket, "msg", "app", "strength-10+20+100+100").await;
    }

    async fn server_read(socket: &mut WebSocketStream<TcpStream>) -> Value {
        loop {
            if let Some(Ok(Message::Text(text))) = socket.next().await {
                return serde_json::from_str(text.as_ref()).unwrap();
            }
        }
    }

    async fn wait_ready(events: &mut mpsc::Receiver<SessionEvent>) {
        timeout(Duration::from_secs(3), async {
            loop {
                if let Some(SessionEvent::Device { device, .. }) = events.recv().await
                    && device.initialization == InitializationState::Ready
                {
                    assert_eq!(device.intensity_a, 10);
                    assert_eq!(device.intensity_b, 20);
                    assert_eq!(device.power, None);
                    assert!(!device.capabilities.load_status);
                    return;
                }
            }
        })
        .await
        .unwrap();
    }

    async fn wait_operation(
        events: &mut mpsc::Receiver<SessionEvent>,
        expected: &str,
    ) -> (Result<(), TransportError>, bool) {
        timeout(Duration::from_secs(4), async {
            loop {
                if let Some(SessionEvent::OperationFinished {
                    request_id,
                    result,
                    confirmed,
                    ..
                }) = events.recv().await
                    && request_id == expected
                {
                    return (result, confirmed);
                }
            }
        })
        .await
        .unwrap()
    }

    fn adjust(request_id: &str, channel: Channel, delta: i32) -> DeviceOperation {
        DeviceOperation::AdjustIntensity {
            request_id: request_id.to_owned(),
            slot_id: SLOT_ID.to_owned(),
            channel,
            delta,
        }
    }

    #[test]
    fn strength_report_validates_all_fields() {
        assert_eq!(
            parse_strength("strength-20+30+100+150"),
            Some(StrengthReport {
                intensity: [20, 30],
                limit: [100, 150],
            })
        );
        for invalid in [
            "strength-1+2+3",
            "strength-1+2+3+4+5",
            "strength-201+2+100+100",
            "strength--1+2+100+100",
        ] {
            assert_eq!(parse_strength(invalid), None);
        }
    }

    #[test]
    fn relative_adjustments_are_unit_steps() {
        assert_eq!(
            relative_strength_message(Channel::A, true),
            "strength-1+1+1"
        );
        assert_eq!(
            relative_strength_message(Channel::B, false),
            "strength-2+0+1"
        );
        assert_eq!(zero_strength_message(Channel::B), "strength-2+2+0");
    }

    #[test]
    fn pairing_uses_the_official_v3_fragment() {
        assert_eq!(
            pairing_url(DEFAULT_RELAY_ENDPOINT, "controller").unwrap(),
            "https://www.dungeon-lab.com/app-download.php#DGLAB-SOCKET#wss%3A%2F%2Fws.dungeon-lab.cn%2Fcontroller"
        );
    }

    #[test]
    fn steps_wait_for_actual_unit_feedback() {
        let mut steps = StrengthSteps::new(10, 100, 3).unwrap();
        assert_eq!(steps.expected, 11);
        assert_eq!(steps.observe(10), StepFeedback::Unchanged);
        assert_eq!(steps.expected, 11);
        assert_eq!(steps.observe(11), StepFeedback::Continue);
        assert_eq!(steps.expected, 12);
        assert_eq!(steps.observe(12), StepFeedback::Continue);
        assert_eq!(steps.observe(13), StepFeedback::Completed);
        assert!(StrengthSteps::new(99, 100, 2).is_none());
    }

    #[test]
    fn unrelated_strength_changes_cannot_confirm_a_step() {
        let mut steps = StrengthSteps::new(10, 100, -2).unwrap();
        assert_eq!(steps.observe(11), StepFeedback::Unexpected);
        assert_eq!(steps.current, 10);
        assert_eq!(steps.observe(9), StepFeedback::Continue);
        assert_eq!(steps.observe(8), StepFeedback::Completed);
    }

    #[tokio::test]
    async fn local_relay_pairs_streams_and_confirms_each_strength_step() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            server_pair(&mut socket).await;
            let wave = server_read(&mut socket).await;
            assert_eq!(wave["type"], "msg");
            assert_eq!(wave["clientId"], "controller");
            assert_eq!(wave["targetId"], "app");
            assert_eq!(wave["message"], "pulse-A:[\"0A0A0A0A00000000\"]");
            for actual in 11..=13 {
                let adjustment = server_read(&mut socket).await;
                assert_eq!(adjustment["message"], "strength-1+1+1");
                // A B-only observation must not release A's unit step.
                if actual == 11 {
                    server_message(&mut socket, "msg", "app", "strength-10+21+100+100").await;
                    assert!(
                        timeout(Duration::from_millis(80), socket.next())
                            .await
                            .is_err()
                    );
                }
                server_message(
                    &mut socket,
                    "msg",
                    "app",
                    &format!("strength-{actual}+21+100+100"),
                )
                .await;
            }
            assert!(matches!(socket.next().await, Some(Ok(Message::Close(_)))));
        });
        let (events_tx, mut events) = mpsc::channel(128);
        let (handle, task) = spawn(events_tx);
        handle
            .connect(format!("ws://{address}/"), None, 0)
            .await
            .unwrap();
        wait_ready(&mut events).await;
        handle
            .try_send(
                DeviceOperation::Wave {
                    request_id: "wave".to_owned(),
                    slot_id: SLOT_ID.to_owned(),
                    channel: Channel::A,
                    frame: crate::model::WaveFrame::silent(),
                },
                0,
            )
            .unwrap();
        let (wave_result, wave_confirmed) = wait_operation(&mut events, "wave").await;
        assert!(wave_result.is_ok());
        assert!(!wave_confirmed);
        handle
            .try_send(adjust("add-three", Channel::A, 3), 0)
            .unwrap();
        let (result, confirmed) = wait_operation(&mut events, "add-three").await;
        assert!(result.is_ok());
        assert!(confirmed);
        handle.disconnect().await.unwrap();
        server.await.unwrap();
        handle.shutdown_now();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn emergency_stop_cancels_a_waiting_step_without_waiting_for_feedback() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (first_step, first_seen) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            server_pair(&mut socket).await;
            assert_eq!(server_read(&mut socket).await["message"], "strength-1+1+1");
            first_step.send(()).unwrap();
            for message in ["clear-1", "strength-1+2+0", "clear-2", "strength-2+2+0"] {
                assert_eq!(server_read(&mut socket).await["message"], message);
            }
            // A late observation of the cancelled first step must never send
            // the second increment after clear/zero.
            server_message(&mut socket, "msg", "app", "strength-11+0+100+100").await;
            assert!(
                timeout(Duration::from_millis(150), socket.next())
                    .await
                    .is_err()
            );
            assert!(matches!(socket.next().await, Some(Ok(Message::Close(_)))));
        });
        let (events_tx, mut events) = mpsc::channel(128);
        let (handle, task) = spawn(events_tx);
        handle
            .connect(format!("ws://{address}/"), None, 0)
            .await
            .unwrap();
        wait_ready(&mut events).await;
        handle
            .try_send(adjust("pending", Channel::A, 5), 0)
            .unwrap();
        first_seen.await.unwrap();
        timeout(
            Duration::from_millis(500),
            handle.stop(SLOT_ID.to_owned(), None, true, 1),
        )
        .await
        .unwrap()
        .unwrap();
        let (result, confirmed) = wait_operation(&mut events, "pending").await;
        assert_eq!(result.unwrap_err().code, "operation_cancelled");
        assert!(!confirmed);
        tokio::time::sleep(Duration::from_millis(180)).await;
        handle.disconnect().await.unwrap();
        server.await.unwrap();
        handle.shutdown_now();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn timed_out_step_is_not_retried_or_resumed_by_late_feedback() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (late_feedback, ready_for_late) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            server_pair(&mut socket).await;
            assert_eq!(server_read(&mut socket).await["message"], "strength-1+1+1");
            ready_for_late.await.unwrap();
            server_message(&mut socket, "msg", "app", "strength-11+20+100+100").await;
            let next = socket.next().await;
            assert!(
                matches!(next, Some(Ok(Message::Close(_)))),
                "unexpected frame: {next:?}"
            );
        });
        let (events_tx, mut events) = mpsc::channel(128);
        let (handle, task) = spawn(events_tx);
        handle
            .connect(format!("ws://{address}/"), None, 0)
            .await
            .unwrap();
        wait_ready(&mut events).await;
        handle
            .try_send(adjust("timeout", Channel::A, 3), 0)
            .unwrap();
        let (result, _) = wait_operation(&mut events, "timeout").await;
        assert_eq!(result.unwrap_err().code, "intensity_timeout");
        handle.try_send(adjust("next", Channel::A, 1), 0).unwrap();
        let (result, _) = wait_operation(&mut events, "next").await;
        assert_eq!(result.unwrap_err().code, "intensity_unconfirmed");
        late_feedback.send(()).unwrap();
        timeout(Duration::from_secs(1), async {
            loop {
                if let Some(SessionEvent::Device { device, .. }) = events.recv().await
                    && device.intensity_a == 11
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
        handle.disconnect().await.unwrap();
        server.await.unwrap();
        handle.shutdown_now();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn stopped_generation_cannot_reconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (events_tx, _events) = mpsc::channel(16);
        let (handle, task) = spawn(events_tx);
        handle.invalidate_operations(1);
        let result = handle
            .connect(format!("ws://{}/", listener.local_addr().unwrap()), None, 0)
            .await;
        assert_eq!(result.unwrap_err().code, "operation_cancelled");
        assert!(
            timeout(Duration::from_millis(80), listener.accept())
                .await
                .is_err()
        );
        handle.shutdown_now();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn channel_clear_discards_queued_a_frames_and_preserves_b() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            server_pair(&mut socket).await;
            assert_eq!(server_read(&mut socket).await["message"], "clear-1");
            assert_eq!(
                server_read(&mut socket).await["message"],
                "pulse-B:[\"0A0A0A0A00000000\"]"
            );
            assert_eq!(
                server_read(&mut socket).await["message"],
                "pulse-A:[\"0A0A0A0A00000000\"]"
            );
            assert!(matches!(socket.next().await, Some(Ok(Message::Close(_)))));
        });
        let (events_tx, mut events) = mpsc::channel(128);
        let (handle, task) = spawn(events_tx);
        handle
            .connect(format!("ws://{address}/"), None, 0)
            .await
            .unwrap();
        wait_ready(&mut events).await;
        let wave = |request_id: &str, channel| DeviceOperation::Wave {
            request_id: request_id.to_owned(),
            slot_id: SLOT_ID.to_owned(),
            channel,
            frame: crate::model::WaveFrame::silent(),
        };
        // No await between enqueue and clear: the priority queue is observable
        // before the actor starts consuming these ordinary requests.
        for index in 0..4 {
            handle
                .try_send(wave(&format!("old-a-{index}"), Channel::A), 0)
                .unwrap();
        }
        handle.try_send(wave("b", Channel::B), 0).unwrap();
        handle
            .stop(SLOT_ID.to_owned(), Some(Channel::A), false, 0)
            .await
            .unwrap();
        handle.try_send(wave("new-a", Channel::A), 0).unwrap();
        let (result, confirmed) = wait_operation(&mut events, "new-a").await;
        assert!(result.is_ok());
        assert!(!confirmed);
        handle.disconnect().await.unwrap();
        server.await.unwrap();
        handle.shutdown_now();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn emergency_stop_precedes_a_full_ordinary_wave_queue() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            server_pair(&mut socket).await;
            for expected in ["clear-1", "strength-1+2+0", "clear-2", "strength-2+2+0"] {
                assert_eq!(server_read(&mut socket).await["message"], expected);
            }
            assert!(matches!(socket.next().await, Some(Ok(Message::Close(_)))));
        });
        let (events_tx, mut events) = mpsc::channel(512);
        let (handle, task) = spawn(events_tx);
        handle
            .connect(format!("ws://{address}/"), None, 0)
            .await
            .unwrap();
        wait_ready(&mut events).await;
        for index in 0..256 {
            handle
                .try_send(
                    DeviceOperation::Wave {
                        request_id: format!("old-{index}"),
                        slot_id: SLOT_ID.to_owned(),
                        channel: Channel::A,
                        frame: crate::model::WaveFrame::silent(),
                    },
                    0,
                )
                .unwrap();
        }
        assert_eq!(
            handle
                .try_send(adjust("overflow", Channel::B, 1), 0)
                .unwrap_err()
                .code,
            "queue_busy"
        );
        timeout(
            Duration::from_millis(500),
            handle.stop(SLOT_ID.to_owned(), None, true, 1),
        )
        .await
        .unwrap()
        .unwrap();
        handle.disconnect().await.unwrap();
        server.await.unwrap();
        handle.shutdown_now();
        task.await.unwrap();
    }
}
