//! 郊狼 3.0 的独立、有界 BLE 会话。
//!
//! 基础强度只来自 B1；BF 的 GATT 写成功不等于设备确认。

#[path = "ble/backend.rs"]
mod backend;
#[path = "ble/protocol.rs"]
mod protocol;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant, MissedTickBehavior, timeout};

use crate::hub::{ChannelStatus, ConnectionState};
use crate::model::{Channel, WaveFrame};

use self::backend::{GattConnection, GattFactory, GattNotification, NativeFactory};
use self::event_delivery::EventSink;
use self::protocol::{Feedback, StrengthChange};
use super::event_delivery;
use super::{
    BleParameters, BluetoothDevice, DeviceCapabilities, DeviceOperation, InitializationState,
    QueuedOperation, SessionCommand, SessionDevice, SessionEvent, SessionHandle, SessionQueues,
    SessionReply, TransportConnectionSnapshot, TransportError, TransportKind,
    bluetooth_connection_id, session_channel,
};

const WRITE_TIMEOUT: Duration = Duration::from_secs(1);
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
const ACK_TIMEOUT: Duration = Duration::from_secs(2);
const EXTENSION_TIMEOUT: Duration = Duration::from_millis(600);
const MAX_PENDING_STRENGTH: usize = 32;
const SLOT_ID: &str = "ble";

pub async fn scan(duration_ms: u64) -> Result<Vec<BluetoothDevice>, TransportError> {
    backend::scan(duration_ms).await
}

pub fn spawn(
    device_id: String,
    events: mpsc::Sender<SessionEvent>,
) -> (SessionHandle, JoinHandle<()>) {
    spawn_with_factory(device_id, events, Arc::new(NativeFactory))
}

fn spawn_with_factory(
    device_id: String,
    events: mpsc::Sender<SessionEvent>,
    factory: Arc<dyn GattFactory>,
) -> (SessionHandle, JoinHandle<()>) {
    let (handle, queues) = session_channel(256);
    let task = tokio::spawn(run(device_id, events, factory, queues));
    (handle, task)
}

struct PendingStrength {
    operation: QueuedOperation,
    sequence: u8,
    deadline: Instant,
}

enum Phase {
    Query { sequence: u8, deadline: Instant },
    Mode { deadline: Instant },
    Wheel { expected: u8, deadline: Instant },
    Ready,
}

struct PendingConfiguration {
    parameters: BleParameters,
    previous: BleParameters,
    generation: u64,
    reply: SessionReply,
    expected_wheel: u8,
    deadline: Instant,
}

struct Session {
    connection: GattConnection,
    device: SessionDevice,
    generation: u64,
    connect_reply: Option<SessionReply>,
    phase: Phase,
    sequence: u8,
    strength_failed: bool,
    pending_strength: Option<PendingStrength>,
    strength_queue: VecDeque<QueuedOperation>,
    waves: [Option<QueuedOperation>; 2],
    last_waves: [Option<WaveFrame>; 2],
    configuration: Option<PendingConfiguration>,
    rollback: Option<BleParameters>,
    next_b0_write: Instant,
    safety_sequence: Option<u8>,
}

impl Session {
    fn ready(&self) -> bool {
        matches!(self.phase, Phase::Ready)
    }
    fn current(&self, handle: &SessionHandle) -> bool {
        self.generation >= handle.operation_floor.load(Ordering::Acquire)
    }
}

fn not_connected() -> TransportError {
    TransportError::new("device_disconnected", "蓝牙设备尚未连接或初始化未完成")
}

async fn emit(events: &EventSink, event: SessionEvent) {
    // Delivery is bounded and best effort. An observer never blocks native safety writes.
    events.push(event);
}

async fn device_event(events: &EventSink, device_id: &str, device: &SessionDevice) {
    emit(
        events,
        SessionEvent::Device {
            connection_id: bluetooth_connection_id(device_id),
            client_id: device_id.to_owned(),
            device: device.clone(),
        },
    )
    .await;
}

async fn log(events: &EventSink, device_id: &str, message: impl Into<String>) {
    emit(
        events,
        SessionEvent::Log {
            connection_id: bluetooth_connection_id(device_id),
            message: message.into(),
            warning: true,
        },
    )
    .await;
}

