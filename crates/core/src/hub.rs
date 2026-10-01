use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use thiserror::Error;
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::time::{Duration, Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::dglab::client::{RelayClientError, RelayClientHandle, RelayEvent, spawn_relay_client};
use crate::dglab::v4::pairing_url as build_pairing_url;
use crate::model::{Channel, WaveFrame};
use crate::sources::audio::{
    AudioAction, AudioChannelConfig, AudioEngine, AudioMappingRuntime, AudioSnapshot,
};
use crate::sources::touch::{TouchConfig, TouchInput, TouchRuntime};
use crate::sources::{WaveSource, WaveformConfig, builtin_registry};

const HUB_COMMAND_CAPACITY: usize = 64;
const HUB_SAFETY_COMMAND_CAPACITY: usize = 8;
const RELAY_COMMAND_CAPACITY: usize = 256;
const RELAY_EVENT_CAPACITY: usize = 128;
const MAX_OUTPUT_DEVICES: usize = 32;
const MAX_PENDING_WAVE_OPERATIONS: usize = 256;
const WAVE_OPERATION_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_LOGS: usize = 100;
const DEFAULT_CONNECTION_TIMEOUT_MINUTES: u16 = 60;
const RELAY_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const RELAY_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
const RELAY_RECONNECT_MAX_DELAY_SECONDS: u64 = 30;
const MAX_CUSTOM_WAVEFORMS: usize = 128;
const MAX_CUSTOM_WAVEFORM_FRAMES: usize = 16_384;
const FIXED_WAVEFORM_SOURCE_ID: &str = "source-fixed-waveform";
const TOUCH_SOURCE_ID: &str = "source-touch";
const AUDIO_SOURCE_ID: &str = "source-audio";

struct TouchInputSlot {
    latest: TouchInput,
    transitions: VecDeque<(TouchInput, std::time::Instant)>,
    pending: bool,
    received_at: std::time::Instant,
}

type TouchMailbox = Arc<Mutex<BTreeMap<String, TouchInputSlot>>>;
const MAX_TOUCH_TRANSITIONS: usize = 8;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioBindingSnapshot {
    pub device_id: String,
    pub channel: Channel,
    pub config: AudioChannelConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputModesSnapshot {
    pub touch_config: TouchConfig,
    pub audio: AudioSnapshot,
    pub audio_bindings: Vec<AudioBindingSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Waiting,
    Connected,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputState {
    Idle,
    Running,
    Stopped,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelStatus {
    Idle,
    Ready,
    Active,
    Disabled,
    Disconnected,
    Fault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSnapshot {
    pub state: ConnectionState,
    pub endpoint: String,
    pub controller_id: Option<String>,
    pub pairing_url: Option<String>,
    pub app_count: usize,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    pub source_id_a: Option<String>,
    pub source_id_b: Option<String>,
    pub waveform_id_a: Option<String>,
    pub waveform_id_b: Option<String>,
    pub waveform_name_a: Option<String>,
    pub waveform_name_b: Option<String>,
    pub source_sync: bool,
    pub output_active: bool,
    pub channel_a_status: ChannelStatus,
    pub channel_b_status: ChannelStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSnapshot {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub enabled: bool,
    pub assigned_channel_count: usize,
    pub selected_preset_id: Option<String>,
    pub selected_preset_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomWaveformSnapshot {
    pub id: String,
    pub name: String,
    pub frame_count: usize,
    pub duration_ms: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputSnapshot {
    pub state: OutputState,
    pub frames_sent: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelSnapshot {
    pub intensity: u16,
    pub limit: u16,
    pub status: ChannelStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelsSnapshot {
    pub a: ChannelSnapshot,
    pub b: ChannelSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetySnapshot {
    pub connection_timeout_enabled: bool,
    pub connection_timeout_minutes: u16,
    pub allow_app_intensity_control: bool,
}

impl Default for SafetySnapshot {
    fn default() -> Self {
        Self {
            connection_timeout_enabled: false,
            connection_timeout_minutes: DEFAULT_CONNECTION_TIMEOUT_MINUTES,
            allow_app_intensity_control: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogSnapshot {
    pub id: String,
    pub level: LogLevel,
    pub message: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    pub custom_waveforms: Vec<CustomWaveformSnapshot>,
    pub default_source_id: Option<String>,
    pub input_modes: InputModesSnapshot,
    pub output: OutputSnapshot,
    pub channels: ChannelsSnapshot,
    pub safety: SafetySnapshot,
    pub logs: Vec<LogSnapshot>,
}

impl HubSnapshot {
    fn initial(
        endpoint: String,
        sources: Vec<SourceSnapshot>,
        custom_waveforms: Vec<CustomWaveformSnapshot>,
        default_source_id: Option<String>,
        safety: SafetySnapshot,
    ) -> Self {
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
            custom_waveforms,
            default_source_id,
            input_modes: InputModesSnapshot {
                touch_config: TouchConfig::default(),
                audio: AudioSnapshot::default(),
                audio_bindings: Vec::new(),
            },
            output: OutputSnapshot {
                state: OutputState::Idle,
                frames_sent: 0,
                last_error: None,
            },
            channels: ChannelsSnapshot {
                a: ChannelSnapshot {
                    intensity: 0,
                    limit: 0,
                    status: ChannelStatus::Disconnected,
                },
                b: ChannelSnapshot {
                    intensity: 0,
                    limit: 0,
                    status: ChannelStatus::Disconnected,
                },
            },
            safety,
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
            Self::InvalidSourceConfig(_) => "invalid_source_config",
            Self::CustomWaveformLimit => "custom_waveform_limit",
            Self::CustomWaveformFrameLimit => "custom_waveform_frame_limit",
            Self::InvalidDelta => "invalid_delta",
            Self::IntensityLimit => "intensity_limit",
            Self::InvalidConnectionTimeout => "invalid_connection_timeout",
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
    SetTouchConfig {
        config: TouchConfig,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    SetAudioConfig {
        device_id: String,
        channel: Channel,
        config: AudioChannelConfig,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    AudioControl {
        action: AudioAction,
        safety_epoch: u64,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    Connect {
        safety_epoch: u64,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    RefreshPairing {
        safety_epoch: u64,
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
    SelectDevice {
        device_id: String,
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
    StopOutput {
        device_id: String,
        reply: oneshot::Sender<Result<(), HubError>>,
    },
    EmergencyStop(oneshot::Sender<Result<(), HubError>>),
    Disconnect(oneshot::Sender<Result<(), HubError>>),
}

#[derive(Clone)]
pub struct HubHandle {
    touch_mailbox: TouchMailbox,
    commands: mpsc::Sender<HubCommand>,
    safety_commands: mpsc::Sender<HubSafetyCommand>,
    snapshots: watch::Receiver<HubSnapshot>,
    shutdown: CancellationToken,
    completion: watch::Receiver<Option<Result<(), HubError>>>,
    safety_epoch: Arc<AtomicU64>,
    safety_wakeup: Arc<Notify>,
}

impl HubHandle {
    pub fn update_touch_input(&self, input: TouchInput) -> Result<(), HubError> {
        let snapshot = self.snapshots.borrow();
        let device = snapshot
            .devices
            .iter()
            .find(|device| device.control_id == input.device_id)
            .ok_or(HubError::DeviceUnavailable)?;
        if snapshot.output.state != OutputState::Running
            || !device.output_active
            || (device.source_id_a.as_deref() != Some(TOUCH_SOURCE_ID)
                && device.source_id_b.as_deref() != Some(TOUCH_SOURCE_ID))
        {
            return Err(HubError::SourceUnavailable("请先开始触控源输出".to_owned()));
        }
        if input.owner_id.is_empty()
            || input.owner_id.len() > 128
            || input.pointers.len() > 2
            || input.pointers.iter().any(|pointer| {
                !pointer.x.is_finite()
                    || !pointer.y.is_finite()
                    || !(0.0..=1.0).contains(&pointer.x)
                    || !(0.0..=1.0).contains(&pointer.y)
                    || pointer.cell.is_some_and(|cell| cell >= 16)
            })
        {
            return Err(HubError::InvalidSourceConfig(
                "触控坐标或触点数量无效".to_owned(),
            ));
        }
        let mut mailbox = self
            .touch_mailbox
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if mailbox.len() >= MAX_OUTPUT_DEVICES && !mailbox.contains_key(&input.device_id) {
            return Err(HubError::QueueBusy);
        }
        let now = std::time::Instant::now();
        if let Some(slot) = mailbox.get_mut(&input.device_id) {
            if slot.latest.owner_id == input.owner_id && slot.latest.sequence >= input.sequence {
                return Ok(());
            }
            if slot.latest.owner_id != input.owner_id
                && !slot.latest.pointers.is_empty()
                && slot.received_at.elapsed() < crate::sources::touch::TOUCH_INPUT_LEASE
            {
                return Err(HubError::SourceUnavailable(
                    "该设备正在由另一个窗口触控".to_owned(),
                ));
            }
            let transition = slot.latest.owner_id != input.owner_id
                || slot.latest.pointers.len() != input.pointers.len()
                || slot
                    .latest
                    .pointers
                    .iter()
                    .zip(&input.pointers)
                    .any(|(previous, next)| previous.id != next.id || previous.cell != next.cell);
            if transition {
                if slot.transitions.len() >= MAX_TOUCH_TRANSITIONS {
                    if input.pointers.is_empty() {
                        slot.transitions.clear();
                    } else {
                        return Err(HubError::QueueBusy);
                    }
                }
                slot.transitions.push_back((input.clone(), now));
            }
            slot.latest = input;
            slot.pending = true;
            slot.received_at = now;
        } else {
            mailbox.insert(
                input.device_id.clone(),
                TouchInputSlot {
                    transitions: VecDeque::from([(input.clone(), now)]),
                    latest: input,
                    pending: true,
                    received_at: now,
                },
            );
        }
        Ok(())
    }

    pub async fn set_touch_config(&self, config: TouchConfig) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(HubCommand::SetTouchConfig { config, reply })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn set_audio_config(
        &self,
        device_id: String,
        channel: Channel,
        config: AudioChannelConfig,
    ) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(HubCommand::SetAudioConfig {
                device_id,
                channel,
                config,
                reply,
            })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn audio_control(&self, action: AudioAction) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        let safety_epoch = self.safety_epoch.load(Ordering::Acquire);
        self.commands
            .send(HubCommand::AudioControl {
                action,
                safety_epoch,
                reply,
            })
            .await
            .map_err(|_| HubError::Stopped)?;
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub fn snapshot(&self) -> HubSnapshot {
        self.snapshots.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<HubSnapshot> {
        self.snapshots.clone()
    }

    pub async fn connect_relay(&self) -> Result<(), HubError> {
        let safety_epoch = self.safety_epoch.load(Ordering::Acquire);
        self.request(|reply| HubCommand::Connect {
            safety_epoch,
            reply,
        })
        .await
    }

    pub async fn disconnect_relay(&self) -> Result<(), HubError> {
        self.safety_epoch.fetch_add(1, Ordering::AcqRel);
        self.safety_request(HubSafetyCommand::Disconnect).await
    }

    pub async fn refresh_pairing(&self) -> Result<(), HubError> {
        let safety_epoch = self.safety_epoch.load(Ordering::Acquire);
        self.request(|reply| HubCommand::RefreshPairing {
            safety_epoch,
            reply,
        })
        .await
    }

    pub async fn adjust_device_intensity(
        &self,
        device_id: Option<String>,
        channel: Channel,
        delta: i32,
    ) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        let safety_epoch = self.safety_epoch.load(Ordering::Acquire);
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
        let safety_epoch = self.safety_epoch.load(Ordering::Acquire);
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

    pub async fn stop_output(&self, device_id: String) -> Result<(), HubError> {
        self.safety_epoch.fetch_add(1, Ordering::AcqRel);
        let (reply, response) = oneshot::channel();
        self.safety_commands
            .send(HubSafetyCommand::StopOutput { device_id, reply })
            .await
            .map_err(|_| HubError::Stopped)?;
        self.safety_wakeup.notify_waiters();
        response.await.map_err(|_| HubError::Stopped)?
    }

    pub async fn emergency_stop(&self) -> Result<(), HubError> {
        self.safety_epoch.fetch_add(1, Ordering::AcqRel);
        self.safety_request(HubSafetyCommand::EmergencyStop).await
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

    pub async fn select_device(&self, device_id: String) -> Result<(), HubError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(HubCommand::SelectDevice { device_id, reply })
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
        let safety_epoch = self.safety_epoch.load(Ordering::Acquire);
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
        self.safety_wakeup.notify_waiters();
        response.await.map_err(|_| HubError::Stopped)?
    }
}

struct SourceRuntime {
    snapshot: SourceSnapshot,
    source: Box<dyn WaveSource>,
}

struct FixedWaveformRuntime {
    config: WaveformConfig,
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SourceBindingKey {
    device: DeviceKey,
    channel: Channel,
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

pub struct HubRuntime {
    touch_mailbox: TouchMailbox,
    touch_config: TouchConfig,
    touch_runtimes: BTreeMap<DeviceKey, TouchRuntime>,
    touch_active_channels: BTreeMap<DeviceKey, [bool; 2]>,
    audio_engine: AudioEngine,
    audio_features_active: bool,
    audio_bindings: BTreeMap<SourceBindingKey, AudioMappingRuntime>,
    commands: mpsc::Receiver<HubCommand>,
    safety_commands: mpsc::Receiver<HubSafetyCommand>,
    snapshot_sender: watch::Sender<HubSnapshot>,
    completion_sender: watch::Sender<Option<Result<(), HubError>>>,
    shutdown: CancellationToken,
    snapshot: HubSnapshot,
    sources: BTreeMap<String, SourceRuntime>,
    default_fixed_waveform: Option<WaveformConfig>,
    fixed_waveform_bindings: BTreeMap<SourceBindingKey, FixedWaveformRuntime>,
    custom_waveforms: Vec<WaveformConfig>,
    default_source_id: Option<String>,
    device_source_bindings: BTreeMap<SourceBindingKey, String>,
    initialized_source_devices: BTreeSet<DeviceKey>,
    source_sync_devices: BTreeSet<DeviceKey>,
    relay: Option<RelayClientHandle>,
    apps: BTreeSet<String>,
    devices: BTreeMap<DeviceKey, Value>,
    selected_device: Option<DeviceKey>,
    output_devices: BTreeSet<DeviceKey>,
    pending_wave_operations: HashMap<String, PendingWaveOperation>,
    pending_intensity_operations: BTreeMap<IntensityKey, PendingIntensityOperation>,
    pending_intensity_requests: HashMap<String, IntensityKey>,
    intensity_lock_targets: BTreeMap<DeviceKey, IntensityLockTarget>,
    operation_generation: u64,
    safety_epoch: Arc<AtomicU64>,
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
                kind: "test.secondary".to_owned(),
                name: "测试辅助源".to_owned(),
                enabled: true,
                assigned_channel_count: 0,
                selected_preset_id: Some("TEST_SECONDARY".to_owned()),
                selected_preset_name: Some("测试辅助源".to_owned()),
            },
            source: registry.build("builtin.fixed_waveform", &config).unwrap(),
        },
    );
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
        kind: "builtin.fixed_waveform".to_owned(),
        name: "固定波形".to_owned(),
        enabled: true,
        assigned_channel_count: 0,
        selected_preset_id: None,
        selected_preset_name: None,
    };
    sources.insert(
        FIXED_WAVEFORM_SOURCE_ID.to_owned(),
        SourceRuntime {
            snapshot: source_snapshot.clone(),
            source,
        },
    );

    for (id, kind) in [
        (TOUCH_SOURCE_ID, "builtin.touch"),
        (AUDIO_SOURCE_ID, "builtin.audio"),
    ] {
        let descriptor = registry
            .list_descriptors()
            .iter()
            .find(|descriptor| descriptor.kind == kind)
            .unwrap();
        sources.insert(
            id.to_owned(),
            SourceRuntime {
                snapshot: SourceSnapshot {
                    id: id.to_owned(),
                    kind: kind.to_owned(),
                    name: descriptor.display_name.to_owned(),
                    enabled: true,
                    assigned_channel_count: 0,
                    selected_preset_id: None,
                    selected_preset_name: None,
                },
                source: registry
                    .build(kind, &registry.default_config(kind).unwrap())
                    .expect("内置动态源配置必须有效"),
            },
        );
    }

    let requested_default_source_id = requested_default_source_id.map(|id| match id.as_str() {
        "source-test-pattern"
        | "source-manual"
        | "source-default-waveform"
        | "source-custom-waveform" => FIXED_WAVEFORM_SOURCE_ID.to_owned(),
        _ => id,
    });
    let default_source_id = requested_default_source_id.filter(|id| sources.contains_key(id));
    let custom_waveform_snapshots = custom_waveform_snapshots(&custom_waveforms);
    let snapshot = HubSnapshot::initial(
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
    let safety_wakeup = Arc::new(Notify::new());
    let (completion_sender, completion_receiver) = watch::channel(None);
    let touch_mailbox = Arc::new(Mutex::new(BTreeMap::new()));
    (
        HubHandle {
            touch_mailbox: Arc::clone(&touch_mailbox),
            commands: command_sender,
            safety_commands: safety_sender,
            snapshots: snapshot_receiver,
            shutdown: shutdown.clone(),
            completion: completion_receiver,
            safety_epoch: Arc::clone(&safety_epoch),
            safety_wakeup: Arc::clone(&safety_wakeup),
        },
        HubRuntime {
            touch_mailbox,
            touch_config: TouchConfig::default(),
            touch_runtimes: BTreeMap::new(),
            touch_active_channels: BTreeMap::new(),
            audio_engine: AudioEngine::new(),
            audio_features_active: false,
            audio_bindings: BTreeMap::new(),
            commands: command_receiver,
            safety_commands: safety_receiver,
            snapshot_sender,
            completion_sender,
            shutdown,
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
            apps: BTreeSet::new(),
            devices: BTreeMap::new(),
            selected_device: None,
            output_devices: BTreeSet::new(),
            pending_wave_operations: HashMap::new(),
            pending_intensity_operations: BTreeMap::new(),
            pending_intensity_requests: HashMap::new(),
            intensity_lock_targets: BTreeMap::new(),
            operation_generation: 0,
            safety_epoch,
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
    pub fn set_initial_touch_config(&mut self, config: TouchConfig) -> Result<(), HubError> {
        TouchRuntime::new(config.clone())
            .map_err(|error| HubError::InvalidSourceConfig(error.to_string()))?;
        self.touch_config = config;
        self.refresh_input_modes();
        self.publish();
        Ok(())
    }

    fn refresh_input_modes(&mut self) {
        self.snapshot.input_modes = InputModesSnapshot {
            touch_config: self.touch_config.clone(),
            audio: self.audio_engine.snapshot(),
            audio_bindings: self
                .audio_bindings
                .iter()
                .filter(|(binding, _)| self.devices.contains_key(&binding.device))
                .map(|(binding, runtime)| AudioBindingSnapshot {
                    device_id: binding.device.control_id(),
                    channel: binding.channel,
                    config: runtime.config.clone(),
                })
                .collect(),
        };
    }

    fn reset_device_inputs(&mut self, device: &DeviceKey) {
        self.touch_mailbox
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&device.control_id());
        if let Some(runtime) = self.touch_runtimes.get_mut(device) {
            runtime.reset();
        }
        self.touch_active_channels.remove(device);
        for (binding, runtime) in &mut self.audio_bindings {
            if binding.device == *device {
                runtime.reset();
            }
        }
    }

    fn reset_changed_inputs(&mut self, device: &DeviceKey, channels: &[Channel]) {
        for channel in channels {
            if let Some(runtime) = self.touch_runtimes.get_mut(device) {
                runtime.reset_channel(*channel);
            }
            if let Some(active) = self.touch_active_channels.get_mut(device) {
                active[channel.as_v4() as usize] = false;
            }
            if let Some(runtime) = self.audio_bindings.get_mut(&SourceBindingKey {
                device: device.clone(),
                channel: *channel,
            }) {
                runtime.reset();
            }
        }
    }

    async fn clear_input_channel(&mut self, binding: &SourceBindingKey) -> Result<(), HubError> {
        if !self.output_devices.contains(&binding.device) {
            return Ok(());
        }
        let clear = self
            .relay
            .as_ref()
            .ok_or(HubError::Stopped)?
            .clear_wave_channel(
                &binding.device.client_id,
                &binding.device.slot_id,
                binding.channel,
                clear_channel_request(&binding.device.slot_id, binding.channel),
                self.operation_generation,
            );
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

    async fn clear_audio_output(&mut self) -> Result<(), HubError> {
        self.audio_features_active = false;
        let bindings = self
            .device_source_bindings
            .iter()
            .filter(|(_, source)| source.as_str() == AUDIO_SOURCE_ID)
            .map(|(binding, _)| binding.clone())
            .collect::<Vec<_>>();
        for binding in bindings {
            self.clear_input_channel(&binding).await?;
        }
        for runtime in self.audio_bindings.values_mut() {
            runtime.reset();
        }
        Ok(())
    }

    async fn set_touch_config(&mut self, config: TouchConfig) -> Result<(), HubError> {
        TouchRuntime::new(config.clone())
            .map_err(|error| HubError::InvalidSourceConfig(error.to_string()))?;
        let bindings = self
            .device_source_bindings
            .iter()
            .filter(|(_, source)| source.as_str() == TOUCH_SOURCE_ID)
            .map(|(binding, _)| binding.clone())
            .collect::<Vec<_>>();
        for binding in bindings {
            self.clear_input_channel(&binding).await?;
        }
        self.touch_config = config;
        self.touch_runtimes.clear();
        self.touch_active_channels.clear();
        self.touch_mailbox
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        self.refresh_input_modes();
        self.publish();
        Ok(())
    }

    async fn set_audio_config(
        &mut self,
        device_id: &str,
        channel: Channel,
        config: AudioChannelConfig,
    ) -> Result<(), HubError> {
        let runtime = AudioMappingRuntime::new(config)
            .map_err(|error| HubError::InvalidSourceConfig(error.to_string()))?;
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
            .is_some_and(|source| source == AUDIO_SOURCE_ID)
        {
            self.clear_input_channel(&binding).await?;
        }
        self.audio_bindings.insert(binding, runtime);
        self.refresh_input_modes();
        self.publish();
        Ok(())
    }

    async fn input_tick(&mut self) {
        let audio_active = self.audio_engine.latest().active;
        if self.audio_features_active
            && !audio_active
            && let Err(error) = self.clear_audio_output().await
        {
            if !matches!(error, HubError::QueueBusy) {
                self.fail_output(format!("清理过期音频波形失败：{error}"))
                    .await;
            }
            return;
        }
        self.audio_features_active = audio_active;
        let inputs = {
            let mut mailbox = self
                .touch_mailbox
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let mut inputs = Vec::new();
            for slot in mailbox.values_mut().filter(|slot| slot.pending) {
                inputs.extend(slot.transitions.drain(..));
                if inputs.last().is_none_or(|(previous, _)| {
                    previous.device_id != slot.latest.device_id
                        || previous.owner_id != slot.latest.owner_id
                        || previous.sequence != slot.latest.sequence
                }) {
                    inputs.push((slot.latest.clone(), slot.received_at));
                }
                slot.pending = false;
            }
            inputs
        };
        let now = std::time::Instant::now();
        let mut released = BTreeSet::new();
        for (input, received_at) in inputs {
            if now.duration_since(received_at) >= crate::sources::touch::TOUCH_INPUT_LEASE {
                continue;
            }
            let Some(device) = self
                .output_devices
                .iter()
                .find(|device| device.control_id() == input.device_id)
                .cloned()
            else {
                continue;
            };
            let runtime = self
                .touch_runtimes
                .entry(device.clone())
                .or_insert_with(|| {
                    TouchRuntime::new(self.touch_config.clone()).expect("已验证的触控配置")
                });
            let previous = runtime.touch_intents(now);
            // 过期序列和另一个窗口的触点不会抢占持有中的触控会话。
            let _ = runtime.update(&input, received_at);
            let current = runtime.touch_intents(now);
            for (index, channel) in Channel::ALL.into_iter().enumerate() {
                if previous[index].is_some() && previous[index] != current[index] {
                    released.insert(SourceBindingKey {
                        device: device.clone(),
                        channel,
                    });
                }
            }
        }
        let devices = self.touch_runtimes.keys().cloned().collect::<Vec<_>>();
        for device in devices {
            let active = self.touch_runtimes[&device].active_touch_channels(now);
            let previous = self
                .touch_active_channels
                .insert(device.clone(), active)
                .unwrap_or([false; 2]);
            for (index, channel) in Channel::ALL.into_iter().enumerate() {
                let binding = SourceBindingKey {
                    device: device.clone(),
                    channel,
                };
                if previous[index] && !active[index] {
                    released.insert(binding);
                }
            }
        }
        for binding in released {
            if self
                .device_source_bindings
                .get(&binding)
                .is_some_and(|source| source == TOUCH_SOURCE_ID)
                && let Err(error) = self.clear_input_channel(&binding).await
            {
                if !matches!(error, HubError::QueueBusy) {
                    self.fail_output(format!("清理触控通道失败：{error}")).await;
                }
                return;
            }
        }
        let previous = self.snapshot.input_modes.clone();
        self.refresh_input_modes();
        if self.snapshot.input_modes != previous {
            self.publish();
        }
    }

    pub async fn run(mut self) {
        let (event_sender, mut events) = mpsc::channel(RELAY_EVENT_CAPACITY);
        let (relay, mut relay_task) = spawn_relay_client(event_sender, RELAY_COMMAND_CAPACITY);
        self.relay = Some(relay);

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
                    self.disconnect_if_timed_out().await;
                    self.input_tick().await;
                    self.output_tick().await;
                }
            }

            if !commands_open && !safety_commands_open {
                break;
            }

            self.reconnect_if_due();
        }

        self.snapshot.output.state = OutputState::Stopped;
        self.audio_engine.shutdown();
        self.snapshot.output.last_error = None;
        self.connection_started_at = None;
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
        if self.connection_timed_out() {
            let _ = self.disconnect_all(true).await;
        }
    }

    async fn disconnect_all(&mut self, timed_out: bool) -> Result<(), HubError> {
        self.disable_auto_reconnect();
        self.connection_started_at = None;
        self.snapshot.output.state = OutputState::Idle;
        let stop_result = self.send_stop_operations(false).await;
        if matches!(stop_result, Err(HubError::QueueBusy)) {
            return stop_result;
        }
        let disconnect_result = if let Some(relay) = &self.relay {
            self.wait_for_ordinary_relay(async {
                tokio::time::timeout(RELAY_DISCONNECT_TIMEOUT, relay.disconnect())
                    .await
                    .map_err(|_| HubError::Relay("断开 Relay 超时".to_owned()))
                    .and_then(|result| result.map_err(Into::into))
            })
            .await
        } else {
            Err(HubError::Stopped)
        };
        let result = stop_result.and(disconnect_result);
        if result.is_ok() {
            self.reset_connection_state(ConnectionState::Disconnected);
            self.snapshot.output.state = OutputState::Idle;
            self.log(
                LogLevel::Warning,
                if timed_out {
                    "连接时长已到，所有设备输出与 Relay 连接已断开"
                } else {
                    "已断开 DG-LAB Relay"
                },
            );
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
        result
    }

    async fn handle_command(&mut self, command: HubCommand) {
        match command {
            HubCommand::SetTouchConfig { config, reply } => {
                let _ = reply.send(self.set_touch_config(config).await);
            }
            HubCommand::SetAudioConfig {
                device_id,
                channel,
                config,
                reply,
            } => {
                let _ = reply.send(self.set_audio_config(&device_id, channel, config).await);
            }
            HubCommand::AudioControl {
                action,
                safety_epoch,
                reply,
            } => {
                if safety_epoch != self.safety_epoch.load(Ordering::Acquire) {
                    let _ = reply.send(Err(HubError::QueueBusy));
                    return;
                }
                let clear_result = if matches!(
                    &action,
                    AudioAction::SaveRecording { .. } | AudioAction::SetPlaybackOptions { .. }
                ) {
                    Ok(())
                } else {
                    self.clear_audio_output().await
                };
                let result = clear_result.and_then(|()| {
                    if safety_epoch != self.safety_epoch.load(Ordering::Acquire) {
                        return Err(HubError::QueueBusy);
                    }
                    self.audio_engine
                        .control(action)
                        .map_err(|error| HubError::InvalidSourceConfig(error.to_string()))
                });
                self.refresh_input_modes();
                self.publish();
                let _ = reply.send(result);
            }
            HubCommand::Connect {
                safety_epoch,
                reply,
            } => {
                if safety_epoch != self.safety_epoch.load(Ordering::Acquire) {
                    let _ = reply.send(Err(HubError::QueueBusy));
                    return;
                }
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
            HubCommand::RefreshPairing {
                safety_epoch,
                reply,
            } => {
                if safety_epoch != self.safety_epoch.load(Ordering::Acquire) {
                    let _ = reply.send(Err(HubError::QueueBusy));
                    return;
                }
                self.enable_auto_reconnect();
                self.snapshot.output.state = OutputState::Idle;
                let mut result = self.send_stop_operations(false).await;
                if result.is_ok() && safety_epoch != self.safety_epoch.load(Ordering::Acquire) {
                    result = Err(HubError::QueueBusy);
                }
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
            HubCommand::SelectDevice { device_id, reply } => {
                let result = self.select_device(device_id).await;
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
            HubSafetyCommand::Disconnect(reply) => {
                let _ = reply.send(self.disconnect_all(false).await);
            }
            HubSafetyCommand::StopOutput { device_id, reply } => {
                let result = self.stop_device_output(&device_id).await;
                let _ = reply.send(result);
            }
            HubSafetyCommand::EmergencyStop(reply) => {
                let result = self
                    .stop_all_output(
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
                self.connection_started_at = Some(Instant::now());
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
                self.connection_started_at = None;
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
            let mut completed_intensity = None;

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
                self.refresh_selected_device_snapshot();
                self.reconcile_intensity_lock();
                self.publish();
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
        let previous_values = self
            .devices
            .iter()
            .filter(|(key, _)| key.client_id == client_id)
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        let previous = previous_values.keys().cloned().collect::<BTreeSet<_>>();
        self.devices
            .retain(|key, _| key.client_id.as_str() != client_id);
        for device in devices {
            if let Some(slot_id) = device.get("slotId").and_then(Value::as_str) {
                let key = DeviceKey {
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
            .filter(|key| key.client_id == client_id)
            .cloned()
            .collect::<BTreeSet<_>>();
        for removed in previous.difference(&current) {
            self.clear_pending_for_device(removed);
        }
        self.observe_projected_intensities(client_id);
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
        self.observe_projected_intensities(client_id);
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
        self.touch_runtimes
            .retain(|device, _| self.devices.contains_key(device));
        self.touch_active_channels
            .retain(|device, _| self.devices.contains_key(device));
        self.audio_bindings
            .retain(|binding, _| self.devices.contains_key(&binding.device));
        let connected_devices = self.devices.keys().cloned().collect::<Vec<_>>();
        for device in connected_devices {
            for channel in Channel::ALL {
                self.audio_bindings
                    .entry(SourceBindingKey {
                        device: device.clone(),
                        channel,
                    })
                    .or_insert_with(|| {
                        AudioMappingRuntime::new(AudioChannelConfig::default())
                            .expect("音频默认配置")
                    });
            }
            if self.initialized_source_devices.insert(device.clone())
                && let Some(default_source_id) = &self.default_source_id
            {
                for channel in Channel::ALL {
                    let binding = SourceBindingKey {
                        device: device.clone(),
                        channel,
                    };
                    self.device_source_bindings
                        .insert(binding.clone(), default_source_id.clone());
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
                snapshot.source_id_a = self
                    .device_source_bindings
                    .get(&SourceBindingKey {
                        device: key.clone(),
                        channel: Channel::A,
                    })
                    .cloned();
                snapshot.source_id_b = self
                    .device_source_bindings
                    .get(&SourceBindingKey {
                        device: key.clone(),
                        channel: Channel::B,
                    })
                    .cloned();
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
                snapshot.source_sync = self.source_sync_devices.contains(key);
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
            self.snapshot.channels.a.limit = device.intensity_limit_a;
            self.snapshot.channels.b.limit = device.intensity_limit_b;
        } else {
            self.snapshot.channels.a.intensity = 0;
            self.snapshot.channels.b.intensity = 0;
            self.snapshot.channels.a.limit = 0;
            self.snapshot.channels.b.limit = 0;
        }
        self.refresh_channel_statuses();
    }

    fn refresh_source_snapshots(&mut self) {
        let mut assigned_counts = HashMap::<&str, usize>::new();
        for (binding, source_id) in &self.device_source_bindings {
            if self.devices.contains_key(&binding.device) {
                *assigned_counts.entry(source_id.as_str()).or_default() += 1;
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

    fn start_output(&mut self, device_id: &str) -> Result<(), HubError> {
        if self.apps.is_empty() {
            return Err(HubError::NotConnected);
        }
        let device = self
            .devices
            .keys()
            .find(|device| device.control_id() == device_id)
            .cloned()
            .ok_or(HubError::DeviceUnavailable)?;
        if self.output_devices.contains(&device) {
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
            self.sources.get(source_id).ok_or(HubError::NoSource)?;
        }
        self.snapshot.output.state = OutputState::Running;
        self.reset_device_inputs(&device);
        self.snapshot.output.last_error = None;
        self.output_devices.insert(device.clone());
        self.refresh_selected_device_snapshot();
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
                let Some(source_id) = self.device_source_bindings.get(&binding).cloned() else {
                    self.fail_output(format!(
                        "设备 {} 的 {} 通道尚未分配输入源",
                        device.control_id(),
                        channel_label(channel)
                    ))
                    .await;
                    return;
                };
                bindings_by_source
                    .entry(source_id)
                    .or_default()
                    .push(binding);
            }
        }
        let mut frames_by_source = BTreeMap::<String, String>::new();
        let mut frames_by_binding = BTreeMap::<SourceBindingKey, String>::new();
        let audio_features = self.audio_engine.latest();
        for (source_id, bindings) in &bindings_by_source {
            if source_id == TOUCH_SOURCE_ID {
                let mut touch_frames = BTreeMap::new();
                for binding in bindings {
                    let frames = touch_frames
                        .entry(binding.device.clone())
                        .or_insert_with(|| {
                            self.touch_runtimes
                                .entry(binding.device.clone())
                                .or_insert_with(|| {
                                    TouchRuntime::new(self.touch_config.clone())
                                        .expect("已验证的触控配置")
                                })
                                .next_frames(std::time::Instant::now())
                        });
                    frames_by_binding.insert(
                        binding.clone(),
                        encode_wave_frame(frames[binding.channel.as_v4() as usize]),
                    );
                }
                continue;
            }
            if source_id == AUDIO_SOURCE_ID {
                for binding in bindings {
                    let runtime = self
                        .audio_bindings
                        .entry(binding.clone())
                        .or_insert_with(|| {
                            AudioMappingRuntime::new(AudioChannelConfig::default())
                                .expect("音频默认配置")
                        });
                    frames_by_binding.insert(
                        binding.clone(),
                        encode_wave_frame(runtime.next_frame(&audio_features)),
                    );
                }
                continue;
            }
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
                    frames_by_binding.insert(binding.clone(), encode_wave_frame(frame));
                }
                continue;
            }
            let frame_result = match self.sources.get_mut(source_id) {
                Some(source) if source.snapshot.enabled => {
                    Some(source.source.next_frame().map_err(|error| {
                        format!("输入源 {} 运行失败：{error}", source.snapshot.name)
                    }))
                }
                Some(_) => None,
                None => Some(Err(format!("输入源 {source_id} 不存在"))),
            };
            let Some(frame_result) = frame_result else {
                continue;
            };
            let frame = match frame_result {
                Ok(frame) => frame,
                Err(message) => {
                    self.fail_output(message).await;
                    return;
                }
            };
            frames_by_source.insert(source_id.clone(), encode_wave_frame(frame));
        }
        let mut sent = 0_u64;
        for (source_id, bindings) in bindings_by_source {
            for binding in bindings {
                let frame_hex = if matches!(
                    source_id.as_str(),
                    FIXED_WAVEFORM_SOURCE_ID | TOUCH_SOURCE_ID | AUDIO_SOURCE_ID
                ) {
                    frames_by_binding.get(&binding)
                } else {
                    frames_by_source.get(&source_id)
                };
                let Some(frame_hex) = frame_hex else {
                    continue;
                };
                let request_id = Uuid::new_v4().to_string();
                let request = append_pulse_request(
                    &request_id,
                    &binding.device.slot_id,
                    binding.channel,
                    frame_hex,
                );
                let send_result = self
                    .relay
                    .as_ref()
                    .ok_or(HubError::Stopped)
                    .and_then(|relay| {
                        relay
                            .try_send_wave_operation(
                                &binding.device.client_id,
                                &binding.device.slot_id,
                                binding.channel,
                                request,
                                self.operation_generation,
                            )
                            .map_err(Into::into)
                    });
                match send_result {
                    Ok(()) => {
                        self.pending_wave_operations.insert(
                            request_id,
                            PendingWaveOperation {
                                device: binding.device,
                                channel: binding.channel,
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
                self.device_source_bindings.get(&SourceBindingKey {
                    device: device.clone(),
                    channel: *channel,
                }) != Some(&source_id)
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
            self.device_source_bindings
                .insert(binding.clone(), source_id.clone());
            if source_id == FIXED_WAVEFORM_SOURCE_ID
                && !self.fixed_waveform_bindings.contains_key(&binding)
                && let Some(config) = self.default_fixed_waveform.clone()
            {
                self.fixed_waveform_bindings
                    .insert(binding, build_fixed_waveform_runtime(config)?);
            }
        }
        self.refresh_source_snapshots();
        self.refresh_selected_device_snapshot();
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
            self.refresh_selected_device_snapshot();
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
                    .map(String::as_str)
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
                self.device_source_bindings
                    .insert(binding.clone(), source_id.clone());
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
        self.refresh_selected_device_snapshot();
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
            .map(String::as_str)
            != Some(FIXED_WAVEFORM_SOURCE_ID)
        {
            return Err(HubError::SourceUnavailable(
                FIXED_WAVEFORM_SOURCE_ID.to_owned(),
            ));
        }
        let selected_name = config.as_ref().map(|item| item.preset_name.clone());
        if let Some(config) = config {
            self.fixed_waveform_bindings
                .insert(binding, build_fixed_waveform_runtime(config)?);
        } else {
            self.fixed_waveform_bindings.remove(&binding);
        }
        self.refresh_selected_device_snapshot();
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
        self.refresh_selected_device_snapshot();
        self.log(LogLevel::Info, "固定波形库已更新");
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

    #[cfg(test)]
    fn set_sync_all_devices(&mut self, enabled: bool) -> Result<(), HubError> {
        self.set_sync_all_devices_from(None, enabled)
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
        if self.snapshot.sync_all_devices == enabled {
            return Ok(());
        }
        if enabled {
            let selected = match device_id {
                Some(id) => self
                    .devices
                    .keys()
                    .find(|key| key.control_id() == id)
                    .cloned()
                    .ok_or(HubError::DeviceUnavailable)?,
                None => self.selected_device.clone().ok_or(HubError::NoDevice)?,
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
        }
        self.snapshot.sync_all_devices = enabled;
        self.refresh_selected_device_snapshot();
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

    async fn stop_device_output(&mut self, device_id: &str) -> Result<(), HubError> {
        let device = self
            .devices
            .keys()
            .find(|device| device.control_id() == device_id)
            .cloned()
            .or_else(|| {
                self.output_devices
                    .iter()
                    .find(|device| device.control_id() == device_id)
                    .cloned()
            })
            .ok_or(HubError::DeviceUnavailable)?;
        if !self.output_devices.contains(&device) {
            self.reset_device_inputs(&device);
            return Ok(());
        }

        self.send_safety_requests(&device, stop_operation_requests(&device.slot_id, false))
            .await?;
        self.output_devices.remove(&device);
        self.reset_device_inputs(&device);
        self.pending_wave_operations
            .retain(|_, pending| pending.device != device);
        if self.output_devices.is_empty() {
            self.snapshot.output.state = OutputState::Idle;
        }
        self.snapshot.output.last_error = None;
        self.refresh_selected_device_snapshot();
        self.log(
            LogLevel::Info,
            format!("设备 {device_id} 的波形输出已停止并清空任务"),
        );
        self.publish();
        Ok(())
    }

    async fn stop_all_output(
        &mut self,
        emergency: bool,
        success_state: OutputState,
        success_level: LogLevel,
        success_message: &str,
    ) -> Result<(), HubError> {
        self.snapshot.output.state = success_state;
        self.snapshot.output.last_error = None;
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
        self.touch_mailbox
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        self.touch_runtimes.clear();
        self.touch_active_channels.clear();
        for runtime in self.audio_bindings.values_mut() {
            runtime.reset();
        }
        if emergency {
            self.audio_engine.emergency_stop();
        }
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
            let operation = relay.safety_stop(device.client_id, requests, generation);
            let result = if emergency {
                operation.await.map_err(HubError::from)
            } else {
                self.wait_for_ordinary_relay(operation).await
            };
            if let Err(error) = result {
                if matches!(error, HubError::QueueBusy) {
                    return Err(error);
                }
                first_error.get_or_insert(error);
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

    fn observe_projected_intensities(&mut self, client_id: &str) {
        let keys = self
            .pending_intensity_operations
            .keys()
            .filter(|key| key.device.client_id == client_id)
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
        self.connection_started_at = None;
        self.safety_epoch.fetch_add(1, Ordering::AcqRel);
        self.apps.clear();
        self.devices.clear();
        self.device_source_bindings.clear();
        self.fixed_waveform_bindings.clear();
        self.touch_runtimes.clear();
        self.touch_active_channels.clear();
        self.touch_mailbox
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        self.audio_bindings.clear();
        self.audio_engine.emergency_stop();
        self.initialized_source_devices.clear();
        self.source_sync_devices.clear();
        self.selected_device = None;
        self.output_devices.clear();
        self.intensity_lock_targets.clear();
        self.advance_operation_generation();
        self.snapshot.connection.state = state;
        self.snapshot.connection.controller_id = None;
        self.snapshot.connection.pairing_url = None;
        self.snapshot.connection.app_count = 0;
        self.snapshot.device = None;
        self.snapshot.devices.clear();
        self.refresh_source_snapshots();
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
        self.refresh_input_modes();
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

fn clear_channel_request(slot_id: &str, channel: Channel) -> Value {
    json!({
        "t": "req",
        "reqId": Uuid::new_v4().to_string(),
        "m": "device.op.clear",
        "data": { "s": slot_id, "c": channel.as_v4() },
    })
}

const fn channel_label(channel: Channel) -> &'static str {
    match channel {
        Channel::A => "A",
        Channel::B => "B",
    }
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
            .unwrap_or(0),
        intensity_limit_b: slot_state
            .and_then(|state| state.get("channelB"))
            .and_then(|channel| channel.get("intensityMax"))
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(0),
        source_id_a: None,
        source_id_b: None,
        waveform_id_a: None,
        waveform_id_b: None,
        waveform_name_a: None,
        waveform_name_b: None,
        source_sync: false,
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
        assert!(value["connection"].get("controllerId").is_some());
        assert_eq!(value["connection"]["state"], "disconnected");
        assert_eq!(value["output"]["state"], "idle");
        assert_eq!(value["channels"]["a"]["status"], "disconnected");
        assert_eq!(value["sources"][0]["kind"], "builtin.fixed_waveform");
        assert!(value["sources"][0].get("assignedChannelCount").is_some());
        assert!(value["defaultSourceId"].is_null());
        assert!(value.get("devices").is_some());
        assert!(value.get("selectedDeviceId").is_some());
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
                .device
                .as_ref()
                .unwrap()
                .waveform_id_a
                .as_deref(),
            Some("BUBBLE")
        );
        assert_eq!(
            runtime
                .snapshot
                .device
                .as_ref()
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
                .any(|source_id| source_id == FIXED_WAVEFORM_SOURCE_ID)
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
            runtime.snapshot.device.as_ref().unwrap().waveform_id_a,
            None
        );
        assert_eq!(
            runtime
                .snapshot
                .device
                .as_ref()
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
    async fn input_sources_register_and_touch_mailbox_keeps_latest_active_device_input() {
        let (hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        let device = runtime.selected_device.clone().unwrap();
        runtime
            .set_device_channel_source(device.control_id(), Channel::A, TOUCH_SOURCE_ID.to_owned())
            .await
            .unwrap();
        let mut input = TouchInput {
            device_id: device.control_id(),
            owner_id: "test-window".to_owned(),
            sequence: 1,
            pointers: vec![crate::sources::touch::TouchPointer {
                id: 1,
                x: 0.5,
                y: 0.5,
                cell: None,
            }],
        };
        assert!(matches!(
            hub.update_touch_input(input.clone()),
            Err(HubError::SourceUnavailable(_))
        ));
        runtime.start_output(&device.control_id()).unwrap();
        hub.update_touch_input(input.clone()).unwrap();
        input.sequence = 2;
        input.pointers[0].x = 0.7;
        hub.update_touch_input(input.clone()).unwrap();
        input.sequence = 1;
        hub.update_touch_input(input).unwrap();
        assert_eq!(runtime.touch_mailbox.lock().unwrap().len(), 1);
        runtime.input_tick().await;
        assert!(runtime.touch_runtimes[&device].has_active_input(std::time::Instant::now()));
        assert_eq!(runtime.snapshot.device.as_ref().unwrap().intensity_a, 10);
        assert!(runtime.sources.contains_key(AUDIO_SOURCE_ID));
        runtime.reset_device_inputs(&device);
        assert!(!runtime.touch_runtimes[&device].has_active_input(std::time::Instant::now()));
        assert!(runtime.touch_mailbox.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn audio_mapping_configuration_is_independent_per_device_and_channel() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 20);
        let device = runtime.selected_device.clone().unwrap();
        let config = AudioChannelConfig {
            gain: 7.0,
            enabled: false,
            ..AudioChannelConfig::default()
        };
        runtime
            .set_audio_config(&device.control_id(), Channel::A, config.clone())
            .await
            .unwrap();
        assert_eq!(
            runtime.audio_bindings[&source_binding(&device, Channel::A)].config,
            config
        );
        assert_eq!(
            runtime.audio_bindings[&source_binding(&device, Channel::B)]
                .config
                .gain,
            2.5
        );
        assert_eq!(
            runtime
                .snapshot
                .input_modes
                .audio_bindings
                .iter()
                .filter(|binding| binding.config.gain == 7.0)
                .count(),
            1
        );
        let invalid = AudioChannelConfig {
            gain: f64::NAN,
            ..AudioChannelConfig::default()
        };
        assert!(
            runtime
                .set_audio_config(&device.control_id(), Channel::A, invalid)
                .await
                .is_err()
        );
        assert_eq!(
            runtime.audio_bindings[&source_binding(&device, Channel::A)].config,
            config
        );
    }

    #[tokio::test]
    async fn touch_mailbox_preserves_edges_rejects_contenders_and_discards_stale_intent() {
        let (hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        let device = runtime.selected_device.clone().unwrap();
        runtime
            .set_device_channel_source(device.control_id(), Channel::A, TOUCH_SOURCE_ID.to_owned())
            .await
            .unwrap();
        runtime.start_output(&device.control_id()).unwrap();
        let mut input = TouchInput {
            device_id: device.control_id(),
            owner_id: "window-1".to_owned(),
            sequence: 1,
            pointers: vec![crate::sources::touch::TouchPointer {
                id: 1,
                x: 0.5,
                y: 0.5,
                cell: None,
            }],
        };
        hub.update_touch_input(input.clone()).unwrap();
        let mut contender = input.clone();
        contender.owner_id = "window-2".to_owned();
        assert!(matches!(
            hub.update_touch_input(contender),
            Err(HubError::SourceUnavailable(_))
        ));
        input.sequence = 2;
        input.pointers[0].x = 0.8;
        hub.update_touch_input(input.clone()).unwrap();
        input.sequence = 3;
        input.pointers.clear();
        hub.update_touch_input(input.clone()).unwrap();
        input.sequence = 4;
        input.pointers.push(crate::sources::touch::TouchPointer {
            id: 2,
            x: 0.5,
            y: 0.5,
            cell: Some(1),
        });
        hub.update_touch_input(input).unwrap();
        {
            let mut mailbox = runtime.touch_mailbox.lock().unwrap();
            let slot = mailbox.get_mut(&device.control_id()).unwrap();
            assert_eq!(slot.latest.owner_id, "window-1");
            assert_eq!(
                slot.transitions
                    .iter()
                    .map(|(input, _)| input.sequence)
                    .collect::<Vec<_>>(),
                [1, 3, 4]
            );
            let expired = std::time::Instant::now() - std::time::Duration::from_secs(2);
            slot.received_at = expired;
            for (_, received_at) in &mut slot.transitions {
                *received_at = expired;
            }
        }
        runtime.input_tick().await;
        assert!(!runtime.touch_runtimes.contains_key(&device));
        assert_eq!(runtime.snapshot.output.state, OutputState::Running);
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
                .map(String::as_str),
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
                .map(String::as_str),
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
        let device = runtime.selected_device.clone().unwrap();
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
                .is_some_and(|source| source == "source-fixed-waveform")
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
                .is_some_and(|source| source == "source-fixed-waveform")
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
                .map(String::as_str),
            Some("source-fixed-waveform")
        );
        assert_eq!(
            runtime
                .device_source_bindings
                .get(&source_binding(&device, Channel::B))
                .map(String::as_str),
            Some("source-fixed-waveform")
        );
    }

    #[tokio::test]
    async fn source_sync_with_ask_each_time_default_unassigns_both_channels() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        let device = runtime.selected_device.clone().unwrap();
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
        let first = runtime.selected_device.clone().unwrap();

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
                .map(String::as_str),
            Some("source-fixed-waveform")
        );
        assert_eq!(
            runtime
                .device_source_bindings
                .get(&source_binding(&second, Channel::B))
                .map(String::as_str),
            Some("source-fixed-waveform")
        );
    }

    #[test]
    fn clearing_default_source_leaves_new_devices_unassigned() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        let first = runtime.selected_device.clone().unwrap();

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
                .map(String::as_str),
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
        let first = runtime.selected_device.clone().unwrap();
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
            .source = Box::new(CountingSource {
            calls: Arc::clone(&calls),
        });

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
            .source = Box::new(CountingSource {
            calls: Arc::clone(&test_calls),
        });
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
        let selected = runtime.selected_device.clone().unwrap();
        runtime.devices.get_mut(&selected).unwrap()["slotState"]["channelA"]["isMuted"] =
            Value::Bool(true);
        runtime.refresh_selected_device_snapshot();

        assert_eq!(runtime.snapshot.channels.a.status, ChannelStatus::Disabled);
        assert_eq!(runtime.start_output(&selected.control_id()), Ok(()));
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
        assert_eq!(runtime.start_output(&selected.control_id()), Ok(()));
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
        assert_eq!(runtime.intensity_lock_corrections(), Vec::new());
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
    fn explicit_device_starts_and_single_disconnect_preserve_parallel_output_set() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        let first = runtime.selected_device.clone().unwrap();
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
        let selected = runtime.selected_device.clone().unwrap();
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
        runtime.selected_device = Some(DeviceKey {
            client_id: "app-1".to_owned(),
            slot_id: "slot-a".to_owned(),
        });
        let devices = runtime.devices.keys().cloned().collect::<Vec<_>>();
        for device in devices {
            runtime.start_output(&device.control_id()).unwrap();
        }

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
    async fn intensity_response_waits_for_matching_patch_without_reapplying_delta() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 20);
        let device = runtime.selected_device.clone().unwrap();
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
        let device = runtime.selected_device.clone().unwrap();
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
    async fn synchronization_uses_explicit_baseline_without_changing_gui_focus() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 10);
        install_test_device(&mut runtime, "app-2", "slot-b", 30);
        let (events, _receiver) = mpsc::channel(8);
        let (relay, relay_task) = spawn_relay_client(events, 8);
        runtime.relay = Some(relay.clone());
        let original_focus = runtime.selected_device.clone();
        let baseline = DeviceKey {
            client_id: "app-2".to_owned(),
            slot_id: "slot-b".to_owned(),
        }
        .control_id();
        runtime
            .set_sync_all_devices_from(Some(baseline), true)
            .unwrap();
        assert_eq!(runtime.selected_device, original_focus);
        let first = IntensityKey {
            device: DeviceKey {
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
    async fn channel_limits_follow_each_reported_device_channel() {
        let (_hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        install_test_device(&mut runtime, "app-1", "slot-a", 90);
        let device = runtime.selected_device.clone().unwrap();
        runtime.devices.get_mut(&device).unwrap()["slotState"]["channelA"]["intensityMax"] =
            Value::from(120);
        runtime.devices.get_mut(&device).unwrap()["slotState"]["channelB"]["intensityMax"] =
            Value::from(7);
        runtime.refresh_selected_device_snapshot();
        assert_eq!(runtime.snapshot.channels.a.limit, 120);
        assert_eq!(runtime.snapshot.channels.b.limit, 7);
        let (event_sender, _events) = mpsc::channel(8);
        let (relay, relay_task) = spawn_relay_client(event_sender, 8);
        runtime.relay = Some(relay.clone());

        assert_eq!(
            runtime.adjust_device_intensity(None, Channel::A, 31),
            Err(HubError::IntensityLimit)
        );
        assert_eq!(
            runtime.adjust_device_intensity(None, Channel::B, 8),
            Err(HubError::IntensityLimit)
        );
        assert_eq!(
            runtime.adjust_device_intensity(None, Channel::A, 15),
            Ok(())
        );
        assert_eq!(runtime.adjust_device_intensity(None, Channel::B, 7), Ok(()));

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

        assert_eq!(
            runtime.snapshot.connection.state,
            ConnectionState::Disconnected
        );
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
            client_id: "app-1".to_owned(),
            slot_id: "slot-a".to_owned(),
        });
        runtime.pending_wave_operations.insert(
            "wave-timeout".to_owned(),
            PendingWaveOperation {
                device: DeviceKey {
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
        runtime.apps.insert("app-1".to_owned());
        runtime.snapshot.output.state = OutputState::Running;
        runtime.pending_wave_operations.insert(
            "wave-1".to_owned(),
            PendingWaveOperation {
                device: DeviceKey {
                    client_id: "app-1".to_owned(),
                    slot_id: "slot-a".to_owned(),
                },
                channel: Channel::A,
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
    async fn late_notification_from_an_already_taken_safety_command_does_not_cancel_its_ack() {
        let (hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        hub.safety_epoch.fetch_add(1, Ordering::AcqRel);
        let (reply, _response) = oneshot::channel();
        hub.safety_commands
            .try_send(HubSafetyCommand::Disconnect(reply))
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
        let stop = tokio::spawn(async move { hub.emergency_stop().await });
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
        assert_eq!(runtime.snapshot.output.state, OutputState::Stopped);

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
            hub.snapshot().connection.state,
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
        hub.connect_relay().await.unwrap();
        wait_for_snapshot(&hub, |snapshot| {
            snapshot.connection.controller_id.as_deref() == Some("explicit-controller")
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
    async fn stop_invalidates_queued_audio_intensity_sync_and_reconnection_commands() {
        let (hub, mut runtime) = create_hub("wss://example.test/v4".to_owned());
        type CommandFactory = fn(oneshot::Sender<Result<(), HubError>>) -> HubCommand;
        let commands: [CommandFactory; 5] = [
            |reply| HubCommand::AudioControl {
                action: AudioAction::StartRecording,
                safety_epoch: 0,
                reply,
            },
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
            |reply| HubCommand::Connect {
                safety_epoch: 0,
                reply,
            },
            |reply| HubCommand::RefreshPairing {
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
        assert!(!runtime.auto_reconnect_enabled);
        assert_eq!(
            runtime.snapshot.connection.state,
            ConnectionState::Disconnected
        );
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
                device_id: runtime.selected_device.as_ref().unwrap().control_id(),
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
        let (source_switch_sender, source_switch_received) = oneshot::channel();
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
            let mut sent_source_switch_signal = false;
            let mut sent_clear_signal = false;
            let mut pulse_sender = Some(pulse_sender);
            let mut source_switch_sender = Some(source_switch_sender);
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
                        emergency_sender.take().unwrap().send(()).unwrap();
                        break;
                    }
                }
            }
        });

        let (hub, runtime) =
            create_hub_with_default_source(endpoint, Some("source-fixed-waveform".to_owned()));
        let runtime_task = tokio::spawn(runtime.run());
        hub.connect_relay().await.unwrap();
        wait_for_snapshot(&hub, |snapshot| snapshot.devices.len() == 2).await;
        assert_eq!(hub.snapshot().connection.state, ConnectionState::Connected);
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
        hub.select_device(second_device.clone()).await.unwrap();
        assert_eq!(
            hub.snapshot().selected_device_id.as_deref(),
            Some(second_device.as_str())
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
        if runtime.devices.is_empty() && runtime.default_source_id.is_none() {
            runtime
                .set_default_source(Some("source-fixed-waveform".to_owned()))
                .unwrap();
        }
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
        runtime.reconcile_connected_devices();
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
