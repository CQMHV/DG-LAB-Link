use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::model::Channel;

type WaveScope = (String, String, Channel);
type WaveFloors = Arc<RwLock<BTreeMap<WaveScope, u64>>>;
type DeviceScope = (String, String);
type DeviceFloors = Arc<RwLock<BTreeMap<DeviceScope, u64>>>;
const MAX_WAVE_SCOPES: usize = 256;

pub const DEFAULT_RELAY_ENDPOINT: &str = "wss://trex.dungeon-lab.cn/v4";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
// Global cleanup can fan out to every tracked device without competing with ordinary writes.
const SAFETY_COMMAND_CAPACITY: usize = MAX_WAVE_SCOPES;
const SOCKET_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
const SOCKET_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
const SAFETY_ACK_TIMEOUT: Duration = Duration::from_secs(5);
const EVENT_SEND_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq)]
pub enum RelayEvent {
    Connecting {
        endpoint: String,
    },
    Connected {
        endpoint: String,
    },
    Hello {
        controller_id: String,
    },
    ClientAttached {
        client_id: String,
    },
    ClientDisconnected {
        client_id: String,
    },
    Message {
        client_id: String,
        data: Value,
    },
    Heartbeat,
    Pong {
        timestamp: Option<i64>,
    },
    IdleTimeout,
    RelayError {
        code: String,
        message: Option<String>,
    },
    Unknown(Value),
    Disconnected {
        reason: String,
        retryable: bool,
    },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RelayClientError {
    #[error("Relay 客户端已停止")]
    Stopped,
    #[error("Relay 命令队列已满")]
    QueueFull,
    #[error("Relay 地址无效：{0}")]
    InvalidEndpoint(String),
    #[error("尚未连接 Relay")]
    NotConnected,
    #[error("连接 Relay 超时")]
    ConnectTimeout,
    #[error("Relay 连接失败：{0}")]
    Transport(String),
}

#[derive(Debug)]
enum RelayCommand {
    Connect {
        endpoint: String,
        reply: oneshot::Sender<Result<(), RelayClientError>>,
    },
    Disconnect {
        reply: oneshot::Sender<Result<(), RelayClientError>>,
    },
    SendMessage {
        client_id: String,
        data: Value,
        operation_generation: Option<u64>,
        device_scope: Option<(DeviceScope, u64)>,
        wave_scope: Option<(WaveScope, u64)>,
        reply: Option<oneshot::Sender<Result<(), RelayClientError>>>,
    },
    SafetyStop {
        client_id: String,
        requests: Vec<Value>,
        global_generation: Option<u64>,
        reply: oneshot::Sender<Result<(), RelayClientError>>,
    },
}

#[derive(Clone, Debug)]
pub struct RelayClientHandle {
    commands: mpsc::Sender<RelayCommand>,
    safety_commands: mpsc::Sender<RelayCommand>,
    shutdown: CancellationToken,
    operation_floor: Arc<AtomicU64>,
    wave_floors: WaveFloors,
    device_floors: DeviceFloors,
}

impl RelayClientHandle {
    pub async fn connect(&self, endpoint: impl Into<String>) -> Result<(), RelayClientError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(RelayCommand::Connect {
                endpoint: endpoint.into(),
                reply,
            })
            .await
            .map_err(|_| RelayClientError::Stopped)?;
        response.await.map_err(|_| RelayClientError::Stopped)?
    }