async fn finish(
    events: &EventSink,
    device_id: &str,
    operation: &QueuedOperation,
    result: Result<(), TransportError>,
    confirmed: bool,
) {
    emit(
        events,
        SessionEvent::OperationFinished {
            connection_id: bluetooth_connection_id(device_id),
            client_id: device_id.to_owned(),
            request_id: operation.operation.request_id().to_owned(),
            result,
            confirmed,
        },
    )
    .await;
}

async fn connection_event(
    events: &EventSink,
    device_id: &str,
    state: ConnectionState,
    error: Option<String>,
) {
    emit(
        events,
        SessionEvent::Connection(TransportConnectionSnapshot {
            connection_id: bluetooth_connection_id(device_id),
            transport: TransportKind::Ble,
            state,
            endpoint: device_id.to_owned(),
            controller_id: None,
            pairing_url: None,
            app_count: usize::from(state == ConnectionState::Connected),
            last_error: error,
        }),
    )
    .await;
}

async fn write(
    backend: &Arc<dyn backend::GattBackend>,
    bytes: &[u8],
) -> Result<(), TransportError> {
    timeout(WRITE_TIMEOUT, backend.write(bytes))
        .await
        .map_err(|_| {
            TransportError::new("bluetooth_write_timeout", "蓝牙写入超时，操作未自动重试")
        })?
}

fn parameter_bytes(parameters: &BleParameters) -> [u8; 7] {
    protocol::bf(
        [
            parameters.max_strength_a as u8,
            parameters.max_strength_b as u8,
        ],
        [
            parameters.frequency_balance_a,
            parameters.frequency_balance_b,
        ],
        [parameters.strength_balance_a, parameters.strength_balance_b],
    )
}

fn wheel_value(parameters: &BleParameters) -> u8 {
    if parameters.wheel_protection_enabled {
        parameters.wheel_protection_value
    } else {
        255
    }
}

fn device_from_connection(
    device_id: &str,
    connection: &GattConnection,
    parameters: BleParameters,
) -> SessionDevice {
    SessionDevice {
        id: device_id.to_owned(),
        slot_id: SLOT_ID.to_owned(),
        name: connection.info.name.clone(),
        device_type: "郊狼 3.0".to_owned(),
        power: connection.info.battery,
        intensity_a: 0,
        intensity_b: 0,
        intensity_limit_a: parameters.max_strength_a,
        intensity_limit_b: parameters.max_strength_b,
        channel_a_status: ChannelStatus::Idle,
        channel_b_status: ChannelStatus::Idle,
        initialization: InitializationState::Initializing,
        capabilities: DeviceCapabilities {
            battery: connection.info.battery.is_some() || connection.info.battery_notifications,
            load_status: connection.info.load_notifications,
            soft_limits: true,
            balance: true,
            wheel_protection: connection.info.firmware.is_some_and(|v| v >= 10),
            standard_mode: false,
            operation_confirmation: true,
        },
        ble_parameters: Some(parameters),
        configuration_status: None,
    }
}

async fn cancel_operations(session: &mut Session, events: &EventSink, device_id: &str) {
    if let Some(pending) = session.pending_strength.take() {
        finish(
            events,
            device_id,
            &pending.operation,
            Err(TransportError::busy()),
            false,
        )
        .await;
    }
    for pending in session.strength_queue.drain(..) {
        finish(
            events,
            device_id,
            &pending,
            Err(TransportError::busy()),
            false,
        )
        .await;
    }
    for wave in &mut session.waves {
        if let Some(pending) = wave.take() {
            finish(
                events,
                device_id,
                &pending,
                Err(TransportError::busy()),
                false,
            )
            .await;
        }
    }
    if let Some(config) = session.configuration.take() {
        let _ = config.reply.send(Err(TransportError::busy()));
    }
}

async fn close_session(session: &mut Session, events: &EventSink, device_id: &str) {
    cancel_operations(session, events, device_id).await;
    if let Some(reply) = session.connect_reply.take() {
        let _ = reply.send(Err(TransportError::stopped()));
    }
    let zero = protocol::b0(0, [StrengthChange::Zero; 2], [None, None]);
    let _ = write(&session.connection.backend, &zero).await;
    let _ = timeout(WRITE_TIMEOUT, session.connection.backend.disconnect()).await;
    emit(
        events,
        SessionEvent::Removed {
            connection_id: bluetooth_connection_id(device_id),
            client_id: device_id.to_owned(),
        },
    )
    .await;
}

