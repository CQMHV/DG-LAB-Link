use crate::model::Channel;
use crate::sources::WaveformConfig;
use crate::transport::{BleParameters, TransportKind};
use crate::waveforms::WaveformFile;
use dg_lab_link_plugin_sdk::{ActionParams, InputParams, UiParams};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "command",
    content = "params",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ControlCommand {
    GetConnections,
    ConnectTransport {
        transport: TransportKind,
        endpoint: Option<String>,
    },
    DisconnectConnection {
        connection_id: String,
    },
    RefreshConnectionPairing {
        connection_id: String,
    },
    SetRelayEndpoint {
        transport: TransportKind,
        endpoint: String,
    },
    ScanBluetooth {
        duration_ms: u64,
    },
    ConnectBluetooth {
        device_id: String,
    },
    DisconnectBluetooth {
        device_id: String,
    },
    SetBluetoothConfig {
        device_id: String,
        config: BleParameters,
    },
    GetBluetoothConfig {
        device_id: String,
    },
    GetHubSnapshot,
    GetAppPreferences,
    SetCloseToTray {
        enabled: bool,
    },
    SetStartMinimized {
        enabled: bool,
    },
    AdjustIntensity {
        device_id: String,
        channel: Channel,
        delta: i32,
    },
    StartOutput {
        device_id: String,
    },
    ClearDeviceChannel {
        device_id: String,
        channel: Channel,
    },
    StopOutput {
        device_id: String,
    },
    ListPlugins,
    InstallPlugin {
        path: String,
    },
    UpdatePlugin {
        path: String,
    },
    UninstallPlugin {
        plugin_id: String,
        #[serde(default)]
        delete_data: bool,
    },
    CreateSource {
        plugin_id: String,
        name: String,
    },
    DeleteSource {
        source_id: String,
        #[serde(default)]
        delete_data: bool,
    },
    SetSourceEnabled {
        source_id: String,
        enabled: bool,
    },
    StartSource {
        source_id: String,
    },
    StopSource {
        source_id: String,
    },
    SetSourceConfig {
        source_id: String,
        config: Value,
        #[serde(default)]
        binding_id: Option<String>,
        expected_revision: u64,
    },
    GetSourceUi {
        source_id: String,
        params: UiParams,
    },
    SourceAction {
        source_id: String,
        params: ActionParams,
    },
    SourceInput {
        source_id: String,
        params: InputParams,
    },
    SetDeviceChannelSource {
        device_id: String,
        channel: Channel,
        source_id: String,
    },
    SetDeviceChannelSourceSync {
        device_id: String,
        enabled: bool,
    },
    SetDefaultSource {
        source_id: Option<String>,
    },
    SetFixedWaveform {
        device_id: String,
        channel: Channel,
        config: WaveformConfig,
    },
    ListWaveforms,
    GetCustomWaveform {
        preset_id: String,
    },
    SelectWaveform {
        device_id: String,
        channel: Channel,
        preset_id: String,
    },
    SelectCustomWaveform {
        device_id: String,
        channel: Channel,
        preset_id: String,
    },
    ImportCustomWaveforms {
        configs: Vec<WaveformConfig>,
    },
    ParseWaveformFiles {
        files: Vec<WaveformFile>,
    },
    ImportWaveformFiles {
        files: Vec<WaveformFile>,
    },
    DeleteCustomWaveform {
        preset_id: String,
    },
    ReorderCustomWaveforms {
        preset_ids: Vec<String>,
    },
    SetSyncAllDevices {
        device_id: String,
        enabled: bool,
    },
    UpdateSafety {
        connection_timeout_enabled: bool,
        connection_timeout_minutes: i32,
        allow_app_intensity_control: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlError {
    pub code: String,
    pub message: String,
}

impl ControlError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ControlError {}

pub struct CommandDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub read_only: bool,
}

impl ControlCommand {
    pub fn from_call(name: &str, params: Value) -> Result<Self, ControlError> {
        let mut value = serde_json::json!({"command": name});
        let unit = matches!(
            name,
            "get_hub_snapshot"
                | "get_connections"
                | "get_app_preferences"
                | "list_plugins"
                | "list_waveforms"
        );
        if !unit || (params != Value::Null && params != serde_json::json!({})) {
            value["params"] = params;
        }
        serde_json::from_value(value)
            .map_err(|error| ControlError::new("invalid_params", error.to_string()))
    }

    pub fn is_safety(&self) -> bool {
        matches!(
            self,
            Self::StopOutput { .. }
                | Self::DisconnectConnection { .. }
                | Self::DisconnectBluetooth { .. }
        )
    }

    pub fn is_business(&self) -> bool {
        !matches!(
            self,
            Self::GetAppPreferences | Self::SetCloseToTray { .. } | Self::SetStartMinimized { .. }
        )
    }

    pub fn persists(&self) -> bool {
        matches!(
            self,
            Self::SetCloseToTray { .. }
                | Self::SetStartMinimized { .. }
                | Self::SetDefaultSource { .. }
                | Self::UpdateSafety { .. }
                | Self::ImportCustomWaveforms { .. }
                | Self::ImportWaveformFiles { .. }
                | Self::DeleteCustomWaveform { .. }
                | Self::ReorderCustomWaveforms { .. }
                | Self::SetRelayEndpoint { .. }
                | Self::SetBluetoothConfig { .. }
                | Self::ConnectTransport { .. }
        )
    }

    /// Every transport uses the same classification, including arbitrary plugin actions.
    pub fn may_resume_output(&self) -> bool {
        matches!(
            self,
            Self::StartOutput { .. }
                | Self::AdjustIntensity { .. }
                | Self::SourceInput { .. }
                | Self::SourceAction { .. }
                | Self::GetSourceUi { .. }
                | Self::StartSource { .. }
                | Self::SetDeviceChannelSource { .. }
                | Self::SetDeviceChannelSourceSync { .. }
                | Self::SetSourceConfig { .. }
                | Self::SetSyncAllDevices { .. }
                | Self::ConnectTransport { .. }
                | Self::RefreshConnectionPairing { .. }
                | Self::ConnectBluetooth { .. }
                | Self::SetBluetoothConfig { .. }
        )
    }

    pub fn descriptors() -> &'static [CommandDescriptor] {
        static DESCRIPTORS: std::sync::OnceLock<Vec<CommandDescriptor>> =
            std::sync::OnceLock::new();
        DESCRIPTORS.get_or_init(|| {
            let schema = serde_json::to_value(schemars::schema_for!(Self)).expect("命令 Schema 可序列化");
            schema["oneOf"].as_array().expect("命令是带标签的枚举").iter().map(|variant| {
                let name = variant["properties"]["command"]["const"].as_str()
                    .or_else(|| variant["properties"]["command"]["enum"][0].as_str())
                    .expect("命令 Schema 含名称").to_owned();
                let mut input_schema = variant["properties"]["params"].clone();
                if input_schema.is_null() {
                    input_schema = serde_json::json!({"type":"object","properties":{},"additionalProperties":false});
                }
                if let Some(definitions) = schema.get("$defs").and_then(Value::as_object) {
                    let mut needed = serde_json::Map::new();
                    collect_definitions(&input_schema, definitions, &mut needed);
                    if !needed.is_empty() { input_schema["$defs"] = Value::Object(needed); }
                }
                let read_only = name != "get_source_ui" && (name.starts_with("get_") || name.starts_with("list_") || name == "parse_waveform_files");
                CommandDescriptor { description: command_description(&name).to_owned(), name, input_schema, read_only }
            }).filter(|descriptor| !matches!(descriptor.name.as_str(), "get_app_preferences" | "set_close_to_tray" | "set_start_minimized")).collect()
        })
    }
}