    pub async fn disconnect(&self) -> Result<(), RelayClientError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(RelayCommand::Disconnect { reply })
            .await
            .map_err(|_| RelayClientError::Stopped)?;
        response.await.map_err(|_| RelayClientError::Stopped)?
    }

    pub async fn send_message(
        &self,
        client_id: impl Into<String>,
        data: Value,
    ) -> Result<(), RelayClientError> {
        let client_id = client_id.into();
        let device_scope = self.message_scope(&client_id, &data);
        let (reply, response) = oneshot::channel();
        self.commands
            .send(RelayCommand::SendMessage {
                client_id,
                data,
                operation_generation: None,
                device_scope,
                wave_scope: None,
                reply: Some(reply),
            })
            .await
            .map_err(|_| RelayClientError::Stopped)?;
        response.await.map_err(|_| RelayClientError::Stopped)?
    }

    pub fn try_send_message(
        &self,
        client_id: impl Into<String>,
        data: Value,
    ) -> Result<(), RelayClientError> {
        let client_id = client_id.into();
        let device_scope = self.message_scope(&client_id, &data);
        self.commands
            .try_send(RelayCommand::SendMessage {
                client_id,
                data,
                operation_generation: None,
                device_scope,
                wave_scope: None,
                reply: None,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => RelayClientError::QueueFull,
                mpsc::error::TrySendError::Closed(_) => RelayClientError::Stopped,
            })
    }

    /// 设备操作携带单调递增的代次。安全停止确认后，socket actor 会丢弃所有旧代次操作，
    /// 防止已经排队的波形或强度调整在 clear / 归零之后重新生效。
    pub fn try_send_operation(
        &self,
        client_id: impl Into<String>,
        data: Value,
        operation_generation: u64,
    ) -> Result<(), RelayClientError> {
        let client_id = client_id.into();
        let device_scope = self.message_scope(&client_id, &data);
        if operation_generation < self.operation_floor.load(Ordering::Acquire) {
            return Err(RelayClientError::Transport(
                "设备操作已被安全停止取代".to_owned(),
            ));
        }
        if device_scope
            .as_ref()
            .is_some_and(|(_, floor)| operation_generation < *floor)
        {
            return Err(RelayClientError::Transport(
                "设备操作已被安全停止取代".to_owned(),
            ));
        }
        self.commands
            .try_send(RelayCommand::SendMessage {
                client_id,
                data,
                operation_generation: Some(operation_generation),
                device_scope,
                wave_scope: None,
                reply: None,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => RelayClientError::QueueFull,
                mpsc::error::TrySendError::Closed(_) => RelayClientError::Stopped,
            })
    }

    /// 安全停止使用独立有界队列，并等待所有 clear / 归零帧实际写入 WebSocket。
    pub fn try_send_wave_operation(
        &self,
        client_id: &str,
        slot_id: &str,
        channel: Channel,
        data: Value,
        operation_generation: u64,
    ) -> Result<(), RelayClientError> {
        let device_scope = self.scope_generation(client_id, slot_id);
        if operation_generation < device_scope.1 {
            return Err(RelayClientError::Transport(
                "设备操作已被安全停止取代".to_owned(),
            ));
        }
        let scope = (client_id.to_owned(), slot_id.to_owned(), channel);
        let floor = self
            .wave_floors
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(&scope)
            .copied()
            .unwrap_or(0);
        self.commands
            .try_send(RelayCommand::SendMessage {
                client_id: client_id.to_owned(),
                data,
                operation_generation: Some(operation_generation),
                device_scope: Some(device_scope),
                wave_scope: Some((scope, floor)),
                reply: None,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => RelayClientError::QueueFull,
                mpsc::error::TrySendError::Closed(_) => RelayClientError::Stopped,
            })
    }

    /// 触控释放只使目标通道的旧波形失效，保留其他通道和强度调整。
    pub async fn clear_wave_channel(
        &self,
        client_id: &str,
        slot_id: &str,
        channel: Channel,
        request: Value,
        _operation_generation: u64,
    ) -> Result<(), RelayClientError> {
        {
            let mut floors = self
                .wave_floors
                .write()
                .unwrap_or_else(|error| error.into_inner());
            let scope = (client_id.to_owned(), slot_id.to_owned(), channel);
            if !floors.contains_key(&scope) && floors.len() >= MAX_WAVE_SCOPES {
                return Err(RelayClientError::QueueFull);
            }
            let floor = floors.entry(scope).or_default();
            *floor = floor.saturating_add(1);
        }
        // This barrier is narrower than a device stop: strength adjustments and
        // the other channel remain valid even when their generation is older.
        self.safety_write(client_id.into(), vec![request], None)
            .await
    }

    pub async fn safety_stop(
        &self,
        client_id: impl Into<String>,
        requests: Vec<Value>,
        operation_generation: u64,
    ) -> Result<(), RelayClientError> {
        self.invalidate_operations(operation_generation);
        self.safety_write(client_id.into(), requests, Some(operation_generation))
            .await
    }

    /// Stop only this device's queued waves and strength operations. Other
    /// devices on the same APP/Relay retain their pending confirmations.
    pub async fn safety_stop_device(
        &self,
        client_id: impl Into<String>,
        slot_id: impl Into<String>,
        requests: Vec<Value>,
        operation_generation: u64,
    ) -> Result<(), RelayClientError> {
        let client_id = client_id.into();
        let scope = (client_id.clone(), slot_id.into());
        {
            let mut floors = self
                .device_floors
                .write()
                .unwrap_or_else(|error| error.into_inner());
            if !floors.contains_key(&scope) && floors.len() >= MAX_WAVE_SCOPES {
                return Err(RelayClientError::QueueFull);
            }
            let floor = floors.entry(scope).or_default();
            *floor = (*floor).max(operation_generation);
        }
        self.safety_write(client_id, requests, None).await
    }

    fn message_scope(&self, client_id: &str, data: &Value) -> Option<(DeviceScope, u64)> {
        let slot_id = data.get("data")?.get("s")?.as_str()?;
        Some(self.scope_generation(client_id, slot_id))
    }

    fn scope_generation(&self, client_id: &str, slot_id: &str) -> (DeviceScope, u64) {
        let scope = (client_id.to_owned(), slot_id.to_owned());
        let floor = self
            .device_floors
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(&scope)
            .copied()
            .unwrap_or(0)
            .max(self.operation_floor.load(Ordering::Acquire));
        (scope, floor)
    }

    async fn safety_write(
        &self,
        client_id: String,
        requests: Vec<Value>,
        global_generation: Option<u64>,
    ) -> Result<(), RelayClientError> {
        let (reply, response) = oneshot::channel();
        self.safety_commands
            .try_send(RelayCommand::SafetyStop {
                client_id,
                requests,
                global_generation,
                reply,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => RelayClientError::QueueFull,
                mpsc::error::TrySendError::Closed(_) => RelayClientError::Stopped,
            })?;

        timeout(SAFETY_ACK_TIMEOUT, response)
            .await
            .map_err(|_| RelayClientError::Transport("等待安全停止写入确认超时".to_owned()))?
            .map_err(|_| RelayClientError::Stopped)?
    }

    pub fn shutdown_now(&self) {
        self.shutdown.cancel();
    }

    pub fn invalidate_operations(&self, operation_generation: u64) {
        let previous = self
            .operation_floor
            .fetch_max(operation_generation, Ordering::AcqRel);
        if operation_generation > previous {
            self.wave_floors
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .clear();
            // Older device barriers can be represented by the global floor;
            // keep only barriers newer than that global stop.
            self.device_floors
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .retain(|_, floor| *floor > operation_generation);
        }
    }
}