async fn open_session(
    device_id: &str,
    parameters: BleParameters,
    generation: u64,
    reply: SessionReply,
    factory: Arc<dyn GattFactory>,
    queues: &mut SessionQueues,
    events: &EventSink,
) -> Option<Session> {
    if let Err(error) = parameters.validate() {
        let _ = reply.send(Err(error));
        return None;
    }
    if generation < queues.handle.operation_floor.load(Ordering::Acquire) {
        let _ = reply.send(Err(TransportError::busy()));
        return None;
    }
    connection_event(events, device_id, ConnectionState::Connecting, None).await;
    let id = device_id.to_owned();
    let mut opening = tokio::spawn(async move {
        timeout(OPEN_TIMEOUT, factory.open(&id))
            .await
            .map_err(|_| {
                TransportError::new("bluetooth_connect_timeout", "蓝牙设备初始化连接超时")
            })?
    });
    let mut poll = tokio::time::interval(Duration::from_millis(10));
    let result = loop {
        tokio::select! {
            biased;
            _ = queues.handle.shutdown.cancelled() => break None,
            command = queues.safety.recv() => {
                if let Some(SessionCommand::Stop { reply, .. } | SessionCommand::Disconnect { reply }) = command {
                    let _ = reply.send(Ok(()));
                }
                break None;
            }
            _ = poll.tick() => {
                if generation < queues.handle.operation_floor.load(Ordering::Acquire) { break None; }
            }
            result = &mut opening => break Some(result.unwrap_or_else(|_| Err(TransportError::stopped()))),
        }
    };
    let Some(result) = result else {
        let _ = reply.send(Err(TransportError::busy()));
        // A native connect may complete after cancellation. Own it until it can be disconnected.
        tokio::spawn(async move {
            if let Ok(Ok(connection)) = opening.await {
                let _ = timeout(WRITE_TIMEOUT, connection.backend.disconnect()).await;
            }
        });
        connection_event(events, device_id, ConnectionState::Disconnected, None).await;
        return None;
    };
    let connection = match result {
        Ok(connection) => connection,
        Err(error) => {
            connection_event(
                events,
                device_id,
                ConnectionState::Error,
                Some(error.message.clone()),
            )
            .await;
            let _ = reply.send(Err(error));
            return None;
        }
    };
    let device = device_from_connection(device_id, &connection, parameters);
    let mut session = Session {
        connection,
        device,
        generation,
        connect_reply: Some(reply),
        phase: Phase::Query {
            sequence: 1,
            deadline: Instant::now() + ACK_TIMEOUT,
        },
        sequence: 1,
        strength_failed: false,
        pending_strength: None,
        strength_queue: VecDeque::new(),
        waves: [None, None],
        last_waves: [None, None],
        configuration: None,
        rollback: None,
        next_b0_write: Instant::now(),
        safety_sequence: None,
    };
    if !session.current(&queues.handle) {
        close_session(&mut session, events, device_id).await;
        return None;
    }
    device_event(events, device_id, &session.device).await;
    if let Err(error) = write(&session.connection.backend, &protocol::query_strength(1)).await {
        if let Some(reply) = session.connect_reply.take() {
            let _ = reply.send(Err(error.clone()));
        }
        connection_event(
            events,
            device_id,
            ConnectionState::Error,
            Some(error.message),
        )
        .await;
        close_session(&mut session, events, device_id).await;
        return None;
    }
    Some(session)
}

async fn ready(session: &mut Session, events: &EventSink, device_id: &str, handle: &SessionHandle) {
    if !session.current(handle) {
        return;
    }
    session.phase = Phase::Ready;
    session.device.initialization = InitializationState::Ready;
    device_event(events, device_id, &session.device).await;
    connection_event(events, device_id, ConnectionState::Connected, None).await;
    if let Some(reply) = session.connect_reply.take() {
        let _ = reply.send(Ok(()));
    }
}

