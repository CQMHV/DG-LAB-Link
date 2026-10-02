//! Device operations shared by WebSocket and direct BLE sessions.
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, Instant, timeout};
use tokio_util::sync::CancellationToken;

use crate::model::{Channel, WaveFrame};

pub(crate) type StopCompletion =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), TransportError>> + Send>>;

pub mod ble;
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
        self.request_stop(address, channel, zero, generation)?.await
    }

    /// Install the target barrier and enqueue priority cleanup before returning.
    /// Waiting for a transport write must never cancel already admitted cleanup.
    pub(crate) fn request_stop(
        &self,
        address: &DeviceAddress,
        channel: Option<Channel>,
        zero: bool,
        generation: u64,
    ) -> Result<StopCompletion, TransportError> {
        match self {
            Self::Local(handle) => Ok(Box::pin(handle.request_stop(
                address.slot_id.clone(),
                channel,
                zero,
                generation,
            )?)),
            Self::V4(handle) => match channel {
                Some(channel) => {
                    let completion = handle
                        .request_clear_wave_channel(
                            &address.client_id,
                            &address.slot_id,
                            channel,
                            v4::clear_channel_request(&address.slot_id, channel),
                            generation,
                        )
                        .map_err(relay_error)?;
                    Ok(Box::pin(
                        async move { completion.await.map_err(relay_error) },
                    ))
                }
                None => {
                    let completion = handle
                        .request_stop_device(
                            address.client_id.clone(),
                            address.slot_id.clone(),
                            v4::stop_operation_requests(&address.slot_id, zero),
                            generation,
                        )
                        .map_err(relay_error)?;
                    Ok(Box::pin(
                        async move { completion.await.map_err(relay_error) },
                    ))
                }
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

pub use dg_lab_link_contracts::transport::*;

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
        self.request_stop(slot_id, channel, zero, generation)?.await
    }

    pub(crate) fn request_stop(
        &self,
        slot_id: String,
        channel: Option<Channel>,
        zero: bool,
        generation: u64,
    ) -> Result<
        impl std::future::Future<Output = Result<(), TransportError>> + Send + 'static,
        TransportError,
    > {
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
        let (reply, response) = oneshot::channel();
        self.safety
            .try_send(SessionCommand::Stop {
                slot_id,
                channel,
                zero,
                generation,
                reply,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => TransportError::busy(),
                _ => TransportError::stopped(),
            })?;
        Ok(async move {
            timeout(Duration::from_secs(8), response)
                .await
                .map_err(|_| TransportError::new("transport_timeout", "等待设备操作超时"))?
                .map_err(|_| TransportError::stopped())?
        })
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