pub fn spawn_relay_client(
    event_sender: mpsc::Sender<RelayEvent>,
    command_capacity: usize,
) -> (RelayClientHandle, JoinHandle<()>) {
    let (command_sender, command_receiver) = mpsc::channel(command_capacity.max(1));
    let (safety_sender, safety_receiver) = mpsc::channel(SAFETY_COMMAND_CAPACITY);
    let shutdown = CancellationToken::new();
    let operation_floor = Arc::new(AtomicU64::new(0));
    let wave_floors = Arc::new(RwLock::new(BTreeMap::new()));
    let device_floors = Arc::new(RwLock::new(BTreeMap::new()));
    let handle = RelayClientHandle {
        commands: command_sender,
        safety_commands: safety_sender,
        shutdown: shutdown.clone(),
        operation_floor: Arc::clone(&operation_floor),
        wave_floors: Arc::clone(&wave_floors),
        device_floors: Arc::clone(&device_floors),
    };
    let task = tokio::spawn(run_relay_client(
        command_receiver,
        safety_receiver,
        event_sender,
        shutdown,
        operation_floor,
        wave_floors,
        device_floors,
    ));
    (handle, task)
}

enum SessionExit {
    Disconnected,
    Reconnect {
        endpoint: String,
        reply: oneshot::Sender<Result<(), RelayClientError>>,
    },
    Shutdown,
}