async fn begin_wheel_or_ready(
    session: &mut Session,
    events: &EventSink,
    device_id: &str,
    handle: &SessionHandle,
) {
    if !session.current(handle) {
        return;
    }
    if session.device.capabilities.wheel_protection {
        let value = wheel_value(
            session
                .device
                .ble_parameters
                .as_ref()
                .expect("BLE parameters"),
        );
        match write(&session.connection.backend, &[0xC4, value]).await {
            Ok(()) => {
                session.phase = Phase::Wheel {
                    expected: value,
                    deadline: Instant::now() + EXTENSION_TIMEOUT,
                }
            }
            Err(error) => {
                session.device.capabilities.wheel_protection = false;
                log(
                    events,
                    device_id,
                    format!("旋钮保护不可用：{}", error.message),
                )
                .await;
                ready(session, events, device_id, handle).await;
            }
        }
    } else {
        ready(session, events, device_id, handle).await;
    }
}

async fn after_query(
    session: &mut Session,
    events: &EventSink,
    device_id: &str,
    handle: &SessionHandle,
) -> Result<(), TransportError> {
    if !session.current(handle) {
        return Err(TransportError::busy());
    }
    let parameters = session
        .device
        .ble_parameters
        .as_ref()
        .expect("BLE parameters");
    write(&session.connection.backend, &parameter_bytes(parameters)).await?;
    session.device.configuration_status = Some("sent".to_owned());
    if !session.current(handle) {
        return Err(TransportError::busy());
    }
    match write(&session.connection.backend, &protocol::STANDARD_MODE).await {
        Ok(()) => {
            session.phase = Phase::Mode {
                deadline: Instant::now() + EXTENSION_TIMEOUT,
            }
        }
        Err(error) => {
            log(
                events,
                device_id,
                format!("标准模式初始化未确认：{}", error.message),
            )
            .await;
            begin_wheel_or_ready(session, events, device_id, handle).await;
        }
    }
    Ok(())
}

fn load_status(value: u8) -> ChannelStatus {
    match value {
        0 => ChannelStatus::Idle,
        1 => ChannelStatus::Ready,
        2 => ChannelStatus::Active,
        3 => ChannelStatus::Fault,
        4 => ChannelStatus::Disabled,
        _ => ChannelStatus::Disconnected,
    }
}

async fn notify(
    notification: GattNotification,
    session: &mut Session,
    events: &EventSink,
    device_id: &str,
    handle: &SessionHandle,
) -> Result<(), TransportError> {
    match notification {
        GattNotification::Battery(value) => session.device.power = Some(u16::from(value)),
        GattNotification::Load(values) => {
            session.device.channel_a_status = load_status(values[0]);
            session.device.channel_b_status = load_status(values[1]);
        }
        GattNotification::Message(bytes) => match protocol::feedback(&bytes) {
            Feedback::Strength { sequence, values } => {
                let query_matches = matches!(session.phase, Phase::Query { sequence: expected, .. } if sequence == expected);
                let adjustment_matches = session
                    .pending_strength
                    .as_ref()
                    .is_some_and(|p| p.sequence == sequence);
                let safety_matches = session.safety_sequence == Some(sequence) && values == [0, 0];
                if sequence != 0 && !query_matches && !adjustment_matches && !safety_matches {
                    // A late command acknowledgement must not revive state from before a stop.
                    return Ok(());
                }
                // Sequence zero is a physical wheel event, and is authoritative too.
                session.device.intensity_a = u16::from(values[0]);
                session.device.intensity_b = u16::from(values[1]);
                if safety_matches {
                    session.safety_sequence = None;
                }
                if query_matches {
                    after_query(session, events, device_id, handle).await?;
                } else if adjustment_matches {
                    let pending = session
                        .pending_strength
                        .take()
                        .expect("pending strength checked");
                    let result = if handle.is_current(&pending.operation) {
                        Ok(())
                    } else {
                        Err(TransportError::busy())
                    };
                    finish(events, device_id, &pending.operation, result, true).await;
                }
            }
            Feedback::StandardMode if matches!(session.phase, Phase::Mode { .. }) => {
                session.device.capabilities.standard_mode = true;
                begin_wheel_or_ready(session, events, device_id, handle).await;
            }
            Feedback::WheelProtection(value) => {
                if matches!(session.phase, Phase::Wheel { expected, .. } if expected == value) {
                    ready(session, events, device_id, handle).await;
                }
                if session
                    .configuration
                    .as_ref()
                    .is_some_and(|c| c.expected_wheel == value)
                {
                    let config = session.configuration.take().expect("configuration checked");
                    session.device.ble_parameters = Some(config.parameters.clone());
                    session.device.intensity_limit_a = config.parameters.max_strength_a;
                    session.device.intensity_limit_b = config.parameters.max_strength_b;
                    session.device.configuration_status = Some("sent".to_owned());
                    session.rollback = None;
                    let _ = config.reply.send(Ok(()));
                }
            }
            _ => return Ok(()),
        },
    }
    device_event(events, device_id, &session.device).await;
    Ok(())
}

