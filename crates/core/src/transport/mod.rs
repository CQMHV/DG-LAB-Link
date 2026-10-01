//! Device operations shared by WebSocket and direct BLE sessions.
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, Instant, timeout};
use tokio_util::sync::CancellationToken;

use crate::hub::{ChannelStatus, ConnectionState};
use crate::model::{Channel, WaveFrame};

pub mod ble {
    use super::*;
    pub async fn scan(_duration_ms: u64) -> Result<Vec<BluetoothDevice>, TransportError> {
        Err(TransportError::new(
            "bluetooth_unsupported",
            "蓝牙适配器尚未启用",
        ))
    }
    pub fn spawn(
        _device_id: String,
        _events: mpsc::Sender<SessionEvent>,
    ) -> (SessionHandle, tokio::task::JoinHandle<()>) {
        super::spawn_disabled()
    }
}
pub(crate) mod event_delivery;
pub mod v3;
pub mod v4;

#[derive(Clone)]
pub enum DeviceSession {
    V4(crate::dglab::client::RelayClientHandle),
    Local(SessionHandle),
}
impl DeviceSession {
    pub fn try_send(
        &self,
        address: &DeviceAddress,
        operation: DeviceOperation,
        generation: u64,
    ) -> Result<(), TransportError> {
        match self {
            Self::Local(handle) => handle.try_send(operation, generation),
            Self::V4(handle) => {
                let result = match operation {
                    DeviceOperation::Wave {
                        request_id,
                        slot_id,
                        channel,
                        frame,
                    } => handle.try_send_wave_operation(
                        &address.client_id,
                        &slot_id,
                        channel,
                        v4::append_pulse_request(
                            &request_id,
                            &slot_id,
                            channel,
                            &v4::encode_wave_frame(frame),
                        ),
                        generation,
                    ),
                    DeviceOperation::AdjustIntensity {
                        request_id,
                        slot_id,
                        channel,
                        delta,
                    } => handle.try_send_operation(
                        &address.client_id,
                        v4::add_intensity_request(&request_id, &slot_id, channel, delta),
                        generation,
                    ),
                };
                result.map_err(relay_error)
            }
        }
    }
    pub async fn stop(
        &self,
        address: &DeviceAddress,
        channel: Option<Channel>,
        zero: bool,
        generation: u64,
    ) -> Result<(), TransportError> {
        match self {
            Self::Local(handle) => {
                handle
                    .stop(address.slot_id.clone(), channel, zero, generation)
                    .await
            }
            Self::V4(handle) => match channel {
                Some(channel) => handle
                    .clear_wave_channel(
                        &address.client_id,
                        &address.slot_id,
                        channel,
                        v4::clear_channel_request(&address.slot_id, channel),
                        generation,
                    )
                    .await
                    .map_err(relay_error),
                None => handle
                    .safety_stop_device(
                        &address.client_id,
                        &address.slot_id,
                        v4::stop_operation_requests(&address.slot_id, zero),
                        generation,
                    )
                    .await
                    .map_err(relay_error),
            },
        }
    }
}
fn relay_error(error: crate::dglab::client::RelayClientError) -> TransportError {
    use crate::dglab::client::RelayClientError;
    TransportError::new(
        if matches!(error, RelayClientError::QueueFull) {
            "queue_busy"
        } else {
            "relay_error"
        },
        error.to_string(),
    )
}

pub const V4_CONNECTION_ID: &str = "ws-v4";
pub const V3_CONNECTION_ID: &str = "ws-v3";
pub const DEFAULT_V3_ENDPOINT: &str = "wss://ws.dungeon-lab.cn/";

pub fn bluetooth_connection_id(device_id: &str) -> String {
    format!("ble:{device_id}")
}