async fn run_relay_client(
    mut commands: mpsc::Receiver<RelayCommand>,
    mut safety_commands: mpsc::Receiver<RelayCommand>,
    events: mpsc::Sender<RelayEvent>,
    shutdown: CancellationToken,
    operation_floor: Arc<AtomicU64>,
    wave_floors: WaveFloors,
    device_floors: DeviceFloors,
) {
    let mut pending_connect: Option<(String, oneshot::Sender<Result<(), RelayClientError>>)> = None;
    let mut minimum_operation_generation = 0_u64;

    loop {
        let (endpoint, reply) = match pending_connect.take() {
            Some(pending) => pending,
            None => match tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                command = receive_command(&mut safety_commands, &mut commands) => command,
            } {
                Some(RelayCommand::Connect { endpoint, reply }) => (endpoint, reply),
                Some(RelayCommand::Disconnect { reply }) => {
                    let _ = reply.send(Ok(()));
                    continue;
                }
                Some(RelayCommand::SendMessage { reply, .. }) => {
                    if let Some(reply) = reply {
                        let _ = reply.send(Err(RelayClientError::NotConnected));
                    }
                    continue;
                }
                Some(RelayCommand::SafetyStop {
                    requests,
                    global_generation,
                    reply,
                    ..
                }) => {
                    if let Some(generation) = global_generation {
                        minimum_operation_generation = minimum_operation_generation.max(generation);
                    }
                    let result = if requests.is_empty() {
                        Ok(())
                    } else {
                        Err(RelayClientError::NotConnected)
                    };
                    let _ = reply.send(result);
                    continue;
                }
                None => break,
            },
        };

        let endpoint = match validate_endpoint(&endpoint) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                let _ = reply.send(Err(error.clone()));
                emit(
                    &events,
                    RelayEvent::Disconnected {
                        reason: error.to_string(),
                        retryable: false,
                    },
                )
                .await;
                continue;
            }
        };
        emit(
            &events,
            RelayEvent::Connecting {
                endpoint: endpoint.clone(),
            },
        )
        .await;

        let connection = tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                let _ = reply.send(Err(RelayClientError::Stopped));
                break;
            }
            connection = timeout(CONNECT_TIMEOUT, connect_async(&endpoint)) => connection,
        };
        let socket = match connection {
            Ok(Ok((socket, _))) => socket,
            Ok(Err(error)) => {
                let error = RelayClientError::Transport(describe_websocket_error(&error));
                let _ = reply.send(Err(error.clone()));
                emit(
                    &events,
                    RelayEvent::Disconnected {
                        reason: error.to_string(),
                        retryable: true,
                    },
                )
                .await;
                continue;
            }
            Err(_) => {
                let error = RelayClientError::ConnectTimeout;
                let _ = reply.send(Err(error.clone()));
                emit(
                    &events,
                    RelayEvent::Disconnected {
                        reason: error.to_string(),
                        retryable: true,
                    },
                )
                .await;
                continue;
            }
        };

        let _ = reply.send(Ok(()));
        emit(
            &events,
            RelayEvent::Connected {
                endpoint: endpoint.clone(),
            },
        )
        .await;

        match run_session(
            socket,
            &mut safety_commands,
            &mut commands,
            &events,
            &shutdown,
            &mut minimum_operation_generation,
            &operation_floor,
            &wave_floors,
            &device_floors,
        )
        .await
        {
            SessionExit::Reconnect { endpoint, reply } => {
                pending_connect = Some((endpoint, reply));
            }
            SessionExit::Disconnected => {}
            SessionExit::Shutdown => break,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    mut socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    safety_commands: &mut mpsc::Receiver<RelayCommand>,
    commands: &mut mpsc::Receiver<RelayCommand>,
    events: &mpsc::Sender<RelayEvent>,
    shutdown: &CancellationToken,
    minimum_operation_generation: &mut u64,
    operation_floor: &AtomicU64,
    wave_floors: &WaveFloors,
    device_floors: &DeviceFloors,
) -> SessionExit {
    let mut disconnect_reason = "Relay 连接已关闭".to_owned();
    let mut retryable = true;

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                let _ = close_socket(&mut socket).await;
                return SessionExit::Shutdown;
            }
            command = receive_command(safety_commands, commands) => {
                match command {
                    Some(RelayCommand::SendMessage {
                        client_id,
                        data,
                        operation_generation,
                        device_scope,
                        wave_scope,
                        reply,
                    }) => {
                        let current_floor = (*minimum_operation_generation)
                            .max(operation_floor.load(Ordering::Acquire));
                        if operation_generation
                            .is_some_and(|generation| generation < current_floor)
                            || device_scope.as_ref().is_some_and(|(scope,captured_floor)| {
                                let device_floor = device_floors.read().unwrap_or_else(|error|error.into_inner())
                                    .get(scope).copied().unwrap_or(0);
                                *captured_floor < device_floor
                                    || operation_generation.is_some_and(|generation| generation < device_floor)
                                    || (operation_generation.is_none() && *captured_floor < current_floor)
                            })
                            || wave_scope.as_ref().is_some_and(|(scope, generation)| {
                                *generation < wave_floors.read().unwrap_or_else(|error| error.into_inner())
                                    .get(scope).copied().unwrap_or(0)
                            })
                        {
                            if let Some(reply) = reply {
                                let _ = reply.send(Err(RelayClientError::Transport(
                                    "设备操作已被安全停止取代".to_owned(),
                                )));
                            }
                            continue;
                        }

                        let result = send_application_message(&mut socket, &client_id, data).await;
                        if let Some(reply) = reply {
                            let _ = reply.send(result.clone());
                        }
                        if let Err(error) = result {
                            disconnect_reason = error.to_string();
                            break;
                        }
                    }
                    Some(RelayCommand::SafetyStop {
                        client_id,
                        requests,
                        global_generation,
                        reply,
                    }) => {
                        if let Some(generation) = global_generation {
                            *minimum_operation_generation = (*minimum_operation_generation).max(generation);
                        }
                        let mut result = Ok(());
                        for request in requests {
                            if let Err(error) = send_application_message(
                                &mut socket,
                                &client_id,
                                request,
                            )
                            .await
                            {
                                result = Err(error);
                                break;
                            }
                        }
                        let failed = result.as_ref().err().cloned();
                        let _ = reply.send(result);
                        if let Some(error) = failed {
                            disconnect_reason = error.to_string();
                            break;
                        }
                    }
                    Some(RelayCommand::Disconnect { reply }) => {
                        let result = close_socket(&mut socket).await;
                        let _ = reply.send(result);
                        disconnect_reason = "用户已断开 Relay".to_owned();
                        retryable = false;
                        break;
                    }
                    Some(RelayCommand::Connect { endpoint, reply }) => {
                        let _ = close_socket(&mut socket).await;
                        emit(
                            events,
                            RelayEvent::Disconnected {
                                reason: "正在切换 Relay".to_owned(),
                                retryable: false,
                            },
                        )
                        .await;
                        return SessionExit::Reconnect { endpoint, reply };
                    }
                    None => {
                        let _ = close_socket(&mut socket).await;
                        return SessionExit::Shutdown;
                    }
                }
            }
            incoming = socket.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        handle_text(text.as_ref(), events).await;
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if let Err(error) = send_socket_message(&mut socket, Message::Pong(payload)).await {
                            disconnect_reason = error.to_string();
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(frame))) => {
                        if let Some(frame) = frame {
                            let code = u16::from(frame.code);
                            disconnect_reason = if code == 4002 {
                                "Relay 配对等待已超时".to_owned()
                            } else {
                                format!("Relay 已关闭（{code}：{}）", frame.reason)
                            };
                        }
                        break;
                    }
                    Some(Ok(Message::Binary(_))) | Some(Ok(Message::Frame(_))) => {}
                    Some(Err(error)) => {
                        disconnect_reason = format!(
                            "Relay 连接中断：{}",
                            describe_websocket_error(&error)
                        );
                        break;
                    }
                    None => break,
                }
            }
        }
    }

    emit(
        events,
        RelayEvent::Disconnected {
            reason: disconnect_reason,
            retryable,
        },
    )
    .await;
    SessionExit::Disconnected
}