async fn configure(
    session: &mut Session,
    parameters: BleParameters,
    generation: u64,
    reply: SessionReply,
    handle: &SessionHandle,
    events: &EventSink,
    device_id: &str,
) {
    if !session.ready() || session.configuration.is_some() {
        let _ = reply.send(Err(not_connected()));
        return;
    }
    if generation < handle.operation_floor.load(Ordering::Acquire) {
        let _ = reply.send(Err(TransportError::busy()));
        return;
    }
    if let Err(error) = parameters.validate() {
        let _ = reply.send(Err(error));
        return;
    }
    let previous = session
        .device
        .ble_parameters
        .clone()
        .expect("BLE parameters");
    session.rollback = Some(previous.clone());
    let result = write(&session.connection.backend, &parameter_bytes(&parameters)).await;
    if let Err(error) = result {
        if generation < handle.operation_floor.load(Ordering::Acquire) {
            // Defer compensation until the safety write has run.
            let _ = reply.send(Err(error));
            return;
        }
        if write(&session.connection.backend, &parameter_bytes(&previous))
            .await
            .is_err()
        {
            session.rollback = None;
            session.device.initialization = InitializationState::Fault;
            session.last_waves = [None, None];
            let _ = write(
                &session.connection.backend,
                &protocol::b0(0, [StrengthChange::Unchanged; 2], [None, None]),
            )
            .await;
        } else {
            session.rollback = None;
        }
        let _ = reply.send(Err(error));
        return;
    }
    if generation < handle.operation_floor.load(Ordering::Acquire) {
        // Safety owns output now; restore BF on the next tick after its zero write.
        let _ = reply.send(Err(TransportError::busy()));
        return;
    }
    let expected_wheel = wheel_value(&parameters);
    if session.device.capabilities.wheel_protection && expected_wheel != wheel_value(&previous) {
        if write(&session.connection.backend, &[0xC4, expected_wheel])
            .await
            .is_ok()
        {
            session.configuration = Some(PendingConfiguration {
                parameters,
                previous,
                generation,
                reply,
                expected_wheel,
                deadline: Instant::now() + EXTENSION_TIMEOUT,
            });
            return;
        }
        session.device.capabilities.wheel_protection = false;
        log(events, device_id, "旋钮保护写入失败，已禁用该扩展能力").await;
    }
    session.device.intensity_limit_a = parameters.max_strength_a;
    session.device.intensity_limit_b = parameters.max_strength_b;
    session.device.ble_parameters = Some(parameters);
    session.device.configuration_status = Some("sent".to_owned());
    session.rollback = None;
    device_event(events, device_id, &session.device).await;
    let _ = reply.send(Ok(()));
}

async fn stop(
    session: &mut Session,
    channel: Option<Channel>,
    zero: bool,
    events: &EventSink,
    device_id: &str,
) -> Result<(), TransportError> {
    if channel.is_none() || zero {
        cancel_operations(session, events, device_id).await;
        session.last_waves = [None, None];
    } else if let Some(channel) = channel {
        let index = channel.as_v4() as usize;
        session.last_waves[index] = None;
        if let Some(pending) = session.waves[index].take() {
            finish(
                events,
                device_id,
                &pending,
                Err(TransportError::busy()),
                false,
            )
            .await;
        }
    }
    let changes = if zero {
        [StrengthChange::Zero; 2]
    } else {
        [StrengthChange::Unchanged; 2]
    };
    // Safety does not wait for the old B1. Zero is idempotent and has no ambiguous relative delta.
    let sequence = if zero {
        session.sequence = protocol::next_sequence(session.sequence);
        session.safety_sequence = Some(session.sequence);
        session.sequence
    } else {
        0
    };
    let bytes = protocol::b0(sequence, changes, session.last_waves);
    write(&session.connection.backend, &bytes).await
}