fn collect_definitions(
    value: &Value,
    definitions: &serde_json::Map<String, Value>,
    needed: &mut serde_json::Map<String, Value>,
) {
    match value {
        Value::Object(object) => {
            if let Some(name) = object
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|reference| reference.strip_prefix("#/$defs/"))
                && !needed.contains_key(name)
                && let Some(definition) = definitions.get(name)
            {
                needed.insert(name.to_owned(), definition.clone());
                collect_definitions(definition, definitions, needed);
            }
            for value in object.values() {
                collect_definitions(value, definitions, needed);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_definitions(value, definitions, needed);
            }
        }
        _ => {}
    }
}

fn command_description(name: &str) -> &str {
    match name {
        "get_hub_snapshot" => "读取共享核心的连接、设备、输入源、输出状态和运行记录。",
        "get_connections" => {
            "读取 Socket V4、Socket V3 与郊狼 3.0 蓝牙连接状态、connectionId 和 APP 配对链接。"
        }
        "connect_transport" => {
            "连接 ws_v4 或 ws_v3 Relay；endpoint 可省略以使用已保存端点，之后读取连接的配对链接。"
        }
        "disconnect_connection" => {
            "按 connectionId 停止并断开指定连接；仅清理所属设备，其他连接继续运行。"
        }
        "refresh_connection_pairing" => {
            "按 connectionId 刷新 V4 或 V3 APP 配对；会断开该连接已有的 APP 和设备。"
        }
        "set_relay_endpoint" => {
            "持久保存 ws_v4 或 ws_v3 端点；连接运行时先断开，支持 ws:// 与 wss://。"
        }
        "scan_bluetooth" => {
            "主动扫描郊狼 3.0 BLE 广播，durationMs 为扫描毫秒数；结果包含发现 deviceId，不自动连接或输出。"
        }
        "connect_bluetooth" => {
            "连接 scan_bluetooth 返回的 deviceId（蓝牙发现标识，不是 controlId）；完成安全初始化后读取新设备的 controlId。"
        }
        "disconnect_bluetooth" => {
            "按设备 controlId（不是扫描 deviceId）停止并断开指定郊狼 3.0 蓝牙设备；其他设备继续运行。"
        }
        "get_bluetooth_config" => {
            "按设备 controlId（不是扫描 deviceId）读取郊狼 3.0 的持久参数，含软上限、频率/强度平衡与旋钮保护。"
        }
        "set_bluetooth_config" => {
            "按设备 controlId（不是扫描 deviceId）校验、下发并持久保存郊狼 3.0 参数；BF 无设备回执，已下发不等于设备已确认。"
        }
        "adjust_intensity" => {
            "按 deviceId（设备 controlId）和 a/b 通道相对调整强度，受设备上限与同步设置约束。"
        }
        "start_output" => "开始指定 deviceId 的输出；两路必须先完成输入源分配。",
        "clear_device_channel" => {
            "清空指定设备的一路旧波形，保持基础强度与输出活动；返回绑定代次，不代表设备已确认。"
        }
        "stop_output" => "停止指定 deviceId 的输出，其他设备继续运行。",
        "list_plugins" => "读取已安装插件、版本、发布者和预装来源。",
        "install_plugin" => "从绝对路径的本地 .dglabplugin 包安装插件。",
        "update_plugin" => "更新本地插件包；保留实例与配置，停止相关输出，失败回滚。",
        "uninstall_plugin" => "卸载插件并解除绑定；deleteData 为 true 时清除保存数据。",
        "create_source" => "从已安装插件创建独立输入源实例。",
        "delete_source" => "删除实例并解除绑定；deleteData 为 true 时清除数据。",
        "set_source_enabled" => "持久保存实例启用状态；启用后按需启动。",
        "start_source" => "启动插件实例进程；设备输出仍需单独开始。",
        "stop_source" => "停止插件实例和关联通道波形；其他实例继续运行。",
        "set_source_config" => {
            "携带 expectedRevision 校验并应用配置；无 bindingId 时保存实例配置，有 bindingId 时更新会话通道配置；版本冲突不应用。"
        }
        "get_source_ui" => "按 settings/control surface 读取公开语义界面与动作 Schema。",
        "source_action" => {
            "调用插件声明的离散动作；配置写入必须使用 set_source_config 并携带 expectedRevision。"
        }
        "source_input" => "提交带 owner、sequence 的持续输入；处理使用独立有界队列。",
        "set_sync_all_devices" => {
            "开启或关闭全设备强度同步；deviceId 明确指定开启同步时的基准设备。"
        }
        "list_waveforms" => "读取内置及自定义波形目录。",
        "select_waveform" => "按 presetId 为指定设备通道选择内置或自定义波形。",
        "import_waveform_files" => "解析并导入 .pulse、JSON 或 .pulses 文本，每个文件最多 2 MiB。",
        "parse_waveform_files" => "解析波形文件文本并返回标准波形，不修改波形库。",
        "update_safety" => "更新并持久保存连接超时和手机反向强度控制设置。",
        "set_device_channel_source" => "为指定设备通道绑定一个输入源。",
        "set_device_channel_source_sync" => "更新指定设备的 A/B 输入源同步设置。",
        "set_default_source" => "更新并保存新设备默认输入源；null 表示每次询问。",
        "set_fixed_waveform" => "为指定设备通道设置完整固定波形配置。",
        "import_custom_waveforms" => "批量校验并导入标准自定义波形配置。",
        "get_custom_waveform" => "读取指定自定义波形完整配置。",
        "select_custom_waveform" => "为指定设备通道选择自定义波形。",
        "delete_custom_waveform" => "删除自定义波形并清理引用它的通道。",
        "reorder_custom_waveforms" => "保存完整自定义波形排序，必须包含全部 ID 且无重复。",
        _ => "共享核心业务操作。",
    }
}