fn describe_websocket_error(error: &WebSocketError) -> String {
    let message = error.to_string();
    if message.contains("close_notify")
        || message.contains("Connection reset without closing handshake")
    {
        "远端未完成 WebSocket/TLS 关闭握手便断开了连接".to_owned()
    } else {
        message
    }
}

async fn send_application_message(
    socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
    client_id: &str,
    data: Value,
) -> Result<(), RelayClientError> {
    let frame = json!({
        "type": "message",
        "clientId": client_id,
        "data": data,
    });
    send_socket_message(socket, Message::Text(frame.to_string().into())).await
}

async fn send_socket_message(
    socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
    message: Message,
) -> Result<(), RelayClientError> {
    timeout(SOCKET_WRITE_TIMEOUT, socket.send(message))
        .await
        .map_err(|_| RelayClientError::Transport("写入 Relay 超时".to_owned()))?
        .map_err(|error| RelayClientError::Transport(error.to_string()))
}

async fn close_socket(
    socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
) -> Result<(), RelayClientError> {
    timeout(SOCKET_CLOSE_TIMEOUT, socket.close(None))
        .await
        .map_err(|_| RelayClientError::Transport("关闭 Relay 连接超时".to_owned()))?
        .map_err(|error| RelayClientError::Transport(error.to_string()))
}

async fn receive_command(
    safety_commands: &mut mpsc::Receiver<RelayCommand>,
    commands: &mut mpsc::Receiver<RelayCommand>,
) -> Option<RelayCommand> {
    tokio::select! {
        biased;
        command = safety_commands.recv() => command,
        command = commands.recv() => command,
    }
}

async fn handle_text(text: &str, events: &mpsc::Sender<RelayEvent>) {
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(_) => return,
    };
    let frame_type = value.get("type").and_then(Value::as_str);
    let event = match frame_type {
        Some("hello") => value
            .get("clientId")
            .and_then(Value::as_str)
            .map(|client_id| RelayEvent::Hello {
                controller_id: client_id.to_owned(),
            }),
        Some("client_attached") => value
            .get("clientId")
            .and_then(Value::as_str)
            .map(|client_id| RelayEvent::ClientAttached {
                client_id: client_id.to_owned(),
            }),
        Some("client_disconnected") => {
            value
                .get("clientId")
                .and_then(Value::as_str)
                .map(|client_id| RelayEvent::ClientDisconnected {
                    client_id: client_id.to_owned(),
                })
        }
        Some("message") => match (
            value.get("clientId").and_then(Value::as_str),
            value.get("data"),
        ) {
            (Some(client_id), Some(data)) => Some(RelayEvent::Message {
                client_id: client_id.to_owned(),
                data: data.clone(),
            }),
            _ => None,
        },
        Some("heartbeat") => Some(RelayEvent::Heartbeat),
        Some("pong") => Some(RelayEvent::Pong {
            timestamp: value.get("ts").and_then(Value::as_i64),
        }),
        Some("idle_timeout") => Some(RelayEvent::IdleTimeout),
        Some("error") => Some(RelayEvent::RelayError {
            code: value
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            message: value
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }),
        _ => Some(RelayEvent::Unknown(value)),
    };
    if let Some(event) = event {
        emit(events, event).await;
    }
}