async fn tick(
    session: &mut Session,
    events: &EventSink,
    device_id: &str,
    handle: &SessionHandle,
) -> Result<(), TransportError> {
    if !session.current(handle) && !session.ready() {
        return Err(TransportError::busy());
    }
    match session.phase {
        Phase::Query { deadline, .. } if Instant::now() >= deadline => {
            return Err(TransportError::new(
                "bluetooth_strength_timeout",
                "读取蓝牙基础强度超时，输出未启用",
            ));
        }
        Phase::Mode { deadline } if Instant::now() >= deadline => {
            log(events, device_id, "标准模式未确认，已禁用该扩展能力").await;
            begin_wheel_or_ready(session, events, device_id, handle).await;
        }
        Phase::Wheel { deadline, .. } if Instant::now() >= deadline => {
            session.device.capabilities.wheel_protection = false;
            log(events, device_id, "旋钮保护未确认，已禁用该扩展能力").await;
            ready(session, events, device_id, handle).await;
        }
        _ => {}
    }
    if session.configuration.is_none()
        && let Some(parameters) = session.rollback.take()
    {
        if let Err(error) = write(&session.connection.backend, &parameter_bytes(&parameters)).await
        {
            session.device.initialization = InitializationState::Fault;
            session.last_waves = [None, None];
            return Err(TransportError::new(
                "bluetooth_rollback_failed",
                format!("蓝牙配置恢复失败：{}", error.message),
            ));
        }
        session.device.intensity_limit_a = parameters.max_strength_a;
        session.device.intensity_limit_b = parameters.max_strength_b;
        session.device.ble_parameters = Some(parameters);
        session.device.configuration_status = Some("sent".to_owned());
        device_event(events, device_id, &session.device).await;
    }
    if session
        .pending_strength
        .as_ref()
        .is_some_and(|p| Instant::now() >= p.deadline || !handle.is_current(&p.operation))
    {
        let pending = session
            .pending_strength
            .take()
            .expect("pending strength checked");
        session.strength_failed = true;
        finish(
            events,
            device_id,
            &pending.operation,
            Err(TransportError::new(
                "bluetooth_strength_timeout",
                "强度反馈超时或操作被停止取代，未自动重试",
            )),
            false,
        )
        .await;
        for pending in session.strength_queue.drain(..) {
            finish(
                events,
                device_id,
                &pending,
                Err(TransportError::busy()),
                false,
            )
            .await;
        }
    }
    if session.configuration.as_ref().is_some_and(|c| {
        Instant::now() >= c.deadline
            || c.generation < handle.operation_floor.load(Ordering::Acquire)
    }) {
        let config = session.configuration.take().expect("configuration checked");
        // The extension is optional; a timeout records unsupported capability, BF remains sent.
        if config.generation < handle.operation_floor.load(Ordering::Acquire) {
            session.rollback = Some(config.previous);
            let _ = config.reply.send(Err(TransportError::busy()));
        } else {
            session.device.capabilities.wheel_protection = false;
            session.device.intensity_limit_a = config.parameters.max_strength_a;
            session.device.intensity_limit_b = config.parameters.max_strength_b;
            session.device.ble_parameters = Some(config.parameters);
            session.device.configuration_status = Some("sent".to_owned());
            session.rollback = None;
            log(events, device_id, "旋钮保护未确认，BF 参数已下发").await;
            device_event(events, device_id, &session.device).await;
            let _ = config.reply.send(Ok(()));
        }
    }
    if !session.ready() || session.device.initialization != InitializationState::Ready {
        return Ok(());
    }
    if Instant::now() < session.next_b0_write {
        return Ok(());
    }
    let mut selected = [None, None];
    for (index, queued) in session.waves.iter_mut().enumerate() {
        if let Some(operation) = queued.take() {
            if handle.is_current(&operation) {
                if let DeviceOperation::Wave { frame, .. } = operation.operation {
                    session.last_waves[index] = Some(frame);
                }
                selected[index] = Some(operation);
            } else {
                finish(
                    events,
                    device_id,
                    &operation,
                    Err(TransportError::busy()),
                    false,
                )
                .await;
                session.last_waves[index] = None;
            }
        } else {
            session.last_waves[index] = None;
        }
    }
    let adjustment = if session.pending_strength.is_none() && !session.strength_failed {
        let mut selected = None;
        while let Some(queued) = session.strength_queue.pop_front() {
            if handle.is_current(&queued) {
                selected = Some(queued);
                break;
            }
            finish(
                events,
                device_id,
                &queued,
                Err(TransportError::busy()),
                false,
            )
            .await;
        }
        selected
    } else {
        None
    };
    if adjustment.is_none() && selected.iter().all(Option::is_none) {
        return Ok(());
    }
    let mut sequence = 0;
    let mut changes = [StrengthChange::Unchanged; 2];
    if let Some(queued) = adjustment.as_ref()
        && let DeviceOperation::AdjustIntensity { channel, delta, .. } = queued.operation
    {
        if let Some(relative) = protocol::relative(channel, delta) {
            session.sequence = protocol::next_sequence(session.sequence);
            sequence = session.sequence;
            changes = relative;
        } else {
            finish(
                events,
                device_id,
                queued,
                Err(TransportError::new(
                    "invalid_intensity_delta",
                    "蓝牙相对强度须为 -200..200 且非零",
                )),
                false,
            )
            .await;
        }
    }
    // No relative changes are cached: subsequent wave writes always use unchanged strength.
    let bytes = protocol::b0(sequence, changes, session.last_waves);
    // Anchor to the start of native I/O, so its latency does not accumulate phase drift.
    // A/B mailboxes share this deadline and are always consumed into one packet.
    session.next_b0_write = Instant::now() + WaveFrame::DURATION;
    let result = write(&session.connection.backend, &bytes).await;
    for queued in selected.into_iter().flatten() {
        finish(events, device_id, &queued, result.clone(), false).await;
    }
    if let Some(operation) = adjustment {
        if result.is_ok() && sequence != 0 {
            session.pending_strength = Some(PendingStrength {
                operation,
                sequence,
                deadline: Instant::now() + ACK_TIMEOUT,
            });
        } else if result.is_err() {
            finish(events, device_id, &operation, result.clone(), false).await;
        }
    }
    result
}

