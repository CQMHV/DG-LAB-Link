use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Map, Value, json};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Duration, Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::dglab::client::{RelayClientError, RelayClientHandle, RelayEvent, spawn_relay_client};
use crate::dglab::v4::pairing_url as build_pairing_url;
use crate::model::{Channel, WaveFrame};
use crate::sources::{WaveSource, builtin_registry};

const HUB_COMMAND_CAPACITY: usize = 64;
const HUB_SAFETY_COMMAND_CAPACITY: usize = 8;
const RELAY_COMMAND_CAPACITY: usize = 256;
const RELAY_EVENT_CAPACITY: usize = 128;
const MAX_OUTPUT_DEVICES: usize = 32;
const MAX_PENDING_WAVE_OPERATIONS: usize = 256;
const WAVE_OPERATION_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_LOGS: usize = 100;
const DEFAULT_CHANNEL_LIMIT: u16 = 80;
const DEFAULT_MAX_DURATION_MINUTES: u16 = 30;
const RELAY_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const RELAY_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
const RELAY_RECONNECT_MAX_DELAY_SECONDS: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Waiting,
    Connected,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputState {
    Idle,
    Running,
    Stopped,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelStatus {
    Idle,
    Ready,
    Active,
    Disabled,
    Disconnected,
    Fault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSnapshot {
    pub state: ConnectionState,
    pub endpoint: String,
    pub controller_id: Option<String>,
    pub pairing_url: Option<String>,
    pub app_count: usize,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSnapshot {
    pub control_id: String,
    pub id: Value,
    pub name: String,
    #[serde(rename = "type")]
    pub device_type: String,
    pub slot_id: String,
    pub power: u16,
    pub intensity_a: u16,
    pub intensity_b: u16,
    pub intensity_limit_a: u16,
    pub intensity_limit_b: u16,
    pub output_active: bool,
    pub channel_a_status: ChannelStatus,
    pub channel_b_status: ChannelStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSnapshot {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub enabled: bool,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputSnapshot {
    pub state: OutputState,
    pub frames_sent: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelSnapshot {
    pub intensity: u16,
    pub limit: u16,
    pub status: ChannelStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelsSnapshot {
    pub a: ChannelSnapshot,
    pub b: ChannelSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetySnapshot {
    pub channel_limit: u16,
    pub max_duration_minutes: u16,
    pub allow_app_intensity_control: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogSnapshot {
    pub id: String,
    pub level: LogLevel,
    pub message: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSnapshot {
    pub revision: u64,
    pub connection: ConnectionSnapshot,
    pub device: Option<DeviceSnapshot>,
    pub devices: Vec<DeviceSnapshot>,
    pub selected_device_id: Option<String>,
    pub sync_all_devices: bool,
    pub output_device_count: usize,
    pub sources: Vec<SourceSnapshot>,
    pub active_source_id: Option<String>,
    pub output: OutputSnapshot,
    pub channels: ChannelsSnapshot,
    pub safety: SafetySnapshot,
    pub logs: Vec<LogSnapshot>,
}

impl HubSnapshot {
    fn initial(endpoint: String, sources: Vec<SourceSnapshot>) -> Self {
        let active_source_id = sources.first().map(|source| source.id.clone());
        Self {
            revision: 0,
            connection: ConnectionSnapshot {
                state: ConnectionState::Disconnected,
                endpoint,
                controller_id: None,
                pairing_url: None,
                app_count: 0,
                last_error: None,
            },
            device: None,
            devices: Vec::new(),
            selected_device_id: None,
            sync_all_devices: false,
            output_device_count: 0,
            sources,
            active_source_id,
            output: OutputSnapshot {
                state: OutputState::Idle,
                frames_sent: 0,
                last_error: None,
            },
            channels: ChannelsSnapshot {
                a: ChannelSnapshot {
                    intensity: 0,
                    limit: DEFAULT_CHANNEL_LIMIT,
                    status: ChannelStatus::Disconnected,
                },
                b: ChannelSnapshot {
                    intensity: 0,
                    limit: DEFAULT_CHANNEL_LIMIT,
                    status: ChannelStatus::Disconnected,
                },
            },
            safety: SafetySnapshot {
                channel_limit: DEFAULT_CHANNEL_LIMIT,
                max_duration_minutes: DEFAULT_MAX_DURATION_MINUTES,
                allow_app_intensity_control: false,
            },
            logs: Vec::new(),
        }
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
    #[error("强度调整值不能为 0，且必须在 -200..=200 范围内")]
    InvalidDelta,
    #[error("调整后的强度会超过安全上限或低于 0")]
    IntensityLimit,
    #[error("通道安全上限必须在 1..=200 范围内")]
    InvalidChannelLimit,
    #[error("最长输出时间必须在 1..=120 分钟范围内")]
    InvalidMaxDuration,
    #[error("实时输出队列繁忙，请稍后重试")]
    QueueBusy,
    #[error("Relay 操作失败：{0}")]
    Relay(String),
}

impl HubError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Stopped => "hub_stopped",
            Self::NotConnected => "not_connected",
            Self::NoDevice => "no_device",
            Self::DeviceUnavailable => "device_unavailable",
            Self::TooManyDevices => "too_many_devices",
            Self::NoSource => "no_source",
            Self::SourceUnavailable(_) => "source_unavailable",
            Self::InvalidDelta => "invalid_delta",
            Self::IntensityLimit => "intensity_limit",
            Self::InvalidChannelLimit => "invalid_channel_limit",
            Self::InvalidMaxDuration => "invalid_max_duration",
            Self::QueueBusy => "queue_busy",
            Self::Relay(_) => "relay_error",
        }
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
    Connect(oneshot::Sender<Result<(), HubError>>),
    Disconnect(oneshot::Sender<Result<(), HubError>>),
    RefreshPairing(oneshot::Sender<Result<(), HubError>>),
    AdjustIntensity {
        device_id: Option<String>,
        channel: Channel,
        delta: i32,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    StartOutput {
        safety_epoch: u64,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    SetActiveSource {
        source_id: String,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    SelectDevice {
        device_id: String,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    SetSyncAllDevices {
        enabled: bool,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    SetChannelLimit {
        limit: i32,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    UpdateSafety {
        channel_limit: i32,
        max_duration_minutes: i32,
        allow_app_intensity_control: bool,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
}

enum HubSafetyCommand {
    StopOutput(oneshot::Sender<Result<(), HubError>>),
    EmergencyStop(oneshot::Sender<Result<(), HubError>>),
}

#[derive(Clone)]
pub struct HubHandle {
    commands: mpsc::Sender<HubCommand>,
    safety_commands: mpsc::Sender<HubSafetyCommand>,
    snapshots: watch::Receiver<HubSnapshot>,
    shutdown: CancellationToken,
    completion: watch::Receiver<Option<Result<(), HubError>>>,
    safety_epoch: Arc<AtomicU64>,
}

impl HubHandle {
    pub fn snapshot(&self) -> HubSnapshot {
        self.snapshots.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<HubSnapshot> {
        self.snapshots.clone()
    }

    pub async fn connect_relay(&self) -> Result<(), HubError> {
        self.request(HubCommand::Connect).await
    }

    pub async fn disconnect_relay(&self) -> Result<(), HubError> {
        self.request(HubCommand::Disconnect).await
    }

    pub async fn refresh_pairing(&self) -> Result<(), HubError> {
        self.request(HubCommand::RefreshPairing).await
    }

    pub async fn adjust_device_intensity(
        &self,
        device_id: Option<String>,
        channel: Channel,
        delta: i32,
    ) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(HubCommand::AdjustIntensity {
                device_id,
                channel,
                delta,
                reply,
            })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn start_output(&self) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        let safety_epoch = self.safety_epoch.load(Ordering::Acquire);
        self.commands
            .send(HubCommand::StartOutput {
                safety_epoch,
                reply,
            })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn stop_output(&self) -> Result<(), HubError> {
        self.safety_epoch.fetch_add(1, Ordering::AcqRel);
        self.safety_request(HubSafetyCommand::StopOutput).await
    }

    pub async fn emergency_stop(&self) -> Result<(), HubError> {
        self.safety_epoch.fetch_add(1, Ordering::AcqRel);
        self.safety_request(HubSafetyCommand::EmergencyStop).await
    }

    pub async fn set_active_source(&self, source_id: String) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(HubCommand::SetActiveSource { source_id, reply })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn select_device(&self, device_id: String) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(HubCommand::SelectDevice { device_id, reply })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn set_sync_all_devices(&self, enabled: bool) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(HubCommand::SetSyncAllDevices { enabled, reply })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn set_channel_limit(&self, limit: i32) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(HubCommand::SetChannelLimit { limit, reply })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn update_safety(
        &self,
        channel_limit: i32,
        max_duration_minutes: i32,
        allow_app_intensity_control: bool,
    ) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(HubCommand::UpdateSafety {
                channel_limit,
                max_duration_minutes,
                allow_app_intensity_control,
                reply,
            })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub fn shutdown_now(&self) {
        self.shutdown.cancel();
    }

    pub async fn shutdown_gracefully(&self) -> Result<(), HubError> {
        self.shutdown.cancel();
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
        response.await.map_err(|_| HubError::Stopped)?
    }
}

struct SourceRuntime {
    snapshot: SourceSnapshot,
    source: Box<dyn WaveSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DeviceKey {
    client_id: String,
    slot_id: String,
}

impl DeviceKey {
    fn control_id(&self) -> String {
        format!(
            "{}:{}{}",
            self.client_id.len(),
            self.client_id,
            self.slot_id
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct IntensityKey {
    device: DeviceKey,
    channel: Channel,
}

#[derive(Debug, Clone)]
struct PendingWaveOperation {
    device: DeviceKey,
    generation: u64,
    sent_at: Instant,
}

#[derive(Debug, Clone)]
struct PendingIntensityOperation {
    request_id: String,
    projected: u16,
    lock_correction: bool,
}

#[derive(Debug, Clone)]
struct IntensityLockTarget {
    a: u16,
    b: u16,
}

pub struct HubRuntime {
    commands: mpsc::Receiver<HubCommand>,
    safety_commands: mpsc::Receiver<HubSafetyCommand>,
    snapshot_sender: watch::Sender<HubSnapshot>,
    completion_sender: watch::Sender<Option<Result<(), HubError>>>,
    shutdown: CancellationToken,
    snapshot: HubSnapshot,
    sources: BTreeMap<String, SourceRuntime>,
    relay: Option<RelayClientHandle>,
    apps: BTreeSet<String>,
    devices: BTreeMap<DeviceKey, Value>,
    selected_device: Option<DeviceKey>,
    output_devices: BTreeSet<DeviceKey>,
    pending_wave_operations: HashMap<String, PendingWaveOperation>,
    pending_intensity_operations: BTreeMap<IntensityKey, PendingIntensityOperation>,
    pending_intensity_requests: HashMap<String, IntensityKey>,
    pending_intensity_refreshes: HashMap<String, IntensityKey>,
    intensity_lock_targets: BTreeMap<DeviceKey, IntensityLockTarget>,
    operation_generation: u64,
    safety_epoch: Arc<AtomicU64>,
    output_started_at: Option<Instant>,
    reconnect_at: Option<Instant>,
    reconnect_attempt: u32,
    auto_reconnect_enabled: bool,
}

pub fn create_hub(endpoint: String) -> (HubHandle, HubRuntime) {
    let registry = builtin_registry();
    let mut sources = BTreeMap::new();
    let mut source_snapshots = Vec::new();

    for descriptor in registry.list_descriptors() {
        let id = match descriptor.kind {
            "builtin.test_pattern" => "source-test-pattern".to_owned(),
            "builtin.manual" => "source-manual".to_owned(),
            kind => format!("source-{kind}"),
        };
        let source = registry
            .default_config(descriptor.kind)
            .and_then(|config| registry.build(descriptor.kind, &config))
            .expect("内置输入源默认配置必须有效");
        let snapshot = SourceSnapshot {
            id: id.clone(),
            kind: descriptor.kind.to_owned(),
            name: descriptor.display_name.to_owned(),
            enabled: true,
            active: sources.is_empty(),
        };
        source_snapshots.push(snapshot.clone());
        sources.insert(id, SourceRuntime { snapshot, source });
    }

    let snapshot = HubSnapshot::initial(endpoint, source_snapshots);
    let (snapshot_sender, snapshot_receiver) = watch::channel(snapshot.clone());
    let (command_sender, command_receiver) = mpsc::channel(HUB_COMMAND_CAPACITY);
    let (safety_sender, safety_receiver) = mpsc::channel(HUB_SAFETY_COMMAND_CAPACITY);
    let shutdown = CancellationToken::new();
    let safety_epoch = Arc::new(AtomicU64::new(0));
    let (completion_sender, completion_receiver) = watch::channel(None);
    (
        HubHandle {
            commands: command_sender,
            safety_commands: safety_sender,
            snapshots: snapshot_receiver,
            shutdown: shutdown.clone(),
            completion: completion_receiver,
            safety_epoch: Arc::clone(&safety_epoch),
        },
        HubRuntime {
            commands: command_receiver,
            safety_commands: safety_receiver,
            snapshot_sender,
            completion_sender,
            shutdown,
            snapshot,
            sources,
            relay: None,
            apps: BTreeSet::new(),
            devices: BTreeMap::new(),
            selected_device: None,
            output_devices: BTreeSet::new(),
            pending_wave_operations: HashMap::new(),
            pending_intensity_operations: BTreeMap::new(),
            pending_intensity_requests: HashMap::new(),
            pending_intensity_refreshes: HashMap::new(),
            intensity_lock_targets: BTreeMap::new(),
            operation_generation: 0,
            safety_epoch,
            output_started_at: None,
            reconnect_at: None,
            reconnect_attempt: 0,
            auto_reconnect_enabled: true,
        },
    )
}

impl HubRuntime {
    pub async fn run(mut self) {
        let (event_sender, mut events) = mpsc::channel(RELAY_EVENT_CAPACITY);
        let (relay, mut relay_task) = spawn_relay_client(event_sender, RELAY_COMMAND_CAPACITY);
        self.relay = Some(relay);
        self.begin_connect();

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
                event = events.recv(), if events_open => {
                    match event {
                        Some(event) => self.handle_relay_event(event).await,
                        None => events_open = false,
                    }
                }
                _ = ticker.tick() => {
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
        self.output_started_at = None;
        let mut shutdown_result = self.send_stop_operations(true).await;
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
        let join_result = match tokio::time::timeout(RELAY_JOIN_TIMEOUT, &mut relay_task).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(HubError::Relay(format!("Relay 任务异常结束：{error}"))),
            Err(_) => {
                relay_task.abort();
                let _ = relay_task.await;
                Err(HubError::Relay("Relay 任务退出超时".to_owned()))
            }
        };
        if shutdown_result.is_ok() {
            shutdown_result = join_result;
        }
        let _ = self.completion_sender.send(Some(shutdown_result));
    }

    fn begin_connect(&mut self) {
        if self.snapshot.output.state == OutputState::Running {
            self.snapshot.output.state = OutputState::Idle;
            self.snapshot.output.last_error = None;
        }
        self.reset_connection_state(ConnectionState::Connecting);
        self.log(LogLevel::Info, "正在连接 DG-LAB Relay");
        self.publish();
        if let Some(relay) = self.relay.clone() {
            let endpoint = self.snapshot.connection.endpoint.clone();
            tokio::spawn(async move {
                let _ = relay.connect(endpoint).await;
            });
        }
    }

    async fn handle_command(&mut self, command: HubCommand) {
        match command {
            HubCommand::Connect(reply) => {
                self.enable_auto_reconnect();
                if !matches!(
                    self.snapshot.connection.state,
                    ConnectionState::Connecting
                        | ConnectionState::Waiting
                        | ConnectionState::Connected
                ) {
                    self.begin_connect();
                }
                let _ = reply.send(Ok(()));
            }
            HubCommand::Disconnect(reply) => {
                self.disable_auto_reconnect();
                self.snapshot.output.state = OutputState::Idle;
                self.output_started_at = None;
                let stop_result = self.send_stop_operations(false).await;
                let disconnect_result = if let Some(relay) = &self.relay {
                    tokio::time::timeout(RELAY_DISCONNECT_TIMEOUT, relay.disconnect())
                        .await
                        .map_err(|_| HubError::Relay("断开 Relay 超时".to_owned()))
                        .and_then(|result| result.map_err(Into::into))
                } else {
                    Err(HubError::Stopped)
                };
                let result = stop_result.and(disconnect_result);
                if result.is_ok() {
                    self.reset_connection_state(ConnectionState::Disconnected);
                    self.snapshot.output.state = OutputState::Idle;
                    self.log(LogLevel::Warning, "已断开 DG-LAB Relay");
                    self.publish();
                } else if let Err(error) = &result {
                    self.snapshot.connection.state = ConnectionState::Error;
                    self.snapshot.connection.last_error = Some(error.to_string());
                    self.snapshot.output.state = OutputState::Error;
                    self.snapshot.output.last_error = Some(error.to_string());
                    self.refresh_channel_statuses();
                    self.log(
                        LogLevel::Error,
                        format!("断开 Relay 时安全清理失败：{error}"),
                    );
                    self.publish();
                }
                let _ = reply.send(result);
            }
            HubCommand::RefreshPairing(reply) => {
                self.enable_auto_reconnect();
                self.snapshot.output.state = OutputState::Idle;
                self.output_started_at = None;
                let result = self.send_stop_operations(false).await;
                if result.is_ok() {
                    self.begin_connect();
                } else if let Err(error) = &result {
                    self.snapshot.output.state = OutputState::Error;
                    self.snapshot.output.last_error = Some(error.to_string());
                    self.refresh_channel_statuses();
                    self.log(LogLevel::Error, format!("刷新配对前清理设备失败：{error}"));
                    self.publish();
                }
                let _ = reply.send(result);
            }
            HubCommand::AdjustIntensity {
                device_id,
                channel,
                delta,
                reply,
            } => {
                let result = self.adjust_device_intensity(device_id.as_deref(), channel, delta);
                let _ = reply.send(result);
            }
            HubCommand::StartOutput {
                safety_epoch,
                reply,
            } => {
                let result = if safety_epoch == self.safety_epoch.load(Ordering::Acquire) {
                    self.start_output()
                } else {
                    Err(HubError::QueueBusy)
                };
                let _ = reply.send(result);
            }
            HubCommand::SetActiveSource { source_id, reply } => {
                let result = self.set_active_source(source_id).await;
                let _ = reply.send(result);
            }
            HubCommand::SelectDevice { device_id, reply } => {
                let result = self.select_device(device_id).await;
                let _ = reply.send(result);
            }
            HubCommand::SetSyncAllDevices { enabled, reply } => {
                let result = self.set_sync_all_devices(enabled);
                let _ = reply.send(result);
            }
            HubCommand::SetChannelLimit { limit, reply } => {
                let result = self.set_channel_limit(limit).await;
                let _ = reply.send(result);
            }
            HubCommand::UpdateSafety {
                channel_limit,
                max_duration_minutes,
                allow_app_intensity_control,
                reply,
            } => {
                let result = self
                    .update_safety(
                        channel_limit,
                        max_duration_minutes,
                        allow_app_intensity_control,
                    )
                    .await;
                let _ = reply.send(result);
            }
        }
    }

    async fn handle_safety_command(&mut self, command: HubSafetyCommand) {
        match command {
            HubSafetyCommand::StopOutput(reply) => {
                let result = self
                    .stop_output(
                        false,
                        OutputState::Idle,
                        LogLevel::Info,
                        "波形输出已停止并清空设备任务",
                    )
                    .await;
                let _ = reply.send(result);
            }
            HubSafetyCommand::EmergencyStop(reply) => {
                let result = self
                    .stop_output(
                        true,
                        OutputState::Stopped,
                        LogLevel::Warning,
                        "已紧急停止、清空任务并将 A/B 强度归零",
                    )
                    .await;
                if result.is_ok() {
                    self.set_all_intensity_lock_targets(0, 0);
                    self.publish();
                }
                let _ = reply.send(result);
            }
        }
    }

    async fn handle_relay_event(&mut self, event: RelayEvent) {
        match event {
            RelayEvent::Connecting { endpoint } => {
                self.snapshot.connection.endpoint = endpoint;
                self.snapshot.connection.state = ConnectionState::Connecting;
                self.snapshot.connection.last_error = None;
                self.publish();
            }
            RelayEvent::Connected { .. } => {
                self.snapshot.connection.state = ConnectionState::Waiting;
                self.snapshot.connection.last_error = None;
                self.log(LogLevel::Info, "Relay 已连接，正在等待握手");
                self.publish();
            }
            RelayEvent::Hello { controller_id } => {
                self.reconnect_at = None;
                self.reconnect_attempt = 0;
                self.snapshot.connection.controller_id = Some(controller_id.clone());
                self.snapshot.connection.pairing_url =
                    pairing_url(&self.snapshot.connection.endpoint, &controller_id).ok();
                self.snapshot.connection.state = if self.apps.is_empty() {
                    ConnectionState::Waiting
                } else {
                    ConnectionState::Connected
                };
                self.log(LogLevel::Info, "已生成新的 APP 配对二维码");
                self.publish();
            }
            RelayEvent::ClientAttached { client_id } => {
                let first = self.apps.insert(client_id.clone());
                self.snapshot.connection.app_count = self.apps.len();
                self.snapshot.connection.state = ConnectionState::Connected;
                self.snapshot.connection.last_error = None;
                if first {
                    self.log(LogLevel::Info, format!("DG-LAB APP 已连接：{client_id}"));
                    let request = devices_get_request();
                    let _ = self.send_to_app(&client_id, request);
                }
                self.refresh_channel_statuses();
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
                self.snapshot.connection.state = ConnectionState::Error;
                self.snapshot.connection.last_error =
                    Some("Relay 因长时间无 APP 接入而断开".to_owned());
                self.log(
                    LogLevel::Warning,
                    "Relay 配对等待已超时，可点击重新连接生成新二维码",
                );
                self.publish();
            }
            RelayEvent::RelayError { code, message } => {
                let text = message.unwrap_or_else(|| code.clone());
                self.snapshot.connection.last_error = Some(text.clone());
                self.log(LogLevel::Error, format!("Relay 返回错误 {code}：{text}"));
                self.publish();
            }
            RelayEvent::Disconnected { reason, retryable } => {
                self.reset_connection_state(ConnectionState::Disconnected);
                if self.snapshot.output.state == OutputState::Running {
                    self.snapshot.output.state = OutputState::Error;
                    self.snapshot.output.last_error = Some("Relay 断开，输出已停止".to_owned());
                }
                if retryable && self.auto_reconnect_enabled {
                    let delay = self.schedule_reconnect();
                    let message = format!("{reason}，将在 {} 秒后自动重连", delay.as_secs());
                    self.snapshot.connection.last_error = Some(message.clone());
                    self.log(LogLevel::Warning, message);
                } else {
                    self.snapshot.connection.last_error = Some(reason.clone());
                    self.log(LogLevel::Warning, reason);
                }
                self.publish();
            }
            RelayEvent::Heartbeat | RelayEvent::Pong { .. } | RelayEvent::Unknown(_) => {}
        }
    }

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

            if let Some(request_id) = request_id {
                if let Some(pending) = self.pending_wave_operations.remove(request_id) {
                    if pending.device.client_id != client_id {
                        self.pending_wave_operations
                            .insert(request_id.to_owned(), pending);
                    } else if error.is_some()
                        && pending.generation == self.operation_generation
                        && self.snapshot.output.state == OutputState::Running
                    {
                        wave_failure = error.clone();
                    }
                }

                if let Some(key) = self.pending_intensity_requests.remove(request_id) {
                    if key.device.client_id == client_id {
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
                        } else if self.pending_intensity_operations.contains_key(&key) {
                            let refresh_id = Uuid::new_v4().to_string();
                            match self
                                .send_to_app(client_id, devices_get_request_with_id(&refresh_id))
                            {
                                Ok(()) => {
                                    self.pending_intensity_refreshes.insert(refresh_id, key);
                                }
                                Err(refresh_error) => {
                                    self.log(
                                        LogLevel::Error,
                                        format!(
                                            "强度调整后刷新设备状态失败，通道将保持锁定：{refresh_error}"
                                        ),
                                    );
                                    self.publish();
                                }
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
                if let Some(request_id) = request_id
                    && let Some(key) = self.pending_intensity_refreshes.remove(request_id)
                {
                    self.remove_pending_intensity(&key);
                    self.reconcile_intensity_lock();
                }
            }

            if let Some(error) = wave_failure {
                self.fail_output(format!("APP 拒绝波形操作：{error}")).await;
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
        let previous = self
            .devices
            .keys()
            .filter(|key| key.client_id == client_id)
            .cloned()
            .collect::<BTreeSet<_>>();
        self.devices
            .retain(|key, _| key.client_id.as_str() != client_id);
        for device in devices {
            if let Some(slot_id) = device.get("slotId").and_then(Value::as_str) {
                self.devices.insert(
                    DeviceKey {
                        client_id: client_id.to_owned(),
                        slot_id: slot_id.to_owned(),
                    },
                    device.clone(),
                );
            }
        }
        let current = self
            .devices
            .keys()
            .filter(|key| key.client_id == client_id)
            .cloned()
            .collect::<BTreeSet<_>>();
        for removed in previous.difference(&current) {
            self.clear_pending_for_device(removed);
        }
        self.reconcile_connected_devices();
        self.refresh_selected_device_snapshot();
        self.reconcile_intensity_lock();
        self.publish();
    }

    fn apply_devices_patch(&mut self, client_id: &str, data: &Value) {
        if let Some(removed) = data.get("removed").and_then(Value::as_array) {
            for slot_id in removed.iter().filter_map(Value::as_str) {
                let key = DeviceKey {
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
                            client_id: client_id.to_owned(),
                            slot_id: slot_id.to_owned(),
                        },
                        device.clone(),
                    );
                }
            }
        }
        self.reconcile_connected_devices();
        self.refresh_selected_device_snapshot();
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
                        client_id: client_id.to_owned(),
                        slot_id: slot_id.to_owned(),
                    };
                    if let Some(device) = self.devices.get_mut(&key) {
                        deep_merge(device, patch);
                    }
                }
            }
        }
        self.refresh_selected_device_snapshot();
        self.reconcile_intensity_lock();
        self.publish();
    }

    fn remove_app(&mut self, client_id: &str) {
        self.apps.remove(client_id);
        self.devices
            .retain(|key, _| key.client_id.as_str() != client_id);
        self.clear_pending_for_client(client_id);
        self.snapshot.connection.app_count = self.apps.len();
        self.snapshot.connection.state = if self.apps.is_empty() {
            ConnectionState::Waiting
        } else {
            ConnectionState::Connected
        };
        self.reconcile_connected_devices();
        self.refresh_selected_device_snapshot();
        self.reconcile_intensity_lock();
    }

    fn reconcile_connected_devices(&mut self) {
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
        if self.snapshot.output.state == OutputState::Running {
            for device in self.devices.keys() {
                if self.output_devices.len() >= MAX_OUTPUT_DEVICES {
                    break;
                }
                self.output_devices.insert(device.clone());
            }
            if self.devices.len() > MAX_OUTPUT_DEVICES {
                self.snapshot.output.last_error = Some(format!(
                    "在线设备超过 {MAX_OUTPUT_DEVICES} 台，新增设备未加入输出"
                ));
            }
            if self.output_devices.is_empty() {
                self.snapshot.output.state = OutputState::Error;
                self.snapshot.output.last_error = Some("所有输出设备均已断开".to_owned());
                self.output_started_at = None;
                self.safety_epoch.fetch_add(1, Ordering::AcqRel);
                self.advance_operation_generation();
                self.log(LogLevel::Error, "所有输出设备均已断开，输出已停止");
            }
        }
        if self
            .selected_device
            .as_ref()
            .is_some_and(|key| self.devices.contains_key(key))
        {
            return;
        }
        self.selected_device = self.devices.keys().next().cloned();
    }

    fn refresh_selected_device_snapshot(&mut self) {
        let allow_app_control = self.snapshot.safety.allow_app_intensity_control;
        let selected_id = self.selected_device.as_ref().map(DeviceKey::control_id);
        let devices = self
            .devices
            .iter()
            .filter_map(|(key, value)| {
                let mut snapshot = device_snapshot_from_value(key, value)?;
                if !allow_app_control && let Some(lock) = self.intensity_lock_targets.get(key) {
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
                Some(snapshot)
            })
            .collect::<Vec<_>>();
        self.snapshot.device = selected_id.as_ref().and_then(|selected_id| {
            devices
                .iter()
                .find(|device| &device.control_id == selected_id)
                .cloned()
        });
        self.snapshot.devices = devices;
        self.snapshot.selected_device_id = selected_id;
        self.snapshot.output_device_count = self.output_devices.len();
        if let Some(device) = &self.snapshot.device {
            self.snapshot.channels.a.intensity = device.intensity_a;
            self.snapshot.channels.b.intensity = device.intensity_b;
            self.snapshot.channels.a.limit = effective_limit(
                self.snapshot.safety.channel_limit,
                selected_device_limit(self.selected_device.as_ref(), &self.devices, Channel::A),
            );
            self.snapshot.channels.b.limit = effective_limit(
                self.snapshot.safety.channel_limit,
                selected_device_limit(self.selected_device.as_ref(), &self.devices, Channel::B),
            );
        } else {
            self.snapshot.channels.a.intensity = 0;
            self.snapshot.channels.b.intensity = 0;
            self.snapshot.channels.a.limit = self.snapshot.safety.channel_limit;
            self.snapshot.channels.b.limit = self.snapshot.safety.channel_limit;
        }
        self.refresh_channel_statuses();
    }

    fn refresh_channel_statuses(&mut self) {
        if self.snapshot.device.is_none() {
            self.snapshot.channels.a.status = ChannelStatus::Disconnected;
            self.snapshot.channels.b.status = ChannelStatus::Disconnected;
            return;
        }
        if let Some(device) = &self.snapshot.device {
            let running = self.snapshot.output.state == OutputState::Running
                && self
                    .selected_device
                    .as_ref()
                    .is_some_and(|selected| self.output_devices.contains(selected));
            self.snapshot.channels.a.status = running_status(device.channel_a_status, running);
            self.snapshot.channels.b.status = running_status(device.channel_b_status, running);
        }
    }

    fn start_output(&mut self) -> Result<(), HubError> {
        if self.apps.is_empty() {
            return Err(HubError::NotConnected);
        }
        if self.devices.is_empty() {
            return Err(HubError::NoDevice);
        }
        if self.devices.len() > MAX_OUTPUT_DEVICES {
            return Err(HubError::TooManyDevices);
        }
        let source_id = self
            .snapshot
            .active_source_id
            .as_ref()
            .ok_or(HubError::NoSource)?;
        if !self.sources.contains_key(source_id) {
            return Err(HubError::NoSource);
        }
        self.snapshot.output.state = OutputState::Running;
        self.snapshot.output.last_error = None;
        self.output_devices = self.devices.keys().cloned().collect();
        self.output_started_at = Some(Instant::now());
        self.refresh_selected_device_snapshot();
        self.log(
            LogLevel::Info,
            format!("波形输出已开始，共 {} 台设备", self.output_devices.len()),
        );
        self.publish();
        Ok(())
    }

    async fn output_tick(&mut self) {
        if self.snapshot.output.state != OutputState::Running {
            return;
        }
        if self.output_duration_expired() {
            self.snapshot.output.state = OutputState::Stopped;
            self.snapshot.output.last_error = None;
            self.output_started_at = None;
            let stop_result = self.send_stop_operations(false).await;
            if let Err(error) = stop_result {
                self.snapshot.output.state = OutputState::Error;
                self.snapshot.output.last_error = Some(error.to_string());
                self.log(
                    LogLevel::Error,
                    format!("达到最长输出时间，但清空设备任务失败：{error}"),
                );
            } else {
                self.log(LogLevel::Warning, "已达到最长输出时间，设备任务已自动清空");
            }
            self.refresh_channel_statuses();
            self.publish();
            return;
        }
        if self
            .pending_wave_operations
            .values()
            .any(|pending| pending.sent_at.elapsed() >= WAVE_OPERATION_RESPONSE_TIMEOUT)
        {
            self.fail_output("设备波形响应超时，输出已停止").await;
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
        let Some(source_id) = self.snapshot.active_source_id.clone() else {
            self.fail_output("活动输入源已丢失").await;
            return;
        };
        let frame = match self.sources.get_mut(&source_id) {
            Some(source) if source.snapshot.enabled => match source.source.next_frame() {
                Ok(frame) => frame,
                Err(error) => {
                    self.fail_output(format!("输入源运行失败：{error}")).await;
                    return;
                }
            },
            _ => {
                self.fail_output("活动输入源不可用").await;
                return;
            }
        };
        let devices = self.output_devices.iter().cloned().collect::<Vec<_>>();
        if devices.is_empty() {
            self.fail_output("所有输出设备均已断开").await;
            return;
        }
        let frame_hex = encode_wave_frame(frame);
        let mut sent = 0_u64;
        for device in devices {
            for channel in Channel::ALL {
                let request_id = Uuid::new_v4().to_string();
                let request =
                    append_pulse_request(&request_id, &device.slot_id, channel, &frame_hex);
                match self.send_operation(&device.client_id, request) {
                    Ok(()) => {
                        self.pending_wave_operations.insert(
                            request_id,
                            PendingWaveOperation {
                                device: device.clone(),
                                generation: self.operation_generation,
                                sent_at: Instant::now(),
                            },
                        );
                        sent += 1;
                    }
                    Err(error) => {
                        self.fail_output(error.to_string()).await;
                        return;
                    }
                }
            }
        }
        self.snapshot.output.frames_sent = self.snapshot.output.frames_sent.saturating_add(sent);
        self.publish();
    }

    async fn fail_output(&mut self, message: impl Into<String>) {
        let primary_message = message.into();
        self.snapshot.output.state = OutputState::Error;
        self.snapshot.output.last_error = Some(primary_message.clone());
        self.output_started_at = None;
        self.refresh_channel_statuses();
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
        if self.apps.is_empty() {
            return Err(HubError::NotConnected);
        }
        let selected = match device_id {
            Some(device_id) => self
                .devices
                .keys()
                .find(|device| device.control_id() == device_id)
                .cloned()
                .ok_or(HubError::DeviceUnavailable)?,
            None => self.selected_device.clone().ok_or(HubError::NoDevice)?,
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
        self.refresh_selected_device_snapshot();
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
        if !self.snapshot.safety.allow_app_intensity_control
            && let Some(lock) = self.intensity_lock_targets.get(device)
        {
            snapshot.intensity_a = lock.a;
            snapshot.intensity_b = lock.b;
        }
        let (current, device_limit) = match channel {
            Channel::A => (snapshot.intensity_a, snapshot.intensity_limit_a),
            Channel::B => (snapshot.intensity_b, snapshot.intensity_limit_b),
        };
        Some((
            current,
            effective_limit(self.snapshot.safety.channel_limit, Some(device_limit)),
        ))
    }

    fn queue_intensity_adjustment(
        &mut self,
        device: DeviceKey,
        channel: Channel,
        current: u16,
        target: u16,
        lock_correction: bool,
    ) -> Result<(), HubError> {
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
        self.send_operation(
            &device.client_id,
            add_intensity_request(&request_id, &device.slot_id, channel, delta),
        )?;
        self.pending_intensity_requests
            .insert(request_id.clone(), key.clone());
        self.pending_intensity_operations.insert(
            key,
            PendingIntensityOperation {
                request_id,
                projected: target,
                lock_correction,
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

    async fn set_active_source(&mut self, source_id: String) -> Result<(), HubError> {
        let source = self
            .sources
            .get(&source_id)
            .ok_or_else(|| HubError::SourceUnavailable(source_id.clone()))?;
        if !source.snapshot.enabled {
            return Err(HubError::SourceUnavailable(source_id));
        }
        if self.snapshot.active_source_id.as_deref() == Some(source_id.as_str()) {
            return Ok(());
        }
        if self.snapshot.output.state == OutputState::Running {
            self.snapshot.output.state = OutputState::Idle;
            self.output_started_at = None;
            if let Err(error) = self.send_stop_operations(false).await {
                self.snapshot.output.state = OutputState::Error;
                self.snapshot.output.last_error = Some(error.to_string());
                self.refresh_channel_statuses();
                self.log(
                    LogLevel::Error,
                    format!("切换输入源前清空设备任务失败：{error}"),
                );
                self.publish();
                return Err(error);
            }
        }
        self.snapshot.active_source_id = Some(source_id.clone());
        for (id, source) in &mut self.sources {
            source.snapshot.active = id == &source_id;
        }
        self.snapshot.sources = self
            .sources
            .values()
            .map(|source| source.snapshot.clone())
            .collect();
        self.log(LogLevel::Info, format!("已切换活动输入源：{source_id}"));
        self.publish();
        Ok(())
    }

    async fn select_device(&mut self, device_id: String) -> Result<(), HubError> {
        let device = self
            .devices
            .keys()
            .find(|device| device.control_id() == device_id)
            .cloned()
            .ok_or(HubError::DeviceUnavailable)?;
        if self.selected_device.as_ref() == Some(&device) {
            return Ok(());
        }
        self.selected_device = Some(device.clone());
        self.refresh_selected_device_snapshot();
        self.log(
            LogLevel::Info,
            format!("已切换当前控制设备：{}", device.control_id()),
        );
        self.publish();
        Ok(())
    }

    fn set_sync_all_devices(&mut self, enabled: bool) -> Result<(), HubError> {
        if enabled && self.devices.is_empty() {
            return Err(HubError::NoDevice);
        }
        if enabled && self.devices.len() > MAX_OUTPUT_DEVICES {
            return Err(HubError::TooManyDevices);
        }
        if self.snapshot.sync_all_devices == enabled {
            return Ok(());
        }
        if enabled {
            let selected = self.selected_device.clone().ok_or(HubError::NoDevice)?;
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
        }
        self.snapshot.sync_all_devices = enabled;
        self.refresh_selected_device_snapshot();
        self.log(
            LogLevel::Info,
            if enabled {
                "已开启所有设备强度同步控制，并向当前控制设备的 A/B 强度对齐"
            } else {
                "已关闭所有设备强度同步控制"
            },
        );
        self.publish();
        Ok(())
    }

    async fn set_channel_limit(&mut self, limit: i32) -> Result<(), HubError> {
        if !(1..=200).contains(&limit) {
            return Err(HubError::InvalidChannelLimit);
        }
        let limit = limit as u16;
        let mut resets = Vec::new();
        for (device, value) in &self.devices {
            let Some(snapshot) = device_snapshot_from_value(device, value) else {
                continue;
            };
            let mut reset_requests = Vec::new();
            let mut reset_channels = Vec::new();
            for channel in Channel::ALL {
                let device_limit = match channel {
                    Channel::A => snapshot.intensity_limit_a,
                    Channel::B => snapshot.intensity_limit_b,
                };
                let channel_limit = effective_limit(limit, Some(device_limit));
                let current = match channel {
                    Channel::A => snapshot.intensity_a,
                    Channel::B => snapshot.intensity_b,
                };
                let key = IntensityKey {
                    device: device.clone(),
                    channel,
                };
                let projected = self
                    .pending_intensity_operations
                    .get(&key)
                    .map_or(current, |pending| current.max(pending.projected));
                if projected > channel_limit {
                    reset_requests.push(zero_intensity_request(&device.slot_id, channel));
                    reset_channels.push(channel);
                }
            }
            if !reset_requests.is_empty() {
                resets.push((device.clone(), reset_requests, reset_channels));
            }
        }

        for (device, requests, channels) in resets {
            self.send_safety_requests(&device, requests).await?;
            if let Some(lock) = self.intensity_lock_targets.get_mut(&device) {
                for channel in channels {
                    match channel {
                        Channel::A => lock.a = 0,
                        Channel::B => lock.b = 0,
                    }
                }
            }
        }

        self.snapshot.safety.channel_limit = limit;
        self.refresh_selected_device_snapshot();
        self.log(LogLevel::Info, format!("通道安全上限已更新为 {limit}"));
        self.publish();
        Ok(())
    }

    async fn update_safety(
        &mut self,
        channel_limit: i32,
        max_duration_minutes: i32,
        allow_app_intensity_control: bool,
    ) -> Result<(), HubError> {
        if !(1..=120).contains(&max_duration_minutes) {
            return Err(HubError::InvalidMaxDuration);
        }
        self.set_channel_limit(channel_limit).await?;
        self.snapshot.safety.max_duration_minutes = max_duration_minutes as u16;
        self.set_allow_app_intensity_control(allow_app_intensity_control);
        self.log(
            LogLevel::Info,
            format!("最长输出时间已更新为 {max_duration_minutes} 分钟"),
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
        self.refresh_selected_device_snapshot();
        self.publish();
    }

    #[cfg(test)]
    fn set_intensity_lock_target(&mut self, a: u16, b: u16) {
        if self.snapshot.safety.allow_app_intensity_control {
            return;
        }
        if let Some(device) = self.selected_device.clone() {
            self.intensity_lock_targets
                .insert(device, IntensityLockTarget { a, b });
            self.refresh_selected_device_snapshot();
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
        self.refresh_selected_device_snapshot();
    }

    fn reconcile_intensity_lock(&mut self) {
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
        if !self.snapshot.sync_all_devices {
            return;
        }
        let Some(selected) = self.selected_device.clone() else {
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
        self.refresh_selected_device_snapshot();
    }

    fn sync_intensity_lock_targets(&mut self) {
        if self.snapshot.safety.allow_app_intensity_control {
            self.intensity_lock_targets.clear();
            return;
        }
        self.intensity_lock_targets
            .retain(|device, _| self.devices.contains_key(device));
        for (device, value) in &self.devices {
            let Some(snapshot) = device_snapshot_from_value(device, value) else {
                continue;
            };
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

    fn output_duration_expired(&self) -> bool {
        self.output_started_at.is_some_and(|started_at| {
            started_at.elapsed()
                >= Duration::from_secs(u64::from(self.snapshot.safety.max_duration_minutes) * 60)
        })
    }

    async fn stop_output(
        &mut self,
        emergency: bool,
        success_state: OutputState,
        success_level: LogLevel,
        success_message: &str,
    ) -> Result<(), HubError> {
        self.snapshot.output.state = success_state;
        self.snapshot.output.last_error = None;
        self.output_started_at = None;
        self.refresh_channel_statuses();
        self.publish();

        match self.send_stop_operations(emergency).await {
            Ok(()) => {
                self.log(success_level, success_message);
                self.publish();
                Ok(())
            }
            Err(error) => {
                self.snapshot.output.state = OutputState::Error;
                self.snapshot.output.last_error = Some(error.to_string());
                self.refresh_channel_statuses();
                self.log(LogLevel::Error, format!("设备安全停止失败：{error}"));
                self.publish();
                Err(error)
            }
        }
    }

    async fn send_stop_operations(&mut self, emergency: bool) -> Result<(), HubError> {
        let generation = self.advance_operation_generation();
        let devices = if emergency {
            self.devices.keys().cloned().collect::<Vec<_>>()
        } else {
            self.output_devices.iter().cloned().collect::<Vec<_>>()
        };
        if devices.is_empty() {
            return Ok(());
        }
        let relay = self.relay.as_ref().cloned().ok_or(HubError::Stopped)?;
        let mut first_error = None;
        for device in devices {
            let requests = stop_operation_requests(&device.slot_id, emergency);
            if let Err(error) = relay
                .safety_stop(device.client_id, requests, generation)
                .await
            {
                first_error.get_or_insert_with(|| HubError::from(error));
            }
        }
        if let Some(error) = first_error {
            Err(error)
        } else {
            self.output_devices.clear();
            self.refresh_selected_device_snapshot();
            Ok(())
        }
    }

    async fn send_safety_requests(
        &mut self,
        device: &DeviceKey,
        requests: Vec<Value>,
    ) -> Result<(), HubError> {
        let generation = self.advance_operation_generation();
        self.relay
            .as_ref()
            .ok_or(HubError::Stopped)?
            .safety_stop(device.client_id.clone(), requests, generation)
            .await
            .map_err(Into::into)
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
        self.operation_generation
    }

    fn clear_pending_intensities(&mut self) {
        self.pending_intensity_operations.clear();
        self.pending_intensity_requests.clear();
        self.pending_intensity_refreshes.clear();
    }

    fn clear_pending_for_client(&mut self, client_id: &str) {
        self.clear_pending_intensities_for_client(client_id);
        self.pending_wave_operations
            .retain(|_, pending| pending.device.client_id != client_id);
    }

    fn clear_pending_intensities_for_client(&mut self, client_id: &str) {
        let keys = self
            .pending_intensity_operations
            .keys()
            .filter(|key| key.device.client_id == client_id)
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

    fn remove_pending_intensity(&mut self, key: &IntensityKey) {
        if let Some(pending) = self.pending_intensity_operations.remove(key) {
            self.pending_intensity_requests.remove(&pending.request_id);
        }
        self.pending_intensity_refreshes
            .retain(|_, pending_key| pending_key != key);
    }

    fn send_operation(&self, client_id: &str, data: Value) -> Result<(), HubError> {
        self.relay
            .as_ref()
            .ok_or(HubError::Stopped)?
            .try_send_operation(client_id.to_owned(), data, self.operation_generation)
            .map_err(Into::into)
    }

    fn send_to_app(&self, client_id: &str, data: Value) -> Result<(), HubError> {
        self.relay
            .as_ref()
            .ok_or(HubError::Stopped)?
            .try_send_message(client_id.to_owned(), data)
            .map_err(Into::into)
    }

    fn reset_connection_state(&mut self, state: ConnectionState) {
        self.safety_epoch.fetch_add(1, Ordering::AcqRel);
        self.apps.clear();
        self.devices.clear();
        self.selected_device = None;
        self.output_devices.clear();
        self.intensity_lock_targets.clear();
        self.advance_operation_generation();
        self.output_started_at = None;
        self.snapshot.connection.state = state;
        self.snapshot.connection.controller_id = None;
        self.snapshot.connection.pairing_url = None;
        self.snapshot.connection.app_count = 0;
        self.snapshot.device = None;
        self.snapshot.devices.clear();
        self.snapshot.selected_device_id = None;
        self.snapshot.output_device_count = 0;
        self.snapshot.channels.a.intensity = 0;
        self.snapshot.channels.b.intensity = 0;
        self.refresh_channel_statuses();
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
        self.snapshot.revision = self.snapshot.revision.saturating_add(1);
        let _ = self.snapshot_sender.send(self.snapshot.clone());
    }
}

fn devices_get_request() -> Value {
    devices_get_request_with_id(&Uuid::new_v4().to_string())
}

fn devices_get_request_with_id(request_id: &str) -> Value {
    json!({
        "t": "req",
        "reqId": request_id,
        "m": "devices.get",
    })
}

fn append_pulse_request(
    request_id: &str,
    slot_id: &str,
    channel: Channel,
    frame_hex: &str,
) -> Value {
    json!({
        "t": "req",
        "reqId": request_id,
        "m": "device.op",
        "data": {
            "s": slot_id,
            "t": 0,
            "c": channel.as_v4(),
            "p": 1,
            "d": 100,
            "ver": 3,
            "v": [frame_hex],
        },
    })
}

fn add_intensity_request(request_id: &str, slot_id: &str, channel: Channel, delta: i32) -> Value {
    json!({
        "t": "req",
        "reqId": request_id,
        "m": "device.op",
        "data": {
            "s": slot_id,
            "t": 3,
            "c": channel.as_v4(),
            "p": 1,
            "v": delta,
        },
    })
}

fn clear_request(slot_id: &str) -> Value {
    json!({
        "t": "req",
        "reqId": Uuid::new_v4().to_string(),
        "m": "device.op.clear",
        "data": { "s": slot_id },
    })
}

fn zero_intensity_request(slot_id: &str, channel: Channel) -> Value {
    json!({
        "t": "req",
        "reqId": Uuid::new_v4().to_string(),
        "m": "device.op",
        "data": {
            "s": slot_id,
            "t": 7,
            "c": channel.as_v4(),
            "p": 1,
            "v": 0,
        },
    })
}

fn stop_operation_requests(slot_id: &str, emergency: bool) -> Vec<Value> {
    let mut requests = vec![clear_request(slot_id)];
    if emergency {
        requests.extend(Channel::ALL.map(|channel| zero_intensity_request(slot_id, channel)));
    }
    requests
}

fn encode_wave_frame(frame: WaveFrame) -> String {
    let mut encoded = String::with_capacity(16);
    for sample in frame.samples() {
        encoded.push_str(&format!("{:02X}", sample.frequency()));
    }
    for sample in frame.samples() {
        encoded.push_str(&format!("{:02X}", sample.pulse_intensity()));
    }
    encoded
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
        power: object_u16(props, "power").unwrap_or(0),
        intensity_a,
        intensity_b,
        intensity_limit_a: slot_state
            .and_then(|state| state.get("channelA"))
            .and_then(|channel| channel.get("intensityMax"))
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(200),
        intensity_limit_b: slot_state
            .and_then(|state| state.get("channelB"))
            .and_then(|channel| channel.get("intensityMax"))
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(200),
        output_active: false,
        channel_a_status: protocol_channel_status(
            object_u16(props, "channelAStatus"),
            has_device,
            slot_state.is_some_and(|state| channel_is_muted(state, Channel::A)),
        ),
        channel_b_status: protocol_channel_status(
            object_u16(props, "channelBStatus"),
            has_device,
            slot_state.is_some_and(|state| channel_is_muted(state, Channel::B)),
        ),
    })
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
        Some(2) | None => ChannelStatus::Ready,
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

fn selected_device_limit(
    selected: Option<&DeviceKey>,
    devices: &BTreeMap<DeviceKey, Value>,
    channel: Channel,
) -> Option<u16> {
    let device = devices.get(selected?)?;
    let channel_key = match channel {
        Channel::A => "channelA",
        Channel::B => "channelB",
    };
    device
        .get("slotState")?
        .get(channel_key)?
        .get("intensityMax")?
        .as_u64()
        .and_then(|value| u16::try_from(value).ok())
}

fn effective_limit(configured: u16, device: Option<u16>) -> u16 {
    device.map_or(configured, |device| configured.min(device))
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    #[test]
    fn snapshot_contract_is_camel_case_and_matches_frontend_values() {
        let (hub, _runtime) = create_hub("wss://example.test/v4".to_owned());
        let value = serde_json::to_value(hub.snapshot()).unwrap();
        assert!(value.get("activeSourceId").is_some());
        assert!(value["connection"].get("controllerId").is_some());
        assert_eq!(value["connection"]["state"], "disconnected");
        assert_eq!(value["output"]["state"], "idle");
        assert_eq!(value["channels"]["a"]["status"], "disconnected");
        assert_eq!(value["sources"][0]["kind"], "builtin.test_pattern");
        assert!(value.get("devices").is_some());
        assert!(value.get("selectedDeviceId").is_some());
        assert_eq!(value["syncAllDevices"], false);
        assert_eq!(value["outputDeviceCount"], 0);
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
        let selected = runtime.selected_device.clone().unwrap();
        runtime.devices.get_mut(&selected).unwrap()["slotState"]["channelA"]["isMuted"] =
            Value::Bool(true);
        runtime.refresh_selected_device_snapshot();

        assert_eq!(runtime.snapshot.channels.a.status, ChannelStatus::Disabled);
        assert_eq!(runtime.start_output(), Ok(()));
    }

    #[test]
    fn both_muted_channels_still_accept_control_output() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 20);
        let selected = runtime.selected_device.clone().unwrap();
        let slot_state = &mut runtime.devices.get_mut(&selected).unwrap()["slotState"];
        slot_state["channelA"]["isMuted"] = Value::Bool(true);
        slot_state["channelB"]["isMuted"] = Value::Bool(true);
        runtime.refresh_selected_device_snapshot();

        assert_eq!(runtime.snapshot.channels.a.status, ChannelStatus::Disabled);
        assert_eq!(runtime.snapshot.channels.b.status, ChannelStatus::Disabled);
        assert_eq!(runtime.start_output(), Ok(()));
    }

    #[test]
    fn app_intensity_lock_keeps_pc_value_and_detects_phone_changes() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 20);
        runtime.set_allow_app_intensity_control(false);

        let selected = runtime.selected_device.clone().unwrap();
        runtime.devices.get_mut(&selected).unwrap()["props"]["intensityA"] = Value::from(35);
        runtime.refresh_selected_device_snapshot();

        assert_eq!(runtime.snapshot.channels.a.intensity, 20);
        assert_eq!(
            runtime.devices[&selected]["props"]["intensityA"],
            Value::from(35)
        );
        assert_eq!(
            runtime.intensity_lock_corrections(),
            vec![(selected.clone(), Channel::A, 35, 20)]
        );

        runtime.set_intensity_lock_target(0, 0);
        assert_eq!(runtime.snapshot.channels.a.intensity, 0);
        assert_eq!(
            runtime.intensity_lock_corrections(),
            vec![(selected, Channel::A, 35, 0)]
        );
        runtime.set_allow_app_intensity_control(true);
        assert_eq!(runtime.snapshot.channels.a.intensity, 35);
        assert!(runtime.intensity_lock_corrections().is_empty());
    }

    #[test]
    fn intensity_locks_are_independent_for_each_device() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 30);
        runtime.set_allow_app_intensity_control(false);

        let first = DeviceKey {
            client_id: "app-1".to_owned(),
            slot_id: "slot-a".to_owned(),
        };
        let second = DeviceKey {
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
    fn device_join_and_single_disconnect_preserve_parallel_output_set() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        runtime.start_output().unwrap();
        assert_eq!(runtime.output_devices.len(), 1);

        install_test_device(&mut runtime, "app-2", "slot-b", 20);
        runtime.reconcile_connected_devices();
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
        runtime.start_output().unwrap();

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
        assert_eq!(runtime.snapshot.channels.a.status, ChannelStatus::Active);
        assert_eq!(runtime.snapshot.channels.b.status, ChannelStatus::Disabled);
    }

    #[test]
    fn emergency_stop_orders_clear_before_both_zero_operations() {
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
    fn timestamp_is_valid_iso_shape_at_unix_epoch_boundaries() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_678), (2026, 8, 13));
    }

    #[tokio::test]
    async fn one_device_disconnect_keeps_other_devices_running_and_ignores_ghost_messages() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 20);
        runtime.selected_device = Some(DeviceKey {
            client_id: "app-1".to_owned(),
            slot_id: "slot-a".to_owned(),
        });
        runtime.start_output().unwrap();

        runtime.remove_app("app-1");
        assert_eq!(runtime.snapshot.output.state, OutputState::Running);
        assert_eq!(runtime.output_devices.len(), 1);
        assert_eq!(
            runtime
                .selected_device
                .as_ref()
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
            .adjust_device_intensity(None, Channel::A, 10)
            .unwrap();
        assert_eq!(
            runtime.adjust_device_intensity(None, Channel::A, 1),
            Err(HubError::QueueBusy)
        );
        runtime.apply_slots_patch(
            "app-1",
            &json!({
                "slots": [{"slotId": "slot-a", "props": {"intensityA": 75}}]
            }),
        );
        assert_eq!(runtime.snapshot.channels.a.intensity, 75);
        assert_eq!(
            runtime.adjust_device_intensity(None, Channel::A, 1),
            Err(HubError::QueueBusy),
            "无关 slots.patch 不能提前确认 in-flight 相对强度操作"
        );
        let key = IntensityKey {
            device: runtime.selected_device.clone().unwrap(),
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
    async fn synchronized_intensity_adjustment_targets_every_device() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 10);
        let (event_sender, _events) = mpsc::channel(8);
        let (relay, relay_task) = spawn_relay_client(event_sender, 8);
        runtime.relay = Some(relay.clone());

        runtime.set_sync_all_devices(true).unwrap();
        runtime
            .adjust_device_intensity(None, Channel::A, 5)
            .unwrap();

        let first = IntensityKey {
            device: DeviceKey {
                client_id: "app-1".to_owned(),
                slot_id: "slot-a".to_owned(),
            },
            channel: Channel::A,
        };
        let second = IntensityKey {
            device: DeviceKey {
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
    async fn device_specific_intensity_adjustment_does_not_change_selected_device() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 30);
        let (event_sender, _events) = mpsc::channel(8);
        let (relay, relay_task) = spawn_relay_client(event_sender, 8);
        runtime.relay = Some(relay.clone());

        let first = runtime.selected_device.clone().unwrap();
        let second = DeviceKey {
            client_id: "app-2".to_owned(),
            slot_id: "slot-b".to_owned(),
        };
        runtime
            .adjust_device_intensity(Some(&second.control_id()), Channel::A, 5)
            .unwrap();

        assert_eq!(runtime.selected_device.as_ref(), Some(&first));
        assert_eq!(runtime.snapshot.channels.a.intensity, 10);
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
    async fn enabling_synchronization_aligns_to_selected_device_actual_strengths() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 30);
        let (event_sender, _events) = mpsc::channel(8);
        let (relay, relay_task) = spawn_relay_client(event_sender, 8);
        runtime.relay = Some(relay.clone());

        runtime.set_sync_all_devices(true).unwrap();

        let second = IntensityKey {
            device: DeviceKey {
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
            client_id: "app-2".to_owned(),
            slot_id: "slot-b".to_owned(),
        };
        runtime.devices.get_mut(&second).unwrap()["slotState"]["channelA"]["intensityMax"] =
            Value::from(12);
        runtime.refresh_selected_device_snapshot();
        let (event_sender, _events) = mpsc::channel(8);
        let (relay, relay_task) = spawn_relay_client(event_sender, 8);
        runtime.relay = Some(relay.clone());

        runtime.set_sync_all_devices(true).unwrap();
        assert_eq!(
            runtime.adjust_device_intensity(None, Channel::A, 5),
            Err(HubError::IntensityLimit)
        );
        assert!(runtime.pending_intensity_operations.is_empty());

        relay.shutdown_now();
        relay_task.await.unwrap();
    }

    #[tokio::test]
    async fn lowering_limit_only_commits_after_safety_write_ack() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 90);
        let (event_sender, _events) = mpsc::channel(8);
        let (relay, relay_task) = spawn_relay_client(event_sender, 8);
        runtime.relay = Some(relay.clone());

        assert!(matches!(
            runtime.set_channel_limit(50).await,
            Err(HubError::Relay(_))
        ));
        assert_eq!(runtime.snapshot.safety.channel_limit, DEFAULT_CHANNEL_LIMIT);
        assert_eq!(runtime.snapshot.channels.a.limit, DEFAULT_CHANNEL_LIMIT);

        relay.shutdown_now();
        relay_task.await.unwrap();
    }

    #[tokio::test]
    async fn pending_wave_high_water_mark_applies_backpressure_without_stopping_output() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        runtime.snapshot.output.state = OutputState::Running;
        runtime.output_started_at = Some(Instant::now());
        for index in 0..MAX_PENDING_WAVE_OPERATIONS {
            runtime.pending_wave_operations.insert(
                format!("wave-{index}"),
                PendingWaveOperation {
                    device: DeviceKey {
                        client_id: "app-1".to_owned(),
                        slot_id: "slot-a".to_owned(),
                    },
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
        runtime.output_started_at = Some(Instant::now());
        runtime.pending_wave_operations.insert(
            "wave-timeout".to_owned(),
            PendingWaveOperation {
                device: DeviceKey {
                    client_id: "app-1".to_owned(),
                    slot_id: "slot-a".to_owned(),
                },
                generation: runtime.operation_generation,
                sent_at: Instant::now() - WAVE_OPERATION_RESPONSE_TIMEOUT,
            },
        );

        runtime.output_tick().await;

        assert_eq!(runtime.snapshot.output.state, OutputState::Error);
        assert_eq!(
            runtime.snapshot.output.last_error.as_deref(),
            Some("设备波形响应超时，输出已停止")
        );
        assert!(runtime.pending_wave_operations.is_empty());
    }

    #[tokio::test]
    async fn current_wave_error_from_app_stops_output() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        runtime.apps.insert("app-1".to_owned());
        runtime.snapshot.output.state = OutputState::Running;
        runtime.pending_wave_operations.insert(
            "wave-1".to_owned(),
            PendingWaveOperation {
                device: DeviceKey {
                    client_id: "app-1".to_owned(),
                    slot_id: "slot-a".to_owned(),
                },
                generation: runtime.operation_generation,
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
                .snapshot
                .connection
                .last_error
                .as_deref()
                .is_some_and(|message| message.contains("1 秒后自动重连"))
        );

        runtime.reconnect_at = Some(Instant::now());
        runtime.reconnect_if_due();
        assert_eq!(
            runtime.snapshot.connection.state,
            ConnectionState::Connecting
        );

        runtime.disable_auto_reconnect();
        runtime
            .handle_relay_event(RelayEvent::Disconnected {
                reason: "用户已断开 Relay".to_owned(),
                retryable: false,
            })
            .await;
        assert!(runtime.reconnect_at.is_none());
        assert_eq!(runtime.reconnect_attempt, 0);
        assert_eq!(
            runtime.snapshot.connection.state,
            ConnectionState::Disconnected
        );
    }

    #[tokio::test]
    async fn emergency_stop_invalidates_an_already_queued_start() {
        let (hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);

        let (start_reply, start_response) = oneshot::channel();
        hub.commands
            .try_send(HubCommand::StartOutput {
                safety_epoch: hub.safety_epoch.load(Ordering::Acquire),
                reply: start_reply,
            })
            .unwrap();
        hub.safety_epoch.fetch_add(1, Ordering::AcqRel);
        let (stop_reply, _stop_response) = oneshot::channel();
        hub.safety_commands
            .try_send(HubSafetyCommand::EmergencyStop(stop_reply))
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
        let (clear_sender, clear_received) = oneshot::channel();
        let (emergency_sender, emergency_received) = oneshot::channel();

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
            let mut sent_clear_signal = false;
            let mut pulse_sender = Some(pulse_sender);
            let mut clear_sender = Some(clear_sender);
            let mut emergency_sender = Some(emergency_sender);
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
                        emergency_sender.take().unwrap().send(()).unwrap();
                        break;
                    }
                }
            }
        });

        let (hub, runtime) = create_hub(endpoint);
        let runtime_task = tokio::spawn(runtime.run());
        wait_for_snapshot(&hub, |snapshot| snapshot.devices.len() == 2).await;
        assert_eq!(hub.snapshot().connection.state, ConnectionState::Connected);
        hub.start_output().await.unwrap();
        assert_eq!(hub.snapshot().output_device_count, 2);
        tokio::time::timeout(Duration::from_secs(2), pulse_received)
            .await
            .unwrap()
            .unwrap();

        let second_device = hub
            .snapshot()
            .devices
            .iter()
            .find(|device| device.slot_id == "slot-b")
            .unwrap()
            .control_id
            .clone();
        hub.select_device(second_device.clone()).await.unwrap();
        assert_eq!(
            hub.snapshot().selected_device_id.as_deref(),
            Some(second_device.as_str())
        );
        assert_eq!(hub.snapshot().output.state, OutputState::Running);
        assert_eq!(hub.snapshot().devices.len(), 2);

        hub.stop_output().await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), clear_received)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hub.snapshot().output.state, OutputState::Idle);
        assert_eq!(hub.snapshot().output_device_count, 0);

        hub.start_output().await.unwrap();
        hub.emergency_stop().await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), emergency_received)
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

    fn install_test_device(
        runtime: &mut HubRuntime,
        client_id: &str,
        slot_id: &str,
        intensity_a: u16,
    ) {
        runtime.apps.insert(client_id.to_owned());
        let key = DeviceKey {
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
        if runtime.selected_device.is_none() {
            runtime.selected_device = Some(key);
        }
        runtime.snapshot.connection.state = ConnectionState::Connected;
        runtime.snapshot.connection.app_count = runtime.apps.len();
        runtime.refresh_selected_device_snapshot();
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