fn validate_endpoint(endpoint: &str) -> Result<String, RelayClientError> {
    let url = Url::parse(endpoint)
        .map_err(|error| RelayClientError::InvalidEndpoint(error.to_string()))?;
    if !matches!(url.scheme(), "ws" | "wss") || url.host_str().is_none() {
        return Err(RelayClientError::InvalidEndpoint(
            "仅支持包含主机名的 ws:// 或 wss:// 地址".to_owned(),
        ));
    }
    Ok(url.to_string())
}

async fn emit(events: &mpsc::Sender<RelayEvent>, event: RelayEvent) {
    let _ = timeout(EVENT_SEND_TIMEOUT, events.send(event)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    fn scoped_test_operation(request_id: &str, slot_id: &str) -> Value {
        json!({"t":"req","reqId":request_id,"m":"device.op","data":{"s":slot_id,"t":3,"c":0,"p":1,"v":1}})
    }

    #[tokio::test]
    async fn device_stop_preserves_other_slots_and_clients_queued_strength_and_waves() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (observed, observation) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let mut requests = Vec::new();
            while requests.len() < 7 {
                let Some(Ok(Message::Text(text))) = socket.next().await else {
                    panic!("expected queued application request");
                };
                let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
                let id = frame["data"]["reqId"].as_str().unwrap().to_owned();
                assert!(
                    !id.starts_with("old-a"),
                    "stopped slot's stale operation was sent: {id}"
                );
                requests.push(id);
            }
            observed.send(requests).unwrap();
            assert!(matches!(socket.next().await, Some(Ok(Message::Close(_)))));
        });
        let (events_tx, _events) = mpsc::channel(16);
        let (client, task) = spawn_relay_client(events_tx, 32);
        client.connect(format!("ws://{address}/v4")).await.unwrap();
        client
            .try_send_operation("app", scoped_test_operation("old-a-adjust", "a"), 0)
            .unwrap();
        client
            .try_send_wave_operation(
                "app",
                "a",
                Channel::A,
                scoped_test_operation("old-a-wave", "a"),
                0,
            )
            .unwrap();
        client
            .try_send_message("app", scoped_test_operation("old-a-message", "a"))
            .unwrap();
        client
            .try_send_operation("app", scoped_test_operation("keep-b-adjust", "b"), 0)
            .unwrap();
        client
            .try_send_wave_operation(
                "app",
                "b",
                Channel::B,
                scoped_test_operation("keep-b-wave", "b"),
                0,
            )
            .unwrap();
        client
            .try_send_operation(
                "other-app",
                scoped_test_operation("keep-other-adjust", "a"),
                0,
            )
            .unwrap();
        client
            .try_send_wave_operation(
                "other-app",
                "a",
                Channel::A,
                scoped_test_operation("keep-other-wave", "a"),
                0,
            )
            .unwrap();
        client
            .safety_stop_device(
                "app",
                "a",
                vec![json!({"t":"req","reqId":"clear-a","m":"device.op.clear","data":{"s":"a"}})],
                1,
            )
            .await
            .unwrap();
        assert_eq!(client.operation_floor.load(Ordering::Acquire), 0);
        assert!(
            client
                .try_send_operation("app", scoped_test_operation("old-a-late", "a"), 0)
                .is_err()
        );
        client
            .try_send_operation("app", scoped_test_operation("new-a-adjust", "a"), 1)
            .unwrap();
        client
            .try_send_wave_operation(
                "app",
                "a",
                Channel::A,
                scoped_test_operation("new-a-wave", "a"),
                1,
            )
            .unwrap();
        let requests = timeout(Duration::from_secs(2), observation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            requests,
            [
                "clear-a",
                "keep-b-adjust",
                "keep-b-wave",
                "keep-other-adjust",
                "keep-other-wave",
                "new-a-adjust",
                "new-a-wave"
            ]
        );
        client.disconnect().await.unwrap();
        server.await.unwrap();
        client.shutdown_now();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn channel_clear_preserves_queued_strength_and_the_other_channel_at_old_generation() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (observed, observation) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let mut requests = Vec::new();
            while requests.len() < 3 {
                let Some(Ok(Message::Text(text))) = socket.next().await else {
                    panic!("expected request");
                };
                let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
                let id = frame["data"]["reqId"].as_str().unwrap().to_owned();
                assert_ne!(id, "old-a-wave");
                requests.push(id);
            }
            observed.send(requests).unwrap();
            assert!(matches!(socket.next().await, Some(Ok(Message::Close(_)))));
        });
        let (events_tx, _events) = mpsc::channel(16);
        let (client, task) = spawn_relay_client(events_tx, 16);
        client.connect(format!("ws://{address}/v4")).await.unwrap();
        client
            .try_send_wave_operation(
                "app",
                "slot",
                Channel::A,
                scoped_test_operation("old-a-wave", "slot"),
                0,
            )
            .unwrap();
        client
            .try_send_operation("app", scoped_test_operation("keep-strength", "slot"), 0)
            .unwrap();
        client
            .try_send_wave_operation(
                "app",
                "slot",
                Channel::B,
                scoped_test_operation("keep-b-wave", "slot"),
                0,
            )
            .unwrap();
        client.clear_wave_channel("app","slot",Channel::A,json!({"t":"req","reqId":"clear-a","m":"device.op.clear","data":{"s":"slot","c":0}}),10).await.unwrap();
        assert_eq!(client.operation_floor.load(Ordering::Acquire), 0);
        assert_eq!(
            timeout(Duration::from_secs(2), observation)
                .await
                .unwrap()
                .unwrap(),
            ["clear-a", "keep-strength", "keep-b-wave"]
        );
        client.disconnect().await.unwrap();
        server.await.unwrap();
        client.shutdown_now();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn device_stop_scopes_are_bounded_and_global_stops_reclaim_older_scopes() {
        let (events_tx, _events) = mpsc::channel(16);
        let (client, task) = spawn_relay_client(events_tx, 16);
        for index in 0..MAX_WAVE_SCOPES {
            client
                .safety_stop_device("app", format!("slot-{index}"), vec![], 1)
                .await
                .unwrap();
        }
        assert_eq!(client.device_floors.read().unwrap().len(), MAX_WAVE_SCOPES);
        assert_eq!(
            client
                .safety_stop_device("app", "overflow", vec![], 1)
                .await,
            Err(RelayClientError::QueueFull)
        );
        client.invalidate_operations(2);
        assert!(client.device_floors.read().unwrap().is_empty());
        client
            .safety_stop_device("app", "fresh", vec![], 3)
            .await
            .unwrap();
        assert_eq!(client.device_floors.read().unwrap().len(), 1);
        client.invalidate_operations(2);
        assert_eq!(
            client.device_floors.read().unwrap().len(),
            1,
            "an older global generation cannot remove a newer scoped barrier"
        );
        client.shutdown_now();
        task.await.unwrap();
    }

    #[test]
    fn endpoint_requires_websocket_scheme() {
        assert_eq!(DEFAULT_RELAY_ENDPOINT, "wss://trex.dungeon-lab.cn/v4");
        assert!(validate_endpoint("https://example.test/v4").is_err());
        assert_eq!(
            validate_endpoint("wss://example.test/v4").unwrap(),
            "wss://example.test/v4"
        );
    }

    #[test]
    fn unexpected_tls_eof_has_a_user_facing_reason() {
        let error = WebSocketError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "peer closed connection without sending TLS close_notify",
        ));
        assert_eq!(
            describe_websocket_error(&error),
            "远端未完成 WebSocket/TLS 关闭握手便断开了连接"
        );
    }

    #[tokio::test]
    async fn fake_relay_handles_hello_pairing_message_and_close() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (pong_seen, wait_for_pong) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(
                    r#"{"type":"hello","clientId":"controller-1"}"#.to_owned().into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    r#"{"type":"client_attached","clientId":"app-1"}"#.to_owned().into(),
                ))
                .await
                .unwrap();

            let outbound = socket.next().await.unwrap().unwrap();
            let Message::Text(outbound) = outbound else {
                panic!("expected a text message");
            };
            let outbound: Value = serde_json::from_str(outbound.as_ref()).unwrap();
            assert_eq!(outbound["type"], "message");
            assert_eq!(outbound["clientId"], "app-1");
            assert_eq!(outbound["data"]["m"], "devices.get");

            socket
                .send(Message::Ping(vec![1, 2, 3].into()))
                .await
                .unwrap();
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                Message::Pong(_)
            ));
            let _ = pong_seen.send(());
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                Message::Close(_)
            ));
        });

        let (event_sender, mut events) = mpsc::channel(16);
        let (client, task) = spawn_relay_client(event_sender, 8);
        client.connect(format!("ws://{address}/v4")).await.unwrap();

        assert!(matches!(
            events.recv().await,
            Some(RelayEvent::Connecting { .. })
        ));
        assert!(matches!(
            events.recv().await,
            Some(RelayEvent::Connected { .. })
        ));
        assert_eq!(
            events.recv().await,
            Some(RelayEvent::Hello {
                controller_id: "controller-1".to_owned()
            })
        );
        assert_eq!(
            events.recv().await,
            Some(RelayEvent::ClientAttached {
                client_id: "app-1".to_owned()
            })
        );

        client
            .send_message(
                "app-1",
                json!({"t":"req","reqId":"request-1","m":"devices.get"}),
            )
            .await
            .unwrap();
        wait_for_pong.await.unwrap();
        client.disconnect().await.unwrap();
        server.await.unwrap();
        client.shutdown_now();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn safety_stop_preempts_and_invalidates_older_operations() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (verified, verification) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let mut clear_seen = false;
            while let Some(Ok(message)) = socket.next().await {
                let Message::Text(text) = message else {
                    continue;
                };
                let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
                let request_id = frame["data"]["reqId"].as_str();
                match request_id {
                    Some("clear-1") => clear_seen = true,
                    Some("old-1") if clear_seen => {
                        panic!("旧代次操作不得在安全清空后写入 socket")
                    }
                    Some("new-1") => {
                        assert!(clear_seen, "新代次操作应排在安全清空之后");
                        verified.send(()).unwrap();
                        break;
                    }
                    _ => {}
                }
            }
        });

        let (event_sender, _events) = mpsc::channel(16);
        let (client, task) = spawn_relay_client(event_sender, 8);
        client.connect(format!("ws://{address}/v4")).await.unwrap();
        client
            .try_send_operation(
                "app-1",
                json!({"t":"req","reqId":"old-1","m":"device.op"}),
                0,
            )
            .unwrap();
        client
            .safety_stop(
                "app-1",
                vec![json!({"t":"req","reqId":"clear-1","m":"device.op.clear"})],
                1,
            )
            .await
            .unwrap();
        assert!(matches!(
            client.try_send_operation(
                "app-1",
                json!({"t":"req","reqId":"stale-1","m":"device.op"}),
                0,
            ),
            Err(RelayClientError::Transport(_))
        ));
        client
            .try_send_operation(
                "app-1",
                json!({"t":"req","reqId":"new-1","m":"device.op"}),
                1,
            )
            .unwrap();

        timeout(Duration::from_secs(2), verification)
            .await
            .unwrap()
            .unwrap();
        server.await.unwrap();
        client.shutdown_now();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn touch_release_discards_only_target_channel_waves() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (verified, verification) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let mut clear_seen = false;
            let mut other_seen = false;
            let mut intensity_seen = false;
            while let Some(Ok(Message::Text(text))) = socket.next().await {
                let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
                match frame["data"]["reqId"].as_str() {
                    Some("clear-a") => clear_seen = true,
                    Some("old-a") => assert!(!clear_seen, "释放后不能发送目标通道的旧波形"),
                    Some("other-b") => other_seen = true,
                    Some("intensity-b") => intensity_seen = true,
                    Some("new-a") => {
                        assert!(clear_seen && other_seen && intensity_seen);
                        verified.send(()).unwrap();
                        break;
                    }
                    _ => {}
                }
            }
        });
        let (event_sender, _events) = mpsc::channel(16);
        let (client, task) = spawn_relay_client(event_sender, 8);
        client.connect(format!("ws://{address}/v4")).await.unwrap();
        client
            .try_send_wave_operation("app-1", "slot-a", Channel::A, json!({"reqId":"old-a"}), 0)
            .unwrap();
        client
            .try_send_wave_operation("app-1", "slot-a", Channel::B, json!({"reqId":"other-b"}), 0)
            .unwrap();
        client
            .try_send_operation("app-1", json!({"reqId":"intensity-b"}), 0)
            .unwrap();
        client
            .clear_wave_channel("app-1", "slot-a", Channel::A, json!({"reqId":"clear-a"}), 0)
            .await
            .unwrap();
        client
            .try_send_wave_operation("app-1", "slot-a", Channel::A, json!({"reqId":"new-a"}), 0)
            .unwrap();
        timeout(Duration::from_secs(2), verification)
            .await
            .unwrap()
            .unwrap();
        server.await.unwrap();
        client.shutdown_now();
        task.await.unwrap();
    }
}