async fn run(
    device_id: String,
    events: mpsc::Sender<SessionEvent>,
    factory: Arc<dyn GattFactory>,
    mut queues: SessionQueues,
) {
    let events = EventSink::new(events);
    let mut session: Option<Session> = None;
    // Hub produces 100ms frames. Service its queue promptly instead of adding a second
    // 100ms phase (which would expire a fresh frame at the admission boundary).
    let mut interval = tokio::time::interval(Duration::from_millis(10));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        enum Input {
            Command(Option<SessionCommand>),
            Notification(Option<GattNotification>),
            Tick,
            Shutdown,
        }
        let input = tokio::select! {
            biased;
            _ = queues.handle.shutdown.cancelled() => Input::Shutdown,
            command = queues.safety.recv() => Input::Command(command),
            notification = async { session.as_mut().expect("session guard").connection.notifications.next().await }, if session.is_some() => Input::Notification(notification),
            _ = interval.tick(), if session.is_some() => Input::Tick,
            command = queues.commands.recv() => Input::Command(command),
        };
        let mut failure = None;
        match input {
            Input::Shutdown | Input::Command(None) => break,
            Input::Command(Some(SessionCommand::Connect {
                endpoint,
                parameters,
                generation,
                reply,
            })) => {
                if session.is_some() || endpoint != device_id {
                    let _ = reply.send(Err(TransportError::new(
                        "bluetooth_already_connected",
                        "该 BLE 会话已连接或设备标识不匹配",
                    )));
                } else {
                    session = open_session(
                        &device_id,
                        parameters.unwrap_or_default(),
                        generation,
                        reply,
                        Arc::clone(&factory),
                        &mut queues,
                        &events,
                    )
                    .await;
                }
            }
            Input::Command(Some(SessionCommand::Disconnect { reply })) => {
                if let Some(mut connected) = session.take() {
                    close_session(&mut connected, &events, &device_id).await;
                }
                connection_event(&events, &device_id, ConnectionState::Disconnected, None).await;
                let _ = reply.send(Ok(()));
            }
            Input::Command(Some(SessionCommand::Stop {
                slot_id,
                channel,
                zero,
                generation,
                reply,
            })) => {
                if slot_id != SLOT_ID {
                    let _ = reply.send(Err(not_connected()));
                    continue;
                }
                let result = if let Some(connected) = session.as_mut() {
                    if channel.is_none() {
                        connected.generation = generation;
                    }
                    let result = stop(connected, channel, zero, &events, &device_id).await;
                    if !connected.ready() {
                        if let Some(pending) = connected.connect_reply.take() {
                            let _ = pending.send(Err(TransportError::busy()));
                        }
                        failure = Some(TransportError::busy());
                    }
                    result
                } else {
                    Ok(())
                };
                let _ = reply.send(result);
            }
            Input::Command(Some(SessionCommand::Configure {
                parameters,
                generation,
                reply,
            })) => {
                if let Some(connected) = session.as_mut() {
                    configure(
                        connected,
                        parameters,
                        generation,
                        reply,
                        &queues.handle,
                        &events,
                        &device_id,
                    )
                    .await;
                } else {
                    let _ = reply.send(Err(not_connected()));
                }
            }
            Input::Command(Some(SessionCommand::Operation(operation))) => {
                if !queues.handle.is_current(&operation) {
                    finish(
                        &events,
                        &device_id,
                        &operation,
                        Err(TransportError::busy()),
                        false,
                    )
                    .await;
                } else if let Some(connected) = session
                    .as_mut()
                    .filter(|s| s.ready() && s.device.initialization == InitializationState::Ready)
                {
                    let (slot, channel) = operation.operation.scope();
                    if slot != SLOT_ID {
                        finish(&events, &device_id, &operation, Err(not_connected()), false).await;
                    } else if matches!(operation.operation, DeviceOperation::Wave { .. }) {
                        let index = channel.as_v4() as usize;
                        if let Some(previous) = connected.waves[index].replace(operation) {
                            finish(
                                &events,
                                &device_id,
                                &previous,
                                Err(TransportError::busy()),
                                false,
                            )
                            .await;
                        }
                    } else if connected.strength_failed
                        || connected.strength_queue.len() >= MAX_PENDING_STRENGTH
                    {
                        finish(
                            &events,
                            &device_id,
                            &operation,
                            Err(TransportError::new(
                                "bluetooth_strength_blocked",
                                "强度调整已因未确认操作暂停，请重新连接",
                            )),
                            false,
                        )
                        .await;
                    } else {
                        connected.strength_queue.push_back(operation);
                    }
                } else {
                    finish(&events, &device_id, &operation, Err(not_connected()), false).await;
                }
            }
            Input::Notification(Some(notification)) => {
                if let Some(connected) = session.as_mut() {
                    failure = notify(notification, connected, &events, &device_id, &queues.handle)
                        .await
                        .err();
                }
            }
            Input::Notification(None) => {
                failure = Some(TransportError::new(
                    "bluetooth_disconnected",
                    "蓝牙通知链路已断开",
                ))
            }
            Input::Tick => {
                if let Some(connected) = session.as_mut() {
                    failure = if events.overflowed() {
                        Some(TransportError::new(
                            "bluetooth_event_overflow",
                            "蓝牙事件观察队列已满，已停止设备",
                        ))
                    } else {
                        tick(connected, &events, &device_id, &queues.handle)
                            .await
                            .err()
                    };
                }
            }
        }
        if let Some(error) = failure {
            if let Some(mut connected) = session.take() {
                if let Some(reply) = connected.connect_reply.take() {
                    let _ = reply.send(Err(error.clone()));
                }
                close_session(&mut connected, &events, &device_id).await;
            }
            connection_event(
                &events,
                &device_id,
                ConnectionState::Error,
                Some(error.message),
            )
            .await;
        }
    }
    if let Some(mut connected) = session {
        close_session(&mut connected, &events, &device_id).await;
    }
    connection_event(&events, &device_id, ConnectionState::Disconnected, None).await;
}

#[cfg(test)]
#[path = "ble/tests.rs"]
mod tests;
