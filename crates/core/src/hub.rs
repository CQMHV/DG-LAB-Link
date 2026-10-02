use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use thiserror::Error;
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::time::{Duration, Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::dglab::client::{
    RelayClientError, RelayClientHandle, RelayEvent, RelaySessionEvent, spawn_relay_client,
};
use crate::dglab::v4::pairing_url as build_pairing_url;
use crate::model::{Channel, WaveFrame};
use crate::sources::{WaveSource, WaveformConfig, builtin_registry};
#[cfg(test)]
use crate::transport::DeviceCapabilities;
use crate::transport::v4::devices_get_request;
#[cfg(test)]
use crate::transport::v4::{append_pulse_request, clear_channel_request, stop_operation_requests};
use crate::transport::{
    BluetoothDevice, DEFAULT_V3_ENDPOINT, DeviceAddress, DeviceOperation, DeviceSession,
    InitializationState, SessionEvent, SessionHandle, StopCompletion, TransportAction,
    TransportConnectionSnapshot, TransportError, TransportKind, V3_CONNECTION_ID, V4_CONNECTION_ID,
    bluetooth_connection_id,
};
use dg_lab_link_plugin_runtime::PluginManager;
use dg_lab_link_plugin_sdk::Binding as PluginBinding;

const HUB_COMMAND_CAPACITY: usize = 64;
const HUB_SAFETY_COMMAND_CAPACITY: usize = 8;
const RELAY_COMMAND_CAPACITY: usize = 256;
const RELAY_EVENT_CAPACITY: usize = 128;
const MAX_OUTPUT_DEVICES: usize = 32;
const MAX_PENDING_WAVE_OPERATIONS: usize = 256;
const WAVE_OPERATION_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_LOGS: usize = 100;
const RELAY_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const RELAY_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
const RELAY_RECONNECT_MAX_DELAY_SECONDS: u64 = 30;
const MAX_CUSTOM_WAVEFORMS: usize = 128;
const MAX_CUSTOM_WAVEFORM_FRAMES: usize = 16_384;
const FIXED_WAVEFORM_SOURCE_ID: &str = "source-fixed-waveform";
#[cfg(test)]
const TOUCH_SOURCE_ID: &str = "source-touch";
#[cfg(test)]
const AUDIO_SOURCE_ID: &str = "source-audio";

pub use dg_lab_link_contracts::hub::*;

fn initial_snapshot(
    endpoint: String,
    sources: Vec<SourceSnapshot>,
    custom_waveforms: Vec<CustomWaveformSnapshot>,
    default_source_id: Option<String>,
    safety: SafetySnapshot,
) -> HubSnapshot {
    HubSnapshot {
        revision: 0,
        connections: vec![TransportConnectionSnapshot {
            connection_id: V4_CONNECTION_ID.to_owned(),
            transport: TransportKind::WsV4,
            state: ConnectionState::Disconnected,
            endpoint,
            controller_id: None,
            pairing_url: None,
            app_count: 0,
            last_error: None,
        }],
        bluetooth: Vec::new(),
        devices: Vec::new(),
        sync_all_devices: false,
        output_device_count: 0,
        sources,
        plugins: Vec::new(),
        source_bindings: Vec::new(),
        custom_waveforms,
        default_source_id,
        output: OutputSnapshot {
            state: OutputState::Idle,
            frames_sent: 0,
            last_error: None,
        },
        safety,
        logs: Vec::new(),
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum HubError {
    #[error("连接中枢已停止")]
    Stopped,
    #[error("尚未连接 DG-LAB APP")]
    NotConnected,
    #[error("尚未检测到可用的 DG-LAB 设备")]
    NoDevice,
    #[error("指定的 DG-LAB 设备不存在或已断开")]
    DeviceUnavailable,
    #[error("同时输出的设备不能超过 {MAX_OUTPUT_DEVICES} 台")]
    TooManyDevices,
    #[error("尚未选择可用输入源")]
    NoSource,
    #[error("输入源不可用：{0}")]
    SourceUnavailable(String),
    #[error("输入源配置无效：{0}")]
    InvalidSourceConfig(String),
    #[error("自定义波形库最多保存 {MAX_CUSTOM_WAVEFORMS} 项")]
    CustomWaveformLimit,
    #[error("自定义波形库总帧数不能超过 {MAX_CUSTOM_WAVEFORM_FRAMES}")]
    CustomWaveformFrameLimit,
    #[error("强度调整值不能为 0，且必须在 -200..=200 范围内")]
    InvalidDelta,
    #[error("调整后的强度会超过设备上报的通道上限或低于 0")]
    IntensityLimit,
    #[error("连接超时断开时间必须在 1..=1440 分钟范围内")]
    InvalidConnectionTimeout,
    #[error("绑定配置已变更，请重新读取后提交")]
    ConfigConflict,
    #[error("实时输出队列繁忙，请稍后重试")]
    QueueBusy,
    #[error("Relay 操作失败：{0}")]
    Relay(String),
    #[error("{0}")]
    Transport(TransportError),
}

impl HubError {
    pub fn code(&self) -> &str {
        match self {
            Self::Stopped => "hub_stopped",
            Self::NotConnected => "not_connected",
            Self::NoDevice => "no_device",
            Self::DeviceUnavailable => "device_unavailable",
            Self::TooManyDevices => "too_many_devices",
            Self::NoSource => "no_source",
            Self::SourceUnavailable(_) => "source_unavailable",
            Self::InvalidSourceConfig(_) => "invalid_source_config",
            Self::CustomWaveformLimit => "custom_waveform_limit",
            Self::CustomWaveformFrameLimit => "custom_waveform_frame_limit",
            Self::InvalidDelta => "invalid_delta",
            Self::IntensityLimit => "intensity_limit",
            Self::InvalidConnectionTimeout => "invalid_connection_timeout",
            Self::QueueBusy => "queue_busy",
            Self::ConfigConflict => "config_conflict",
            Self::Relay(_) => "relay_error",
            Self::Transport(error) => &error.code,
        }
    }
}

impl From<TransportError> for HubError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

impl From<RelayClientError> for HubError {
    fn from(error: RelayClientError) -> Self {
        match error {
            RelayClientError::QueueFull => Self::QueueBusy,
            other => Self::Relay(other.to_string()),
        }
    }
}

enum HubCommand {
    ConnectionCleanupFinished {
        connection_id: String,
        generation: u64,
        result: Result<(), HubError>,
    },
    StopOutputFinished {
        device: DeviceKey,
        generation: u64,
        result: Result<(), HubError>,
    },
    ChannelClearFailed {
        binding: SourceBindingKey,
        generation: u64,
        message: String,
    },

    ScanResult {
        devices: Vec<BluetoothDevice>,
    },
    Transport {
        action: TransportAction,
        safety_epoch: u64,
        reply: oneshot::Sender<Result<Value, HubError>>,
    },
    RefreshPlugins {
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    SetPluginBindingConfig {
        source_id: String,
        binding_id: String,
        config: Value,
        expected_revision: u64,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    AdjustIntensity {
        device_id: Option<String>,
        channel: Channel,
        delta: i32,
        safety_epoch: u64,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    StartOutput {
        device_id: String,
        safety_epoch: u64,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    SetDeviceChannelSource {
        device_id: String,
        channel: Channel,
        source_id: String,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    SetDeviceChannelSourceSync {
        device_id: String,
        enabled: bool,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    SetDefaultSource {
        source_id: Option<String>,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    SetFixedWaveform {
        device_id: String,
        channel: Channel,
        config: Option<WaveformConfig>,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    SetWaveformState {
        waveforms: Vec<WaveformConfig>,
        selected: Option<WaveformConfig>,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    SetSyncAllDevices {
        device_id: String,
        enabled: bool,
        safety_epoch: u64,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    UpdateSafety {
        connection_timeout_enabled: bool,
        connection_timeout_minutes: i32,
        allow_app_intensity_control: bool,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
}

enum HubSafetyCommand {
    DisconnectConnection {
        connection_id: String,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    ClearDeviceChannel {
        device_id: String,
        channel: Channel,
        reply: oneshot::Sender<Result<(String, u64), HubError>>,
    },
    StopOutput {
        device_id: String,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
}

#[derive(Clone)]
pub struct HubHandle {
    commands: mpsc::Sender<HubCommand>,
    safety_commands: mpsc::Sender<HubSafetyCommand>,
    snapshots: watch::Receiver<HubSnapshot>,
    shutdown: CancellationToken,
    completion: watch::Receiver<Option<Result<(), HubError>>>,
    safety_epoch: Arc<AtomicU64>,
    epoch_sender: watch::Sender<u64>,
    safety_wakeup: Arc<Notify>,
}

impl HubHandle {
    pub async fn refresh_plugins(&self) -> Result<(), HubError> {
        self.request(|reply| HubCommand::RefreshPlugins { reply })
            .await
    }

    pub async fn set_plugin_binding_config(
        &self,
        source_id: String,
        binding_id: String,
        config: Value,
        expected_revision: u64,
    ) -> Result<(), HubError> {
        self.request(|reply| HubCommand::SetPluginBindingConfig {
            source_id,
            binding_id,
            config,
            expected_revision,
            reply,
        })
        .await
    }
    pub fn command_epoch(&self) -> u64 {
        self.safety_epoch.load(Ordering::Acquire)
    }
    pub fn subscribe_command_epoch(&self) -> watch::Receiver<u64> {
        self.epoch_sender.subscribe()
    }
    pub fn accept_command(&self, safety: bool) -> u64 {
        if !safety {
            return self.command_epoch();
        }
        revoke_command_epoch(&self.safety_epoch, &self.epoch_sender, &self.safety_wakeup)
    }
    fn accepted_generation(&self) -> u64 {
        dg_lab_link_plugin_sdk::OPERATION_EPOCH
            .try_with(|epoch| *epoch)
            .unwrap_or_else(|_| self.command_epoch())
    }
    pub async fn transport(&self, action: TransportAction) -> Result<Value, HubError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(HubCommand::Transport {
                action,
                safety_epoch: self.accepted_generation(),
                reply,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => HubError::QueueBusy,
                _ => HubError::Stopped,
            })?;
        response.await.map_err(|_| HubError::Stopped)?
    }
    pub async fn disconnect_connection(&self, connection_id: String) -> Result<(), HubError> {
        self.accept_command(true);
        self.enqueue_disconnect_connection(connection_id).await
    }
    pub(crate) async fn enqueue_disconnect_connection(
        &self,
        connection_id: String,
    ) -> Result<(), HubError> {
        self.safety_request(|reply| HubSafetyCommand::DisconnectConnection {
            connection_id,
            reply,
        })
        .await
    }
    pub fn snapshot(&self) -> HubSnapshot {
        self.snapshots.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<HubSnapshot> {
        self.snapshots.clone()
    }

    pub async fn adjust_device_intensity(
        &self,
        device_id: Option<String>,
        channel: Channel,
        delta: i32,
    ) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        let safety_epoch = self.accepted_generation();
        self.commands
            .send(HubCommand::AdjustIntensity {
                device_id,
                channel,
                delta,
                safety_epoch,
                reply,
            })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn start_output(&self, device_id: String) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        let safety_epoch = self.accepted_generation();
        self.commands
            .send(HubCommand::StartOutput {
                device_id,
                safety_epoch,
                reply,
            })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn clear_device_channel(
        &self,
        device_id: String,
        channel: Channel,
    ) -> Result<(String, u64), HubError> {
        let (reply, response) = oneshot::channel();
        self.safety_commands
            .send(HubSafetyCommand::ClearDeviceChannel {
                device_id,
                channel,
                reply,
            })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn stop_output(&self, device_id: String) -> Result<(), HubError> {
        self.accept_command(true);
        self.enqueue_stop_output(device_id).await
    }
    pub(crate) async fn enqueue_stop_output(&self, device_id: String) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.safety_commands
            .send(HubSafetyCommand::StopOutput { device_id, reply })
            .await
            .map_err(|_| HubError::Stopped)?;
        self.safety_wakeup.notify_waiters();
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn set_device_channel_source(
        &self,
        device_id: String,
        channel: Channel,
        source_id: String,
    ) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(HubCommand::SetDeviceChannelSource {
                device_id,
                channel,
                source_id,
                reply,
            })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn set_default_source(&self, source_id: Option<String>) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(HubCommand::SetDefaultSource { source_id, reply })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn set_fixed_waveform(
        &self,
        device_id: String,
        channel: Channel,
        config: Option<WaveformConfig>,
    ) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(HubCommand::SetFixedWaveform {
                device_id,
                channel,
                config,
                reply,
            })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn set_waveform_state(
        &self,
        waveforms: Vec<WaveformConfig>,
        selected: Option<WaveformConfig>,
    ) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(HubCommand::SetWaveformState {
                waveforms,
                selected,
                reply,
            })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn set_device_channel_source_sync(
        &self,
        device_id: String,
        enabled: bool,
    ) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(HubCommand::SetDeviceChannelSourceSync {
                device_id,
                enabled,
                reply,
            })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn set_sync_all_devices(
        &self,
        device_id: String,
        enabled: bool,
    ) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        let safety_epoch = self.accepted_generation();
        self.commands
            .send(HubCommand::SetSyncAllDevices {
                device_id,
                enabled,
                safety_epoch,
                reply,
            })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn update_safety(
        &self,
        connection_timeout_enabled: bool,
        connection_timeout_minutes: i32,
        allow_app_intensity_control: bool,
    ) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(HubCommand::UpdateSafety {
                connection_timeout_enabled,
                connection_timeout_minutes,
                allow_app_intensity_control,
                reply,
            })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub fn shutdown_now(&self) {
        self.accept_command(true);
        self.shutdown.cancel();
    }

    pub async fn shutdown_gracefully(&self) -> Result<(), HubError> {
        self.shutdown_now();
        let mut completion = self.completion.clone();
        loop {
            if let Some(result) = completion.borrow().clone() {
                return result;
            }
            completion.changed().await.map_err(|_| HubError::Stopped)?;
        }
    }

    async fn request(
        &self,
        make_command: impl FnOnce(oneshot::Sender<Result<(), HubError>>) -> HubCommand,
    ) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(make_command(reply))
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    async fn safety_request(
        &self,
        make_command: impl FnOnce(oneshot::Sender<Result<(), HubError>>) -> HubSafetyCommand,
    ) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.safety_commands
            .send(make_command(reply))
            .await
            .map_err(|_| HubError::Stopped)?;
        self.safety_wakeup.notify_waiters();
        response.await.map_err(|_| HubError::Stopped)?
    }
}

fn revoke_command_epoch(epoch: &AtomicU64, sender: &watch::Sender<u64>, wakeup: &Notify) -> u64 {
    let next = epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
    sender.send_if_modified(|current| {
        if next > *current {
            *current = next;
            true
        } else {
            false
        }
    });
    wakeup.notify_waiters();
    next
}

struct SourceRuntime {
    snapshot: SourceSnapshot,
    source: Option<Box<dyn WaveSource>>,
}

struct FixedWaveformRuntime {
    config: WaveformConfig,
    source: Box<dyn WaveSource>,
}

type DeviceKey = DeviceAddress;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct IntensityKey {
    device: DeviceKey,
    channel: Channel,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SourceBindingKey {
    device: DeviceKey,
    channel: Channel,
}

#[derive(Debug, Clone)]
struct SourceBindingRuntime {
    id: String,
    source_id: String,
    config: Value,
    revision: u64,
    generation: u64,
    active: bool,
}

#[derive(Debug, Clone)]
struct PendingWaveOperation {
    device: DeviceKey,
    channel: Channel,
    generation: u64,
    sent_at: Instant,
}

#[derive(Debug, Clone)]
struct PendingIntensityOperation {
    request_id: String,
    projected: u16,
    lock_correction: bool,
    response_received: bool,
    projected_observed: bool,
}

#[derive(Debug, Clone)]
struct IntensityLockTarget {
    a: u16,
    b: u16,
}

struct PendingDeviceStop {
    generation: u64,
    replies: Vec<oneshot::Sender<Result<(), HubError>>>,
}

enum ConnectionReply {
    Unit(oneshot::Sender<Result<(), HubError>>),
    Value(oneshot::Sender<Result<Value, HubError>>),
}
impl ConnectionReply {
    fn send(self, result: Result<(), HubError>) {
        match self {
            Self::Unit(reply) => {
                let _ = reply.send(result);
            }
            Self::Value(reply) => {
                let _ = reply.send(result.map(|_| Value::Null));
            }
        }
    }
}

#[derive(Clone)]
enum CleanupConnection {
    V4(RelayClientHandle),
    Local(SessionHandle),
}
struct PendingConnectionCleanup {
    generation: u64,
    accepted_epoch: u64,
    restart_endpoint: Option<String>,
    reply: Option<ConnectionReply>,
    cancellation: CancellationToken,
    devices: Vec<(DeviceKey, DeviceSession)>,
    connection: CleanupConnection,
}

pub struct HubRuntime {
    plugins: Option<PluginManager>,
    plugin_catalog_revision: Option<u64>,
    plugin_last_frame: BTreeMap<SourceBindingKey, Instant>,
    faulted_bindings: BTreeSet<SourceBindingKey>,
    commands: mpsc::Receiver<HubCommand>,
    command_sender: mpsc::Sender<HubCommand>,
    safety_commands: mpsc::Receiver<HubSafetyCommand>,
    snapshot_sender: watch::Sender<HubSnapshot>,
    completion_sender: watch::Sender<Option<Result<(), HubError>>>,
    shutdown: CancellationToken,
    snapshot: HubSnapshot,
    v4_connection: TransportConnectionSnapshot,
    v4_session_generation: Option<u64>,
    sources: BTreeMap<String, SourceRuntime>,
    default_fixed_waveform: Option<WaveformConfig>,
    fixed_waveform_bindings: BTreeMap<SourceBindingKey, FixedWaveformRuntime>,
    custom_waveforms: Vec<WaveformConfig>,
    default_source_id: Option<String>,
    device_source_bindings: BTreeMap<SourceBindingKey, SourceBindingRuntime>,
    initialized_source_devices: BTreeSet<DeviceKey>,
    source_sync_devices: BTreeSet<DeviceKey>,
    relay: Option<RelayClientHandle>,
    relay_events: Option<mpsc::Sender<RelaySessionEvent>>,
    sessions: BTreeMap<String, SessionHandle>,
    session_events: Option<mpsc::Sender<(u64, SessionEvent)>>,
    session_identities: BTreeMap<String, u64>,
    next_session_identity: u64,
    session_tasks: Vec<tokio::task::JoinHandle<()>>,
    connection_started: BTreeMap<String, Instant>,
    apps: BTreeSet<String>,
    devices: BTreeMap<DeviceKey, Value>,
    sync_baseline_device: Option<DeviceKey>,
    shutdown_zero_pending: BTreeSet<DeviceKey>,
    output_devices: BTreeSet<DeviceKey>,
    pending_stops: BTreeMap<DeviceKey, PendingDeviceStop>,
    pending_connection_cleanups: BTreeMap<String, PendingConnectionCleanup>,
    pending_wave_operations: HashMap<String, PendingWaveOperation>,
    pending_intensity_operations: BTreeMap<IntensityKey, PendingIntensityOperation>,
    pending_intensity_requests: HashMap<String, IntensityKey>,
    intensity_lock_targets: BTreeMap<DeviceKey, IntensityLockTarget>,
    operation_generation: u64,
    safety_epoch: Arc<AtomicU64>,
    epoch_sender: watch::Sender<u64>,
    safety_wakeup: Arc<Notify>,
    connection_started_at: Option<Instant>,
    reconnect_at: Option<Instant>,
    reconnect_attempt: u32,
    auto_reconnect_enabled: bool,
}

#[cfg(test)]
fn create_hub(endpoint: String) -> (HubHandle, HubRuntime) {
    create_hub_with_default_source(endpoint, None)
}

#[cfg(test)]
fn create_hub_with_default_source(
    endpoint: String,
    requested_default_source_id: Option<String>,
) -> (HubHandle, HubRuntime) {
    let (hub, mut runtime) = create_hub_with_source_preferences(
        endpoint,
        requested_default_source_id,
        Some(WaveformConfig::default()),
        vec![WaveformConfig {
            preset_id: "TEST_CUSTOM".to_owned(),
            preset_name: "测试自定义波形".to_owned(),
            frames: vec!["0A0A0A0A64646464".to_owned()],
        }],
        SafetySnapshot::default(),
    );
    let config = serde_json::to_value(WaveformConfig {
        preset_id: "TEST_SECONDARY".to_owned(),
        preset_name: "测试辅助源".to_owned(),
        frames: vec!["2D2D2D2D64646464".to_owned()],
    })
    .unwrap();
    let registry = builtin_registry();
    runtime.sources.insert(
        "source-test-secondary".to_owned(),
        SourceRuntime {
            snapshot: SourceSnapshot {
                id: "source-test-secondary".to_owned(),
                revision: 0,
                kind: "test.secondary".to_owned(),
                name: "测试辅助源".to_owned(),
                enabled: true,
                assigned_channel_count: 0,
                selected_preset_id: Some("TEST_SECONDARY".to_owned()),
                selected_preset_name: Some("测试辅助源".to_owned()),
                plugin_id: None,
                runtime_status: "running".into(),
                last_error: None,
                config: Arc::new(json!({})),
                state: json!({}),
            },
            source: Some(registry.build("builtin.fixed_waveform", &config).unwrap()),
        },
    );
    // Protocol/binding tests use deterministic fake sources, never production fallback engines.
    for (id, name) in [
        (TOUCH_SOURCE_ID, "测试触控源"),
        (AUDIO_SOURCE_ID, "测试音频源"),
    ] {
        runtime.sources.insert(
            id.to_owned(),
            SourceRuntime {
                snapshot: SourceSnapshot {
                    id: id.into(),
                    revision: 0,
                    kind: format!("test.{id}"),
                    name: name.into(),
                    enabled: true,
                    assigned_channel_count: 0,
                    selected_preset_id: None,
                    selected_preset_name: None,
                    plugin_id: None,
                    runtime_status: "running".into(),
                    last_error: None,
                    config: Arc::new(json!({})),
                    state: json!({}),
                },
                source: Some(registry.build("builtin.fixed_waveform", &config).unwrap()),
            },
        );
    }
    runtime.refresh_source_snapshots();
    (hub, runtime)
}

pub fn create_hub_with_source_preferences(
    endpoint: String,
    requested_default_source_id: Option<String>,
    selected_waveform: Option<WaveformConfig>,
    custom_waveforms: Vec<WaveformConfig>,
    safety: SafetySnapshot,
) -> (HubHandle, HubRuntime) {
    let registry = builtin_registry();
    let selected_waveform = selected_waveform.filter(|waveform| {
        serde_json::to_value(waveform)
            .is_ok_and(|config| registry.validate("builtin.fixed_waveform", &config).is_ok())
    });
    let mut seen_custom_ids = BTreeSet::new();
    let mut custom_frame_count: usize = 0;
    let custom_waveforms = custom_waveforms
        .into_iter()
        .filter(|config| {
            let next_frame_count = custom_frame_count.saturating_add(config.frames.len());
            let accepted = seen_custom_ids.insert(config.preset_id.clone())
                && next_frame_count <= MAX_CUSTOM_WAVEFORM_FRAMES
                && serde_json::to_value(config)
                    .is_ok_and(|value| registry.validate("builtin.fixed_waveform", &value).is_ok());
            if accepted {
                custom_frame_count = next_frame_count;
            }
            accepted
        })
        .take(MAX_CUSTOM_WAVEFORMS)
        .collect::<Vec<_>>();
    let mut sources = BTreeMap::new();
    let selected_config = selected_waveform
        .as_ref()
        .unwrap_or(&WaveformConfig::default())
        .clone();
    let config = serde_json::to_value(&selected_config).expect("waveform config is serializable");
    let source = registry
        .build("builtin.fixed_waveform", &config)
        .expect("固定波形默认配置必须有效");
    let source_snapshot = SourceSnapshot {
        id: FIXED_WAVEFORM_SOURCE_ID.to_owned(),
        revision: 0,
        kind: "builtin.fixed_waveform".to_owned(),
        name: "固定波形".to_owned(),
        enabled: true,
        assigned_channel_count: 0,
        selected_preset_id: None,
        selected_preset_name: None,
        plugin_id: None,
        runtime_status: "running".into(),
        last_error: None,
        config: Arc::new(json!({})),
        state: json!({}),
    };
    sources.insert(
        FIXED_WAVEFORM_SOURCE_ID.to_owned(),
        SourceRuntime {
            snapshot: source_snapshot.clone(),
            source: Some(source),
        },
    );

    let requested_default_source_id = requested_default_source_id.map(|id| match id.as_str() {
        "source-test-pattern"
        | "source-manual"
        | "source-default-waveform"
        | "source-custom-waveform" => FIXED_WAVEFORM_SOURCE_ID.to_owned(),
        _ => id,
    });
    let default_source_id = requested_default_source_id.filter(|id| sources.contains_key(id));
    let custom_waveform_snapshots = custom_waveform_snapshots(&custom_waveforms);
    let snapshot = initial_snapshot(
        endpoint,
        ordered_source_snapshots(&sources),
        custom_waveform_snapshots,
        default_source_id.clone(),
        safety,
    );
    let (snapshot_sender, snapshot_receiver) = watch::channel(snapshot.clone());
    let (command_sender, command_receiver) = mpsc::channel(HUB_COMMAND_CAPACITY);
    let (safety_sender, safety_receiver) = mpsc::channel(HUB_SAFETY_COMMAND_CAPACITY);
    let shutdown = CancellationToken::new();
    let safety_epoch = Arc::new(AtomicU64::new(0));
    let (epoch_sender, _) = watch::channel(0);
    let safety_wakeup = Arc::new(Notify::new());
    let (completion_sender, completion_receiver) = watch::channel(None);
    (
        HubHandle {
            commands: command_sender.clone(),
            safety_commands: safety_sender,
            snapshots: snapshot_receiver,
            shutdown: shutdown.clone(),
            completion: completion_receiver,
            safety_epoch: Arc::clone(&safety_epoch),
            epoch_sender: epoch_sender.clone(),
            safety_wakeup: Arc::clone(&safety_wakeup),
        },
        HubRuntime {
            plugins: None,
            plugin_catalog_revision: None,
            plugin_last_frame: BTreeMap::new(),
            faulted_bindings: BTreeSet::new(),
            commands: command_receiver,
            command_sender,
            safety_commands: safety_receiver,
            snapshot_sender,
            completion_sender,
            shutdown,
            v4_connection: snapshot.connections[0].clone(),
            v4_session_generation: None,
            snapshot,
            sources,
            default_fixed_waveform: selected_waveform,
            fixed_waveform_bindings: BTreeMap::new(),
            custom_waveforms,
            default_source_id,
            device_source_bindings: BTreeMap::new(),
            initialized_source_devices: BTreeSet::new(),
            source_sync_devices: BTreeSet::new(),
            relay: None,
            relay_events: None,
            sessions: BTreeMap::new(),
            session_events: None,
            session_identities: BTreeMap::new(),
            next_session_identity: 0,
            session_tasks: Vec::new(),
            connection_started: BTreeMap::new(),
            apps: BTreeSet::new(),
            devices: BTreeMap::new(),
            sync_baseline_device: None,
            shutdown_zero_pending: BTreeSet::new(),
            output_devices: BTreeSet::new(),
            pending_stops: BTreeMap::new(),
            pending_connection_cleanups: BTreeMap::new(),
            pending_wave_operations: HashMap::new(),
            pending_intensity_operations: BTreeMap::new(),
            pending_intensity_requests: HashMap::new(),
            intensity_lock_targets: BTreeMap::new(),
            operation_generation: 0,
            safety_epoch,
            epoch_sender,
            safety_wakeup,
            connection_started_at: None,
            reconnect_at: None,
            reconnect_attempt: 0,
            auto_reconnect_enabled: false,
        },
    )
}

fn custom_waveform_snapshots(waveforms: &[WaveformConfig]) -> Vec<CustomWaveformSnapshot> {
    waveforms
        .iter()
        .map(|waveform| CustomWaveformSnapshot {
            id: waveform.preset_id.clone(),
            name: waveform.preset_name.clone(),
            frame_count: waveform.frames.len(),
            duration_ms: waveform.frames.len() * WaveFrame::DURATION.as_millis() as usize,
        })
        .collect()
}

fn ordered_source_snapshots(sources: &BTreeMap<String, SourceRuntime>) -> Vec<SourceSnapshot> {
    let mut snapshots = sources
        .values()
        .map(|source| source.snapshot.clone())
        .collect::<Vec<_>>();
    let registry = builtin_registry();
    snapshots.sort_by_key(|source| {
        registry
            .list_descriptors()
            .iter()
            .position(|descriptor| descriptor.kind == source.kind)
            .unwrap_or(usize::MAX)
    });
    snapshots
}

fn build_fixed_waveform_runtime(config: WaveformConfig) -> Result<FixedWaveformRuntime, HubError> {
    let value = serde_json::to_value(&config)
        .map_err(|error| HubError::InvalidSourceConfig(error.to_string()))?;
    let source = builtin_registry()
        .build("builtin.fixed_waveform", &value)
        .map_err(|error| HubError::InvalidSourceConfig(error.to_string()))?;
    Ok(FixedWaveformRuntime { config, source })
}

impl HubRuntime {
    pub fn set_plugin_manager(&mut self, manager: PluginManager, default_source: Option<String>) {
        self.plugins = Some(manager);
        self.refresh_plugin_sources();
        self.default_source_id = default_source.filter(|id| {
            self.sources
                .get(id)
                .is_some_and(|source| source.snapshot.enabled)
        });
        self.snapshot.default_source_id = self.default_source_id.clone();
        self.refresh_source_snapshots();
        self.publish();
    }

    fn refresh_plugin_sources(&mut self) {
        let Some(manager) = self.plugins.clone() else {
            return;
        };
        let catalog = manager.cached_catalog_snapshot();
        if self.plugin_catalog_revision != Some(catalog.revision) {
            self.plugin_catalog_revision = Some(catalog.revision);
            self.snapshot.plugins = catalog.plugins.clone();
            let ids = catalog
                .sources
                .iter()
                .map(|source| source.id.clone())
                .collect::<BTreeSet<_>>();
            self.sources
                .retain(|id, source| source.source.is_some() || ids.contains(id));
            for source in &catalog.sources {
                let installed = catalog
                    .plugins
                    .iter()
                    .any(|plugin| plugin.manifest.id == source.plugin_id);
                let snapshot = SourceSnapshot {
                    id: source.id.clone(),
                    revision: catalog
                        .source_revisions
                        .get(&source.id)
                        .copied()
                        .unwrap_or(0),
                    kind: source.plugin_id.clone(),
                    name: source.name.clone(),
                    enabled: source.enabled && installed,
                    assigned_channel_count: 0,
                    selected_preset_id: None,
                    selected_preset_name: None,
                    plugin_id: Some(source.plugin_id.clone()),
                    runtime_status: if !source.enabled {
                        "disabled"
                    } else if !installed {
                        "missing"
                    } else {
                        "idle"
                    }
                    .into(),
                    last_error: None,
                    config: Arc::new(source.config.clone()),
                    state: json!({}),
                };
                self.sources.insert(
                    source.id.clone(),
                    SourceRuntime {
                        snapshot,
                        source: None,
                    },
                );
            }
        }
        for state in manager.runtime_states() {
            if let Some(source) = self.sources.get_mut(&state.id) {
                if source.snapshot.enabled {
                    source.snapshot.runtime_status = serde_json::to_value(state.status)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_default();
                }
                source.snapshot.last_error = state.last_error.map(|error| error.message);
                source.snapshot.state = state.state;
            }
        }
        let invalid = self
            .device_source_bindings
            .iter()
            .filter(|(_, id)| {
                self.sources
                    .get(&id.source_id)
                    .is_none_or(|source| !source.snapshot.enabled)
            })
            .map(|(binding, _)| binding.clone())
            .collect::<Vec<_>>();
        for binding in invalid {
            self.fail_plugin_binding(&binding, "输入源已停用、卸载或删除");
            self.device_source_bindings.remove(&binding);
        }
        let stopped = self
            .device_source_bindings
            .iter()
            .filter(|(_, id)| {
                self.sources.get(&id.source_id).is_some_and(|source| {
                    source.source.is_none() && source.snapshot.runtime_status != "running"
                })
            })
            .map(|(binding, _)| binding.clone())
            .collect::<Vec<_>>();
        for binding in stopped {
            self.fail_plugin_binding(&binding, "插件进程已停止或发生故障");
        }
        if self
            .default_source_id
            .as_ref()
            .is_some_and(|id| self.sources.get(id).is_none_or(|s| !s.snapshot.enabled))
        {
            self.default_source_id = None;
            self.snapshot.default_source_id = None;
        }
        self.refresh_source_snapshots();
    }

    fn publish_plugin_bindings(&mut self) {
        let mut snapshots = Vec::new();
        for source in self.sources.values() {
            let bindings = self
                .device_source_bindings
                .iter()
                .filter(|(key, binding)| {
                    binding.source_id == source.snapshot.id
                        && self.devices.contains_key(&key.device)
                })
                .map(|(key, binding)| {
                    (
                        binding.revision,
                        PluginBinding {
                            binding_id: binding.id.clone(),
                            control_id: key.device.control_id(),
                            channel: match key.channel {
                                Channel::A => dg_lab_link_plugin_sdk::Channel::A,
                                Channel::B => dg_lab_link_plugin_sdk::Channel::B,
                            },
                            generation: binding.generation,
                            config: binding.config.clone(),
                            active: binding.active,
                        },
                    )
                })
                .collect::<Vec<_>>();
            snapshots.extend(
                bindings
                    .iter()
                    .map(|(revision, binding)| SourceBindingSnapshot {
                        source_id: source.snapshot.id.clone(),
                        revision: *revision,
                        binding: binding.clone(),
                    }),
            );
            if source.snapshot.plugin_id.is_some()
                && let Some(manager) = &self.plugins
            {
                let bindings = bindings
                    .into_iter()
                    .map(|(_, binding)| binding)
                    .collect::<Vec<_>>();
                if let Err(error) = manager.try_update_bindings(&source.snapshot.id, &bindings) {
                    self.snapshot.output.last_error = Some(error.message);
                }
            }
        }
        self.snapshot.source_bindings = snapshots;
    }

    fn replace_source_binding(&mut self, key: SourceBindingKey, source_id: String) {
        let generation = self.device_source_bindings.remove(&key).map_or(1, |old| {
            if let Some(manager) = &self.plugins {
                manager.frames().invalidate(&old.id, u64::MAX);
            }
            old.generation.saturating_add(1)
        });
        self.fixed_waveform_bindings.remove(&key);
        self.faulted_bindings.remove(&key);
        self.plugin_last_frame.insert(key.clone(), Instant::now());
        self.device_source_bindings.insert(
            key.clone(),
            SourceBindingRuntime {
                id: Uuid::new_v4().to_string(),
                source_id,
                config: json!({}),
                revision: 0,
                generation,
                active: self.output_devices.contains(&key.device),
            },
        );
    }

    fn fail_plugin_binding(&mut self, binding: &SourceBindingKey, message: &str) {
        if !self.output_devices.contains(&binding.device)
            || !self.faulted_bindings.insert(binding.clone())
        {
            return;
        }
        self.plugin_last_frame.remove(binding);
        if let Some(state) = self.device_source_bindings.get_mut(binding) {
            state.generation = state.generation.saturating_add(1);
            state.active = false;
            if let Some(manager) = &self.plugins {
                manager.frames().invalidate(&state.id, state.generation);
            }
        }
        self.pending_wave_operations.retain(|_, pending| {
            pending.device != binding.device || pending.channel != binding.channel
        });
        if let Ok(session) = self.device_session(&binding.device) {
            let generation = self.operation_generation;
            if let Ok(completion) =
                session.request_stop(&binding.device, Some(binding.channel), false, generation)
            {
                tokio::spawn(async move {
                    let _ = completion.await;
                });
            }
        }
        self.snapshot.output.last_error = Some(message.to_owned());
        if Channel::ALL.iter().all(|channel| {
            self.faulted_bindings.contains(&SourceBindingKey {
                device: binding.device.clone(),
                channel: *channel,
            })
        }) {
            self.output_devices.remove(&binding.device);
            if self.output_devices.is_empty() {
                self.snapshot.output.state = OutputState::Error;
            }
        }
        self.log(
            LogLevel::Error,
            format!(
                "{} {} 通道：{message}",
                binding.device.control_id(),
                binding.channel
            ),
        );
    }
    pub fn set_initial_v3_endpoint(&mut self, endpoint: String) {
        self.snapshot.connections.push(TransportConnectionSnapshot {
            connection_id: V3_CONNECTION_ID.to_owned(),
            transport: TransportKind::WsV3,
            state: ConnectionState::Disconnected,
            endpoint,
            controller_id: None,
            pairing_url: None,
            app_count: 0,
            last_error: None,
        });
        self.publish();
    }
    fn device_session(&self, device: &DeviceKey) -> Result<DeviceSession, HubError> {
        let session = if device.connection_id == V4_CONNECTION_ID {
            self.relay.clone().map(DeviceSession::V4)
        } else {
            self.sessions
                .get(&device.connection_id)
                .cloned()
                .map(DeviceSession::Local)
        };
        session
            .or_else(|| {
                self.pending_connection_cleanups
                    .get(&device.connection_id)
                    .and_then(|pending| pending.devices.iter().find(|(key, _)| key == device))
                    .map(|(_, session)| session.clone())
            })
            .ok_or(HubError::DeviceUnavailable)
    }
    fn register_session(
        &mut self,
        connection_id: String,
        spawn: impl FnOnce(mpsc::Sender<SessionEvent>) -> (SessionHandle, tokio::task::JoinHandle<()>),
    ) {
        self.next_session_identity = self.next_session_identity.saturating_add(1);
        let identity = self.next_session_identity;
        self.session_identities
            .insert(connection_id.clone(), identity);
        let (sender, mut events) = mpsc::channel(RELAY_EVENT_CAPACITY);
        let (handle, task) = spawn(sender);
        self.sessions.insert(connection_id, handle);
        self.session_tasks.retain(|task| !task.is_finished());
        self.session_tasks.push(task);
        let sender = self
            .session_events
            .as_ref()
            .expect("session event queue")
            .clone();
        self.session_tasks.push(tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if sender.send((identity, event)).await.is_err() {
                    break;
                }
            }
        }));
    }

    async fn handle_current_session_event(&mut self, identity: u64, event: SessionEvent) {
        let connection_id = match &event {
            SessionEvent::Connection(connection) => &connection.connection_id,
            SessionEvent::Device { connection_id, .. }
            | SessionEvent::Removed { connection_id, .. }
            | SessionEvent::OperationFinished { connection_id, .. }
            | SessionEvent::Log { connection_id, .. } => connection_id,
        };
        if self.session_identities.get(connection_id) == Some(&identity) {
            self.handle_session_event(event).await;
        }
    }

    fn start_v3_connection(&mut self, endpoint: String, reply: ConnectionReply) {
        if self.session_events.is_none() {
            reply.send(Err(HubError::Stopped));
            return;
        }
        if let Some(old) = self.sessions.remove(V3_CONNECTION_ID) {
            old.shutdown_now();
        }
        self.session_identities.remove(V3_CONNECTION_ID);
        self.mark_connection_connecting(V3_CONNECTION_ID, TransportKind::WsV3, &endpoint);
        self.register_session(V3_CONNECTION_ID.to_owned(), crate::transport::v3::spawn);
        let handle = self.sessions[V3_CONNECTION_ID].clone();
        let generation = self.operation_generation;
        tokio::spawn(async move {
            reply.send(
                handle
                    .connect(endpoint, None, generation)
                    .await
                    .map_err(Into::into),
            );
        });
    }

    fn update_connection(&mut self, connection: TransportConnectionSnapshot) {
        if let Some(current) = self
            .snapshot
            .connections
            .iter_mut()
            .find(|c| c.connection_id == connection.connection_id)
        {
            *current = connection;
        } else {
            self.snapshot.connections.push(connection);
        }
    }

    fn mark_connection_connecting(
        &mut self,
        connection_id: &str,
        transport: TransportKind,
        endpoint: &str,
    ) {
        self.update_connection(TransportConnectionSnapshot {
            connection_id: connection_id.to_owned(),
            transport,
            state: ConnectionState::Connecting,
            endpoint: endpoint.to_owned(),
            controller_id: None,
            pairing_url: None,
            app_count: 0,
            last_error: None,
        });
        self.publish();
    }

    async fn handle_transport_action(
        &mut self,
        action: TransportAction,
        accepted_epoch: u64,
        reply: oneshot::Sender<Result<Value, HubError>>,
    ) {
        let generation = self.operation_generation;
        let result: Result<(), HubError> = match action {
            TransportAction::SetEndpoint {
                transport,
                endpoint,
            } => match url::Url::parse(&endpoint) {
                Ok(url)
                    if matches!(url.scheme(), "ws" | "wss")
                        && url.host_str().is_some()
                        && endpoint.len() <= 2048
                        && transport != TransportKind::Ble =>
                {
                    let id = if transport == TransportKind::WsV4 {
                        V4_CONNECTION_ID
                    } else {
                        V3_CONNECTION_ID
                    };
                    if self.pending_connection_cleanups.contains_key(id) {
                        let _ = reply.send(Err(HubError::QueueBusy));
                        return;
                    }
                    let active = self
                        .snapshot
                        .connections
                        .iter()
                        .find(|c| c.connection_id == id)
                        .is_some_and(|c| {
                            matches!(
                                c.state,
                                ConnectionState::Connecting
                                    | ConnectionState::Waiting
                                    | ConnectionState::Connected
                            ) && c.endpoint != endpoint
                        });
                    if active {
                        Err(
                            TransportError::new("connection_active", "请先断开该连接再修改地址")
                                .into(),
                        )
                    } else {
                        if transport == TransportKind::WsV4 {
                            self.v4_connection.endpoint = endpoint;
                        } else if let Some(c) = self
                            .snapshot
                            .connections
                            .iter_mut()
                            .find(|c| c.connection_id == id)
                        {
                            c.endpoint = endpoint;
                        } else {
                            self.set_initial_v3_endpoint(endpoint);
                        }
                        self.publish();
                        Ok(())
                    }
                }
                _ => Err(
                    TransportError::new("invalid_endpoint", "端点必须是有效 WS/WSS 地址").into(),
                ),
            },
            TransportAction::Connect {
                transport,
                endpoint,
            } => {
                let id = if transport == TransportKind::WsV4 {
                    V4_CONNECTION_ID
                } else {
                    V3_CONNECTION_ID
                };
                if transport == TransportKind::Ble {
                    Err(
                        TransportError::new("invalid_transport", "BLE 请使用扫描设备连接命令")
                            .into(),
                    )
                } else if self.pending_connection_cleanups.contains_key(id) {
                    Err(HubError::QueueBusy)
                } else if self
                    .snapshot
                    .connections
                    .iter()
                    .find(|connection| connection.connection_id == id)
                    .is_some_and(|connection| {
                        matches!(
                            connection.state,
                            ConnectionState::Connecting
                                | ConnectionState::Waiting
                                | ConnectionState::Connected
                        )
                    })
                {
                    Err(TransportError::new("already_connected", "连接已建立或正在建立").into())
                } else if transport == TransportKind::WsV4 {
                    self.v4_connection.endpoint = endpoint;
                    self.auto_reconnect_enabled = true;
                    self.begin_connect();
                    Ok(())
                } else if transport == TransportKind::WsV3 {
                    self.start_v3_connection(endpoint, ConnectionReply::Value(reply));
                    return;
                } else {
                    Err(
                        TransportError::new("invalid_transport", "BLE 请使用扫描设备连接命令")
                            .into(),
                    )
                }
            }
            TransportAction::RefreshPairing { connection_id } => {
                if matches!(connection_id.as_str(), V4_CONNECTION_ID | V3_CONNECTION_ID) {
                    let endpoint = self
                        .snapshot
                        .connections
                        .iter()
                        .find(|connection| connection.connection_id == connection_id)
                        .map(|connection| connection.endpoint.clone())
                        .unwrap_or_else(|| DEFAULT_V3_ENDPOINT.to_owned());
                    self.queue_connection_cleanup(
                        &connection_id,
                        Some(endpoint),
                        accepted_epoch,
                        Some(ConnectionReply::Value(reply)),
                    );
                    return;
                } else {
                    Err(
                        TransportError::new("unsupported_operation", "仅 WS 连接支持刷新配对")
                            .into(),
                    )
                }
            }
            TransportAction::Scan { duration_ms } => {
                if !(100..=10_000).contains(&duration_ms) {
                    Err(
                        TransportError::new("invalid_scan_duration", "扫描时长必须在100..=10000ms")
                            .into(),
                    )
                } else {
                    let commands = self.command_sender.clone();
                    self.session_tasks.retain(|task| !task.is_finished());
                    self.session_tasks.push(tokio::spawn(async move {
                        let result = crate::transport::ble::scan(duration_ms).await;
                        if let Ok(devices) = &result {
                            let _ = commands.try_send(HubCommand::ScanResult {
                                devices: devices.clone(),
                            });
                        }
                        let _ = reply.send(
                            result
                                .map(|devices| {
                                    serde_json::to_value(devices).expect("scan serializable")
                                })
                                .map_err(Into::into),
                        );
                    }));
                    return;
                }
            }
            TransportAction::ConnectBluetooth {
                device_id,
                parameters,
            } => {
                let id = bluetooth_connection_id(&device_id);
                if device_id.is_empty() || device_id.len() > 256 {
                    Err(TransportError::new("invalid_device_id", "扫描设备 ID 无效").into())
                } else if let Err(error) = parameters.validate() {
                    Err(error.into())
                } else if self.sessions.len() > MAX_OUTPUT_DEVICES
                    && !self.sessions.contains_key(&id)
                {
                    Err(HubError::TooManyDevices)
                } else if self.pending_connection_cleanups.contains_key(&id) {
                    Err(HubError::QueueBusy)
                } else if self
                    .snapshot
                    .connections
                    .iter()
                    .find(|connection| connection.connection_id == id)
                    .is_some_and(|connection| {
                        matches!(
                            connection.state,
                            ConnectionState::Connecting
                                | ConnectionState::Waiting
                                | ConnectionState::Connected
                        )
                    })
                {
                    Err(TransportError::new("already_connected", "蓝牙设备已连接或正在连接").into())
                } else if self.session_events.is_some() {
                    if let Some(old) = self.sessions.remove(&id) {
                        old.shutdown_now();
                    }
                    self.session_identities.remove(&id);
                    self.mark_connection_connecting(&id, TransportKind::Ble, &device_id);
                    let native_id = device_id.clone();
                    self.register_session(id.clone(), |events| {
                        crate::transport::ble::spawn(native_id, events)
                    });
                    let handle = self.sessions[&id].clone();
                    tokio::spawn(async move {
                        let _ = reply.send(
                            handle
                                .connect(device_id, Some(parameters), generation)
                                .await
                                .map(|_| Value::Null)
                                .map_err(Into::into),
                        );
                    });
                    return;
                } else {
                    Err(HubError::Stopped)
                }
            }
            TransportAction::ConfigureBluetooth {
                device_id,
                parameters,
            } => {
                if let Some(device) = self
                    .devices
                    .keys()
                    .find(|d| d.control_id() == device_id && d.connection_id.starts_with("ble:"))
                    .cloned()
                {
                    let handle = self.sessions[&device.connection_id].clone();
                    tokio::spawn(async move {
                        let _ = reply.send(
                            handle
                                .configure(parameters, generation)
                                .await
                                .map(|_| Value::Null)
                                .map_err(Into::into),
                        );
                    });
                    return;
                } else {
                    Err(HubError::DeviceUnavailable)
                }
            }
        };
        let _ = reply.send(result.map(|_| Value::Null));
    }

    fn queue_connection_cleanup(
        &mut self,
        connection_id: &str,
        restart_endpoint: Option<String>,
        accepted_epoch: u64,
        reply: Option<ConnectionReply>,
    ) {
        if self.pending_connection_cleanups.contains_key(connection_id) {
            if let Some(reply) = reply {
                reply.send(Err(HubError::QueueBusy));
            }
            return;
        }
        let v4_generation = self.v4_session_generation;
        let connection = if connection_id == V4_CONNECTION_ID {
            self.disable_auto_reconnect();
            self.v4_session_generation = None;
            self.connection_started_at = None;
            self.apps.clear();
            self.relay.take().map(CleanupConnection::V4)
        } else {
            self.session_identities.remove(connection_id);
            self.sessions
                .remove(connection_id)
                .map(CleanupConnection::Local)
        };
        let Some(connection) = connection else {
            let known = matches!(connection_id, V4_CONNECTION_ID | V3_CONNECTION_ID)
                || self
                    .snapshot
                    .connections
                    .iter()
                    .any(|connection| connection.connection_id == connection_id);
            if !known {
                if let Some(reply) = reply {
                    reply.send(Err(HubError::DeviceUnavailable));
                }
                return;
            }
            self.mark_connection_disconnected(connection_id);
            self.publish();
            if let Some(endpoint) = restart_endpoint {
                self.resume_connection_after_cleanup(
                    connection_id,
                    endpoint,
                    accepted_epoch,
                    reply,
                );
            } else if let Some(reply) = reply {
                reply.send(Ok(()));
            }
            return;
        };
        self.operation_generation = self.operation_generation.saturating_add(1);
        let generation = self.operation_generation;
        match &connection {
            CleanupConnection::V4(handle) => handle.invalidate_operations(generation),
            CleanupConnection::Local(handle) => handle.invalidate_operations(generation),
        }
        let device_session = match &connection {
            CleanupConnection::V4(handle) => DeviceSession::V4(handle.clone()),
            CleanupConnection::Local(handle) => DeviceSession::Local(handle.clone()),
        };
        let devices = self
            .devices
            .keys()
            .filter(|device| device.connection_id == connection_id)
            .cloned()
            .map(|device| (device, device_session.clone()))
            .collect::<Vec<_>>();
        for (device, _) in &devices {
            self.output_devices.remove(device);
            self.reset_device_inputs(device);
            if let Some(pending) = self.pending_stops.remove(device) {
                for reply in pending.replies {
                    let _ = reply.send(Err(HubError::DeviceUnavailable));
                }
            }
        }
        // Admission installs every target barrier synchronously before publishing inactive state.
        let completions = devices
            .iter()
            .map(|(device, session)| {
                session
                    .request_stop(device, None, false, generation)
                    .map_err(HubError::from)
            })
            .collect::<Vec<_>>();
        self.mark_connection_disconnected(connection_id);
        let cancellation = CancellationToken::new();
        self.pending_connection_cleanups.insert(
            connection_id.to_owned(),
            PendingConnectionCleanup {
                generation,
                accepted_epoch,
                restart_endpoint,
                reply,
                cancellation: cancellation.clone(),
                devices,
                connection: connection.clone(),
            },
        );
        self.publish();
        let commands = self.command_sender.clone();
        let connection_id = connection_id.to_owned();
        tokio::spawn(async move {
            let cleanup = async {
                let mut first_error = None;
                let mut admitted = Vec::new();
                for completion in completions {
                    match completion {
                        Ok(completion) => admitted.push(completion),
                        Err(error) => {
                            first_error.get_or_insert(error);
                        }
                    }
                }
                for result in futures_util::future::join_all(admitted).await {
                    if let Err(error) = result {
                        first_error.get_or_insert(HubError::from(error));
                    }
                }
                let disconnected = match &connection {
                    CleanupConnection::V4(handle) => handle
                        .disconnect_session(v4_generation)
                        .await
                        .map_err(HubError::from),
                    CleanupConnection::Local(handle) => {
                        handle.disconnect().await.map_err(HubError::from)
                    }
                };
                if let Err(error) = disconnected {
                    first_error.get_or_insert(error);
                }
                first_error.map_or(Ok(()), Err)
            };
            let result = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return,
                result = tokio::time::timeout(Duration::from_secs(10), cleanup) => result.unwrap_or_else(|_| Err(HubError::Transport(TransportError::new("transport_timeout", "连接清理超过十秒期限")))),
            };
            match connection {
                CleanupConnection::V4(handle) => handle.shutdown_now(),
                CleanupConnection::Local(handle) => handle.shutdown_now(),
            }
            let _ = commands
                .send(HubCommand::ConnectionCleanupFinished {
                    connection_id,
                    generation,
                    result,
                })
                .await;
        });
    }

    fn mark_connection_disconnected(&mut self, connection_id: &str) {
        self.remove_connection_devices(connection_id);
        if let Some(state) = self
            .snapshot
            .connections
            .iter_mut()
            .find(|state| state.connection_id == connection_id)
        {
            state.state = ConnectionState::Disconnected;
            state.controller_id = None;
            state.pairing_url = None;
            state.app_count = 0;
            state.last_error = None;
        }
        if connection_id == V4_CONNECTION_ID {
            self.v4_connection.state = ConnectionState::Disconnected;
            self.v4_connection.controller_id = None;
            self.v4_connection.pairing_url = None;
            self.v4_connection.app_count = 0;
            self.v4_connection.last_error = None;
        }
    }

    fn finish_connection_cleanup(
        &mut self,
        connection_id: &str,
        generation: u64,
        result: Result<(), HubError>,
    ) {
        if self
            .pending_connection_cleanups
            .get(connection_id)
            .is_none_or(|pending| pending.generation != generation)
        {
            return;
        }
        let pending = self
            .pending_connection_cleanups
            .remove(connection_id)
            .expect("pending cleanup exists");
        if let Some(error) = result.as_ref().err() {
            if let Some(connection) = self
                .snapshot
                .connections
                .iter_mut()
                .find(|connection| connection.connection_id == connection_id)
            {
                connection.state = ConnectionState::Error;
                connection.last_error = Some(error.to_string());
            }
            if connection_id == V4_CONNECTION_ID {
                self.v4_connection.state = ConnectionState::Error;
                self.v4_connection.last_error = Some(error.to_string());
            }
            self.log(
                LogLevel::Error,
                format!("{connection_id} 清理失败：{error}"),
            );
        }
        self.publish();
        if result.is_ok()
            && let Some(endpoint) = pending.restart_endpoint
        {
            self.resume_connection_after_cleanup(
                connection_id,
                endpoint,
                pending.accepted_epoch,
                pending.reply,
            );
        } else if let Some(reply) = pending.reply {
            reply.send(result);
        }
    }

    fn resume_connection_after_cleanup(
        &mut self,
        connection_id: &str,
        endpoint: String,
        accepted_epoch: u64,
        reply: Option<ConnectionReply>,
    ) {
        if accepted_epoch != self.safety_epoch.load(Ordering::Acquire) {
            if let Some(reply) = reply {
                reply.send(Err(HubError::QueueBusy));
            }
        } else if connection_id == V4_CONNECTION_ID {
            self.v4_connection.endpoint = endpoint;
            self.auto_reconnect_enabled = true;
            self.begin_connect();
            if let Some(reply) = reply {
                reply.send(Ok(()));
            }
        } else if let Some(reply) = reply {
            self.start_v3_connection(endpoint, reply);
        }
    }

    async fn handle_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::Connection(connection) => {
                let id = connection.connection_id.clone();
                if matches!(
                    connection.state,
                    ConnectionState::Disconnected | ConnectionState::Error
                ) {
                    self.connection_started.remove(&id);
                    self.remove_connection_devices(&id);
                } else if matches!(
                    connection.state,
                    ConnectionState::Connected | ConnectionState::Waiting
                ) {
                    self.connection_started
                        .entry(id)
                        .or_insert_with(Instant::now);
                }
                self.update_connection(connection);
            }
            SessionEvent::Device {
                connection_id,
                client_id,
                device,
            } => {
                let key = DeviceKey {
                    connection_id: connection_id.clone(),
                    client_id: client_id.clone(),
                    slot_id: device.slot_id.clone(),
                };
                let value = json!({"id":device.id,"slotId":device.slot_id,"name":device.name,"type":device.device_type,
                    "initialization":device.initialization,"capabilities":device.capabilities,"bleParameters":device.ble_parameters,"configurationStatus":device.configuration_status,
                    "channelAStatus":device.channel_a_status,"channelBStatus":device.channel_b_status,
                    "props":{"power":device.power,"intensityA":device.intensity_a,"intensityB":device.intensity_b},
                    "slotState":{"hasDevice":true,"channelA":{"intensityMax":device.intensity_limit_a},"channelB":{"intensityMax":device.intensity_limit_b}}});
                self.devices.insert(key, value);
                self.observe_projected_intensities(&connection_id, &client_id);
                self.reconcile_connected_devices();
                self.reconcile_intensity_lock();
            }
            SessionEvent::Removed {
                connection_id,
                client_id,
            } => {
                let keys = self
                    .devices
                    .keys()
                    .filter(|k| k.connection_id == connection_id && k.client_id == client_id)
                    .cloned()
                    .collect::<Vec<_>>();
                for key in keys {
                    self.clear_pending_for_device(&key);
                    self.reset_device_inputs(&key);
                    self.devices.remove(&key);
                }
                self.reconcile_connected_devices();
            }
            SessionEvent::OperationFinished {
                connection_id,
                client_id,
                request_id,
                result,
                confirmed,
            } => {
                if let Some(key) = self
                    .pending_intensity_requests
                    .get(&request_id)
                    .cloned()
                    .filter(|key| {
                        key.device.connection_id == connection_id
                            && key.device.client_id == client_id
                    })
                {
                    match result {
                        Ok(()) if confirmed => {
                            if let Some(pending) = self.pending_intensity_operations.get_mut(&key) {
                                pending.response_received = true;
                            }
                            self.observe_projected_intensities(&connection_id, &client_id);
                        }
                        Ok(()) => {}
                        Err(error) => {
                            self.remove_pending_intensity(&key);
                            self.log(
                                LogLevel::Error,
                                format!("设备 {} 强度操作失败：{error}", key.device.control_id()),
                            );
                        }
                    }
                } else if let Some(pending) = self
                    .pending_wave_operations
                    .remove(&request_id)
                    .filter(|p| {
                        p.device.connection_id == connection_id && p.device.client_id == client_id
                    })
                {
                    let Err(error) = result else {
                        return;
                    };
                    if error.code == "operation_cancelled" {
                        return;
                    }
                    // An I/O failure is isolated to the reporting device.
                    let keys = vec![pending.device];
                    for key in keys {
                        self.output_devices.remove(&key);
                        self.reset_device_inputs(&key);
                        self.clear_pending_for_device(&key);
                        if let Ok(session) = self.device_session(&key) {
                            let generation = self.operation_generation;
                            tokio::spawn(async move {
                                let _ = session.stop(&key, None, false, generation).await;
                            });
                        }
                    }
                    if self.output_devices.is_empty()
                        && self.snapshot.output.state == OutputState::Running
                    {
                        self.snapshot.output.state = OutputState::Error;
                    }
                    self.log(
                        LogLevel::Error,
                        format!("{connection_id} 操作失败：{error}"),
                    );
                }
            }
            SessionEvent::Log {
                connection_id,
                message,
                warning,
            } => self.log(
                if warning {
                    LogLevel::Warning
                } else {
                    LogLevel::Info
                },
                format!("{connection_id}：{message}"),
            ),
        }
        self.refresh_device_snapshots();
        self.publish();
    }

    fn reset_device_inputs(&mut self, device: &DeviceKey) {
        self.reset_changed_inputs(device, &Channel::ALL);
    }

    fn reset_changed_inputs(&mut self, device: &DeviceKey, channels: &[Channel]) {
        for channel in channels {
            let binding = SourceBindingKey {
                device: device.clone(),
                channel: *channel,
            };
            if let Some(state) = self.device_source_bindings.get_mut(&binding) {
                state.generation = state.generation.saturating_add(1);
                state.active = self.output_devices.contains(device);
                if let Some(manager) = &self.plugins {
                    manager.frames().invalidate(&state.id, state.generation);
                }
            }
            self.plugin_last_frame
                .insert(binding.clone(), Instant::now());
            self.faulted_bindings.remove(&binding);
        }
    }

    async fn clear_input_channel(&mut self, binding: &SourceBindingKey) -> Result<(), HubError> {
        if !self.output_devices.contains(&binding.device) {
            return Ok(());
        }
        let session = self.device_session(&binding.device)?;
        let clear = session.request_stop(
            &binding.device,
            Some(binding.channel),
            false,
            self.operation_generation,
        )?;
        self.wait_for_ordinary_relay(clear).await?;
        self.pending_wave_operations.retain(|_, pending| {
            pending.device != binding.device || pending.channel != binding.channel
        });
        Ok(())
    }

    fn wait_for_ordinary_relay<'a, T, E, F>(
        &'a self,
        operation: F,
    ) -> impl std::future::Future<Output = Result<T, HubError>> + use<'a, T, E, F>
    where
        E: Into<HubError>,
        F: std::future::Future<Output = Result<T, E>>,
    {
        let wakeup = self.safety_wakeup.clone();
        let epoch = self.safety_epoch.clone();
        let baseline = epoch.load(Ordering::Acquire);
        let queue = &self.safety_commands;
        let mut interrupted = Box::pin(wakeup.clone().notified_owned());
        // Register before examining the queue so a concurrent safety send
        // cannot be lost between the check and the first await.
        interrupted.as_mut().enable();
        let shutdown = self.shutdown.clone();
        async move {
            tokio::pin!(operation);
            loop {
                if !queue.is_empty()
                    || epoch.load(Ordering::Acquire) != baseline
                    || shutdown.is_cancelled()
                {
                    return Err(HubError::QueueBusy);
                }
                tokio::select! {
                    biased;
                    _ = &mut interrupted => {
                        // A sender can notify after the actor has already taken
                        // its command. Ignore that late notification unless a
                        // newer epoch or another queued command requires stopping.
                        interrupted = Box::pin(wakeup.clone().notified_owned());
                        interrupted.as_mut().enable();
                    },
                    _ = shutdown.cancelled() => return Err(HubError::QueueBusy),
                    result = &mut operation => return result.map_err(Into::into),
                }
            }
        }
    }

    pub async fn run(mut self) {
        let (event_sender, mut events) = mpsc::channel(RELAY_EVENT_CAPACITY);
        self.relay_events = Some(event_sender);
        let (session_sender, mut session_events) = mpsc::channel(RELAY_EVENT_CAPACITY);
        self.session_events = Some(session_sender.clone());
        self.register_session(V3_CONNECTION_ID.to_owned(), crate::transport::v3::spawn);

        let mut ticker = tokio::time::interval(WaveFrame::DURATION);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        ticker.tick().await;

        let mut commands_open = true;
        let mut safety_commands_open = true;
        let mut events_open = true;
        loop {
            tokio::select! {
                biased;
                _ = self.shutdown.cancelled() => break,
                command = self.safety_commands.recv(), if safety_commands_open => {
                    match command {
                        Some(command) => self.handle_safety_command(command).await,
                        None => safety_commands_open = false,
                    }
                }
                command = self.commands.recv(), if commands_open => {
                    match command {
                        Some(command) => self.handle_command(command).await,
                        None => commands_open = false,
                    }
                }
                Some((identity,event)) = session_events.recv() => self.handle_current_session_event(identity,event).await,
                event = events.recv(), if events_open => {
                    match event {
                        Some(event) => self.handle_current_relay_event(event).await,
                        None => events_open = false,
                    }
                }
                _ = ticker.tick() => {
                    self.disconnect_if_timed_out().await;
                    self.refresh_plugin_sources();
                    self.publish();
                    self.output_tick().await;
                }
            }

            if !commands_open && !safety_commands_open {
                break;
            }

            self.reconnect_if_due();
        }

        self.snapshot.output.state = OutputState::Stopped;
        self.snapshot.output.last_error = None;
        self.connection_started_at = None;
        for pending in self.pending_connection_cleanups.values() {
            pending.cancellation.cancel();
        }
        let mut shutdown_result = self.send_stop_operations(true).await;
        let local_handles = self
            .sessions
            .values()
            .cloned()
            .chain(
                self.pending_connection_cleanups
                    .values()
                    .filter_map(|pending| match &pending.connection {
                        CleanupConnection::Local(handle) => Some(handle.clone()),
                        _ => None,
                    }),
            )
            .collect::<Vec<_>>();
        let local_results =
            futures_util::future::join_all(local_handles.iter().map(|handle| handle.disconnect()))
                .await;
        for result in local_results {
            if shutdown_result.is_ok() {
                shutdown_result = result.map_err(Into::into);
            }
        }
        for handle in local_handles {
            handle.shutdown_now();
        }
        let old_relays = self
            .pending_connection_cleanups
            .values()
            .filter_map(|pending| {
                if let CleanupConnection::V4(handle) = &pending.connection {
                    Some(handle.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for relay in old_relays {
            let _ = tokio::time::timeout(RELAY_DISCONNECT_TIMEOUT, relay.disconnect()).await;
            relay.shutdown_now();
        }
        if let Some(relay) = &self.relay {
            let disconnect_result =
                tokio::time::timeout(RELAY_DISCONNECT_TIMEOUT, relay.disconnect())
                    .await
                    .map_err(|_| HubError::Relay("断开 Relay 超时".to_owned()))
                    .and_then(|result| result.map_err(Into::into));
            if shutdown_result.is_ok() {
                shutdown_result = disconnect_result;
            }
            relay.shutdown_now();
        }
        let joins = self.session_tasks.drain(..).map(|mut task| async move {
            if tokio::time::timeout(RELAY_JOIN_TIMEOUT, &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        });
        futures_util::future::join_all(joins).await;
        self.snapshot.output.state = OutputState::Stopped;
        self.refresh_device_snapshots();
        self.publish();
        let _ = self.completion_sender.send(Some(shutdown_result));
    }

    fn begin_connect(&mut self) {
        if let Some(events) = self.relay_events.clone() {
            if let Some(previous) = self.relay.take() {
                previous.shutdown_now();
            }
            let (relay, task) = spawn_relay_client(events, RELAY_COMMAND_CAPACITY);
            self.session_tasks.retain(|task| !task.is_finished());
            self.session_tasks.push(task);
            self.relay = Some(relay);
        }
        self.reset_connection_state(ConnectionState::Connecting);
        self.v4_session_generation = Some(self.operation_generation);
        self.log(LogLevel::Info, "正在连接 DG-LAB Relay");
        self.publish();
        if let Some(relay) = self.relay.clone() {
            let endpoint = self.v4_connection.endpoint.clone();
            let generation = self.operation_generation;
            tokio::spawn(async move {
                let _ = relay.connect_at(endpoint, generation).await;
            });
        }
    }

    fn connection_timed_out(&self) -> bool {
        self.snapshot.safety.connection_timeout_enabled
            && self.connection_started_at.is_some_and(|started_at| {
                started_at.elapsed()
                    >= Duration::from_secs(
                        u64::from(self.snapshot.safety.connection_timeout_minutes) * 60,
                    )
            })
    }

    async fn disconnect_if_timed_out(&mut self) {
        if !self.snapshot.safety.connection_timeout_enabled {
            return;
        }
        let limit =
            Duration::from_secs(u64::from(self.snapshot.safety.connection_timeout_minutes) * 60);
        let mut expired = self
            .connection_started
            .iter()
            .filter(|(_, started)| started.elapsed() >= limit)
            .map(|(id, _)| id.clone())
            .collect::<BTreeSet<_>>();
        if self.connection_timed_out() {
            expired.insert(V4_CONNECTION_ID.into());
        }
        for id in expired {
            if self.pending_connection_cleanups.contains_key(&id) {
                continue;
            }
            let epoch =
                revoke_command_epoch(&self.safety_epoch, &self.epoch_sender, &self.safety_wakeup);
            self.queue_connection_cleanup(&id, None, epoch, None);
            self.log(
                LogLevel::Warning,
                format!("{id} 连接时长已到，正在清理所属设备"),
            );
        }
    }

    async fn handle_command(&mut self, command: HubCommand) {
        match command {
            HubCommand::ConnectionCleanupFinished {
                connection_id,
                generation,
                result,
            } => self.finish_connection_cleanup(&connection_id, generation, result),
            HubCommand::StopOutputFinished {
                device,
                generation,
                result,
            } => {
                self.finish_device_stop(&device, generation, result);
            }
            HubCommand::ChannelClearFailed {
                binding,
                generation,
                message,
            } => {
                if self
                    .device_source_bindings
                    .get(&binding)
                    .is_some_and(|state| state.generation == generation)
                {
                    self.fail_plugin_binding(&binding, &message);
                    self.refresh_device_snapshots();
                    self.publish();
                }
            }

            HubCommand::ScanResult { devices } => {
                self.snapshot.bluetooth = devices;
                self.publish();
            }
            HubCommand::Transport {
                action,
                safety_epoch,
                reply,
            } => {
                if safety_epoch != self.safety_epoch.load(Ordering::Acquire) {
                    let _ = reply.send(Err(HubError::QueueBusy));
                } else {
                    self.handle_transport_action(action, safety_epoch, reply)
                        .await;
                }
            }
            HubCommand::RefreshPlugins { reply } => {
                self.refresh_plugin_sources();
                self.refresh_source_snapshots();
                self.refresh_device_snapshots();
                self.publish();
                let _ = reply.send(Ok(()));
            }
            HubCommand::SetPluginBindingConfig {
                source_id,
                binding_id,
                config,
                expected_revision,
                reply,
            } => {
                let key = self
                    .device_source_bindings
                    .iter()
                    .find(|(_, state)| state.source_id == source_id && state.id == binding_id)
                    .map(|(key, _)| key.clone());
                let result = if let Some(key) = key {
                    let binding = self
                        .device_source_bindings
                        .get_mut(&key)
                        .expect("binding exists");
                    if binding.revision != expected_revision {
                        Err(HubError::ConfigConflict)
                    } else {
                        binding.config = config;
                        binding.revision = binding.revision.saturating_add(1);
                        self.reset_changed_inputs(&key.device, &[key.channel]);
                        self.publish();
                        Ok(())
                    }
                } else {
                    Err(HubError::ConfigConflict)
                };
                let _ = reply.send(result);
            }
            HubCommand::AdjustIntensity {
                device_id,
                channel,
                delta,
                safety_epoch,
                reply,
            } => {
                let result = if safety_epoch == self.safety_epoch.load(Ordering::Acquire) {
                    self.adjust_device_intensity(device_id.as_deref(), channel, delta)
                } else {
                    Err(HubError::QueueBusy)
                };
                let _ = reply.send(result);
            }
            HubCommand::StartOutput {
                device_id,
                safety_epoch,
                reply,
            } => {
                let result = if safety_epoch == self.safety_epoch.load(Ordering::Acquire) {
                    self.start_output(&device_id)
                } else {
                    Err(HubError::QueueBusy)
                };
                let _ = reply.send(result);
            }
            HubCommand::SetDeviceChannelSource {
                device_id,
                channel,
                source_id,
                reply,
            } => {
                let result = self
                    .set_device_channel_source(device_id, channel, source_id)
                    .await;
                let _ = reply.send(result);
            }
            HubCommand::SetDefaultSource { source_id, reply } => {
                let result = self.set_default_source(source_id);
                let _ = reply.send(result);
            }
            HubCommand::SetFixedWaveform {
                device_id,
                channel,
                config,
                reply,
            } => {
                let result = self.set_fixed_waveform(&device_id, channel, config);
                let _ = reply.send(result);
            }
            HubCommand::SetWaveformState {
                waveforms,
                selected,
                reply,
            } => {
                let result = self.set_waveform_state(waveforms, selected);
                let _ = reply.send(result);
            }
            HubCommand::SetDeviceChannelSourceSync {
                device_id,
                enabled,
                reply,
            } => {
                let result = self
                    .set_device_channel_source_sync(device_id, enabled)
                    .await;
                let _ = reply.send(result);
            }
            HubCommand::SetSyncAllDevices {
                device_id,
                enabled,
                safety_epoch,
                reply,
            } => {
                let result = if safety_epoch == self.safety_epoch.load(Ordering::Acquire) {
                    self.set_sync_all_devices_from(Some(device_id), enabled)
                } else {
                    Err(HubError::QueueBusy)
                };
                let _ = reply.send(result);
            }
            HubCommand::UpdateSafety {
                connection_timeout_enabled,
                connection_timeout_minutes,
                allow_app_intensity_control,
                reply,
            } => {
                let result = self
                    .update_safety(
                        connection_timeout_enabled,
                        connection_timeout_minutes,
                        allow_app_intensity_control,
                    )
                    .await;
                let _ = reply.send(result);
            }
        }
    }

    async fn handle_safety_command(&mut self, command: HubSafetyCommand) {
        match command {
            HubSafetyCommand::DisconnectConnection {
                connection_id,
                reply,
            } => {
                self.queue_connection_cleanup(
                    &connection_id,
                    None,
                    self.safety_epoch.load(Ordering::Acquire),
                    Some(ConnectionReply::Unit(reply)),
                );
            }

            HubSafetyCommand::ClearDeviceChannel {
                device_id,
                channel,
                reply,
            } => {
                let result = self.clear_device_channel(&device_id, channel);
                let _ = reply.send(result);
            }
            HubSafetyCommand::StopOutput { device_id, reply } => {
                match self.prepare_device_stop(&device_id) {
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                    Ok((device, generation, completion)) => {
                        let Some(pending) = self.pending_stops.get_mut(&device) else {
                            let _ = reply.send(Ok(()));
                            return;
                        };
                        if pending.replies.len() >= HUB_COMMAND_CAPACITY {
                            let _ = reply.send(Err(HubError::QueueBusy));
                        } else {
                            pending.replies.push(reply);
                        }
                        if let Some(completion) = completion {
                            let commands = self.command_sender.clone();
                            tokio::spawn(async move {
                                let result = completion.await.map_err(HubError::from);
                                let _ = commands
                                    .send(HubCommand::StopOutputFinished {
                                        device,
                                        generation,
                                        result,
                                    })
                                    .await;
                            });
                        }
                    }
                }
            }
        }
    }

    async fn handle_current_relay_event(&mut self, event: RelaySessionEvent) {
        if self.v4_session_generation == Some(event.generation) {
            self.handle_relay_event(event.event).await;
        }
    }

    async fn handle_relay_event(&mut self, event: RelayEvent) {
        match event {
            RelayEvent::Connecting { endpoint } => {
                self.v4_connection.endpoint = endpoint;
                self.v4_connection.state = ConnectionState::Connecting;
                self.v4_connection.last_error = None;
                self.publish();
            }
            RelayEvent::Connected { .. } => {
                self.connection_started_at = Some(Instant::now());
                self.v4_connection.state = ConnectionState::Waiting;
                self.v4_connection.last_error = None;
                self.log(LogLevel::Info, "Relay 已连接，正在等待握手");
                self.publish();
            }
            RelayEvent::Hello { controller_id } => {
                self.reconnect_at = None;
                self.reconnect_attempt = 0;
                self.v4_connection.controller_id = Some(controller_id.clone());
                self.v4_connection.pairing_url =
                    pairing_url(&self.v4_connection.endpoint, &controller_id).ok();
                self.v4_connection.state = if self.apps.is_empty() {
                    ConnectionState::Waiting
                } else {
                    ConnectionState::Connected
                };
                self.log(LogLevel::Info, "已生成新的 APP 配对二维码");
                self.publish();
            }
            RelayEvent::ClientAttached { client_id } => {
                let first = self.apps.insert(client_id.clone());
                self.v4_connection.app_count = self.apps.len();
                self.v4_connection.state = ConnectionState::Connected;
                self.v4_connection.last_error = None;
                if first {
                    self.log(LogLevel::Info, format!("DG-LAB APP 已连接：{client_id}"));
                    let request = devices_get_request();
                    let _ = self.send_to_app(&client_id, request);
                }
                self.publish();
            }
            RelayEvent::ClientDisconnected { client_id } => {
                self.remove_app(&client_id);
                self.log(LogLevel::Warning, format!("DG-LAB APP 已断开：{client_id}"));
                self.publish();
            }
            RelayEvent::Message { client_id, data } => {
                self.apply_app_message(&client_id, &data).await;
            }
            RelayEvent::IdleTimeout => {
                self.connection_started_at = None;
                self.v4_connection.state = ConnectionState::Error;
                self.v4_connection.last_error = Some("Relay 因长时间无 APP 接入而断开".to_owned());
                self.log(
                    LogLevel::Warning,
                    "Relay 配对等待已超时，可点击重新连接生成新二维码",
                );
                self.publish();
            }
            RelayEvent::RelayError { code, message } => {
                let text = message.unwrap_or_else(|| code.clone());
                self.v4_connection.last_error = Some(text.clone());
                self.log(LogLevel::Error, format!("Relay 返回错误 {code}：{text}"));
                self.publish();
            }
            RelayEvent::Disconnected { reason, retryable } => {
                let had_v4_output = self
                    .output_devices
                    .iter()
                    .any(|device| device.connection_id == V4_CONNECTION_ID);
                self.reset_connection_state(ConnectionState::Disconnected);
                if had_v4_output && self.output_devices.is_empty() {
                    self.snapshot.output.state = OutputState::Error;
                    self.snapshot.output.last_error =
                        Some("V4 Relay 断开，所属设备输出已停止".to_owned());
                }
                if retryable && self.auto_reconnect_enabled {
                    let delay = self.schedule_reconnect();
                    let message = format!("{reason}，将在 {} 秒后自动重连", delay.as_secs());
                    self.v4_connection.last_error = Some(message.clone());
                    self.log(LogLevel::Warning, message);
                } else {
                    self.v4_connection.last_error = Some(reason.clone());
                    self.log(LogLevel::Warning, reason);
                }
                self.publish();
            }
            RelayEvent::Heartbeat | RelayEvent::Pong { .. } | RelayEvent::Unknown(_) => {}
        }
    }

    #[cfg(test)]
    fn enable_auto_reconnect(&mut self) {
        self.auto_reconnect_enabled = true;
        self.reconnect_at = None;
        self.reconnect_attempt = 0;
    }

    fn disable_auto_reconnect(&mut self) {
        self.auto_reconnect_enabled = false;
        self.reconnect_at = None;
        self.reconnect_attempt = 0;
    }

    fn schedule_reconnect(&mut self) -> Duration {
        let exponent = self.reconnect_attempt.min(5);
        let seconds = (1_u64 << exponent).min(RELAY_RECONNECT_MAX_DELAY_SECONDS);
        let delay = Duration::from_secs(seconds);
        self.reconnect_at = Some(Instant::now() + delay);
        self.reconnect_attempt = self.reconnect_attempt.saturating_add(1);
        delay
    }

    fn reconnect_if_due(&mut self) {
        if !self.auto_reconnect_enabled {
            return;
        }
        let Some(reconnect_at) = self.reconnect_at else {
            return;
        };
        if Instant::now() < reconnect_at {
            return;
        }
        self.reconnect_at = None;
        self.begin_connect();
    }

    async fn apply_app_message(&mut self, client_id: &str, data: &Value) {
        // Relay 消息可能与 client_disconnected 交错到达。断开的 APP 不得重新建立幽灵设备状态。
        if !self.apps.contains(client_id) {
            return;
        }

        if data.get("t").and_then(Value::as_str) == Some("resp") {
            let request_id = data.get("reqId").and_then(Value::as_str);
            let error = data.get("error").map(|error| {
                error
                    .as_str()
                    .map_or_else(|| error.to_string(), str::to_owned)
            });
            let mut wave_failure = None;
            let mut completed_intensity = None;

            if let Some(request_id) = request_id {
                if let Some(pending) = self.pending_wave_operations.remove(request_id) {
                    if pending.device.connection_id != V4_CONNECTION_ID
                        || pending.device.client_id != client_id
                    {
                        self.pending_wave_operations
                            .insert(request_id.to_owned(), pending);
                    } else if error.is_some()
                        && self
                            .device_source_bindings
                            .get(&SourceBindingKey {
                                device: pending.device.clone(),
                                channel: pending.channel,
                            })
                            .is_some_and(|binding| {
                                binding.generation == pending.generation && binding.active
                            })
                    {
                        wave_failure = error.clone().map(|error| (pending.device, error));
                    }
                }

                if let Some(key) = self.pending_intensity_requests.remove(request_id) {
                    if key.device.connection_id == V4_CONNECTION_ID
                        && key.device.client_id == client_id
                    {
                        if error.is_some() {
                            if self
                                .pending_intensity_operations
                                .remove(&key)
                                .is_some_and(|pending| pending.lock_correction)
                            {
                                self.log(
                                    LogLevel::Error,
                                    format!("APP 拒绝恢复 {} 通道的电脑端锁定强度", key.channel),
                                );
                            }
                        } else if let Some(pending) =
                            self.pending_intensity_operations.get_mut(&key)
                        {
                            pending.response_received = true;
                            if pending.projected_observed {
                                completed_intensity = Some(key);
                            }
                        }
                    } else {
                        self.pending_intensity_requests
                            .insert(request_id.to_owned(), key);
                    }
                }
            }

            let devices = data
                .get("result")
                .and_then(|result| result.get("devices"))
                .and_then(Value::as_array);
            if let Some(devices) = devices {
                self.replace_devices(client_id, devices);
            }
            if let Some(key) = completed_intensity {
                self.remove_pending_intensity(&key);
                self.refresh_device_snapshots();
                self.reconcile_intensity_lock();
                self.publish();
            }

            if let Some((device, error)) = wave_failure {
                self.fail_device_output(&device, format!("APP 拒绝波形操作：{error}"));
                self.log(
                    LogLevel::Error,
                    format!("APP 拒绝设备 {} 波形操作：{error}", device.control_id()),
                );
                self.publish();
                return;
            }
            if let Some(error) = error {
                self.snapshot.output.last_error = Some(error.clone());
                self.log(LogLevel::Error, format!("APP 操作失败：{error}"));
                self.publish();
            }
            return;
        }

        if data.get("t").and_then(Value::as_str) != Some("ev") {
            return;
        }
        match data.get("ev").and_then(Value::as_str) {
            Some("devices.snapshot") => {
                if let Some(devices) = data.get("devices").and_then(Value::as_array) {
                    self.replace_devices(client_id, devices);
                }
            }
            Some("devices.patch") => {
                self.apply_devices_patch(client_id, data);
            }
            Some("slots.patch") => {
                self.apply_slots_patch(client_id, data);
            }
            _ => {}
        }
    }

    fn replace_devices(&mut self, client_id: &str, devices: &[Value]) {
        let previous_values = self
            .devices
            .iter()
            .filter(|(key, _)| key.connection_id == V4_CONNECTION_ID && key.client_id == client_id)
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        let previous = previous_values.keys().cloned().collect::<BTreeSet<_>>();
        self.devices.retain(|key, _| {
            key.connection_id != V4_CONNECTION_ID || key.client_id.as_str() != client_id
        });
        for device in devices {
            if let Some(slot_id) = device.get("slotId").and_then(Value::as_str) {
                let key = DeviceKey {
                    connection_id: V4_CONNECTION_ID.to_owned(),
                    client_id: client_id.to_owned(),
                    slot_id: slot_id.to_owned(),
                };
                let mut merged = previous_values
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                deep_merge(&mut merged, device);
                self.devices.insert(key, merged);
            }
        }
        let current = self
            .devices
            .keys()
            .filter(|key| key.connection_id == V4_CONNECTION_ID && key.client_id == client_id)
            .cloned()
            .collect::<BTreeSet<_>>();
        for removed in previous.difference(&current) {
            self.clear_pending_for_device(removed);
        }
        self.observe_projected_intensities(V4_CONNECTION_ID, client_id);
        self.reconcile_connected_devices();
        self.refresh_device_snapshots();
        self.reconcile_intensity_lock();
        self.publish();
    }

    fn apply_devices_patch(&mut self, client_id: &str, data: &Value) {
        if let Some(removed) = data.get("removed").and_then(Value::as_array) {
            for slot_id in removed.iter().filter_map(Value::as_str) {
                let key = DeviceKey {
                    connection_id: V4_CONNECTION_ID.to_owned(),
                    client_id: client_id.to_owned(),
                    slot_id: slot_id.to_owned(),
                };
                self.devices.remove(&key);
                self.clear_pending_for_device(&key);
            }
        }
        if let Some(added) = data.get("added").and_then(Value::as_array) {
            for device in added {
                if let Some(slot_id) = device.get("slotId").and_then(Value::as_str) {
                    self.devices.insert(
                        DeviceKey {
                            connection_id: V4_CONNECTION_ID.to_owned(),
                            client_id: client_id.to_owned(),
                            slot_id: slot_id.to_owned(),
                        },
                        device.clone(),
                    );
                }
            }
        }
        self.reconcile_connected_devices();
        self.refresh_device_snapshots();
        self.reconcile_intensity_lock();
        self.publish();
    }

    fn apply_slots_patch(&mut self, client_id: &str, data: &Value) {
        let patches = data
            .get("slots")
            .or_else(|| data.get("patches"))
            .and_then(Value::as_array);
        if let Some(patches) = patches {
            for patch in patches {
                if let Some(slot_id) = patch.get("slotId").and_then(Value::as_str) {
                    let key = DeviceKey {
                        connection_id: V4_CONNECTION_ID.to_owned(),
                        client_id: client_id.to_owned(),
                        slot_id: slot_id.to_owned(),
                    };
                    if let Some(device) = self.devices.get_mut(&key) {
                        deep_merge(device, patch);
                    }
                }
            }
        }
        self.observe_projected_intensities(V4_CONNECTION_ID, client_id);
        self.refresh_device_snapshots();
        self.reconcile_intensity_lock();
        self.publish();
    }

    fn remove_app(&mut self, client_id: &str) {
        self.apps.remove(client_id);
        self.devices.retain(|key, _| {
            key.connection_id != V4_CONNECTION_ID || key.client_id.as_str() != client_id
        });
        self.clear_pending_for_client(client_id);
        self.v4_connection.app_count = self.apps.len();
        self.v4_connection.state = if self.apps.is_empty() {
            ConnectionState::Waiting
        } else {
            ConnectionState::Connected
        };
        self.reconcile_connected_devices();
        self.refresh_device_snapshots();
        self.reconcile_intensity_lock();
    }

    fn reconcile_connected_devices(&mut self) {
        self.shutdown_zero_pending
            .retain(|device| self.devices.contains_key(device));
        if self
            .sync_baseline_device
            .as_ref()
            .is_some_and(|device| !self.devices.contains_key(device))
        {
            self.sync_baseline_device = None;
        }
        let disconnected = self
            .output_devices
            .iter()
            .filter(|device| !self.devices.contains_key(*device))
            .cloned()
            .collect::<Vec<_>>();
        for device in disconnected {
            self.output_devices.remove(&device);
            self.clear_pending_for_device(&device);
            self.log(
                LogLevel::Warning,
                format!("输出设备已断开：{}；其他设备继续输出", device.control_id()),
            );
        }
        self.intensity_lock_targets
            .retain(|device, _| self.devices.contains_key(device));
        self.device_source_bindings
            .retain(|binding, _| self.devices.contains_key(&binding.device));
        self.plugin_last_frame
            .retain(|binding, _| self.devices.contains_key(&binding.device));
        self.faulted_bindings
            .retain(|binding| self.devices.contains_key(&binding.device));
        let connected_devices = self.devices.keys().cloned().collect::<Vec<_>>();
        for device in connected_devices {
            if self.initialized_source_devices.insert(device.clone())
                && let Some(default_source_id) = self.default_source_id.clone()
            {
                for channel in Channel::ALL {
                    let binding = SourceBindingKey {
                        device: device.clone(),
                        channel,
                    };
                    self.replace_source_binding(binding.clone(), default_source_id.clone());
                    if default_source_id == FIXED_WAVEFORM_SOURCE_ID
                        && let Some(config) = self.default_fixed_waveform.clone()
                    {
                        let runtime = build_fixed_waveform_runtime(config)
                            .expect("已验证的默认固定波形必须能够创建实例");
                        self.fixed_waveform_bindings.insert(binding, runtime);
                    }
                }
            }
        }
        self.refresh_source_snapshots();
        if self.snapshot.output.state == OutputState::Running && self.output_devices.is_empty() {
            self.snapshot.output.state = OutputState::Idle;
            self.snapshot.output.last_error = None;
            self.log(LogLevel::Warning, "所有输出设备均已断开，输出已停止");
        }
    }

    fn refresh_device_snapshots(&mut self) {
        let allow_app_control = self.snapshot.safety.allow_app_intensity_control;
        let devices = self
            .devices
            .iter()
            .filter_map(|(key, value)| {
                let mut snapshot = device_snapshot_from_value(key, value)?;
                snapshot.source_id_a = self
                    .device_source_bindings
                    .get(&SourceBindingKey {
                        device: key.clone(),
                        channel: Channel::A,
                    })
                    .map(|binding| binding.source_id.clone());
                snapshot.source_id_b = self
                    .device_source_bindings
                    .get(&SourceBindingKey {
                        device: key.clone(),
                        channel: Channel::B,
                    })
                    .map(|binding| binding.source_id.clone());
                if snapshot.source_id_a.as_deref() == Some(FIXED_WAVEFORM_SOURCE_ID)
                    && let Some(runtime) = self.fixed_waveform_bindings.get(&SourceBindingKey {
                        device: key.clone(),
                        channel: Channel::A,
                    })
                {
                    snapshot.waveform_id_a = Some(runtime.config.preset_id.clone());
                    snapshot.waveform_name_a = Some(runtime.config.preset_name.clone());
                }
                if snapshot.source_id_b.as_deref() == Some(FIXED_WAVEFORM_SOURCE_ID)
                    && let Some(runtime) = self.fixed_waveform_bindings.get(&SourceBindingKey {
                        device: key.clone(),
                        channel: Channel::B,
                    })
                {
                    snapshot.waveform_id_b = Some(runtime.config.preset_id.clone());
                    snapshot.waveform_name_b = Some(runtime.config.preset_name.clone());
                }
                snapshot.binding_id_a = self
                    .device_source_bindings
                    .get(&SourceBindingKey {
                        device: key.clone(),
                        channel: Channel::A,
                    })
                    .map(|binding| binding.id.clone());
                snapshot.binding_id_b = self
                    .device_source_bindings
                    .get(&SourceBindingKey {
                        device: key.clone(),
                        channel: Channel::B,
                    })
                    .map(|binding| binding.id.clone());
                snapshot.source_sync = self.source_sync_devices.contains(key);
                if !allow_app_control
                    && snapshot.transport != TransportKind::Ble
                    && let Some(lock) = self.intensity_lock_targets.get(key)
                {
                    snapshot.intensity_a = lock.a;
                    snapshot.intensity_b = lock.b;
                }
                if self.snapshot.output.state == OutputState::Running
                    && self.output_devices.contains(key)
                {
                    snapshot.output_active = true;
                    snapshot.channel_a_status = running_status(snapshot.channel_a_status, true);
                    snapshot.channel_b_status = running_status(snapshot.channel_b_status, true);
                }
                if self.faulted_bindings.contains(&SourceBindingKey {
                    device: key.clone(),
                    channel: Channel::A,
                }) {
                    snapshot.channel_a_status = ChannelStatus::Fault;
                }
                if self.faulted_bindings.contains(&SourceBindingKey {
                    device: key.clone(),
                    channel: Channel::B,
                }) {
                    snapshot.channel_b_status = ChannelStatus::Fault;
                }
                Some(snapshot)
            })
            .collect::<Vec<_>>();
        self.snapshot.devices = devices;
        self.snapshot.output_device_count = self.output_devices.len();
    }

    fn refresh_source_snapshots(&mut self) {
        let mut assigned_counts = HashMap::<&str, usize>::new();
        for (binding, source_id) in &self.device_source_bindings {
            if self.devices.contains_key(&binding.device) {
                *assigned_counts
                    .entry(source_id.source_id.as_str())
                    .or_default() += 1;
            }
        }
        for source in self.sources.values_mut() {
            source.snapshot.assigned_channel_count = assigned_counts
                .get(source.snapshot.id.as_str())
                .copied()
                .unwrap_or(0);
        }
        self.snapshot.sources = ordered_source_snapshots(&self.sources);
    }

    fn start_output(&mut self, device_id: &str) -> Result<(), HubError> {
        let device = self
            .devices
            .keys()
            .find(|device| device.control_id() == device_id)
            .cloned()
            .ok_or(HubError::DeviceUnavailable)?;
        if self.shutdown_zero_pending.contains(&device) || self.pending_stops.contains_key(&device)
        {
            return Err(HubError::QueueBusy);
        }
        let snapshot = self
            .devices
            .get(&device)
            .and_then(|value| device_snapshot_from_value(&device, value))
            .ok_or(HubError::DeviceUnavailable)?;
        if snapshot.initialization != InitializationState::Ready {
            return Err(HubError::DeviceUnavailable);
        }
        if self.output_devices.contains(&device)
            && Channel::ALL.iter().all(|channel| {
                !self.faulted_bindings.contains(&SourceBindingKey {
                    device: device.clone(),
                    channel: *channel,
                })
            })
        {
            return Ok(());
        }
        if self.output_devices.len() >= MAX_OUTPUT_DEVICES {
            return Err(HubError::TooManyDevices);
        }
        for channel in Channel::ALL {
            let source_id = self
                .device_source_bindings
                .get(&SourceBindingKey {
                    device: device.clone(),
                    channel,
                })
                .ok_or(HubError::NoSource)?;
            let source = self
                .sources
                .get(&source_id.source_id)
                .ok_or(HubError::NoSource)?;
            if !source.snapshot.enabled
                || (source.snapshot.plugin_id.is_some()
                    && source.snapshot.runtime_status != "running")
            {
                return Err(HubError::SourceUnavailable(source_id.source_id.clone()));
            }
        }
        self.snapshot.output.state = OutputState::Running;
        self.output_devices.insert(device.clone());
        self.reset_device_inputs(&device);
        self.snapshot.output.last_error = None;
        self.refresh_device_snapshots();
        self.log(
            LogLevel::Info,
            format!("设备 {} 的波形输出已开始", device.control_id()),
        );
        self.publish();
        Ok(())
    }

    async fn output_tick(&mut self) {
        if self.snapshot.output.state != OutputState::Running {
            return;
        }
        if self.output_devices.is_empty() {
            return;
        }
        let timed_out = self
            .pending_wave_operations
            .values()
            .filter(|pending| pending.sent_at.elapsed() >= WAVE_OPERATION_RESPONSE_TIMEOUT)
            .map(|p| p.device.clone())
            .collect::<BTreeSet<_>>();
        if !timed_out.is_empty() {
            for device in timed_out {
                self.fail_device_output(&device, "设备波形响应超时，输出已停止".to_owned());
            }
            return;
        }
        // `device.op` 会在波形任务播放完毕后才响应。网络或 APP 调度发生短暂抖动时，
        // 等待响应的任务会自然积压；到达高水位后暂停生产，待响应释放容量再继续。
        let operation_count = self.output_devices.len().saturating_mul(Channel::ALL.len());
        if self.pending_wave_operations.len() >= MAX_PENDING_WAVE_OPERATIONS
            || self.pending_wave_operations.len() + operation_count > MAX_PENDING_WAVE_OPERATIONS
        {
            return;
        }
        let devices = self.output_devices.iter().cloned().collect::<Vec<_>>();
        if devices.is_empty() {
            self.fail_output("所有输出设备均已断开").await;
            return;
        }
        let mut bindings_by_source = BTreeMap::<String, Vec<SourceBindingKey>>::new();
        for device in devices {
            for channel in Channel::ALL {
                let binding = SourceBindingKey {
                    device: device.clone(),
                    channel,
                };
                let Some(state) = self.device_source_bindings.get(&binding) else {
                    self.fail_plugin_binding(&binding, "通道尚未分配可用输入源");
                    continue;
                };
                if self.faulted_bindings.contains(&binding) {
                    continue;
                }
                if !state.active {
                    continue;
                }
                let source_id = state.source_id.clone();
                bindings_by_source
                    .entry(source_id)
                    .or_default()
                    .push(binding);
            }
        }
        let mut frames_by_source = BTreeMap::<String, WaveFrame>::new();
        let mut frames_by_binding = BTreeMap::<SourceBindingKey, WaveFrame>::new();
        for (source_id, bindings) in &bindings_by_source {
            if source_id == FIXED_WAVEFORM_SOURCE_ID {
                for binding in bindings {
                    let Some(waveform) = self.fixed_waveform_bindings.get_mut(binding) else {
                        continue;
                    };
                    let frame = match waveform.source.next_frame() {
                        Ok(frame) => frame,
                        Err(error) => {
                            self.fail_output(format!(
                                "设备 {} 的 {} 通道固定波形运行失败：{error}",
                                binding.device.control_id(),
                                channel_label(binding.channel)
                            ))
                            .await;
                            return;
                        }
                    };
                    frames_by_binding.insert(binding.clone(), frame);
                }
                continue;
            }
            let is_plugin = self
                .sources
                .get(source_id)
                .is_some_and(|source| source.snapshot.plugin_id.is_some());
            if is_plugin {
                for binding in bindings {
                    if self.faulted_bindings.contains(binding) {
                        continue;
                    }
                    let Some(state) = self.device_source_bindings.get(binding) else {
                        continue;
                    };
                    if !state.active {
                        continue;
                    }
                    let generation = state.generation;
                    let id = state.id.clone();
                    let frame = self
                        .plugins
                        .as_ref()
                        .and_then(|manager| manager.frames().take(&id, generation));
                    if let Some(frame) = frame {
                        let samples = frame.samples.map(|sample| {
                            crate::model::WaveSample::new(sample.frequency, sample.pulse_intensity)
                                .expect("插件帧已校验")
                        });
                        frames_by_binding.insert(binding.clone(), WaveFrame::new(samples));
                        self.plugin_last_frame
                            .insert(binding.clone(), Instant::now());
                    } else {
                        let last = *self
                            .plugin_last_frame
                            .entry(binding.clone())
                            .or_insert_with(Instant::now);
                        if last.elapsed() >= Duration::from_millis(500) {
                            self.fail_plugin_binding(binding, "插件波形超时，当前通道已停止");
                        } else {
                            frames_by_binding.insert(binding.clone(), WaveFrame::silent());
                        }
                    }
                }
                continue;
            }
            let frame_result = self
                .sources
                .get_mut(source_id)
                .and_then(|source| source.source.as_mut())
                .map(|source| source.next_frame());
            match frame_result {
                Some(Ok(frame)) => {
                    frames_by_source.insert(source_id.clone(), frame);
                }
                Some(Err(error)) => {
                    for binding in bindings {
                        self.fail_plugin_binding(binding, &error.to_string());
                    }
                }
                None => {
                    for binding in bindings {
                        self.fail_plugin_binding(binding, "输入源不可用");
                    }
                }
            }
        }
        let mut sent = 0_u64;
        for (source_id, bindings) in bindings_by_source {
            for binding in bindings {
                let frame_hex = frames_by_binding
                    .get(&binding)
                    .or_else(|| frames_by_source.get(&source_id));
                let Some(frame_hex) = frame_hex else {
                    continue;
                };
                let request_id = Uuid::new_v4().to_string();
                let send_result = self.device_session(&binding.device).and_then(|session| {
                    session
                        .try_send(
                            &binding.device,
                            DeviceOperation::Wave {
                                request_id: request_id.clone(),
                                slot_id: binding.device.slot_id.clone(),
                                channel: binding.channel,
                                frame: *frame_hex,
                            },
                            self.operation_generation,
                        )
                        .map_err(Into::into)
                });
                match send_result {
                    Ok(()) => {
                        self.pending_wave_operations.insert(
                            request_id,
                            PendingWaveOperation {
                                generation: self
                                    .device_source_bindings
                                    .get(&binding)
                                    .map_or(0, |state| state.generation),
                                device: binding.device,
                                channel: binding.channel,
                                sent_at: Instant::now(),
                            },
                        );
                        sent += 1;
                    }
                    Err(HubError::QueueBusy) => continue,
                    Err(HubError::Transport(ref error)) if error.code == "queue_busy" => continue,
                    Err(error) => {
                        self.output_devices.remove(&binding.device);
                        self.reset_device_inputs(&binding.device);
                        self.clear_pending_for_device(&binding.device);
                        self.log(
                            LogLevel::Error,
                            format!("设备 {} 波形提交失败：{error}", binding.device.control_id()),
                        );
                        if self.output_devices.is_empty() {
                            self.snapshot.output.state = OutputState::Error;
                        }
                    }
                }
            }
        }
        self.snapshot.output.frames_sent = self.snapshot.output.frames_sent.saturating_add(sent);
        self.publish();
    }

    fn clear_device_channel(
        &mut self,
        device_id: &str,
        channel: Channel,
    ) -> Result<(String, u64), HubError> {
        let device = self
            .devices
            .keys()
            .find(|device| device.control_id() == device_id)
            .cloned()
            .ok_or(HubError::DeviceUnavailable)?;
        let session = self.device_session(&device)?;
        let binding = SourceBindingKey {
            device: device.clone(),
            channel,
        };
        let state = self
            .device_source_bindings
            .get_mut(&binding)
            .ok_or(HubError::NoSource)?;
        state.generation = state.generation.saturating_add(1);
        let generation = state.generation;
        let binding_id = state.id.clone();
        if let Some(manager) = &self.plugins {
            manager.frames().invalidate(&state.id, generation);
        }
        self.plugin_last_frame
            .insert(binding.clone(), Instant::now());
        self.pending_wave_operations
            .retain(|_, operation| operation.device != device || operation.channel != channel);
        let operation_generation = self.operation_generation;
        let commands = self.command_sender.clone();
        let completion =
            session.request_stop(&device, Some(channel), false, operation_generation)?;
        tokio::spawn(async move {
            if let Err(error) = completion.await {
                let _ = commands
                    .send(HubCommand::ChannelClearFailed {
                        binding,
                        generation,
                        message: format!("通道清理失败：{error}"),
                    })
                    .await;
            }
        });
        self.publish_plugin_bindings();
        self.publish();
        Ok((binding_id, generation))
    }

    fn fail_device_output(&mut self, device: &DeviceKey, message: String) {
        self.output_devices.remove(device);
        self.reset_device_inputs(device);
        self.clear_pending_for_device(device);
        self.operation_generation = self.operation_generation.saturating_add(1);
        if let Ok(session) = self.device_session(device) {
            let generation = self.operation_generation;
            if let Ok(completion) = session.request_stop(device, None, false, generation) {
                tokio::spawn(async move {
                    let _ = completion.await;
                });
            }
        }
        if self.output_devices.is_empty() {
            self.snapshot.output.state = OutputState::Error;
        }
        self.snapshot.output.last_error = Some(message.clone());
        self.log(LogLevel::Error, message);
        self.refresh_device_snapshots();
        self.publish();
    }

    async fn fail_output(&mut self, message: impl Into<String>) {
        let primary_message = message.into();
        self.snapshot.output.state = OutputState::Error;
        self.snapshot.output.last_error = Some(primary_message.clone());
        self.log(LogLevel::Error, primary_message.clone());
        self.publish();

        if let Err(error) = self.send_stop_operations(false).await {
            let message = format!("{primary_message}；清空设备任务失败：{error}");
            self.snapshot.output.last_error = Some(message.clone());
            self.log(LogLevel::Error, message);
            self.publish();
        }
    }

    fn adjust_device_intensity(
        &mut self,
        device_id: Option<&str>,
        channel: Channel,
        delta: i32,
    ) -> Result<(), HubError> {
        if delta == 0 || !(-200..=200).contains(&delta) {
            return Err(HubError::InvalidDelta);
        }
        let selected = match device_id {
            Some(device_id) => self
                .devices
                .keys()
                .find(|device| device.control_id() == device_id)
                .cloned()
                .ok_or(HubError::DeviceUnavailable)?,
            None => return Err(HubError::NoDevice),
        };
        let (selected_current, selected_limit) = self
            .device_channel_control_state(&selected, channel)
            .ok_or(HubError::DeviceUnavailable)?;
        let target = i32::from(selected_current) + delta;
        if !(0..=i32::from(selected_limit)).contains(&target) {
            return Err(HubError::IntensityLimit);
        }
        let target = target as u16;
        let adjustments = if self.snapshot.sync_all_devices {
            self.prepare_absolute_sync_adjustments(&[(channel, target)])?
        } else {
            vec![(selected, channel, selected_current, target)]
        };
        let adjustment_count = adjustments.len();
        for (device, adjustment_channel, current, adjustment_target) in adjustments {
            self.queue_intensity_adjustment(
                device.clone(),
                adjustment_channel,
                current,
                adjustment_target,
                false,
            )?;
            self.update_device_intensity_lock_target(
                &device,
                adjustment_channel,
                adjustment_target,
            );
        }
        self.refresh_device_snapshots();
        if self.snapshot.sync_all_devices {
            self.log(
                LogLevel::Info,
                format!(
                    "已将所有设备的 {channel} 通道同步到强度 {target}（下发 {adjustment_count} 个调整）"
                ),
            );
        }
        self.publish();
        Ok(())
    }

    fn prepare_absolute_sync_adjustments(
        &self,
        targets: &[(Channel, u16)],
    ) -> Result<Vec<(DeviceKey, Channel, u16, u16)>, HubError> {
        if self.devices.is_empty() {
            return Err(HubError::NoDevice);
        }
        if self.devices.len() > MAX_OUTPUT_DEVICES {
            return Err(HubError::TooManyDevices);
        }
        let mut adjustments = Vec::with_capacity(self.devices.len() * targets.len());
        for device in self.devices.keys() {
            for &(channel, target) in targets {
                let (current, limit) = self
                    .device_channel_control_state(device, channel)
                    .ok_or(HubError::DeviceUnavailable)?;
                if target > limit {
                    return Err(HubError::IntensityLimit);
                }
                let key = IntensityKey {
                    device: device.clone(),
                    channel,
                };
                if self.pending_intensity_operations.contains_key(&key) {
                    return Err(HubError::QueueBusy);
                }
                if current != target {
                    adjustments.push((device.clone(), channel, current, target));
                }
            }
        }
        Ok(adjustments)
    }

    fn device_channel_control_state(
        &self,
        device: &DeviceKey,
        channel: Channel,
    ) -> Option<(u16, u16)> {
        let mut snapshot = device_snapshot_from_value(device, self.devices.get(device)?)?;
        if snapshot.initialization != InitializationState::Ready {
            return None;
        }
        if snapshot.transport != TransportKind::Ble
            && !self.snapshot.safety.allow_app_intensity_control
            && let Some(lock) = self.intensity_lock_targets.get(device)
        {
            snapshot.intensity_a = lock.a;
            snapshot.intensity_b = lock.b;
        }
        let (current, device_limit) = match channel {
            Channel::A => (snapshot.intensity_a, snapshot.intensity_limit_a),
            Channel::B => (snapshot.intensity_b, snapshot.intensity_limit_b),
        };
        Some((current, device_limit))
    }

    fn queue_intensity_adjustment(
        &mut self,
        device: DeviceKey,
        channel: Channel,
        current: u16,
        target: u16,
        lock_correction: bool,
    ) -> Result<(), HubError> {
        if self.shutdown_zero_pending.contains(&device)
            || (self.snapshot.sync_all_devices && !self.shutdown_zero_pending.is_empty())
        {
            return Err(HubError::QueueBusy);
        }
        let delta = i32::from(target) - i32::from(current);
        if delta == 0 || !(-200..=200).contains(&delta) {
            return Err(HubError::InvalidDelta);
        }
        let key = IntensityKey {
            device: device.clone(),
            channel,
        };
        if self.pending_intensity_operations.contains_key(&key) {
            return Err(HubError::QueueBusy);
        }
        let request_id = Uuid::new_v4().to_string();
        self.device_session(&device)?.try_send(
            &device,
            DeviceOperation::AdjustIntensity {
                request_id: request_id.clone(),
                slot_id: device.slot_id.clone(),
                channel,
                delta,
            },
            self.operation_generation,
        )?;
        self.pending_intensity_requests
            .insert(request_id.clone(), key.clone());
        self.pending_intensity_operations.insert(
            key,
            PendingIntensityOperation {
                request_id,
                projected: target,
                lock_correction,
                response_received: false,
                projected_observed: false,
            },
        );
        self.log(
            LogLevel::Info,
            format!(
                "已请求{}通道强度相对调整 {delta:+}（预计强度 {target}）",
                channel
            ),
        );
        // 强度不做乐观更新，等待 APP 的 slots.patch 作为唯一权威状态。
        Ok(())
    }

    fn update_device_intensity_lock_target(
        &mut self,
        device: &DeviceKey,
        channel: Channel,
        target: u16,
    ) {
        if self.snapshot.safety.allow_app_intensity_control {
            return;
        }
        if let Some(lock) = self.intensity_lock_targets.get_mut(device) {
            match channel {
                Channel::A => lock.a = target,
                Channel::B => lock.b = target,
            }
        }
    }

    async fn set_device_channel_source(
        &mut self,
        device_id: String,
        channel: Channel,
        source_id: String,
    ) -> Result<(), HubError> {
        let device = self
            .devices
            .keys()
            .find(|device| device.control_id() == device_id)
            .cloned()
            .ok_or(HubError::DeviceUnavailable)?;
        self.sources
            .get(&source_id)
            .ok_or_else(|| HubError::SourceUnavailable(source_id.clone()))?;
        let channels = if self.source_sync_devices.contains(&device) {
            Channel::ALL.as_slice()
        } else {
            std::slice::from_ref(&channel)
        };
        let changed_channels = channels
            .iter()
            .copied()
            .filter(|channel| {
                self.device_source_bindings
                    .get(&SourceBindingKey {
                        device: device.clone(),
                        channel: *channel,
                    })
                    .map(|binding| &binding.source_id)
                    != Some(&source_id)
            })
            .collect::<Vec<_>>();
        if changed_channels.is_empty() {
            return Ok(());
        }
        if self.snapshot.output.state == OutputState::Running
            && self.output_devices.contains(&device)
        {
            for channel in &changed_channels {
                self.clear_input_channel(&SourceBindingKey {
                    device: device.clone(),
                    channel: *channel,
                })
                .await?;
            }
            self.pending_wave_operations.retain(|_, pending| {
                pending.device != device || !changed_channels.contains(&pending.channel)
            });
        }
        self.initialized_source_devices.insert(device.clone());
        self.reset_changed_inputs(&device, &changed_channels);
        for channel in &changed_channels {
            let binding = SourceBindingKey {
                device: device.clone(),
                channel: *channel,
            };
            self.replace_source_binding(binding.clone(), source_id.clone());
            if source_id == FIXED_WAVEFORM_SOURCE_ID
                && !self.fixed_waveform_bindings.contains_key(&binding)
                && let Some(config) = self.default_fixed_waveform.clone()
            {
                self.fixed_waveform_bindings
                    .insert(binding, build_fixed_waveform_runtime(config)?);
            }
        }
        self.refresh_source_snapshots();
        self.refresh_device_snapshots();
        self.log(
            LogLevel::Info,
            format!(
                "设备 {} 的 {} 通道已切换输入源：{source_id}",
                device.control_id(),
                if self.source_sync_devices.contains(&device) {
                    "A/B"
                } else {
                    channel_label(channel)
                }
            ),
        );
        self.publish();
        Ok(())
    }

    async fn set_device_channel_source_sync(
        &mut self,
        device_id: String,
        enabled: bool,
    ) -> Result<(), HubError> {
        let device = self
            .devices
            .keys()
            .find(|device| device.control_id() == device_id)
            .cloned()
            .ok_or(HubError::DeviceUnavailable)?;
        if self.source_sync_devices.contains(&device) == enabled {
            return Ok(());
        }
        if !enabled {
            self.source_sync_devices.remove(&device);
            self.refresh_device_snapshots();
            self.log(
                LogLevel::Info,
                format!("设备 {} 已关闭 A/B 输入源同步", device.control_id()),
            );
            self.publish();
            return Ok(());
        }

        let target_source_id = self.default_source_id.clone();
        let changed_channels = Channel::ALL
            .iter()
            .copied()
            .filter(|channel| {
                self.device_source_bindings
                    .get(&SourceBindingKey {
                        device: device.clone(),
                        channel: *channel,
                    })
                    .map(|binding| binding.source_id.as_str())
                    != target_source_id.as_deref()
            })
            .collect::<Vec<_>>();
        if self.snapshot.output.state == OutputState::Running
            && self.output_devices.contains(&device)
            && !changed_channels.is_empty()
        {
            for channel in &changed_channels {
                self.clear_input_channel(&SourceBindingKey {
                    device: device.clone(),
                    channel: *channel,
                })
                .await?;
            }
            self.pending_wave_operations.retain(|_, pending| {
                pending.device != device || !changed_channels.contains(&pending.channel)
            });
        }

        self.initialized_source_devices.insert(device.clone());
        self.reset_changed_inputs(&device, &changed_channels);
        for channel in Channel::ALL {
            let binding = SourceBindingKey {
                device: device.clone(),
                channel,
            };
            if let Some(source_id) = &target_source_id {
                if changed_channels.contains(&channel) {
                    self.replace_source_binding(binding.clone(), source_id.clone());
                }
                if source_id == FIXED_WAVEFORM_SOURCE_ID
                    && !self.fixed_waveform_bindings.contains_key(&binding)
                    && let Some(config) = self.default_fixed_waveform.clone()
                {
                    self.fixed_waveform_bindings
                        .insert(binding, build_fixed_waveform_runtime(config)?);
                }
            } else {
                self.device_source_bindings.remove(&binding);
            }
        }
        self.source_sync_devices.insert(device.clone());
        let has_all_sources = Channel::ALL.iter().all(|channel| {
            self.device_source_bindings.contains_key(&SourceBindingKey {
                device: device.clone(),
                channel: *channel,
            })
        });
        if !has_all_sources {
            self.output_devices.remove(&device);
            if self.snapshot.output.state == OutputState::Running && self.output_devices.is_empty()
            {
                self.snapshot.output.state = OutputState::Idle;
                self.snapshot.output.last_error = None;
                self.log(
                    LogLevel::Warning,
                    "默认输入源为每次询问，A/B 同步后已停止波形输出",
                );
            }
        }
        self.refresh_source_snapshots();
        self.refresh_device_snapshots();
        self.log(
            LogLevel::Info,
            target_source_id.map_or_else(
                || {
                    format!(
                        "设备 {} 已开启 A/B 输入源同步，并重置为未分配",
                        device.control_id()
                    )
                },
                |source_id| {
                    format!(
                        "设备 {} 已开启 A/B 输入源同步，并重置为默认源：{source_id}",
                        device.control_id()
                    )
                },
            ),
        );
        self.publish();
        Ok(())
    }

    fn set_default_source(&mut self, source_id: Option<String>) -> Result<(), HubError> {
        if let Some(source_id) = &source_id {
            self.sources
                .get(source_id)
                .ok_or_else(|| HubError::SourceUnavailable(source_id.clone()))?;
        }
        if self.default_source_id == source_id {
            return Ok(());
        }
        self.default_source_id.clone_from(&source_id);
        self.snapshot.default_source_id.clone_from(&source_id);
        let message = source_id.map_or_else(
            || "默认输入源已改为每次询问；已有设备绑定保持不变".to_owned(),
            |source_id| format!("默认输入源已切换：{source_id}；已有设备绑定保持不变"),
        );
        self.log(LogLevel::Info, message);
        self.publish();
        Ok(())
    }

    fn set_fixed_waveform(
        &mut self,
        device_id: &str,
        channel: Channel,
        config: Option<WaveformConfig>,
    ) -> Result<(), HubError> {
        let device = self
            .devices
            .keys()
            .find(|device| device.control_id() == device_id)
            .cloned()
            .ok_or(HubError::DeviceUnavailable)?;
        let binding = SourceBindingKey { device, channel };
        if self
            .device_source_bindings
            .get(&binding)
            .map(|binding| binding.source_id.as_str())
            != Some(FIXED_WAVEFORM_SOURCE_ID)
        {
            return Err(HubError::SourceUnavailable(
                FIXED_WAVEFORM_SOURCE_ID.to_owned(),
            ));
        }
        let selected_name = config.as_ref().map(|item| item.preset_name.clone());
        let value = serde_json::to_value(&config)
            .map_err(|error| HubError::InvalidSourceConfig(error.to_string()))?;
        if let Some(config) = config {
            self.fixed_waveform_bindings
                .insert(binding.clone(), build_fixed_waveform_runtime(config)?);
        } else {
            self.fixed_waveform_bindings.remove(&binding);
        }
        let state = self
            .device_source_bindings
            .get_mut(&binding)
            .expect("fixed source binding exists");
        state.config = value;
        state.revision = state.revision.saturating_add(1);
        self.refresh_device_snapshots();
        self.log(
            LogLevel::Info,
            selected_name.map_or_else(
                || format!("设备 {device_id} 的 {channel} 通道已设为无波形"),
                |name| format!("设备 {device_id} 的 {channel} 通道固定波形已切换：{name}"),
            ),
        );
        self.publish();
        Ok(())
    }

    fn set_waveform_state(
        &mut self,
        waveforms: Vec<WaveformConfig>,
        selected: Option<WaveformConfig>,
    ) -> Result<(), HubError> {
        validate_waveform_library(&waveforms, selected.as_ref())?;
        let next_ids = waveforms
            .iter()
            .map(|waveform| waveform.preset_id.as_str())
            .collect::<BTreeSet<_>>();
        let removed_ids = self
            .custom_waveforms
            .iter()
            .filter(|waveform| !next_ids.contains(waveform.preset_id.as_str()))
            .map(|waveform| waveform.preset_id.clone())
            .collect::<BTreeSet<_>>();
        self.default_fixed_waveform = selected;
        self.custom_waveforms = waveforms;
        if !removed_ids.is_empty() {
            self.fixed_waveform_bindings
                .retain(|_, runtime| !removed_ids.contains(&runtime.config.preset_id));
        }
        self.snapshot.custom_waveforms = custom_waveform_snapshots(&self.custom_waveforms);
        self.refresh_device_snapshots();
        self.log(LogLevel::Info, "固定波形库已更新");
        self.publish();
        Ok(())
    }

    #[cfg(test)]
    fn set_sync_all_devices(&mut self, enabled: bool) -> Result<(), HubError> {
        self.set_sync_all_devices_from(
            self.devices.keys().next().map(DeviceKey::control_id),
            enabled,
        )
    }

    fn set_sync_all_devices_from(
        &mut self,
        device_id: Option<String>,
        enabled: bool,
    ) -> Result<(), HubError> {
        if enabled && self.devices.is_empty() {
            return Err(HubError::NoDevice);
        }
        if enabled && self.devices.len() > MAX_OUTPUT_DEVICES {
            return Err(HubError::TooManyDevices);
        }
        if !enabled && !self.snapshot.sync_all_devices {
            return Ok(());
        }
        if enabled {
            if !self.shutdown_zero_pending.is_empty() {
                return Err(HubError::QueueBusy);
            }
            let selected = match device_id {
                Some(id) => self
                    .devices
                    .keys()
                    .find(|key| key.control_id() == id)
                    .cloned()
                    .ok_or(HubError::DeviceUnavailable)?,
                None => return Err(HubError::NoDevice),
            };
            let (target_a, _) = self
                .device_channel_control_state(&selected, Channel::A)
                .ok_or(HubError::DeviceUnavailable)?;
            let (target_b, _) = self
                .device_channel_control_state(&selected, Channel::B)
                .ok_or(HubError::DeviceUnavailable)?;
            let adjustments = self.prepare_absolute_sync_adjustments(&[
                (Channel::A, target_a),
                (Channel::B, target_b),
            ])?;
            for (device, channel, current, target) in adjustments {
                self.queue_intensity_adjustment(device.clone(), channel, current, target, false)?;
                self.update_device_intensity_lock_target(&device, channel, target);
            }
            for device in self.devices.keys().cloned().collect::<Vec<_>>() {
                self.update_device_intensity_lock_target(&device, Channel::A, target_a);
                self.update_device_intensity_lock_target(&device, Channel::B, target_b);
            }
            self.sync_baseline_device = Some(selected);
        } else {
            self.sync_baseline_device = None;
        }
        self.snapshot.sync_all_devices = enabled;
        self.refresh_device_snapshots();
        self.log(
            LogLevel::Info,
            if enabled {
                "已开启所有设备强度同步控制，并向指定基准设备的 A/B 强度对齐"
            } else {
                "已关闭所有设备强度同步控制"
            },
        );
        self.publish();
        Ok(())
    }

    async fn update_safety(
        &mut self,
        connection_timeout_enabled: bool,
        connection_timeout_minutes: i32,
        allow_app_intensity_control: bool,
    ) -> Result<(), HubError> {
        if !(1..=1440).contains(&connection_timeout_minutes) {
            return Err(HubError::InvalidConnectionTimeout);
        }
        self.snapshot.safety.connection_timeout_enabled = connection_timeout_enabled;
        self.snapshot.safety.connection_timeout_minutes = connection_timeout_minutes as u16;
        self.set_allow_app_intensity_control(allow_app_intensity_control);
        self.log(
            LogLevel::Info,
            if connection_timeout_enabled {
                format!("已启用连接超时自动断开：{connection_timeout_minutes} 分钟")
            } else {
                "已关闭连接超时自动断开".to_owned()
            },
        );
        self.publish();
        Ok(())
    }

    fn set_allow_app_intensity_control(&mut self, enabled: bool) {
        self.snapshot.safety.allow_app_intensity_control = enabled;
        if enabled {
            self.intensity_lock_targets.clear();
            self.log(LogLevel::Info, "已允许手机端同步调整强度");
        } else {
            self.sync_intensity_lock_targets();
            self.log(
                LogLevel::Warning,
                "已关闭手机端反向控制；APP 修改后将恢复为电脑端锁定值",
            );
        }
        self.refresh_device_snapshots();
        self.publish();
    }

    #[cfg(test)]
    fn set_intensity_lock_target(&mut self, a: u16, b: u16) {
        if self.snapshot.safety.allow_app_intensity_control {
            return;
        }
        if let Some(device) = self.devices.keys().next().cloned() {
            self.intensity_lock_targets
                .insert(device, IntensityLockTarget { a, b });
            self.refresh_device_snapshots();
        }
    }

    fn set_all_intensity_lock_targets(&mut self, a: u16, b: u16) {
        if self.snapshot.safety.allow_app_intensity_control {
            return;
        }
        self.sync_intensity_lock_targets();
        for lock in self.intensity_lock_targets.values_mut() {
            lock.a = a;
            lock.b = b;
        }
        self.refresh_device_snapshots();
    }

    fn reconcile_intensity_lock(&mut self) {
        // Only authoritative feedback can complete zeroing. Until then, stale
        // strengths must never become synchronization or lock correction targets.
        self.shutdown_zero_pending.retain(|device| {
            self.devices.get(device).is_some_and(|value| {
                device_intensity_from_value(value, Channel::A) != Some(0)
                    || device_intensity_from_value(value, Channel::B) != Some(0)
            })
        });
        if !self.shutdown_zero_pending.is_empty() {
            return;
        }
        if self.snapshot.safety.allow_app_intensity_control {
            self.reconcile_synced_device_strengths();
            return;
        }
        self.sync_intensity_lock_targets();

        for (device, channel, current, target) in self.intensity_lock_corrections() {
            if let Err(error) =
                self.queue_intensity_adjustment(device.clone(), channel, current, target, true)
            {
                self.log(
                    LogLevel::Error,
                    format!(
                        "纠正设备 {} 的 {channel} 通道强度失败：{error}",
                        device.control_id()
                    ),
                );
            } else {
                self.log(
                    LogLevel::Warning,
                    format!(
                        "检测到手机端修改设备 {} 的 {channel} 通道强度，正在恢复到 {target}",
                        device.control_id()
                    ),
                );
            }
        }
        self.reconcile_synced_device_strengths();
    }

    fn reconcile_synced_device_strengths(&mut self) {
        if !self.snapshot.sync_all_devices || !self.shutdown_zero_pending.is_empty() {
            return;
        }
        let Some(selected) = self.sync_baseline_device.clone() else {
            return;
        };
        let Some((target_a, _)) = self.device_channel_control_state(&selected, Channel::A) else {
            return;
        };
        let Some((target_b, _)) = self.device_channel_control_state(&selected, Channel::B) else {
            return;
        };
        let adjustments = match self
            .prepare_absolute_sync_adjustments(&[(Channel::A, target_a), (Channel::B, target_b)])
        {
            Ok(adjustments) => adjustments,
            Err(HubError::QueueBusy) => return,
            Err(error) => {
                self.log(
                    LogLevel::Error,
                    format!("维持所有设备强度同步失败：{error}"),
                );
                return;
            }
        };
        for (device, channel, current, target) in adjustments {
            if let Err(error) =
                self.queue_intensity_adjustment(device.clone(), channel, current, target, false)
            {
                self.log(
                    LogLevel::Error,
                    format!(
                        "同步设备 {} 的 {channel} 通道失败：{error}",
                        device.control_id()
                    ),
                );
                return;
            }
            self.update_device_intensity_lock_target(&device, channel, target);
        }
        self.refresh_device_snapshots();
    }

    fn sync_intensity_lock_targets(&mut self) {
        if self.snapshot.safety.allow_app_intensity_control {
            self.intensity_lock_targets.clear();
            return;
        }
        self.intensity_lock_targets.retain(|device, _| {
            self.devices.contains_key(device)
                && matches!(
                    device.connection_id.as_str(),
                    V4_CONNECTION_ID | V3_CONNECTION_ID
                )
        });
        for (device, value) in &self.devices {
            let Some(snapshot) = device_snapshot_from_value(device, value) else {
                continue;
            };
            if snapshot.transport == TransportKind::Ble
                || snapshot.initialization != InitializationState::Ready
            {
                continue;
            }
            self.intensity_lock_targets
                .entry(device.clone())
                .or_insert(IntensityLockTarget {
                    a: snapshot.intensity_a,
                    b: snapshot.intensity_b,
                });
        }
    }

    fn intensity_lock_corrections(&self) -> Vec<(DeviceKey, Channel, u16, u16)> {
        if self.snapshot.safety.allow_app_intensity_control {
            return Vec::new();
        }
        self.intensity_lock_targets
            .iter()
            .filter_map(|(device, lock)| {
                self.devices
                    .get(device)
                    .and_then(|value| device_snapshot_from_value(device, value))
                    .map(|observed| (device, lock, observed))
            })
            .flat_map(|(device, lock, observed)| {
                [
                    (Channel::A, lock.a, observed.intensity_a),
                    (Channel::B, lock.b, observed.intensity_b),
                ]
                .into_iter()
                .filter_map(move |(channel, target, current)| {
                    let key = IntensityKey {
                        device: device.clone(),
                        channel,
                    };
                    (target != current && !self.pending_intensity_operations.contains_key(&key))
                        .then_some((device.clone(), channel, current, target))
                })
            })
            .collect()
    }

    fn prepare_device_stop(
        &mut self,
        device_id: &str,
    ) -> Result<(DeviceKey, u64, Option<StopCompletion>), HubError> {
        let device = self
            .devices
            .keys()
            .chain(self.output_devices.iter())
            .find(|device| device.control_id() == device_id)
            .cloned()
            .ok_or(HubError::DeviceUnavailable)?;
        if let Some(pending) = self.pending_stops.get(&device) {
            return Ok((device, pending.generation, None));
        }
        // Latch inactive and invalidate buffered frames before any device I/O.
        let was_active = self.output_devices.remove(&device);
        let had_pending = self
            .pending_wave_operations
            .values()
            .any(|pending| pending.device == device);
        self.reset_device_inputs(&device);
        self.clear_pending_for_device(&device);
        if self.output_devices.is_empty() {
            self.snapshot.output.state = OutputState::Idle;
        }
        self.operation_generation = self.operation_generation.saturating_add(1);
        let generation = self.operation_generation;
        self.refresh_device_snapshots();
        self.publish();
        if !was_active && !had_pending {
            return Ok((device, generation, None));
        }
        let completion = self
            .device_session(&device)?
            .request_stop(&device, None, false, generation)
            .map_err(HubError::from)?;
        self.pending_stops.insert(
            device.clone(),
            PendingDeviceStop {
                generation,
                replies: Vec::new(),
            },
        );
        Ok((device, generation, Some(completion)))
    }

    fn finish_device_stop(
        &mut self,
        device: &DeviceKey,
        generation: u64,
        result: Result<(), HubError>,
    ) {
        if self
            .pending_stops
            .get(device)
            .is_none_or(|pending| pending.generation != generation)
        {
            return;
        }
        let pending = self
            .pending_stops
            .remove(device)
            .expect("pending stop exists");
        for reply in pending.replies {
            let _ = reply.send(result.clone());
        }
        if let Err(error) = result {
            self.snapshot.output.last_error = Some(error.to_string());
            self.log(
                LogLevel::Error,
                format!("设备 {} 的停止清理失败：{error}", device.control_id()),
            );
        } else {
            self.snapshot.output.last_error = None;
            self.log(
                LogLevel::Info,
                format!("设备 {} 的波形输出已停止并清空任务", device.control_id()),
            );
        }
        self.refresh_device_snapshots();
        self.publish();
    }

    async fn send_stop_operations(&mut self, zero: bool) -> Result<(), HubError> {
        self.output_devices.clear();
        let devices_to_reset = self.devices.keys().cloned().collect::<Vec<_>>();
        for device in devices_to_reset {
            self.reset_device_inputs(&device);
        }
        if zero {
            self.set_all_intensity_lock_targets(0, 0);
        }
        let generation = self.advance_operation_generation();
        let devices = self
            .devices
            .keys()
            .cloned()
            .chain(
                self.pending_connection_cleanups
                    .values()
                    .flat_map(|pending| pending.devices.iter().map(|(device, _)| device.clone())),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if zero {
            self.shutdown_zero_pending.extend(devices.iter().cloned());
        }
        if devices.is_empty() {
            return Ok(());
        }
        let operations = devices.iter().map(|device| {
            let session = self.device_session(device);
            let device = device.clone();
            async move {
                session?
                    .stop(&device, None, zero, generation)
                    .await
                    .map_err(HubError::from)
            }
        });
        let results = futures_util::future::join_all(operations);
        let results = if zero {
            results.await
        } else {
            self.wait_for_ordinary_relay(async { Ok::<_, HubError>(results.await) })
                .await?
        };
        let first_error = results.into_iter().find_map(Result::err);
        if let Some(error) = first_error {
            Err(error)
        } else {
            self.output_devices.clear();
            self.refresh_device_snapshots();
            Ok(())
        }
    }

    fn advance_operation_generation(&mut self) -> u64 {
        self.operation_generation = self.operation_generation.wrapping_add(1);
        if self.operation_generation == 0 {
            self.operation_generation = 1;
        }
        self.pending_wave_operations.clear();
        self.clear_pending_intensities();
        if let Some(relay) = &self.relay {
            relay.invalidate_operations(self.operation_generation);
        }
        for session in self.sessions.values() {
            session.invalidate_operations(self.operation_generation);
        }
        self.operation_generation
    }

    fn clear_pending_intensities(&mut self) {
        self.pending_intensity_operations.clear();
        self.pending_intensity_requests.clear();
    }

    fn clear_pending_for_client(&mut self, client_id: &str) {
        self.clear_pending_intensities_for_client(client_id);
        self.pending_wave_operations.retain(|_, pending| {
            pending.device.connection_id != V4_CONNECTION_ID
                || pending.device.client_id != client_id
        });
    }

    fn clear_pending_intensities_for_client(&mut self, client_id: &str) {
        let keys = self
            .pending_intensity_operations
            .keys()
            .filter(|key| {
                key.device.connection_id == V4_CONNECTION_ID && key.device.client_id == client_id
            })
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            self.remove_pending_intensity(&key);
        }
    }

    fn clear_pending_for_device(&mut self, device: &DeviceKey) {
        self.pending_wave_operations
            .retain(|_, pending| &pending.device != device);
        let keys = self
            .pending_intensity_operations
            .keys()
            .filter(|key| &key.device == device)
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            self.remove_pending_intensity(&key);
        }
    }

    fn observe_projected_intensities(&mut self, connection_id: &str, client_id: &str) {
        let keys = self
            .pending_intensity_operations
            .keys()
            .filter(|key| {
                key.device.connection_id == connection_id && key.device.client_id == client_id
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut completed = Vec::new();
        for key in keys {
            let observed = self
                .devices
                .get(&key.device)
                .and_then(|device| device_intensity_from_value(device, key.channel));
            let Some(pending) = self.pending_intensity_operations.get_mut(&key) else {
                continue;
            };
            if observed == Some(pending.projected) {
                pending.projected_observed = true;
            }
            if pending.response_received && pending.projected_observed {
                completed.push(key);
            }
        }
        for key in completed {
            self.remove_pending_intensity(&key);
        }
    }

    fn remove_pending_intensity(&mut self, key: &IntensityKey) {
        if let Some(pending) = self.pending_intensity_operations.remove(key) {
            self.pending_intensity_requests.remove(&pending.request_id);
        }
    }

    fn send_to_app(&self, client_id: &str, data: Value) -> Result<(), HubError> {
        self.relay
            .as_ref()
            .ok_or(HubError::Stopped)?
            .try_send_message(client_id.to_owned(), data)
            .map_err(Into::into)
    }

    fn remove_connection_devices(&mut self, connection_id: &str) {
        self.shutdown_zero_pending
            .retain(|device| device.connection_id != connection_id);
        if self
            .sync_baseline_device
            .as_ref()
            .is_some_and(|device| device.connection_id == connection_id)
        {
            self.sync_baseline_device = None;
        }
        self.connection_started.remove(connection_id);
        self.devices
            .retain(|device, _| device.connection_id != connection_id);
        self.device_source_bindings
            .retain(|binding, _| binding.device.connection_id != connection_id);
        self.fixed_waveform_bindings
            .retain(|binding, _| binding.device.connection_id != connection_id);

        self.plugin_last_frame
            .retain(|binding, _| binding.device.connection_id != connection_id);
        self.faulted_bindings
            .retain(|binding| binding.device.connection_id != connection_id);
        self.initialized_source_devices
            .retain(|device| device.connection_id != connection_id);
        self.source_sync_devices
            .retain(|device| device.connection_id != connection_id);
        self.output_devices
            .retain(|device| device.connection_id != connection_id);
        self.intensity_lock_targets
            .retain(|device, _| device.connection_id != connection_id);
        self.pending_wave_operations
            .retain(|_, pending| pending.device.connection_id != connection_id);
        let pending_keys = self
            .pending_intensity_operations
            .keys()
            .filter(|key| key.device.connection_id == connection_id)
            .cloned()
            .collect::<Vec<_>>();
        for key in pending_keys {
            self.remove_pending_intensity(&key);
        }
        if self.output_devices.is_empty() && self.snapshot.output.state == OutputState::Running {
            self.snapshot.output.state = OutputState::Idle;
            self.snapshot.output.last_error = None;
        }
        self.reconcile_connected_devices();
        self.refresh_device_snapshots();
    }

    fn reset_connection_state(&mut self, state: ConnectionState) {
        self.v4_session_generation = None;
        self.connection_started_at = None;
        self.apps.clear();
        self.remove_connection_devices(V4_CONNECTION_ID);
        self.operation_generation = self.operation_generation.wrapping_add(1).max(1);
        if let Some(relay) = &self.relay {
            relay.invalidate_operations(self.operation_generation);
        }
        self.v4_connection.state = state;
        self.v4_connection.controller_id = None;
        self.v4_connection.pairing_url = None;
        self.v4_connection.app_count = 0;
    }

    fn log(&mut self, level: LogLevel, message: impl Into<String>) {
        let mut logs = VecDeque::from(std::mem::take(&mut self.snapshot.logs));
        logs.push_front(LogSnapshot {
            id: Uuid::new_v4().to_string(),
            level,
            message: message.into(),
            timestamp: timestamp_now(),
        });
        logs.truncate(MAX_LOGS);
        self.snapshot.logs = logs.into();
    }

    fn publish(&mut self) {
        self.update_connection(self.v4_connection.clone());
        self.publish_plugin_bindings();
        self.snapshot.revision = self.snapshot.revision.saturating_add(1);
        let _ = self.snapshot_sender.send(self.snapshot.clone());
    }
}

const fn channel_label(channel: Channel) -> &'static str {
    match channel {
        Channel::A => "A",
        Channel::B => "B",
    }
}

fn pairing_url(endpoint: &str, controller_id: &str) -> Result<String, HubError> {
    build_pairing_url(endpoint, controller_id)
        .map_err(|error| HubError::Relay(format!("Relay 地址无效：{error}")))
}

fn deep_merge(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(target), Value::Object(patch)) => {
            for (key, value) in patch {
                match target.get_mut(key) {
                    Some(current) => deep_merge(current, value),
                    None => {
                        target.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (target, patch) => *target = patch.clone(),
    }
}

fn device_snapshot_from_value(key: &DeviceKey, device: &Value) -> Option<DeviceSnapshot> {
    let slot_id = device.get("slotId")?.as_str()?.to_owned();
    let props = device.get("props").and_then(Value::as_object);
    let id = device
        .get("id")
        .filter(|value| value.is_string() || value.is_number())
        .cloned()
        .unwrap_or_else(|| Value::String(slot_id.clone()));
    let intensity_a = object_u16(props, "intensityA").unwrap_or(0);
    let intensity_b = object_u16(props, "intensityB").unwrap_or(0);
    let slot_state = device.get("slotState");
    let has_device = slot_state
        .and_then(|state| state.get("hasDevice"))
        .and_then(Value::as_bool);
    Some(DeviceSnapshot {
        control_id: key.control_id(),
        connection_id: key.connection_id.clone(),
        transport: if key.connection_id == V4_CONNECTION_ID {
            TransportKind::WsV4
        } else if key.connection_id == V3_CONNECTION_ID {
            TransportKind::WsV3
        } else {
            TransportKind::Ble
        },
        initialization: device
            .get("initialization")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default(),
        capabilities: device
            .get("capabilities")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default(),
        ble_parameters: device
            .get("bleParameters")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok()),
        configuration_status: device
            .get("configurationStatus")
            .and_then(Value::as_str)
            .map(str::to_owned),
        id,
        name: device
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("DG-LAB 设备")
            .to_owned(),
        device_type: device
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN")
            .to_owned(),
        slot_id,
        power: object_u16(props, "power"),
        intensity_a,
        intensity_b,
        intensity_limit_a: slot_state
            .and_then(|state| state.get("channelA"))
            .and_then(|channel| channel.get("intensityMax"))
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(0),
        intensity_limit_b: slot_state
            .and_then(|state| state.get("channelB"))
            .and_then(|channel| channel.get("intensityMax"))
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(0),
        source_id_a: None,
        source_id_b: None,
        binding_id_a: None,
        binding_id_b: None,
        waveform_id_a: None,
        waveform_id_b: None,
        waveform_name_a: None,
        waveform_name_b: None,
        source_sync: false,
        output_active: false,
        channel_a_status: device
            .get("channelAStatus")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_else(|| {
                protocol_channel_status(
                    object_u16(props, "channelAStatus"),
                    has_device,
                    slot_state.is_some_and(|state| channel_is_muted(state, Channel::A)),
                )
            }),
        channel_b_status: device
            .get("channelBStatus")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_else(|| {
                protocol_channel_status(
                    object_u16(props, "channelBStatus"),
                    has_device,
                    slot_state.is_some_and(|state| channel_is_muted(state, Channel::B)),
                )
            }),
    })
}

fn device_intensity_from_value(device: &Value, channel: Channel) -> Option<u16> {
    let field = match channel {
        Channel::A => "intensityA",
        Channel::B => "intensityB",
    };
    device
        .get("props")
        .and_then(Value::as_object)
        .and_then(|props| object_u16(Some(props), field))
}

fn object_u16(object: Option<&Map<String, Value>>, key: &str) -> Option<u16> {
    object?
        .get(key)?
        .as_u64()
        .and_then(|value| u16::try_from(value).ok())
}

fn channel_is_muted(slot_state: &Value, channel: Channel) -> bool {
    let key = match channel {
        Channel::A => "channelA",
        Channel::B => "channelB",
    };
    slot_state
        .get(key)
        .and_then(|state| state.get("isMuted"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn protocol_channel_status(
    value: Option<u16>,
    has_device: Option<bool>,
    is_muted: bool,
) -> ChannelStatus {
    if has_device == Some(false) {
        return ChannelStatus::Disconnected;
    }
    if is_muted {
        return ChannelStatus::Disabled;
    }
    match value {
        Some(1 | 3 | 4) => ChannelStatus::Fault,
        Some(0) => ChannelStatus::Idle,
        Some(2) => ChannelStatus::Ready,
        None => ChannelStatus::Unknown,
        Some(_) => ChannelStatus::Fault,
    }
}

fn running_status(status: ChannelStatus, running: bool) -> ChannelStatus {
    if running && matches!(status, ChannelStatus::Idle | ChannelStatus::Ready) {
        ChannelStatus::Active
    } else {
        status
    }
}

fn timestamp_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = day_seconds / 3_600;
    let minute = day_seconds % 3_600 / 60;
    let second = day_seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

pub(crate) fn validate_waveform_library(
    waveforms: &[WaveformConfig],
    selected: Option<&WaveformConfig>,
) -> Result<(), HubError> {
    if waveforms.len() > MAX_CUSTOM_WAVEFORMS {
        return Err(HubError::CustomWaveformLimit);
    }
    if waveforms
        .iter()
        .map(|waveform| waveform.frames.len())
        .sum::<usize>()
        > MAX_CUSTOM_WAVEFORM_FRAMES
    {
        return Err(HubError::CustomWaveformFrameLimit);
    }
    let mut seen_ids = BTreeSet::new();
    if waveforms
        .iter()
        .any(|waveform| !seen_ids.insert(waveform.preset_id.as_str()))
    {
        return Err(HubError::InvalidSourceConfig(
            "自定义波形标识不能重复".to_owned(),
        ));
    }

    let registry = builtin_registry();
    for waveform in waveforms {
        let value = serde_json::to_value(waveform)
            .map_err(|error| HubError::InvalidSourceConfig(error.to_string()))?;
        registry
            .validate("builtin.fixed_waveform", &value)
            .map_err(|error| HubError::InvalidSourceConfig(error.to_string()))?;
    }
    if let Some(selected) = selected {
        let value = serde_json::to_value(selected)
            .map_err(|error| HubError::InvalidSourceConfig(error.to_string()))?;
        registry
            .validate("builtin.fixed_waveform", &value)
            .map_err(|error| HubError::InvalidSourceConfig(error.to_string()))?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use std::sync::atomic::AtomicUsize;
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    #[test]
    fn snapshot_contract_is_camel_case_and_matches_frontend_values() {
        let (hub, _runtime) = create_hub("wss://example.test/v4".to_owned());
        let value = serde_json::to_value(hub.snapshot()).unwrap();
        assert!(value.get("activeSourceId").is_none());
        assert!(value["connections"][0].get("controllerId").is_some());
        assert_eq!(value["connections"][0]["state"], "disconnected");
        assert_eq!(value["output"]["state"], "idle");
        for field in [
            "connection",
            "device",
            "channels",
            "inputModes",
            "selectedDeviceId",
        ] {
            assert!(value.get(field).is_none());
        }
        assert_eq!(value["sources"][0]["kind"], "builtin.fixed_waveform");
        assert!(value["sources"][0].get("assignedChannelCount").is_some());
        assert!(value["defaultSourceId"].is_null());
        assert!(value.get("devices").is_some());
        assert!(value.get("sourceBindings").is_some());
        assert_eq!(value["syncAllDevices"], false);
        assert_eq!(value["outputDeviceCount"], 0);
    }

    #[test]
    fn configured_default_source_is_restored_when_the_hub_starts() {
        let (hub, _runtime) = create_hub_with_default_source(
            "wss://example.test/v4".to_owned(),
            Some("source-fixed-waveform".to_owned()),
        );
        assert_eq!(
            hub.snapshot().default_source_id.as_deref(),
            Some("source-fixed-waveform")
        );

        let (hub, _runtime) = create_hub_with_default_source(
            "wss://example.test/v4".to_owned(),
            Some("source-removed".to_owned()),
        );
        assert_eq!(hub.snapshot().default_source_id, None);
    }

    #[test]
    fn fixed_waveforms_are_independent_per_device_channel() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        let device = runtime.devices.keys().next().unwrap().clone();
        let device_id = device.control_id();
        let config_a = WaveformConfig {
            preset_id: "BUBBLE".to_owned(),
            preset_name: "气泡".to_owned(),
            frames: vec!["2D2D2D2D00000000".to_owned(), "2D2D2D2D64646464".to_owned()],
        };
        let config_b = WaveformConfig {
            preset_id: "CUSTOM_B".to_owned(),
            preset_name: "自定义 B".to_owned(),
            frames: vec!["0A0A0A0A14141414".to_owned()],
        };

        runtime
            .set_fixed_waveform(&device_id, Channel::A, Some(config_a))
            .unwrap();
        runtime
            .set_fixed_waveform(&device_id, Channel::B, Some(config_b))
            .unwrap();

        assert_eq!(
            runtime
                .snapshot
                .devices
                .first()
                .unwrap()
                .waveform_id_a
                .as_deref(),
            Some("BUBBLE")
        );
        assert_eq!(
            runtime
                .snapshot
                .devices
                .first()
                .unwrap()
                .waveform_id_b
                .as_deref(),
            Some("CUSTOM_B")
        );
        let first_a = runtime
            .fixed_waveform_bindings
            .get_mut(&source_binding(&device, Channel::A))
            .unwrap()
            .source
            .next_frame()
            .unwrap();
        let first_b = runtime
            .fixed_waveform_bindings
            .get_mut(&source_binding(&device, Channel::B))
            .unwrap()
            .source
            .next_frame()
            .unwrap();
        assert_eq!(first_a.samples()[0].frequency(), 45);
        assert_eq!(first_a.samples()[0].pulse_intensity(), 0);
        assert_eq!(first_b.samples()[0].frequency(), 10);
        assert_eq!(first_b.samples()[0].pulse_intensity(), 20);
        let source = runtime.sources.get(FIXED_WAVEFORM_SOURCE_ID).unwrap();
        assert!(source.snapshot.enabled);
        assert_eq!(source.snapshot.selected_preset_id, None);
    }

    #[test]
    fn custom_waveform_library_preserves_order_and_default_template() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        let first = WaveformConfig {
            preset_id: "CUSTOM_A".to_owned(),
            preset_name: "自定义 A".to_owned(),
            frames: vec!["0A0A0A0A14141414".to_owned()],
        };
        let second = WaveformConfig {
            preset_id: "CUSTOM_B".to_owned(),
            preset_name: "自定义 B".to_owned(),
            frames: vec!["2D2D2D2D64646464".to_owned()],
        };

        runtime
            .set_waveform_state(vec![second.clone(), first.clone()], Some(first.clone()))
            .unwrap();

        assert_eq!(
            runtime
                .snapshot
                .custom_waveforms
                .iter()
                .map(|waveform| waveform.id.as_str())
                .collect::<Vec<_>>(),
            ["CUSTOM_B", "CUSTOM_A"]
        );
        let source = runtime.sources.get(FIXED_WAVEFORM_SOURCE_ID).unwrap();
        assert!(source.snapshot.enabled);
        assert_eq!(
            runtime
                .default_fixed_waveform
                .as_ref()
                .map(|waveform| waveform.preset_id.as_str()),
            Some("CUSTOM_A")
        );
        assert_eq!(source.snapshot.selected_preset_id, None);

        runtime.set_waveform_state(vec![first], None).unwrap();
        let source = runtime.sources.get(FIXED_WAVEFORM_SOURCE_ID).unwrap();
        assert!(source.snapshot.enabled);
        assert_eq!(source.snapshot.selected_preset_id, None);
        assert_eq!(runtime.default_fixed_waveform, None);
        assert_eq!(runtime.snapshot.custom_waveforms.len(), 1);
    }

    #[test]
    fn deleting_custom_waveform_clears_only_channels_that_use_it() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        let device = runtime.devices.keys().next().unwrap().clone();
        let device_id = device.control_id();
        let first = WaveformConfig {
            preset_id: "CUSTOM_A".to_owned(),
            preset_name: "自定义 A".to_owned(),
            frames: vec!["0A0A0A0A14141414".to_owned()],
        };
        let second = WaveformConfig {
            preset_id: "CUSTOM_B".to_owned(),
            preset_name: "自定义 B".to_owned(),
            frames: vec!["2D2D2D2D64646464".to_owned()],
        };
        runtime
            .set_waveform_state(vec![first.clone(), second.clone()], None)
            .unwrap();
        runtime
            .set_fixed_waveform(&device_id, Channel::A, Some(first))
            .unwrap();
        runtime
            .set_fixed_waveform(&device_id, Channel::B, Some(second))
            .unwrap();

        runtime
            .set_waveform_state(
                vec![WaveformConfig {
                    preset_id: "CUSTOM_B".to_owned(),
                    preset_name: "自定义 B".to_owned(),
                    frames: vec!["2D2D2D2D64646464".to_owned()],
                }],
                None,
            )
            .unwrap();
        assert!(
            runtime
                .device_source_bindings
                .values()
                .any(|source_id| source_id.source_id == FIXED_WAVEFORM_SOURCE_ID)
        );
        assert!(
            !runtime
                .fixed_waveform_bindings
                .contains_key(&source_binding(&device, Channel::A))
        );
        assert_eq!(
            runtime
                .fixed_waveform_bindings
                .get(&source_binding(&device, Channel::B))
                .map(|waveform| waveform.config.preset_id.as_str()),
            Some("CUSTOM_B")
        );
        assert_eq!(
            runtime.snapshot.devices.first().unwrap().waveform_id_a,
            None
        );
        assert_eq!(
            runtime
                .snapshot
                .devices
                .first()
                .unwrap()
                .waveform_id_b
                .as_deref(),
            Some("CUSTOM_B")
        );
    }

    struct CountingSource {
        calls: Arc<AtomicUsize>,
    }

    impl WaveSource for CountingSource {
        fn next_frame(&mut self) -> Result<WaveFrame, crate::sources::SourceError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(WaveFrame::silent())
        }
    }

    fn source_binding(device: &DeviceKey, channel: Channel) -> SourceBindingKey {
        SourceBindingKey {
            device: device.clone(),
            channel,
        }
    }

    #[tokio::test]
    async fn each_device_channel_can_bind_a_different_source() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 20);
        let second = runtime
            .devices
            .keys()
            .find(|device| device.client_id == "app-2")
            .cloned()
            .unwrap();

        assert_eq!(
            runtime
                .device_source_bindings
                .get(&source_binding(&second, Channel::A))
                .map(|binding| binding.source_id.as_str()),
            Some("source-fixed-waveform")
        );
        runtime
            .set_device_channel_source(
                second.control_id(),
                Channel::A,
                "source-test-secondary".to_owned(),
            )
            .await
            .unwrap();

        assert_eq!(
            runtime
                .device_source_bindings
                .get(&source_binding(&second, Channel::A))
                .map(|binding| binding.source_id.as_str()),
            Some("source-test-secondary")
        );
        assert_eq!(
            runtime
                .snapshot
                .devices
                .iter()
                .find(|device| device.control_id == second.control_id())
                .and_then(|device| device.source_id_a.as_deref()),
            Some("source-test-secondary")
        );
        assert_eq!(
            runtime
                .snapshot
                .devices
                .iter()
                .find(|device| device.control_id == second.control_id())
                .and_then(|device| device.source_id_b.as_deref()),
            Some("source-fixed-waveform")
        );
        assert_eq!(
            runtime
                .snapshot
                .sources
                .iter()
                .find(|source| source.id == "source-fixed-waveform")
                .map(|source| source.assigned_channel_count),
            Some(3)
        );
        assert_eq!(
            runtime
                .snapshot
                .sources
                .iter()
                .find(|source| source.id == "source-test-secondary")
                .map(|source| source.assigned_channel_count),
            Some(1)
        );
    }

    #[tokio::test]
    async fn device_channel_source_sync_resets_to_default_and_links_both_channels() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        let device = runtime.devices.keys().next().cloned().unwrap();
        runtime
            .set_device_channel_source(
                device.control_id(),
                Channel::A,
                "source-fixed-waveform".to_owned(),
            )
            .await
            .unwrap();

        runtime
            .set_device_channel_source_sync(device.control_id(), true)
            .await
            .unwrap();
        assert!(runtime.source_sync_devices.contains(&device));
        assert!(Channel::ALL.iter().all(|channel| {
            runtime
                .device_source_bindings
                .get(&source_binding(&device, *channel))
                .is_some_and(|source| source.source_id == "source-fixed-waveform")
        }));
        assert!(
            runtime
                .snapshot
                .devices
                .iter()
                .find(|snapshot| snapshot.control_id == device.control_id())
                .is_some_and(|snapshot| snapshot.source_sync)
        );

        runtime
            .set_device_channel_source(
                device.control_id(),
                Channel::B,
                "source-fixed-waveform".to_owned(),
            )
            .await
            .unwrap();
        assert!(Channel::ALL.iter().all(|channel| {
            runtime
                .device_source_bindings
                .get(&source_binding(&device, *channel))
                .is_some_and(|source| source.source_id == "source-fixed-waveform")
        }));

        runtime
            .set_device_channel_source_sync(device.control_id(), false)
            .await
            .unwrap();
        runtime
            .set_device_channel_source(
                device.control_id(),
                Channel::A,
                "source-fixed-waveform".to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(
            runtime
                .device_source_bindings
                .get(&source_binding(&device, Channel::A))
                .map(|binding| binding.source_id.as_str()),
            Some("source-fixed-waveform")
        );
        assert_eq!(
            runtime
                .device_source_bindings
                .get(&source_binding(&device, Channel::B))
                .map(|binding| binding.source_id.as_str()),
            Some("source-fixed-waveform")
        );
    }

    #[tokio::test]
    async fn source_sync_with_ask_each_time_default_unassigns_both_channels() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        let device = runtime.devices.keys().next().cloned().unwrap();
        runtime.set_default_source(None).unwrap();

        runtime
            .set_device_channel_source_sync(device.control_id(), true)
            .await
            .unwrap();

        assert!(Channel::ALL.iter().all(|channel| {
            !runtime
                .device_source_bindings
                .contains_key(&source_binding(&device, *channel))
        }));
        assert_eq!(
            runtime.start_output(&device.control_id()),
            Err(HubError::NoSource)
        );
    }

    #[test]
    fn changing_default_source_only_affects_new_devices() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        let first = runtime.devices.keys().next().cloned().unwrap();

        runtime
            .set_default_source(Some("source-fixed-waveform".to_owned()))
            .unwrap();
        install_test_device(&mut runtime, "app-2", "slot-b", 20);
        let second = runtime
            .devices
            .keys()
            .find(|device| device.client_id == "app-2")
            .cloned()
            .unwrap();

        assert_eq!(
            runtime.default_source_id.as_deref(),
            Some("source-fixed-waveform")
        );
        assert_eq!(
            runtime.snapshot.default_source_id.as_deref(),
            Some("source-fixed-waveform")
        );
        assert_eq!(
            runtime
                .device_source_bindings
                .get(&source_binding(&first, Channel::A))
                .map(|binding| binding.source_id.as_str()),
            Some("source-fixed-waveform")
        );
        assert_eq!(
            runtime
                .device_source_bindings
                .get(&source_binding(&second, Channel::B))
                .map(|binding| binding.source_id.as_str()),
            Some("source-fixed-waveform")
        );
    }

    #[test]
    fn clearing_default_source_leaves_new_devices_unassigned() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        let first = runtime.devices.keys().next().cloned().unwrap();

        runtime.set_default_source(None).unwrap();
        install_test_device(&mut runtime, "app-2", "slot-b", 20);
        let second = runtime
            .devices
            .keys()
            .find(|device| device.client_id == "app-2")
            .cloned()
            .unwrap();

        assert_eq!(runtime.snapshot.default_source_id, None);
        assert_eq!(
            runtime
                .device_source_bindings
                .get(&source_binding(&first, Channel::A))
                .map(|binding| binding.source_id.as_str()),
            Some("source-fixed-waveform")
        );
        assert!(Channel::ALL.iter().all(|channel| {
            !runtime
                .device_source_bindings
                .contains_key(&source_binding(&second, *channel))
        }));
        assert_eq!(
            runtime.start_output(&second.control_id()),
            Err(HubError::NoSource)
        );
    }

    #[tokio::test]
    async fn running_output_does_not_add_a_new_device_until_explicitly_started() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        let first = runtime.devices.keys().next().cloned().unwrap();
        runtime.start_output(&first.control_id()).unwrap();
        runtime.set_default_source(None).unwrap();

        install_test_device(&mut runtime, "app-2", "slot-b", 20);
        let second = runtime
            .devices
            .keys()
            .find(|device| device.client_id == "app-2")
            .cloned()
            .unwrap();
        assert_eq!(runtime.output_devices.len(), 1);

        runtime
            .set_device_channel_source(
                second.control_id(),
                Channel::A,
                "source-fixed-waveform".to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(runtime.output_devices.len(), 1);

        runtime
            .set_device_channel_source(
                second.control_id(),
                Channel::B,
                "source-test-secondary".to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(runtime.output_devices.len(), 1);

        runtime.start_output(&second.control_id()).unwrap();
        assert_eq!(runtime.output_devices.len(), 2);
    }

    #[tokio::test]
    async fn shared_non_fixed_source_is_sampled_once_per_tick_and_fanned_out() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 20);
        let devices = runtime.devices.keys().cloned().collect::<Vec<_>>();
        for device in &devices {
            for channel in Channel::ALL {
                runtime
                    .set_device_channel_source(
                        device.control_id(),
                        channel,
                        "source-test-secondary".to_owned(),
                    )
                    .await
                    .unwrap();
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        runtime
            .sources
            .get_mut("source-test-secondary")
            .unwrap()
            .source = Some(Box::new(CountingSource {
            calls: Arc::clone(&calls),
        }));

        for device in devices {
            runtime.start_output(&device.control_id()).unwrap();
        }
        runtime.output_tick().await;

        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn fixed_waveforms_sample_per_binding_while_other_sources_are_shared() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 20);
        let second = runtime
            .devices
            .keys()
            .find(|device| device.client_id == "app-2")
            .cloned()
            .unwrap();
        runtime
            .set_device_channel_source(
                second.control_id(),
                Channel::B,
                "source-test-secondary".to_owned(),
            )
            .await
            .unwrap();
        let test_calls = Arc::new(AtomicUsize::new(0));
        let manual_calls = Arc::new(AtomicUsize::new(0));
        runtime
            .sources
            .get_mut("source-test-secondary")
            .unwrap()
            .source = Some(Box::new(CountingSource {
            calls: Arc::clone(&test_calls),
        }));
        for waveform in runtime.fixed_waveform_bindings.values_mut() {
            waveform.source = Box::new(CountingSource {
                calls: Arc::clone(&manual_calls),
            });
        }

        let devices = runtime.devices.keys().cloned().collect::<Vec<_>>();
        for device in devices {
            runtime.start_output(&device.control_id()).unwrap();
        }
        runtime.output_tick().await;

        assert_eq!(test_calls.load(Ordering::Relaxed), 1);
        assert_eq!(manual_calls.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn pairing_url_uses_tid_and_percent_encodes_websocket_url() {
        assert_eq!(
            pairing_url("wss://trex.dungeon-lab.cn/v4", "a1b2c3d4").unwrap(),
            "https://dungeon-lab.cn/s/?v=1&action=socket&url=wss%3A%2F%2Ftrex.dungeon-lab.cn%2Fv4%3Ftid%3Da1b2c3d4"
        );
        assert_eq!(
            pairing_url("wss://relay.example/exact-path", "a1b2c3d4").unwrap(),
            "https://dungeon-lab.cn/s/?v=1&action=socket&url=wss%3A%2F%2Frelay.example%2Fexact-path%3Ftid%3Da1b2c3d4"
        );
    }

    #[test]
    fn nested_device_patch_does_not_erase_siblings() {
        let mut device = json!({
            "slotId": "slot-a",
            "props": { "power": 87, "intensityA": 11, "intensityB": 7 },
            "slotState": {
                "channelA": { "isMuted": false, "intensityMax": 100 }
            }
        });
        deep_merge(
            &mut device,
            &json!({
                "slotId": "slot-a",
                "props": { "intensityA": 12 },
                "slotState": { "channelA": { "isMuted": true } }
            }),
        );
        assert_eq!(device["props"]["power"], 87);
        assert_eq!(device["props"]["intensityB"], 7);
        assert_eq!(device["props"]["intensityA"], 12);
        assert_eq!(device["slotState"]["channelA"]["intensityMax"], 100);
        assert_eq!(device["slotState"]["channelA"]["isMuted"], true);
    }

    #[test]
    fn one_muted_channel_still_accepts_dual_channel_output() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 20);
        let selected = runtime.devices.keys().next().cloned().unwrap();
        runtime.devices.get_mut(&selected).unwrap()["slotState"]["channelA"]["isMuted"] =
            Value::Bool(true);
        runtime.refresh_device_snapshots();

        assert_eq!(
            runtime.snapshot.devices[0].channel_a_status,
            ChannelStatus::Disabled
        );
        assert_eq!(runtime.start_output(&selected.control_id()), Ok(()));
    }

    #[test]
    fn both_muted_channels_still_accept_control_output() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 20);
        let selected = runtime.devices.keys().next().cloned().unwrap();
        let slot_state = &mut runtime.devices.get_mut(&selected).unwrap()["slotState"];
        slot_state["channelA"]["isMuted"] = Value::Bool(true);
        slot_state["channelB"]["isMuted"] = Value::Bool(true);
        runtime.refresh_device_snapshots();

        assert_eq!(
            runtime.snapshot.devices[0].channel_a_status,
            ChannelStatus::Disabled
        );
        assert_eq!(
            runtime.snapshot.devices[0].channel_b_status,
            ChannelStatus::Disabled
        );
        assert_eq!(runtime.start_output(&selected.control_id()), Ok(()));
    }

    #[test]
    fn app_intensity_lock_keeps_pc_value_and_detects_phone_changes() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 20);
        runtime.set_allow_app_intensity_control(false);

        let selected = runtime.devices.keys().next().cloned().unwrap();
        runtime.devices.get_mut(&selected).unwrap()["props"]["intensityA"] = Value::from(35);
        runtime.refresh_device_snapshots();

        assert_eq!(runtime.snapshot.devices[0].intensity_a, 20);
        assert_eq!(
            runtime.devices[&selected]["props"]["intensityA"],
            Value::from(35)
        );
        assert_eq!(
            runtime.intensity_lock_corrections(),
            vec![(selected.clone(), Channel::A, 35, 20)]
        );

        runtime.set_intensity_lock_target(0, 0);
        assert_eq!(runtime.snapshot.devices[0].intensity_a, 0);
        assert_eq!(
            runtime.intensity_lock_corrections(),
            vec![(selected, Channel::A, 35, 0)]
        );
        runtime.set_allow_app_intensity_control(true);
        assert_eq!(runtime.snapshot.devices[0].intensity_a, 35);
        assert_eq!(runtime.intensity_lock_corrections(), Vec::new());
    }

    #[test]
    fn intensity_locks_are_independent_for_each_device() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 30);
        runtime.set_allow_app_intensity_control(false);

        let first = DeviceKey {
            connection_id: V4_CONNECTION_ID.to_owned(),
            client_id: "app-1".to_owned(),
            slot_id: "slot-a".to_owned(),
        };
        let second = DeviceKey {
            connection_id: V4_CONNECTION_ID.to_owned(),
            client_id: "app-2".to_owned(),
            slot_id: "slot-b".to_owned(),
        };
        runtime.devices.get_mut(&first).unwrap()["props"]["intensityA"] = Value::from(15);
        runtime.devices.get_mut(&second).unwrap()["props"]["intensityA"] = Value::from(40);

        assert_eq!(
            runtime.intensity_lock_corrections(),
            vec![(first, Channel::A, 15, 10), (second, Channel::A, 40, 30),]
        );
    }

    #[test]
    fn explicit_device_starts_and_single_disconnect_preserve_parallel_output_set() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        let first = runtime.devices.keys().next().cloned().unwrap();
        runtime.start_output(&first.control_id()).unwrap();
        assert_eq!(runtime.output_devices.len(), 1);

        install_test_device(&mut runtime, "app-2", "slot-b", 20);
        runtime.reconcile_connected_devices();
        assert_eq!(runtime.output_devices.len(), 1);
        let second = runtime
            .devices
            .keys()
            .find(|device| device.client_id == "app-2")
            .cloned()
            .unwrap();
        runtime.start_output(&second.control_id()).unwrap();
        assert_eq!(runtime.output_devices.len(), 2);

        runtime.remove_app("app-1");
        assert_eq!(runtime.snapshot.output.state, OutputState::Running);
        assert_eq!(runtime.output_devices.len(), 1);
        assert!(
            runtime
                .output_devices
                .iter()
                .all(|device| device.client_id == "app-2")
        );
    }

    #[tokio::test]
    async fn muting_one_channel_while_running_does_not_stop_control_output() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 20);
        let selected = runtime.devices.keys().next().cloned().unwrap();
        runtime.start_output(&selected.control_id()).unwrap();

        runtime
            .apply_app_message(
                "app-1",
                &json!({
                    "t": "ev",
                    "ev": "slots.patch",
                    "slots": [{
                        "slotId": "slot-a",
                        "slotState": {"channelB": {"isMuted": true}}
                    }]
                }),
            )
            .await;

        assert_eq!(runtime.snapshot.output.state, OutputState::Running);
        assert_eq!(
            runtime.snapshot.devices[0].channel_a_status,
            ChannelStatus::Active
        );
        assert_eq!(
            runtime.snapshot.devices[0].channel_b_status,
            ChannelStatus::Disabled
        );
    }

    #[test]
    fn shutdown_orders_clear_before_both_zero_operations() {
        let requests = stop_operation_requests("slot-a", true);
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0]["m"], "device.op.clear");
        assert_eq!(requests[1]["data"]["t"], 7);
        assert_eq!(requests[1]["data"]["c"], 0);
        assert_eq!(requests[1]["data"]["v"], 0);
        assert_eq!(requests[2]["data"]["t"], 7);
        assert_eq!(requests[2]["data"]["c"], 1);
    }

    #[test]
    fn pulse_request_contains_one_100ms_frame_without_fake_repeat() {
        let request = append_pulse_request("request-1", "slot-a", Channel::A, "0A0A0A0A00000000");
        assert_eq!(request["data"]["d"], 100);
        assert_eq!(request["data"]["v"].as_array().unwrap().len(), 1);
        assert_eq!(request["data"]["ver"], 3);
    }

    #[test]
    fn source_switch_clear_targets_only_one_channel() {
        let request = clear_channel_request("slot-a", Channel::B);
        assert_eq!(request["m"], "device.op.clear");
        assert_eq!(request["data"]["s"], "slot-a");
        assert_eq!(request["data"]["c"], 1);
    }

    #[test]
    fn timestamp_is_valid_iso_shape_at_unix_epoch_boundaries() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_678), (2026, 8, 13));
    }

    #[tokio::test]
    async fn one_device_disconnect_keeps_other_devices_running_and_ignores_ghost_messages() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 20);

        let devices = runtime.devices.keys().cloned().collect::<Vec<_>>();
        for device in devices {
            runtime.start_output(&device.control_id()).unwrap();
        }

        runtime.remove_app("app-1");
        assert_eq!(runtime.snapshot.output.state, OutputState::Running);
        assert_eq!(runtime.output_devices.len(), 1);
        assert_eq!(
            runtime
                .devices
                .keys()
                .next()
                .map(|key| key.client_id.as_str()),
            Some("app-2")
        );

        runtime
            .apply_app_message(
                "app-1",
                &json!({
                    "t": "ev",
                    "ev": "devices.snapshot",
                    "devices": [{"slotId": "ghost-slot", "props": {}}]
                }),
            )
            .await;
        assert!(
            runtime
                .devices
                .keys()
                .all(|key| key.client_id.as_str() != "app-1")
        );
        assert_eq!(runtime.snapshot.output.state, OutputState::Running);
    }

    #[tokio::test]
    async fn repeated_relative_intensity_is_blocked_until_authoritative_state_arrives() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 70);
        let (event_sender, _events) = mpsc::channel(8);
        let (relay, relay_task) = spawn_relay_client(event_sender, 8);
        runtime.relay = Some(relay.clone());

        runtime
            .adjust_device_intensity(
                Some(&runtime.devices.keys().next().unwrap().control_id()),
                Channel::A,
                10,
            )
            .unwrap();
        assert_eq!(
            runtime.adjust_device_intensity(
                Some(&runtime.devices.keys().next().unwrap().control_id()),
                Channel::A,
                1
            ),
            Err(HubError::QueueBusy)
        );
        runtime.apply_slots_patch(
            "app-1",
            &json!({
                "slots": [{"slotId": "slot-a", "props": {"intensityA": 75}}]
            }),
        );
        assert_eq!(runtime.snapshot.devices[0].intensity_a, 75);
        assert_eq!(
            runtime.adjust_device_intensity(
                Some(&runtime.devices.keys().next().unwrap().control_id()),
                Channel::A,
                1
            ),
            Err(HubError::QueueBusy),
            "无关 slots.patch 不能提前确认 in-flight 相对强度操作"
        );
        let key = IntensityKey {
            device: runtime.devices.keys().next().cloned().unwrap(),
            channel: Channel::A,
        };
        assert_eq!(
            runtime
                .pending_intensity_operations
                .get(&key)
                .map(|pending| pending.projected),
            Some(80)
        );

        relay.shutdown_now();
        relay_task.await.unwrap();
    }

    #[tokio::test]
    async fn intensity_response_waits_for_matching_patch_without_reapplying_delta() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 20);
        let device = runtime.devices.keys().next().cloned().unwrap();
        let key = IntensityKey {
            device: device.clone(),
            channel: Channel::A,
        };
        runtime
            .intensity_lock_targets
            .insert(device.clone(), IntensityLockTarget { a: 30, b: 0 });
        runtime
            .pending_intensity_requests
            .insert("intensity-1".to_owned(), key.clone());
        runtime.pending_intensity_operations.insert(
            key.clone(),
            PendingIntensityOperation {
                request_id: "intensity-1".to_owned(),
                projected: 30,
                lock_correction: false,
                response_received: false,
                projected_observed: false,
            },
        );

        runtime
            .apply_app_message(
                "app-1",
                &json!({"t":"resp","reqId":"intensity-1","result":{}}),
            )
            .await;
        assert_eq!(
            runtime
                .pending_intensity_operations
                .get(&key)
                .map(|pending| pending.response_received),
            Some(true)
        );
        assert_eq!(runtime.intensity_lock_corrections(), Vec::new());

        runtime.replace_devices(
            "app-1",
            &[json!({"slotId":"slot-a","name":"仅包含描述信息的旧快照"})],
        );
        assert_eq!(
            device_intensity_from_value(&runtime.devices[&device], Channel::A),
            Some(20),
            "缺少强度字段的设备快照必须保留已知值"
        );
        assert!(runtime.pending_intensity_operations.contains_key(&key));

        runtime.apply_slots_patch(
            "app-1",
            &json!({
                "slots": [{"slotId":"slot-a","props":{"intensityA":30}}]
            }),
        );
        assert!(!runtime.pending_intensity_operations.contains_key(&key));
        assert_eq!(
            device_intensity_from_value(&runtime.devices[&device], Channel::A),
            Some(30)
        );
        assert_eq!(runtime.intensity_lock_corrections(), Vec::new());
    }

    #[tokio::test]
    async fn matching_intensity_patch_before_response_completes_only_after_response() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 20);
        let device = runtime.devices.keys().next().cloned().unwrap();
        let key = IntensityKey {
            device: device.clone(),
            channel: Channel::A,
        };
        runtime
            .intensity_lock_targets
            .insert(device.clone(), IntensityLockTarget { a: 30, b: 0 });
        runtime
            .pending_intensity_requests
            .insert("intensity-1".to_owned(), key.clone());
        runtime.pending_intensity_operations.insert(
            key.clone(),
            PendingIntensityOperation {
                request_id: "intensity-1".to_owned(),
                projected: 30,
                lock_correction: false,
                response_received: false,
                projected_observed: false,
            },
        );

        runtime.apply_slots_patch(
            "app-1",
            &json!({
                "slots": [{"slotId":"slot-a","props":{"intensityA":30}}]
            }),
        );
        assert!(runtime.pending_intensity_operations.contains_key(&key));

        runtime
            .apply_app_message(
                "app-1",
                &json!({"t":"resp","reqId":"intensity-1","result":{}}),
            )
            .await;
        assert!(!runtime.pending_intensity_operations.contains_key(&key));
        assert!(runtime.intensity_lock_corrections().is_empty());
    }

    #[tokio::test]
    async fn synchronized_intensity_adjustment_targets_every_device() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 10);
        let (event_sender, _events) = mpsc::channel(8);
        let (relay, relay_task) = spawn_relay_client(event_sender, 8);
        runtime.relay = Some(relay.clone());

        runtime.set_sync_all_devices(true).unwrap();
        runtime
            .adjust_device_intensity(
                Some(&runtime.devices.keys().next().unwrap().control_id()),
                Channel::A,
                5,
            )
            .unwrap();

        let first = IntensityKey {
            device: DeviceKey {
                connection_id: V4_CONNECTION_ID.to_owned(),
                client_id: "app-1".to_owned(),
                slot_id: "slot-a".to_owned(),
            },
            channel: Channel::A,
        };
        let second = IntensityKey {
            device: DeviceKey {
                connection_id: V4_CONNECTION_ID.to_owned(),
                client_id: "app-2".to_owned(),
                slot_id: "slot-b".to_owned(),
            },
            channel: Channel::A,
        };
        assert_eq!(
            runtime
                .pending_intensity_operations
                .get(&first)
                .map(|pending| pending.projected),
            Some(15)
        );
        assert_eq!(
            runtime
                .pending_intensity_operations
                .get(&second)
                .map(|pending| pending.projected),
            Some(15)
        );

        relay.shutdown_now();
        relay_task.await.unwrap();
    }

    #[tokio::test]
    async fn device_specific_intensity_adjustment_preserves_other_devices() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 30);
        let (event_sender, _events) = mpsc::channel(8);
        let (relay, relay_task) = spawn_relay_client(event_sender, 8);
        runtime.relay = Some(relay.clone());

        let first = runtime.devices.keys().next().cloned().unwrap();
        let second = DeviceKey {
            connection_id: V4_CONNECTION_ID.to_owned(),
            client_id: "app-2".to_owned(),
            slot_id: "slot-b".to_owned(),
        };
        runtime
            .adjust_device_intensity(Some(&second.control_id()), Channel::A, 5)
            .unwrap();

        assert_eq!(runtime.snapshot.devices[0].intensity_a, 10);
        assert!(
            !runtime
                .pending_intensity_operations
                .contains_key(&IntensityKey {
                    device: first,
                    channel: Channel::A,
                })
        );
        assert_eq!(
            runtime
                .pending_intensity_operations
                .get(&IntensityKey {
                    device: second,
                    channel: Channel::A,
                })
                .map(|pending| pending.projected),
            Some(35)
        );

        relay.shutdown_now();
        relay_task.await.unwrap();
    }

    #[tokio::test]
    async fn enabling_synchronization_aligns_to_baseline_actual_strengths() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 30);
        let (event_sender, _events) = mpsc::channel(8);
        let (relay, relay_task) = spawn_relay_client(event_sender, 8);
        runtime.relay = Some(relay.clone());

        runtime.set_sync_all_devices(true).unwrap();

        let second = IntensityKey {
            device: DeviceKey {
                connection_id: V4_CONNECTION_ID.to_owned(),
                client_id: "app-2".to_owned(),
                slot_id: "slot-b".to_owned(),
            },
            channel: Channel::A,
        };
        assert!(runtime.snapshot.sync_all_devices);
        assert_eq!(
            runtime
                .pending_intensity_operations
                .get(&second)
                .map(|pending| pending.projected),
            Some(10)
        );

        relay.shutdown_now();
        relay_task.await.unwrap();
    }

    #[tokio::test]
    async fn synchronization_uses_explicit_baseline() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 30);
        let (events, _receiver) = mpsc::channel(8);
        let (relay, relay_task) = spawn_relay_client(events, 8);
        runtime.relay = Some(relay.clone());

        let baseline = DeviceKey {
            connection_id: V4_CONNECTION_ID.to_owned(),
            client_id: "app-2".to_owned(),
            slot_id: "slot-b".to_owned(),
        }
        .control_id();
        runtime
            .set_sync_all_devices_from(Some(baseline), true)
            .unwrap();
        let first = IntensityKey {
            device: DeviceKey {
                connection_id: V4_CONNECTION_ID.to_owned(),
                client_id: "app-1".to_owned(),
                slot_id: "slot-a".to_owned(),
            },
            channel: Channel::A,
        };
        assert_eq!(
            runtime
                .pending_intensity_operations
                .get(&first)
                .map(|pending| pending.projected),
            Some(30)
        );
        relay.shutdown_now();
        relay_task.await.unwrap();
    }

    #[tokio::test]
    async fn synchronized_mode_aligns_a_newly_connected_device() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 10);
        let (event_sender, _events) = mpsc::channel(8);
        let (relay, relay_task) = spawn_relay_client(event_sender, 8);
        runtime.relay = Some(relay.clone());
        runtime.set_sync_all_devices(true).unwrap();

        install_test_device(&mut runtime, "app-3", "slot-c", 35);
        runtime.reconcile_intensity_lock();

        let new_device = IntensityKey {
            device: DeviceKey {
                connection_id: V4_CONNECTION_ID.to_owned(),
                client_id: "app-3".to_owned(),
                slot_id: "slot-c".to_owned(),
            },
            channel: Channel::A,
        };
        assert_eq!(
            runtime
                .pending_intensity_operations
                .get(&new_device)
                .map(|pending| pending.projected),
            Some(10)
        );

        relay.shutdown_now();
        relay_task.await.unwrap();
    }

    #[tokio::test]
    async fn synchronized_intensity_preflight_rejects_all_when_one_device_exceeds_limit() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 10);
        let second = DeviceKey {
            connection_id: V4_CONNECTION_ID.to_owned(),
            client_id: "app-2".to_owned(),
            slot_id: "slot-b".to_owned(),
        };
        runtime.devices.get_mut(&second).unwrap()["slotState"]["channelA"]["intensityMax"] =
            Value::from(12);
        runtime.refresh_device_snapshots();
        let (event_sender, _events) = mpsc::channel(8);
        let (relay, relay_task) = spawn_relay_client(event_sender, 8);
        runtime.relay = Some(relay.clone());

        runtime.set_sync_all_devices(true).unwrap();
        assert_eq!(
            runtime.adjust_device_intensity(
                Some(&runtime.devices.keys().next().unwrap().control_id()),
                Channel::A,
                5
            ),
            Err(HubError::IntensityLimit)
        );
        assert!(runtime.pending_intensity_operations.is_empty());

        relay.shutdown_now();
        relay_task.await.unwrap();
    }

    #[tokio::test]
    async fn channel_limits_follow_each_reported_device_channel() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 90);
        let device = runtime.devices.keys().next().cloned().unwrap();
        runtime.devices.get_mut(&device).unwrap()["slotState"]["channelA"]["intensityMax"] =
            Value::from(120);
        runtime.devices.get_mut(&device).unwrap()["slotState"]["channelB"]["intensityMax"] =
            Value::from(7);
        runtime.refresh_device_snapshots();
        assert_eq!(runtime.snapshot.devices[0].intensity_limit_a, 120);
        assert_eq!(runtime.snapshot.devices[0].intensity_limit_b, 7);
        let (event_sender, _events) = mpsc::channel(8);
        let (relay, relay_task) = spawn_relay_client(event_sender, 8);
        runtime.relay = Some(relay.clone());

        assert_eq!(
            runtime.adjust_device_intensity(
                Some(&runtime.devices.keys().next().unwrap().control_id()),
                Channel::A,
                31
            ),
            Err(HubError::IntensityLimit)
        );
        assert_eq!(
            runtime.adjust_device_intensity(
                Some(&runtime.devices.keys().next().unwrap().control_id()),
                Channel::B,
                8
            ),
            Err(HubError::IntensityLimit)
        );
        assert_eq!(
            runtime.adjust_device_intensity(
                Some(&runtime.devices.keys().next().unwrap().control_id()),
                Channel::A,
                15
            ),
            Ok(())
        );
        assert_eq!(
            runtime.adjust_device_intensity(
                Some(&runtime.devices.keys().next().unwrap().control_id()),
                Channel::B,
                7
            ),
            Ok(())
        );

        relay.shutdown_now();
        relay_task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn connection_timeout_counts_from_relay_connection_without_output() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        assert!(!runtime.snapshot.safety.connection_timeout_enabled);
        assert_eq!(runtime.snapshot.safety.connection_timeout_minutes, 60);
        runtime
            .handle_relay_event(RelayEvent::Connected {
                endpoint: "wss://example.test/v4".to_owned(),
            })
            .await;
        assert!(runtime.connection_started_at.is_some());
        assert!(runtime.output_devices.is_empty());
        tokio::time::advance(Duration::from_secs(61 * 60)).await;
        assert!(!runtime.connection_timed_out());

        runtime.update_safety(true, 60, false).await.unwrap();
        assert!(runtime.connection_timed_out());
        runtime.update_safety(true, 120, false).await.unwrap();
        assert!(!runtime.connection_timed_out());
        runtime.update_safety(false, 60, false).await.unwrap();
        assert!(!runtime.connection_timed_out());

        runtime
            .handle_relay_event(RelayEvent::Disconnected {
                reason: "测试断开".to_owned(),
                retryable: false,
            })
            .await;
        assert!(runtime.connection_started_at.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn connection_timeout_disconnects_relay_without_active_output() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        let (event_sender, _events) = mpsc::channel(8);
        let (relay, relay_task) = spawn_relay_client(event_sender, 8);
        runtime.relay = Some(relay.clone());
        runtime
            .handle_relay_event(RelayEvent::Connected {
                endpoint: "wss://example.test/v4".to_owned(),
            })
            .await;
        runtime.update_safety(true, 60, false).await.unwrap();
        tokio::time::advance(Duration::from_secs(61 * 60)).await;

        runtime.disconnect_if_timed_out().await;

        assert_eq!(runtime.v4_connection.state, ConnectionState::Disconnected);
        assert!(runtime.connection_started_at.is_none());
        assert!(!runtime.auto_reconnect_enabled);
        assert!(
            runtime
                .snapshot
                .logs
                .iter()
                .any(|log| log.message.contains("连接时长已到"))
        );

        relay.shutdown_now();
        relay_task.await.unwrap();
    }

    #[tokio::test]
    async fn pending_wave_high_water_mark_applies_backpressure_without_stopping_output() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        runtime.snapshot.output.state = OutputState::Running;
        for index in 0..MAX_PENDING_WAVE_OPERATIONS {
            runtime.pending_wave_operations.insert(
                format!("wave-{index}"),
                PendingWaveOperation {
                    device: DeviceKey {
                        connection_id: V4_CONNECTION_ID.to_owned(),
                        client_id: "app-1".to_owned(),
                        slot_id: "slot-a".to_owned(),
                    },
                    channel: Channel::A,
                    generation: runtime.operation_generation,
                    sent_at: Instant::now(),
                },
            );
        }

        runtime.output_tick().await;

        assert_eq!(runtime.snapshot.output.state, OutputState::Running);
        assert_eq!(
            runtime.pending_wave_operations.len(),
            MAX_PENDING_WAVE_OPERATIONS
        );
        assert_eq!(runtime.snapshot.output.frames_sent, 0);
    }

    #[tokio::test]
    async fn pending_wave_response_timeout_stops_output() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        runtime.snapshot.output.state = OutputState::Running;
        runtime.output_devices.insert(DeviceKey {
            connection_id: V4_CONNECTION_ID.to_owned(),
            client_id: "app-1".to_owned(),
            slot_id: "slot-a".to_owned(),
        });
        runtime.pending_wave_operations.insert(
            "wave-timeout".to_owned(),
            PendingWaveOperation {
                device: DeviceKey {
                    connection_id: V4_CONNECTION_ID.to_owned(),
                    client_id: "app-1".to_owned(),
                    slot_id: "slot-a".to_owned(),
                },
                channel: Channel::A,
                generation: runtime.operation_generation,
                sent_at: Instant::now() - WAVE_OPERATION_RESPONSE_TIMEOUT,
            },
        );

        runtime.output_tick().await;

        assert_eq!(runtime.snapshot.output.state, OutputState::Error);
        assert!(
            runtime
                .snapshot
                .output
                .last_error
                .as_deref()
                .is_some_and(|error| error.starts_with("设备波形响应超时，输出已停止"))
        );
        assert!(runtime.pending_wave_operations.is_empty());
    }

    #[tokio::test]
    async fn current_wave_error_from_app_stops_output() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        let device = runtime.devices.keys().next().cloned().unwrap();
        runtime.start_output(&device.control_id()).unwrap();
        runtime.pending_wave_operations.insert(
            "wave-1".to_owned(),
            PendingWaveOperation {
                device: DeviceKey {
                    connection_id: V4_CONNECTION_ID.to_owned(),
                    client_id: "app-1".to_owned(),
                    slot_id: "slot-a".to_owned(),
                },
                channel: Channel::A,
                generation: runtime.device_source_bindings[&source_binding(&device, Channel::A)]
                    .generation,
                sent_at: Instant::now(),
            },
        );

        runtime
            .apply_app_message(
                "app-1",
                &json!({"t":"resp","reqId":"wave-1","error":"operation rejected"}),
            )
            .await;
        assert_eq!(runtime.snapshot.output.state, OutputState::Error);
        assert!(
            runtime
                .snapshot
                .output
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("operation rejected"))
        );
    }

    #[tokio::test]
    async fn late_notification_from_an_already_taken_safety_command_does_not_cancel_its_ack() {
        let (hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        hub.safety_epoch.fetch_add(1, Ordering::AcqRel);
        let (reply, _response) = oneshot::channel();
        hub.safety_commands
            .try_send(HubSafetyCommand::DisconnectConnection {
                connection_id: V4_CONNECTION_ID.into(),
                reply,
            })
            .unwrap();
        let _already_taken = runtime.safety_commands.recv().await.unwrap();
        let (ack, received) = oneshot::channel();
        let waiting = runtime
            .wait_for_ordinary_relay(async { received.await.map_err(|_| HubError::Stopped) });
        hub.safety_wakeup.notify_waiters();
        ack.send(()).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(200), waiting)
                .await
                .unwrap(),
            Ok(())
        );
    }

    #[tokio::test]
    async fn pending_ordinary_relay_ack_is_interrupted_by_stop_and_shutdown() {
        let (hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        let device_id = runtime.devices.keys().next().unwrap().control_id();
        let stop = tokio::spawn(async move { hub.stop_output(device_id).await });
        let result = tokio::time::timeout(
            Duration::from_millis(200),
            runtime.wait_for_ordinary_relay(std::future::pending::<Result<(), HubError>>()),
        )
        .await
        .unwrap();
        assert_eq!(result, Err(HubError::QueueBusy));
        let command = runtime.safety_commands.recv().await.unwrap();
        runtime.handle_safety_command(command).await;
        stop.await.unwrap().unwrap();
        assert_ne!(runtime.snapshot.output.state, OutputState::Running);

        let (hub, runtime) = create_hub("wss://example.test/v4".to_owned());
        hub.shutdown_now();
        assert_eq!(
            runtime
                .wait_for_ordinary_relay(std::future::pending::<Result<(), HubError>>())
                .await,
            Err(HubError::QueueBusy)
        );
    }

    #[tokio::test]
    async fn cold_start_waits_for_an_explicit_relay_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}/v4", listener.local_addr().unwrap());
        let (hub, runtime) = create_hub(endpoint);
        assert!(!runtime.auto_reconnect_enabled);
        let runtime_task = tokio::spawn(runtime.run());
        assert!(
            tokio::time::timeout(Duration::from_millis(150), listener.accept())
                .await
                .is_err()
        );
        assert_eq!(
            hub.snapshot().connections[0].state,
            ConnectionState::Disconnected
        );
        assert_eq!(hub.snapshot().output_device_count, 0);
        let relay = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(
                    json!({"type":"hello", "clientId":"explicit-controller"})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            while let Some(Ok(message)) = socket.next().await {
                if matches!(message, Message::Close(_)) {
                    break;
                }
            }
        });
        hub.transport(TransportAction::Connect {
            transport: TransportKind::WsV4,
            endpoint: hub.snapshot().connections[0].endpoint.clone(),
        })
        .await
        .unwrap();
        wait_for_snapshot(&hub, |snapshot| {
            snapshot.connections[0].controller_id.as_deref() == Some("explicit-controller")
        })
        .await;
        hub.shutdown_gracefully().await.unwrap();
        runtime_task.await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), relay)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn stop_invalidates_queued_intensity_sync_and_reconnection_commands() {
        let (hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        type CommandFactory = fn(oneshot::Sender<Result<(), HubError>>) -> HubCommand;
        let commands: [CommandFactory; 2] = [
            |reply| HubCommand::AdjustIntensity {
                device_id: Some("missing".to_owned()),
                channel: Channel::A,
                delta: 10,
                safety_epoch: 0,
                reply,
            },
            |reply| HubCommand::SetSyncAllDevices {
                device_id: "missing".to_owned(),
                enabled: true,
                safety_epoch: 0,
                reply,
            },
        ];
        hub.safety_epoch.fetch_add(1, Ordering::AcqRel);
        for make_command in commands {
            let (reply, response) = oneshot::channel();
            runtime.handle_command(make_command(reply)).await;
            assert_eq!(response.await.unwrap(), Err(HubError::QueueBusy));
        }
        for action in [
            TransportAction::Connect {
                transport: TransportKind::WsV4,
                endpoint: "wss://example.test/v4".into(),
            },
            TransportAction::RefreshPairing {
                connection_id: V4_CONNECTION_ID.into(),
            },
        ] {
            let (reply, response) = oneshot::channel();
            runtime
                .handle_command(HubCommand::Transport {
                    action,
                    safety_epoch: 0,
                    reply,
                })
                .await;
            assert_eq!(response.await.unwrap(), Err(HubError::QueueBusy));
        }
        assert!(!runtime.auto_reconnect_enabled);
        assert_eq!(runtime.v4_connection.state, ConnectionState::Disconnected);
        assert!(!runtime.snapshot.sync_all_devices);
    }

    #[tokio::test]
    async fn graceful_shutdown_has_a_completion_signal() {
        let (hub, runtime) = create_hub("not-a-websocket-url".to_owned());
        let runtime_task = tokio::spawn(runtime.run());

        tokio::time::timeout(Duration::from_secs(2), hub.shutdown_gracefully())
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), runtime_task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn retryable_disconnect_reconnects_but_user_disconnect_does_not() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        runtime.enable_auto_reconnect();

        runtime
            .handle_relay_event(RelayEvent::Disconnected {
                reason: "远端连接中断".to_owned(),
                retryable: true,
            })
            .await;
        assert_eq!(runtime.reconnect_attempt, 1);
        assert!(runtime.reconnect_at.is_some());
        assert!(
            runtime
                .v4_connection
                .last_error
                .as_deref()
                .is_some_and(|message| message.contains("1 秒后自动重连"))
        );

        runtime.reconnect_at = Some(Instant::now());
        runtime.reconnect_if_due();
        assert_eq!(runtime.v4_connection.state, ConnectionState::Connecting);

        runtime.disable_auto_reconnect();
        runtime
            .handle_relay_event(RelayEvent::Disconnected {
                reason: "用户已断开 Relay".to_owned(),
                retryable: false,
            })
            .await;
        assert!(runtime.reconnect_at.is_none());
        assert_eq!(runtime.reconnect_attempt, 0);
        assert_eq!(runtime.v4_connection.state, ConnectionState::Disconnected);
    }

    #[tokio::test]
    async fn ordinary_stop_invalidates_an_already_queued_start() {
        let (hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);

        let (start_reply, start_response) = oneshot::channel();
        hub.commands
            .try_send(HubCommand::StartOutput {
                device_id: runtime.devices.keys().next().unwrap().control_id(),
                safety_epoch: hub.safety_epoch.load(Ordering::Acquire),
                reply: start_reply,
            })
            .unwrap();
        hub.safety_epoch.fetch_add(1, Ordering::AcqRel);
        let (stop_reply, _stop_response) = oneshot::channel();
        hub.safety_commands
            .try_send(HubSafetyCommand::StopOutput {
                device_id: runtime.devices.keys().next().unwrap().control_id(),
                reply: stop_reply,
            })
            .unwrap();

        let safety = runtime.safety_commands.recv().await.unwrap();
        runtime.handle_safety_command(safety).await;
        let command = runtime.commands.recv().await.unwrap();
        runtime.handle_command(command).await;

        assert_eq!(start_response.await.unwrap(), Err(HubError::QueueBusy));
        assert_ne!(runtime.snapshot.output.state, OutputState::Running);
    }

    #[tokio::test]
    async fn hub_streams_all_devices_switches_focus_without_stopping_and_clears_all() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}/v4", listener.local_addr().unwrap());
        let (pulse_sender, pulse_received) = oneshot::channel();
        let (source_switch_sender, source_switch_received) = oneshot::channel();
        let (clear_sender, clear_received) = oneshot::channel();
        let (shutdown_sender, shutdown_received) = oneshot::channel();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(
                    json!({"type":"hello","clientId":"controller-1"})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            for client_id in ["app-1", "app-2"] {
                socket
                    .send(Message::Text(
                        json!({"type":"client_attached","clientId":client_id})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
            }

            for _ in 0..2 {
                let devices_get = receive_json(&mut socket).await;
                assert_eq!(devices_get["data"]["m"], "devices.get");
                let client_id = devices_get["clientId"].as_str().unwrap();
                let (id, slot_id, name, device_type, power, intensity_a, intensity_b, limit) =
                    if client_id == "app-1" {
                        (0, "slot-a", "测试设备 A", "COYOTE_030", 90, 5, 6, 100)
                    } else {
                        (1, "slot-b", "测试设备 B", "COYOTE", 80, 7, 8, 80)
                    };
                socket
                    .send(Message::Text(
                        json!({
                            "type": "message",
                            "clientId": client_id,
                            "data": {
                                "t": "ev",
                                "ev": "devices.snapshot",
                                "devices": [{
                                    "id": id,
                                    "slotId": slot_id,
                                    "name": name,
                                    "type": device_type,
                                    "props": {
                                        "power": power,
                                        "intensityA": intensity_a,
                                        "intensityB": intensity_b,
                                        "channelAStatus": 2,
                                        "channelBStatus": 2
                                    },
                                    "slotState": {
                                        "channelA": {"intensityMax": limit},
                                        "channelB": {"intensityMax": limit}
                                    }
                                }]
                            }
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }

            let mut pulse_targets = BTreeSet::new();
            let mut clear_counts = BTreeMap::<String, usize>::new();
            let mut zero_targets = BTreeSet::new();
            let mut sent_pulse_signal = false;
            let mut sent_source_switch_signal = false;
            let mut sent_clear_signal = false;
            let mut pulse_sender = Some(pulse_sender);
            let mut source_switch_sender = Some(source_switch_sender);
            let mut clear_sender = Some(clear_sender);
            let mut shutdown_sender = Some(shutdown_sender);
            while let Some(message) = socket.next().await {
                let Ok(Message::Text(text)) = message else {
                    break;
                };
                let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
                let data = &frame["data"];
                if data["m"] == "device.op" && data["data"]["t"] == 0 {
                    pulse_targets.insert((
                        frame["clientId"].as_str().unwrap().to_owned(),
                        data["data"]["s"].as_str().unwrap().to_owned(),
                        data["data"]["c"].as_u64().unwrap(),
                    ));
                    if pulse_targets
                        == BTreeSet::from([
                            ("app-1".to_owned(), "slot-a".to_owned(), 0),
                            ("app-1".to_owned(), "slot-a".to_owned(), 1),
                            ("app-2".to_owned(), "slot-b".to_owned(), 0),
                            ("app-2".to_owned(), "slot-b".to_owned(), 1),
                        ])
                        && !sent_pulse_signal
                    {
                        pulse_sender.take().unwrap().send(()).unwrap();
                        sent_pulse_signal = true;
                    }
                }
                if data["m"] == "device.op.clear" {
                    let slot = data["data"]["s"].as_str().unwrap().to_owned();
                    *clear_counts.entry(slot).or_default() += 1;
                    if clear_counts.get("slot-b").copied().unwrap_or(0) == 1
                        && clear_counts.get("slot-a").copied().unwrap_or(0) == 0
                        && data["data"]["c"] == 0
                        && !sent_source_switch_signal
                    {
                        source_switch_sender.take().unwrap().send(()).unwrap();
                        sent_source_switch_signal = true;
                    }
                    if clear_counts.get("slot-a").copied().unwrap_or(0) >= 1
                        && clear_counts.get("slot-b").copied().unwrap_or(0) >= 1
                        && !sent_clear_signal
                    {
                        clear_sender.take().unwrap().send(()).unwrap();
                        sent_clear_signal = true;
                    }
                }
                if data["m"] == "device.op" && data["data"]["t"] == 7 {
                    zero_targets.insert((
                        frame["clientId"].as_str().unwrap().to_owned(),
                        data["data"]["s"].as_str().unwrap().to_owned(),
                        data["data"]["c"].as_u64().unwrap(),
                    ));
                    if zero_targets
                        == BTreeSet::from([
                            ("app-1".to_owned(), "slot-a".to_owned(), 0),
                            ("app-1".to_owned(), "slot-a".to_owned(), 1),
                            ("app-2".to_owned(), "slot-b".to_owned(), 0),
                            ("app-2".to_owned(), "slot-b".to_owned(), 1),
                        ])
                    {
                        shutdown_sender.take().unwrap().send(()).unwrap();
                        break;
                    }
                }
            }
        });

        let (hub, runtime) =
            create_hub_with_default_source(endpoint, Some("source-fixed-waveform".to_owned()));
        let runtime_task = tokio::spawn(runtime.run());
        hub.transport(TransportAction::Connect {
            transport: TransportKind::WsV4,
            endpoint: hub.snapshot().connections[0].endpoint.clone(),
        })
        .await
        .unwrap();
        wait_for_snapshot(&hub, |snapshot| snapshot.devices.len() == 2).await;
        assert_eq!(
            hub.snapshot().connections[0].state,
            ConnectionState::Connected
        );
        let first_device = hub
            .snapshot()
            .devices
            .iter()
            .find(|device| device.slot_id == "slot-a")
            .unwrap()
            .control_id
            .clone();
        let second_device = hub
            .snapshot()
            .devices
            .iter()
            .find(|device| device.slot_id == "slot-b")
            .unwrap()
            .control_id
            .clone();
        hub.start_output(first_device.clone()).await.unwrap();
        hub.start_output(second_device.clone()).await.unwrap();
        assert_eq!(hub.snapshot().output_device_count, 2);
        tokio::time::timeout(Duration::from_secs(2), pulse_received)
            .await
            .unwrap()
            .unwrap();

        hub.set_device_channel_source(
            second_device.clone(),
            Channel::A,
            "source-test-secondary".to_owned(),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), source_switch_received)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hub.snapshot().output.state, OutputState::Running);
        assert_eq!(
            hub.snapshot()
                .devices
                .iter()
                .find(|device| device.control_id == second_device)
                .and_then(|device| device.source_id_a.as_deref()),
            Some("source-test-secondary")
        );

        assert_eq!(hub.snapshot().output.state, OutputState::Running);
        assert_eq!(hub.snapshot().devices.len(), 2);

        hub.stop_output(second_device.clone()).await.unwrap();
        assert_eq!(hub.snapshot().output.state, OutputState::Running);
        assert_eq!(hub.snapshot().output_device_count, 1);
        hub.stop_output(first_device.clone()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), clear_received)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hub.snapshot().output.state, OutputState::Idle);
        assert_eq!(hub.snapshot().output_device_count, 0);

        hub.start_output(first_device).await.unwrap();
        hub.start_output(second_device).await.unwrap();
        hub.shutdown_gracefully().await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), shutdown_received)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hub.snapshot().output.state, OutputState::Stopped);
        assert_eq!(hub.snapshot().output_device_count, 0);

        hub.shutdown_now();
        tokio::time::timeout(Duration::from_secs(2), runtime_task)
            .await
            .unwrap()
            .unwrap();
        server.await.unwrap();
    }

    async fn receive_json<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>) -> Value
    where
        tokio_tungstenite::WebSocketStream<S>:
            StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>,
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
            panic!("expected text frame");
        };
        serde_json::from_str(text.as_ref()).unwrap()
    }

    pub(super) fn install_test_device(
        runtime: &mut HubRuntime,
        client_id: &str,
        slot_id: &str,
        intensity_a: u16,
    ) {
        if runtime.devices.is_empty() && runtime.default_source_id.is_none() {
            runtime
                .set_default_source(Some("source-fixed-waveform".to_owned()))
                .unwrap();
        }
        runtime.apps.insert(client_id.to_owned());
        let key = DeviceKey {
            connection_id: V4_CONNECTION_ID.to_owned(),
            client_id: client_id.to_owned(),
            slot_id: slot_id.to_owned(),
        };
        runtime.devices.insert(
            key.clone(),
            json!({
                "id": slot_id,
                "slotId": slot_id,
                "name": "测试设备",
                "type": "COYOTE_030",
                "props": {
                    "power": 90,
                    "intensityA": intensity_a,
                    "intensityB": 0,
                    "channelAStatus": 2,
                    "channelBStatus": 2
                },
                "slotState": {
                    "channelA": {"intensityMax": 100},
                    "channelB": {"intensityMax": 100}
                }
            }),
        );

        runtime.v4_connection.state = ConnectionState::Connected;
        runtime.v4_connection.app_count = runtime.apps.len();
        runtime.reconcile_connected_devices();
        runtime.refresh_device_snapshots();
    }

    fn install_isolation_device(
        runtime: &mut HubRuntime,
        connection_id: &str,
        initialization: InitializationState,
    ) -> DeviceKey {
        if runtime.default_source_id.is_none() {
            runtime
                .set_default_source(Some(FIXED_WAVEFORM_SOURCE_ID.to_owned()))
                .unwrap();
        }
        let device = DeviceKey {
            connection_id: connection_id.to_owned(),
            client_id: "shared-client".to_owned(),
            slot_id: "shared-slot".to_owned(),
        };
        runtime.devices.insert(device.clone(), json!({
            "id":"shared-slot", "slotId":"shared-slot", "name":"隔离测试设备",
            "type":"COYOTE_030", "initialization":initialization,
            "props":{"power":null,"intensityA":30,"intensityB":20},
            "slotState":{"hasDevice":true,"channelA":{"intensityMax":100},"channelB":{"intensityMax":100}},
        }));
        if connection_id == V4_CONNECTION_ID {
            runtime.apps.insert(device.client_id.clone());
        }
        runtime.reconcile_connected_devices();
        runtime.refresh_device_snapshots();
        device
    }

    fn install_isolation_pending(runtime: &mut HubRuntime, device: &DeviceKey) {
        let request_id = format!("intensity-{}", device.connection_id);
        let key = IntensityKey {
            device: device.clone(),
            channel: Channel::A,
        };
        runtime
            .pending_intensity_requests
            .insert(request_id.clone(), key.clone());
        runtime.pending_intensity_operations.insert(
            key,
            PendingIntensityOperation {
                request_id,
                projected: 31,
                lock_correction: false,
                response_received: false,
                projected_observed: false,
            },
        );
        runtime.pending_wave_operations.insert(
            format!("wave-{}", device.connection_id),
            PendingWaveOperation {
                device: device.clone(),
                channel: Channel::A,
                generation: runtime.operation_generation,
                sent_at: Instant::now(),
            },
        );
    }

    #[test]
    fn v4_reset_preserves_other_connection_output_inputs_and_pending_operations() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        let v4 =
            install_isolation_device(&mut runtime, V4_CONNECTION_ID, InitializationState::Ready);
        let v3 =
            install_isolation_device(&mut runtime, V3_CONNECTION_ID, InitializationState::Ready);
        let ble = install_isolation_device(&mut runtime, "ble:fixture", InitializationState::Ready);
        for device in [&v4, &v3, &ble] {
            runtime.output_devices.insert(device.clone());
            install_isolation_pending(&mut runtime, device);
            runtime.reset_device_inputs(device);
        }

        runtime.snapshot.output.state = OutputState::Running;
        let epoch = runtime.safety_epoch.load(Ordering::Acquire);
        runtime.reset_connection_state(ConnectionState::Connecting);
        assert_eq!(runtime.safety_epoch.load(Ordering::Acquire), epoch);
        assert_eq!(runtime.snapshot.output.state, OutputState::Running);
        assert_eq!(
            runtime.devices.keys().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([v3.clone(), ble.clone()])
        );
        assert_eq!(
            runtime.output_devices,
            BTreeSet::from([v3.clone(), ble.clone()])
        );
        assert_eq!(runtime.device_source_bindings.len(), 4);
        assert_eq!(runtime.pending_intensity_operations.len(), 2);
        assert_eq!(runtime.pending_intensity_requests.len(), 2);
        assert_eq!(runtime.pending_wave_operations.len(), 2);
        assert!(runtime.apps.is_empty());
        assert_eq!(runtime.v4_connection.state, ConnectionState::Connecting);
    }

    #[test]
    fn v4_device_updates_and_disconnect_do_not_match_other_transport_client_ids() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        let v4 =
            install_isolation_device(&mut runtime, V4_CONNECTION_ID, InitializationState::Ready);
        let v3 =
            install_isolation_device(&mut runtime, V3_CONNECTION_ID, InitializationState::Ready);
        let ble = install_isolation_device(&mut runtime, "ble:fixture", InitializationState::Ready);
        for device in [&v4, &v3, &ble] {
            install_isolation_pending(&mut runtime, device);
        }
        runtime.apply_slots_patch(
            "shared-client",
            &json!({"slots":[{"slotId":"shared-slot","props":{"intensityA":50}}]}),
        );
        assert_eq!(runtime.devices[&v4]["props"]["intensityA"], 50);
        assert_eq!(runtime.devices[&v3]["props"]["intensityA"], 30);
        assert_eq!(runtime.devices[&ble]["props"]["intensityA"], 30);
        runtime.replace_devices("shared-client", &[]);
        assert!(!runtime.devices.contains_key(&v4));
        assert!(runtime.devices.contains_key(&v3));
        assert!(runtime.devices.contains_key(&ble));
        runtime.remove_app("shared-client");
        assert_eq!(runtime.devices.len(), 2);
        assert_eq!(runtime.pending_intensity_operations.len(), 2);
        assert_eq!(runtime.pending_wave_operations.len(), 2);
    }

    #[test]
    fn ble_accepts_physical_strength_and_does_not_use_the_app_lock() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        let ble = install_isolation_device(&mut runtime, "ble:fixture", InitializationState::Ready);
        assert!(!runtime.snapshot.safety.allow_app_intensity_control);
        runtime
            .intensity_lock_targets
            .insert(ble.clone(), IntensityLockTarget { a: 99, b: 99 });
        runtime.refresh_device_snapshots();
        assert_eq!(runtime.snapshot.devices.first().unwrap().intensity_a, 30);
        assert_eq!(
            runtime.device_channel_control_state(&ble, Channel::A),
            Some((30, 100))
        );
        runtime.sync_intensity_lock_targets();
        assert!(!runtime.intensity_lock_targets.contains_key(&ble));
        runtime.devices.get_mut(&ble).unwrap()["props"]["intensityA"] = json!(42);
        runtime.reconcile_intensity_lock();
        runtime.refresh_device_snapshots();
        assert_eq!(runtime.snapshot.devices.first().unwrap().intensity_a, 42);
        assert!(runtime.pending_intensity_operations.is_empty());
    }

    #[test]
    fn non_v4_devices_are_gated_by_initialization_instead_of_v4_app_presence() {
        for connection_id in [V3_CONNECTION_ID, "ble:fixture"] {
            let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
            let device = install_isolation_device(
                &mut runtime,
                connection_id,
                InitializationState::Initializing,
            );
            let (session, _queues) = crate::transport::session_channel(8);
            runtime.sessions.insert(connection_id.to_owned(), session);
            assert!(runtime.apps.is_empty());
            assert_eq!(
                runtime.start_output(&device.control_id()),
                Err(HubError::DeviceUnavailable)
            );
            assert_eq!(
                runtime.adjust_device_intensity(Some(&device.control_id()), Channel::A, 1),
                Err(HubError::DeviceUnavailable)
            );
            runtime.devices.get_mut(&device).unwrap()["initialization"] =
                json!(InitializationState::Ready);
            assert_eq!(runtime.start_output(&device.control_id()), Ok(()));
            assert_eq!(
                runtime.adjust_device_intensity(Some(&device.control_id()), Channel::A, 1),
                Ok(())
            );
        }
    }

    #[tokio::test]
    async fn v4_relay_failure_leaves_ble_output_running() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        let v4 =
            install_isolation_device(&mut runtime, V4_CONNECTION_ID, InitializationState::Ready);
        let ble = install_isolation_device(&mut runtime, "ble:fixture", InitializationState::Ready);
        runtime.output_devices.extend([v4, ble.clone()]);
        runtime.snapshot.output.state = OutputState::Running;
        runtime
            .handle_relay_event(RelayEvent::Disconnected {
                reason: "fixture disconnect".to_owned(),
                retryable: false,
            })
            .await;
        assert_eq!(runtime.output_devices, BTreeSet::from([ble]));
        assert_eq!(runtime.snapshot.output.state, OutputState::Running);
        assert_eq!(runtime.snapshot.output.last_error, None);
    }

    async fn wait_for_snapshot(hub: &HubHandle, predicate: impl Fn(&HubSnapshot) -> bool) {
        let mut snapshots = hub.subscribe();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if predicate(&snapshots.borrow_and_update()) {
                    break;
                }
                snapshots.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }
}

#[cfg(test)]
#[path = "hub/transport_tests.rs"]
mod transport_tests;
