use crate::transport::{
    BleParameters, BluetoothDevice, DeviceCapabilities, InitializationState,
    TransportConnectionSnapshot, TransportKind,
};
use dg_lab_link_plugin_sdk::{Binding as PluginBinding, InstalledPlugin};
use serde::{Deserialize, Serialize};
use serde_json::Value;
const DEFAULT_CONNECTION_TIMEOUT_MINUTES: u16 = 60;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceBindingSnapshot {
    pub source_id: String,
    pub revision: u64,
    #[serde(flatten)]
    pub binding: PluginBinding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Waiting,
    Connected,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutputState {
    Idle,
    Running,
    Stopped,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelStatus {
    Idle,
    Ready,
    Active,
    Disabled,
    Disconnected,
    Fault,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSnapshot {
    pub control_id: String,
    pub connection_id: String,
    pub transport: TransportKind,
    pub initialization: InitializationState,
    pub capabilities: DeviceCapabilities,
    pub ble_parameters: Option<BleParameters>,
    pub configuration_status: Option<String>,
    pub id: Value,
    pub name: String,
    #[serde(rename = "type")]
    pub device_type: String,
    pub slot_id: String,
    pub power: Option<u16>,
    pub intensity_a: u16,
    pub intensity_b: u16,
    pub intensity_limit_a: u16,
    pub intensity_limit_b: u16,
    pub source_id_a: Option<String>,
    pub source_id_b: Option<String>,
    #[serde(default)]
    pub binding_id_a: Option<String>,
    #[serde(default)]
    pub binding_id_b: Option<String>,
    pub waveform_id_a: Option<String>,
    pub waveform_id_b: Option<String>,
    pub waveform_name_a: Option<String>,
    pub waveform_name_b: Option<String>,
    pub source_sync: bool,
    pub output_active: bool,
    pub channel_a_status: ChannelStatus,
    pub channel_b_status: ChannelStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceSnapshot {
    pub id: String,
    pub revision: u64,
    pub kind: String,
    pub name: String,
    pub enabled: bool,
    pub assigned_channel_count: usize,
    pub selected_preset_id: Option<String>,
    pub selected_preset_name: Option<String>,
    #[serde(default)]
    pub plugin_id: Option<String>,
    #[serde(default)]
    pub runtime_status: String,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub config: std::sync::Arc<Value>,
    #[serde(default)]
    pub state: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CustomWaveformSnapshot {
    pub id: String,
    pub name: String,
    pub frame_count: usize,
    pub duration_ms: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OutputSnapshot {
    pub state: OutputState,
    pub frames_sent: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LogSnapshot {
    pub id: String,
    pub level: LogLevel,
    pub message: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HubSnapshot {
    pub revision: u64,
    #[serde(default)]
    pub connections: Vec<TransportConnectionSnapshot>,
    #[serde(default)]
    pub bluetooth: Vec<BluetoothDevice>,
    pub devices: Vec<DeviceSnapshot>,
    pub sync_all_devices: bool,
    pub output_device_count: usize,
    pub sources: Vec<SourceSnapshot>,
    #[serde(default)]
    pub plugins: Vec<InstalledPlugin>,
    #[serde(default)]
    pub source_bindings: Vec<SourceBindingSnapshot>,
    pub custom_waveforms: Vec<CustomWaveformSnapshot>,
    pub default_source_id: Option<String>,
    pub output: OutputSnapshot,
    pub safety: SafetySnapshot,
    pub logs: Vec<LogSnapshot>,
}