#[derive(Debug)]
pub enum TransportAction {
    Connect {
        transport: TransportKind,
        endpoint: String,
    },
    SetEndpoint {
        transport: TransportKind,
        endpoint: String,
    },
    RefreshPairing {
        connection_id: String,
    },
    Scan {
        duration_ms: u64,
    },
    ConnectBluetooth {
        device_id: String,
        parameters: BleParameters,
    },
    ConfigureBluetooth {
        device_id: String,
        parameters: BleParameters,
    },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    WsV4,
    WsV3,
    Ble,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeviceAddress {
    pub connection_id: String,
    pub client_id: String,
    pub slot_id: String,
}
impl DeviceAddress {
    pub fn control_id(&self) -> String {
        let legacy = format!(
            "{}:{}{}",
            self.client_id.len(),
            self.client_id,
            self.slot_id
        );
        if self.connection_id == V4_CONNECTION_ID {
            legacy
        } else {
            format!("{}:{legacy}", self.connection_id)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportConnectionSnapshot {
    pub connection_id: String,
    pub transport: TransportKind,
    pub state: ConnectionState,
    pub endpoint: String,
    pub controller_id: Option<String>,
    pub pairing_url: Option<String>,
    pub app_count: usize,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InitializationState {
    Initializing,
    #[default]
    Ready,
    Fault,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DeviceCapabilities {
    pub battery: bool,
    pub load_status: bool,
    pub soft_limits: bool,
    pub balance: bool,
    pub wheel_protection: bool,
    pub standard_mode: bool,
    pub operation_confirmation: bool,
}
impl Default for DeviceCapabilities {
    fn default() -> Self {
        Self {
            battery: true,
            load_status: true,
            soft_limits: false,
            balance: false,
            wheel_protection: false,
            standard_mode: false,
            operation_confirmation: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct BleParameters {
    pub max_strength_a: u16,
    pub max_strength_b: u16,
    pub frequency_balance_a: u8,
    pub frequency_balance_b: u8,
    pub strength_balance_a: u8,
    pub strength_balance_b: u8,
    pub wheel_protection_enabled: bool,
    pub wheel_protection_value: u8,
}
impl Default for BleParameters {
    fn default() -> Self {
        Self {
            max_strength_a: 100,
            max_strength_b: 100,
            frequency_balance_a: 160,
            frequency_balance_b: 160,
            strength_balance_a: 0,
            strength_balance_b: 0,
            wheel_protection_enabled: true,
            wheel_protection_value: 10,
        }
    }
}
impl BleParameters {
    pub fn validate(&self) -> Result<(), TransportError> {
        if self.max_strength_a > 200
            || self.max_strength_b > 200
            || !(1..=50).contains(&self.wheel_protection_value)
        {
            return Err(TransportError::new(
                "invalid_ble_parameters",
                "BLE 上限须为 0..200，旋钮保护值须为 1..50",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BluetoothDevice {
    pub device_id: String,
    pub name: String,
    pub rssi: Option<i16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDevice {
    pub id: String,
    pub slot_id: String,
    pub name: String,
    pub device_type: String,
    pub power: Option<u16>,
    pub intensity_a: u16,
    pub intensity_b: u16,
    pub intensity_limit_a: u16,
    pub intensity_limit_b: u16,
    pub channel_a_status: ChannelStatus,
    pub channel_b_status: ChannelStatus,
    pub initialization: InitializationState,
    pub capabilities: DeviceCapabilities,
    pub ble_parameters: Option<BleParameters>,
    /// BF has no device acknowledgement: "sent" differs from "confirmed".
    pub configuration_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct TransportError {
    pub code: String,
    pub message: String,
}
impl TransportError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
    pub fn stopped() -> Self {
        Self::new("transport_stopped", "设备连接已停止")
    }
    pub fn busy() -> Self {
        Self::new("queue_busy", "设备队列繁忙或操作已被停止取代")
    }
}

#[derive(Debug, Clone)]
pub enum DeviceOperation {
    AdjustIntensity {
        request_id: String,
        slot_id: String,
        channel: Channel,
        delta: i32,
    },
    Wave {
        request_id: String,
        slot_id: String,
        channel: Channel,
        frame: WaveFrame,
    },
}
impl DeviceOperation {
    pub fn request_id(&self) -> &str {
        match self {
            Self::AdjustIntensity { request_id, .. } | Self::Wave { request_id, .. } => request_id,
        }
    }
    pub fn scope(&self) -> (&str, Channel) {
        match self {
            Self::AdjustIntensity {
                slot_id, channel, ..
            }
            | Self::Wave {
                slot_id, channel, ..
            } => (slot_id, *channel),
        }
    }
}

#[derive(Debug)]
pub struct QueuedOperation {
    pub operation: DeviceOperation,
    pub generation: u64,
    pub wave_generation: Option<u64>,
    pub queued_at: Instant,
}
pub type SessionReply = oneshot::Sender<Result<(), TransportError>>;
#[derive(Debug)]
pub enum SessionCommand {
    Connect {
        endpoint: String,
        parameters: Option<BleParameters>,
        generation: u64,
        reply: SessionReply,
    },
    Disconnect {
        reply: SessionReply,
    },
    Operation(QueuedOperation),
    Stop {
        slot_id: String,
        channel: Option<Channel>,
        zero: bool,
        generation: u64,
        reply: SessionReply,
    },
    Configure {
        parameters: BleParameters,
        generation: u64,
        reply: SessionReply,
    },
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    Connection(TransportConnectionSnapshot),
    Device {
        connection_id: String,
        client_id: String,
        device: SessionDevice,
    },
    Removed {
        connection_id: String,
        client_id: String,
    },
    OperationFinished {
        connection_id: String,
        client_id: String,
        request_id: String,
        result: Result<(), TransportError>,
        confirmed: bool,
    },
    Log {
        connection_id: String,
        message: String,
        warning: bool,
    },
}

type WaveFloors = Arc<RwLock<BTreeMap<(String, Channel), u64>>>;
#[derive(Clone, Debug)]
pub struct SessionHandle {
    commands: mpsc::Sender<SessionCommand>,
    safety: mpsc::Sender<SessionCommand>,
    pub shutdown: CancellationToken,
    pub operation_floor: Arc<AtomicU64>,
    wave_floors: WaveFloors,
}
pub struct SessionQueues {
    pub commands: mpsc::Receiver<SessionCommand>,
    pub safety: mpsc::Receiver<SessionCommand>,
    pub handle: SessionHandle,
}
pub fn session_channel(capacity: usize) -> (SessionHandle, SessionQueues) {
    let (commands, rx) = mpsc::channel(capacity.max(1));
    let (safety, safety_rx) = mpsc::channel(16);
    let handle = SessionHandle {
        commands,
        safety,
        shutdown: CancellationToken::new(),
        operation_floor: Arc::new(AtomicU64::new(0)),
        wave_floors: Arc::new(RwLock::new(BTreeMap::new())),
    };
    (
        handle.clone(),
        SessionQueues {
            commands: rx,
            safety: safety_rx,
            handle,
        },
    )
}
impl SessionHandle {
    pub fn invalidate_operations(&self, generation: u64) {
        self.operation_floor.fetch_max(generation, Ordering::AcqRel);
    }
    pub fn is_current(&self, operation: &QueuedOperation) -> bool {
        operation.generation >= self.operation_floor.load(Ordering::Acquire)
            && operation.wave_generation.is_none_or(|generation| {
                let (slot, channel) = operation.operation.scope();
                generation == self.wave_floor(slot, channel)
                    && operation.queued_at.elapsed() < WaveFrame::DURATION
            })
    }
    fn wave_floor(&self, slot: &str, channel: Channel) -> u64 {
        *self
            .wave_floors
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(slot.to_owned(), channel))
            .unwrap_or(&0)
    }
    pub fn try_send(
        &self,
        operation: DeviceOperation,
        generation: u64,
    ) -> Result<(), TransportError> {
        if generation < self.operation_floor.load(Ordering::Acquire) {
            return Err(TransportError::busy());
        }
        let wave_generation = if matches!(operation, DeviceOperation::Wave { .. }) {
            let (slot, channel) = operation.scope();
            Some(self.wave_floor(slot, channel))
        } else {
            None
        };
        self.commands
            .try_send(SessionCommand::Operation(QueuedOperation {
                operation,
                generation,
                wave_generation,
                queued_at: Instant::now(),
            }))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => TransportError::busy(),
                _ => TransportError::stopped(),
            })
    }
    async fn request(
        &self,
        safety: bool,
        build: impl FnOnce(SessionReply) -> SessionCommand,
    ) -> Result<(), TransportError> {
        let (reply, rx) = oneshot::channel();
        let queue = if safety { &self.safety } else { &self.commands };
        queue.try_send(build(reply)).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => TransportError::busy(),
            _ => TransportError::stopped(),
        })?;
        timeout(Duration::from_secs(8), rx)
            .await
            .map_err(|_| TransportError::new("transport_timeout", "等待设备操作超时"))?
            .map_err(|_| TransportError::stopped())?
    }
    pub async fn connect(
        &self,
        endpoint: String,
        parameters: Option<BleParameters>,
        generation: u64,
    ) -> Result<(), TransportError> {
        self.request(false, |reply| SessionCommand::Connect {
            endpoint,
            parameters,
            generation,
            reply,
        })
        .await
    }
    pub async fn disconnect(&self) -> Result<(), TransportError> {
        self.request(true, |reply| SessionCommand::Disconnect { reply })
            .await
    }
    pub async fn configure(
        &self,
        parameters: BleParameters,
        generation: u64,
    ) -> Result<(), TransportError> {
        parameters.validate()?;
        self.request(false, |reply| SessionCommand::Configure {
            parameters,
            generation,
            reply,
        })
        .await
    }
    pub async fn stop(
        &self,
        slot_id: String,
        channel: Option<Channel>,
        zero: bool,
        generation: u64,
    ) -> Result<(), TransportError> {
        if let Some(channel) = channel {
            let mut floors = self.wave_floors.write().unwrap_or_else(|e| e.into_inner());
            if floors.len() >= 256 && !floors.contains_key(&(slot_id.clone(), channel)) {
                return Err(TransportError::busy());
            }
            let value = floors.entry((slot_id.clone(), channel)).or_default();
            *value = value.saturating_add(1);
        } else {
            self.invalidate_operations(generation);
        }
        self.request(true, |reply| SessionCommand::Stop {
            slot_id,
            channel,
            zero,
            generation,
            reply,
        })
        .await
    }
    pub fn shutdown_now(&self) {
        self.shutdown.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_preserve_v4_and_separate_protocol_namespaces() {
        let v4 = DeviceAddress {
            connection_id: V4_CONNECTION_ID.to_owned(),
            client_id: "app".to_owned(),
            slot_id: "slot".to_owned(),
        };
        assert_eq!(v4.control_id(), "3:appslot");
        let v3 = DeviceAddress {
            connection_id: V3_CONNECTION_ID.to_owned(),
            ..v4.clone()
        };
        let ble = DeviceAddress {
            connection_id: bluetooth_connection_id("native-id"),
            ..v4.clone()
        };
        assert_ne!(v3.control_id(), v4.control_id());
        assert_ne!(ble.control_id(), v3.control_id());
    }

    #[test]
    fn channel_generations_cancel_only_the_cleared_wave_scope() {
        let (handle, mut queues) = session_channel(4);
        let wave = |channel| DeviceOperation::Wave {
            request_id: "wave".to_owned(),
            slot_id: "slot".to_owned(),
            channel,
            frame: WaveFrame::silent(),
        };
        handle.try_send(wave(Channel::A), 1).unwrap();
        handle.try_send(wave(Channel::B), 1).unwrap();
        handle
            .wave_floors
            .write()
            .unwrap()
            .insert(("slot".to_owned(), Channel::A), 1);
        let SessionCommand::Operation(a) = queues.commands.try_recv().unwrap() else {
            panic!()
        };
        let SessionCommand::Operation(b) = queues.commands.try_recv().unwrap() else {
            panic!()
        };
        assert!(!handle.is_current(&a));
        assert!(handle.is_current(&b));
        handle.invalidate_operations(2);
        assert!(!handle.is_current(&b));
        handle.try_send(wave(Channel::B), 2).unwrap();
    }

    #[test]
    fn ble_defaults_are_valid_and_invalid_limits_are_rejected() {
        assert_eq!(
            serde_json::from_str::<BleParameters>("{}").unwrap(),
            BleParameters::default()
        );
        assert!(BleParameters::default().validate().is_ok());
        assert!(
            BleParameters {
                max_strength_a: 201,
                ..BleParameters::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            BleParameters {
                wheel_protection_value: 0,
                ..BleParameters::default()
            }
            .validate()
            .is_err()
        );
    }
}

fn spawn_disabled() -> (SessionHandle, tokio::task::JoinHandle<()>) {
    let (handle, mut queues) = session_channel(8);
    let task = tokio::spawn(async move {
        loop {
            let command = tokio::select! {biased;_ = queues.handle.shutdown.cancelled()=>break, command=queues.safety.recv()=>command, command=queues.commands.recv()=>command};
            match command {
                Some(SessionCommand::Disconnect { reply } | SessionCommand::Stop { reply, .. }) => {
                    let _ = reply.send(Ok(()));
                }
                Some(
                    SessionCommand::Connect { reply, .. } | SessionCommand::Configure { reply, .. },
                ) => {
                    let _ = reply.send(Err(TransportError::new(
                        "transport_unavailable",
                        "协议适配器尚未启用",
                    )));
                }
                Some(SessionCommand::Operation(_)) => {}
                None => break,
            }
        }
    });
    (handle, task)
}