impl From<std::io::Error> for ControlError {
    fn from(error: std::io::Error) -> Self {
        Self::new("io_error", error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_are_cached_and_include_only_transitively_used_definitions() {
        let descriptors = ControlCommand::descriptors();
        assert!(std::ptr::eq(descriptors, ControlCommand::descriptors()));
        let snapshot = descriptors
            .iter()
            .find(|command| command.name == "get_hub_snapshot")
            .unwrap();
        assert!(snapshot.input_schema.get("$defs").is_none());
        for descriptor in descriptors {
            let mut referenced = serde_json::Map::new();
            let definitions = descriptor
                .input_schema
                .get("$defs")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            collect_definitions(&descriptor.input_schema, &definitions, &mut referenced);
            assert_eq!(
                definitions.keys().collect::<Vec<_>>(),
                referenced.keys().collect::<Vec<_>>()
            );
            assert!(
                ControlCommand::from_call(&descriptor.name, serde_json::json!({})).is_ok()
                    || !descriptor.input_schema["required"].is_null()
            );
        }
        assert!(
            serde_json::to_vec(
                &descriptors
                    .iter()
                    .map(|descriptor| &descriptor.input_schema)
                    .collect::<Vec<_>>()
            )
            .unwrap()
            .len()
                < 100_000
        );
        assert!(ControlCommand::from_call("connect_relay", serde_json::json!({})).is_err());
    }
}
