use std::path::PathBuf;
use std::sync::Arc;

use dg_lab_link_plugin_runtime::{BusinessFuture, BusinessHandler, PluginManager};
use dg_lab_link_plugin_sdk::{ActionParams, InputParams, PluginError, SourceSpec, UiParams};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, Semaphore, watch};

use crate::hub::{
    HubError, HubHandle, HubRuntime, HubSnapshot, SafetySnapshot,
    create_hub_with_source_preferences,
};
use crate::model::Channel;
use crate::preferences::{PreferencesError, PreferencesState};
use crate::sources::WaveformConfig;
use crate::sources::audio::{AudioAction, AudioChannelConfig};
use crate::sources::touch::{TouchConfig, TouchInput};
use crate::transport::{BleParameters, DEFAULT_V3_ENDPOINT, TransportAction, TransportKind};
use crate::waveforms::WaveformFile;

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
    ConnectRelay,
    DisconnectRelay,
    RefreshPairing,
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
    SelectDevice {
        device_id: String,
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
    SetTouchConfig {
        config: TouchConfig,
    },
    UpdateTouchInput {
        input: TouchInput,
    },
    SetAudioConfig {
        device_id: String,
        channel: Channel,
        config: AudioChannelConfig,
    },
    AudioControl {
        action: AudioAction,
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

impl From<HubError> for ControlError {
    fn from(error: HubError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

impl From<PreferencesError> for ControlError {
    fn from(error: PreferencesError) -> Self {
        Self::new("preferences_error", error.to_string())
    }
}

impl From<std::io::Error> for ControlError {
    fn from(error: std::io::Error) -> Self {
        Self::new("io_error", error.to_string())
    }
}

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
                | "connect_relay"
                | "disconnect_relay"
                | "refresh_pairing"
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
                | Self::DisconnectRelay
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

    fn persists(&self) -> bool {
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

    pub fn descriptors() -> Vec<CommandDescriptor> {
        let schema =
            serde_json::to_value(schemars::schema_for!(Self)).expect("命令 Schema 可序列化");
        schema["oneOf"].as_array().expect("命令是带标签的枚举").iter().map(|variant| {
            let name = variant["properties"]["command"]["const"].as_str()
                .or_else(|| variant["properties"]["command"]["enum"][0].as_str())
                .expect("命令 Schema 含名称").to_owned();
            let mut input_schema = variant["properties"]["params"].clone();
            if input_schema.is_null() {
                input_schema = serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false});
            }
            if let Some(definitions) = schema.get("$defs") {
                input_schema["$defs"] = definitions.clone();
            }
            let read_only = name != "get_source_ui" && (name.starts_with("get_") || name.starts_with("list_") || name == "parse_waveform_files");
            CommandDescriptor { description: command_description(&name).to_owned(), name, input_schema, read_only }
        }).filter(|descriptor| !matches!(descriptor.name.as_str(), "get_app_preferences" | "set_close_to_tray" | "set_start_minimized")).collect()
    }
}

#[derive(Clone)]
pub struct ControlService {
    hub: HubHandle,
    preferences: Arc<PreferencesState>,
    configuration: Arc<Mutex<()>>,
    configuration_slots: Arc<Semaphore>,
    plugins: PluginManager,
    plugin_configuration: Arc<std::sync::Mutex<std::collections::BTreeMap<String, Arc<Mutex<()>>>>>,
}

impl ControlService {
    pub fn create(
        config_dir: PathBuf,
        relay_endpoint: String,
    ) -> Result<(Self, HubRuntime), ControlError> {
        crate::initialize_tls();
        let preferences = PreferencesState::load(config_dir.clone()).unwrap_or_else(|error| {
            eprintln!("{error}；本次运行使用默认设置");
            PreferencesState::with_defaults(config_dir.clone())
        });
        let (connection_timeout_enabled, connection_timeout_minutes, allow_app_intensity_control) =
            preferences.safety_settings();
        let (hub, mut runtime) = create_hub_with_source_preferences(
            preferences
                .relay_endpoint(TransportKind::WsV4)
                .unwrap_or(relay_endpoint),
            preferences.default_source_id(),
            preferences.fixed_waveform(),
            preferences.custom_waveforms(),
            SafetySnapshot {
                connection_timeout_enabled,
                connection_timeout_minutes,
                allow_app_intensity_control,
            },
        );
        runtime.set_initial_v3_endpoint(
            preferences
                .relay_endpoint(TransportKind::WsV3)
                .unwrap_or_else(|| DEFAULT_V3_ENDPOINT.to_owned()),
        );
        let plugins = PluginManager::open(config_dir.join("plugins")).map_err(plugin_error)?;
        runtime.set_plugin_manager(plugins.clone(), preferences.default_source_id());
        let service = Self {
            hub,
            preferences: Arc::new(preferences),
            configuration: Arc::new(Mutex::new(())),
            configuration_slots: Arc::new(Semaphore::new(8)),
            plugins: plugins.clone(),
            plugin_configuration: Arc::new(
                std::sync::Mutex::new(std::collections::BTreeMap::new()),
            ),
        };
        plugins.set_business_handler(Arc::new(CorePluginBusiness {
            service: service.clone(),
        }));
        Ok((service, runtime))
    }

    pub async fn initialize_plugins(&self) -> Result<(), ControlError> {
        let executable = std::env::current_exe()?;
        let mut binary_dir = executable
            .parent()
            .ok_or_else(|| ControlError::new("plugin_bundle_missing", "无法找到程序目录"))?
            .to_path_buf();
        if binary_dir.file_name().is_some_and(|name| name == "deps") {
            binary_dir.pop();
        }
        let packages = [
            (
                "cn.dglab.link.touch",
                "source-touch",
                "触控",
                serde_json::to_value(self.preferences.touch_config())
                    .map_err(|e| ControlError::new("invalid_params", e.to_string()))?,
            ),
            (
                "cn.dglab.link.audio",
                "source-audio",
                "音频",
                serde_json::json!({}),
            ),
        ]
        .into_iter()
        .filter_map(|(plugin_id, id, name, config)| {
            let path = binary_dir
                .join("plugins")
                .join(format!("{plugin_id}.dglabplugin"));
            path.is_file().then(|| {
                (
                    path,
                    SourceSpec {
                        id: id.into(),
                        plugin_id: plugin_id.into(),
                        name: name.into(),
                        enabled: true,
                        config,
                    },
                )
            })
        })
        .collect();
        self.plugins
            .seed_preinstalled(packages)
            .await
            .map_err(plugin_error)?;
        self.hub.refresh_plugins().await?;
        if let Some(id) = self.preferences.default_source_id() {
            let _ = self.hub.set_default_source(Some(id)).await;
        }
        Ok(())
    }

    pub fn snapshot(&self) -> HubSnapshot {
        self.hub.snapshot()
    }
    pub fn subscribe(&self) -> watch::Receiver<HubSnapshot> {
        self.hub.subscribe()
    }
    pub async fn shutdown(&self) -> Result<(), ControlError> {
        let (result, ()) = tokio::join!(self.hub.shutdown_gracefully(), self.plugins.shutdown());
        self.plugins.clear_business_handler();
        result.map_err(Into::into)
    }

    pub async fn execute(&self, command: ControlCommand) -> Result<Value, ControlError> {
        let plugin_transaction = matches!(
            &command,
            ControlCommand::SetSourceConfig { .. }
                | ControlCommand::SetTouchConfig { .. }
                | ControlCommand::InstallPlugin { .. }
                | ControlCommand::UpdatePlugin { .. }
                | ControlCommand::UninstallPlugin { .. }
                | ControlCommand::CreateSource { .. }
                | ControlCommand::DeleteSource { .. }
                | ControlCommand::SetSourceEnabled { .. }
        ) || matches!(&command, ControlCommand::SourceAction { params, .. } if params.action == "configure");
        if plugin_transaction {
            let permit = self
                .configuration_slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| ControlError::from(HubError::QueueBusy))?;
            let service = self.clone();
            return tokio::spawn(async move {
                let _permit = permit;
                service.execute_inner(command).await
            })
            .await
            .map_err(|error| ControlError::new("internal_error", error.to_string()))?;
        }
        if command.persists() {
            let permit = self
                .configuration_slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| ControlError::from(HubError::QueueBusy))?;
            let service = self.clone();
            let accepted_epoch = self.hub.safety_generation();
            let may_reconnect = matches!(
                command,
                ControlCommand::ConnectTransport { .. } | ControlCommand::SetBluetoothConfig { .. }
            );
            // A disconnected caller must not cancel a partially persisted transaction.
            return tokio::spawn(async move {
                let _permit = permit;
                let _transaction = service.configuration.lock().await;
                if may_reconnect && accepted_epoch != service.hub.safety_generation() {
                    return Err(HubError::QueueBusy.into());
                }
                service.execute_inner(command).await
            })
            .await
            .map_err(|error| ControlError::new("internal_error", error.to_string()))?;
        }
        self.execute_inner(command).await
    }

    async fn execute_inner(&self, command: ControlCommand) -> Result<Value, ControlError> {
        use ControlCommand::*;
        match command {
            GetConnections => return serialize(self.snapshot().connections),
            ConnectTransport {
                transport,
                endpoint,
            } => {
                let endpoint = endpoint
                    .or_else(|| self.preferences.relay_endpoint(transport))
                    .unwrap_or_else(|| {
                        if transport == TransportKind::WsV3 {
                            DEFAULT_V3_ENDPOINT.to_owned()
                        } else {
                            self.snapshot().connection.endpoint
                        }
                    });
                self.set_relay_endpoint(transport, endpoint.clone()).await?;
                self.hub
                    .transport(TransportAction::Connect {
                        transport,
                        endpoint,
                    })
                    .await?;
            }
            SetRelayEndpoint {
                transport,
                endpoint,
            } => self.set_relay_endpoint(transport, endpoint).await?,
            DisconnectConnection { connection_id } => {
                self.hub.disconnect_connection(connection_id).await?
            }
            RefreshConnectionPairing { connection_id } => {
                self.hub
                    .transport(TransportAction::RefreshPairing { connection_id })
                    .await?;
            }
            ScanBluetooth { duration_ms } => {
                return self
                    .hub
                    .transport(TransportAction::Scan { duration_ms })
                    .await
                    .map_err(Into::into);
            }
            ConnectBluetooth { device_id } => {
                let parameters = self.preferences.ble_parameters(&device_id);
                self.hub
                    .transport(TransportAction::ConnectBluetooth {
                        device_id,
                        parameters,
                    })
                    .await?;
            }
            DisconnectBluetooth { device_id } => {
                let connection_id = self.bluetooth_device(&device_id)?.connection_id;
                self.hub.disconnect_connection(connection_id).await?;
            }
            GetBluetoothConfig { device_id } => {
                return serialize(
                    self.bluetooth_device(&device_id)?
                        .ble_parameters
                        .unwrap_or_default(),
                );
            }
            SetBluetoothConfig { device_id, config } => {
                config.validate().map_err(HubError::from)?;
                let device = self.bluetooth_device(&device_id)?;
                let peripheral_id = device
                    .id
                    .as_str()
                    .ok_or_else(|| ControlError::new("device_unavailable", "蓝牙设备标识缺失"))?
                    .to_owned();
                let previous = self.preferences.ble_parameters(&peripheral_id);
                if let Err(error) = self
                    .hub
                    .transport(TransportAction::ConfigureBluetooth {
                        device_id: device_id.clone(),
                        parameters: config.clone(),
                    })
                    .await
                {
                    if let Err(rollback) = self
                        .hub
                        .transport(TransportAction::ConfigureBluetooth {
                            device_id: device_id.clone(),
                            parameters: previous.clone(),
                        })
                        .await
                    {
                        let _ = self
                            .hub
                            .disconnect_connection(device.connection_id.clone())
                            .await;
                        return Err(ControlError::new(
                            "rollback_failed",
                            format!("{error}；恢复旧蓝牙配置失败：{rollback}"),
                        ));
                    }
                    return Err(error.into());
                }
                if let Err(error) = self.preferences.set_ble_parameters(peripheral_id, config) {
                    if let Err(rollback) = self
                        .hub
                        .transport(TransportAction::ConfigureBluetooth {
                            device_id: device_id.clone(),
                            parameters: previous,
                        })
                        .await
                    {
                        let _ = self.hub.disconnect_connection(device.connection_id).await;
                        return Err(rollback_error(&error, rollback));
                    }
                    return Err(error.into());
                }
            }
            GetHubSnapshot => return serialize(self.snapshot()),
            GetAppPreferences => return serialize(self.preferences.snapshot(false)),
            SetCloseToTray { enabled } => self.preferences.set_close_to_tray(enabled)?,
            SetStartMinimized { enabled } => self.preferences.set_start_minimized(enabled)?,
            ConnectRelay => self.hub.connect_relay().await?,
            DisconnectRelay => self.hub.disconnect_relay().await?,
            RefreshPairing => self.hub.refresh_pairing().await?,
            AdjustIntensity {
                device_id,
                channel,
                delta,
            } => {
                self.hub
                    .adjust_device_intensity(Some(device_id), channel, delta)
                    .await?
            }
            StartOutput { device_id } => {
                let epoch = self.hub.safety_generation();
                let device = self
                    .snapshot()
                    .devices
                    .into_iter()
                    .find(|device| device.control_id == device_id)
                    .ok_or(HubError::DeviceUnavailable)?;
                for source_id in [device.source_id_a, device.source_id_b]
                    .into_iter()
                    .flatten()
                {
                    if self
                        .plugins
                        .snapshot()
                        .sources
                        .iter()
                        .any(|source| source.spec.id == source_id)
                    {
                        self.plugins.start(&source_id).await.map_err(plugin_error)?;
                    }
                }
                self.hub.refresh_plugins().await?;
                if epoch != self.hub.safety_generation() {
                    return Err(HubError::QueueBusy.into());
                }
                self.hub.start_output(device_id).await?;
            }
            ClearDeviceChannel { device_id, channel } => {
                let generation = self
                    .hub
                    .clear_device_channel(device_id.clone(), channel)
                    .await?;
                return Ok(
                    serde_json::json!({"bindingId":format!("{device_id}/{channel}"),"generation":generation}),
                );
            }
            StopOutput { device_id } => self.hub.stop_output(device_id).await?,
            ListPlugins => return serialize(self.plugins.snapshot().plugins),
            InstallPlugin { path } => {
                let plugin = self
                    .plugins
                    .install(absolute_path(&path)?, false)
                    .await
                    .map_err(plugin_error)?;
                self.hub.refresh_plugins().await?;
                return serialize(plugin);
            }
            UpdatePlugin { path } => {
                let plugin = self
                    .plugins
                    .update(absolute_path(&path)?)
                    .await
                    .map_err(plugin_error)?;
                self.hub.refresh_plugins().await?;
                return serialize(plugin);
            }
            UninstallPlugin {
                plugin_id,
                delete_data,
            } => {
                self.plugins
                    .uninstall(&plugin_id, delete_data)
                    .await
                    .map_err(plugin_error)?;
                self.hub.refresh_plugins().await?;
            }
            CreateSource { plugin_id, name } => {
                let source = SourceSpec {
                    id: uuid::Uuid::new_v4().to_string(),
                    plugin_id,
                    name,
                    enabled: true,
                    config: serde_json::json!({}),
                };
                let state = self
                    .plugins
                    .create_source(source)
                    .await
                    .map_err(plugin_error)?;
                self.hub.refresh_plugins().await?;
                return serialize(state);
            }
            DeleteSource {
                source_id,
                delete_data,
            } => {
                self.plugins
                    .delete_source(&source_id, delete_data)
                    .await
                    .map_err(plugin_error)?;
                self.hub.refresh_plugins().await?;
            }
            SetSourceEnabled { source_id, enabled } => {
                self.plugins
                    .set_enabled(&source_id, enabled)
                    .await
                    .map_err(plugin_error)?;
                self.hub.refresh_plugins().await?;
            }
            StartSource { source_id } => {
                self.plugins.start(&source_id).await.map_err(plugin_error)?;
                self.hub.refresh_plugins().await?;
            }
            StopSource { source_id } => {
                self.plugins.stop(&source_id).await.map_err(plugin_error)?;
                self.hub.refresh_plugins().await?;
            }
            SetSourceConfig {
                source_id,
                config,
                binding_id,
            } => return self.configure_source(source_id, config, binding_id).await,
            GetSourceUi { source_id, params } => {
                return serialize(
                    self.plugins
                        .ui(&source_id, params)
                        .await
                        .map_err(plugin_error)?,
                );
            }
            SourceAction { source_id, params } => {
                if params.action == "configure" {
                    return self
                        .configure_source(source_id, params.value, params.binding_id)
                        .await;
                }
                let value = self
                    .plugins
                    .action(&source_id, params)
                    .await
                    .map_err(plugin_error)?;
                self.hub.refresh_plugins().await?;
                return Ok(value);
            }
            SourceInput { source_id, params } => {
                self.plugins
                    .try_input(&source_id, params)
                    .map_err(plugin_error)?;
            }
            SetDeviceChannelSource {
                device_id,
                channel,
                source_id,
            } => {
                self.hub
                    .set_device_channel_source(device_id, channel, source_id)
                    .await?
            }
            SetDeviceChannelSourceSync { device_id, enabled } => {
                self.hub
                    .set_device_channel_source_sync(device_id, enabled)
                    .await?
            }
            SetDefaultSource { source_id } => {
                let previous = self.snapshot().default_source_id;
                self.hub.set_default_source(source_id.clone()).await?;
                if let Err(error) = self.preferences.set_default_source_id(source_id) {
                    self.hub
                        .set_default_source(previous)
                        .await
                        .map_err(|rollback| rollback_error(&error, rollback))?;
                    return Err(error.into());
                }
            }
            SetFixedWaveform {
                device_id,
                channel,
                config,
            } => {
                self.hub
                    .set_fixed_waveform(device_id, channel, Some(config))
                    .await?
            }
            ListWaveforms => {
                return Ok(
                    serde_json::json!({"official": crate::waveforms::official_waveforms(), "custom": self.preferences.custom_waveforms()}),
                );
            }
            GetCustomWaveform { preset_id } => return serialize(self.custom_waveform(&preset_id)?),
            SelectWaveform {
                device_id,
                channel,
                preset_id,
            } => {
                let config = crate::waveforms::official_waveforms()
                    .iter()
                    .find(|waveform| waveform.config.preset_id == preset_id)
                    .map(|waveform| waveform.config.clone())
                    .map(Ok)
                    .unwrap_or_else(|| self.custom_waveform(&preset_id))?;
                self.hub
                    .set_fixed_waveform(device_id, channel, Some(config))
                    .await?;
            }
            SelectCustomWaveform {
                device_id,
                channel,
                preset_id,
            } => {
                self.hub
                    .set_fixed_waveform(device_id, channel, Some(self.custom_waveform(&preset_id)?))
                    .await?
            }
            ImportCustomWaveforms { configs } => self.import_waveforms(configs).await?,
            ParseWaveformFiles { files } => {
                return serialize(crate::waveforms::parse_files(&files)?);
            }
            ImportWaveformFiles { files } => {
                self.import_waveforms(crate::waveforms::parse_files(&files)?)
                    .await?
            }
            DeleteCustomWaveform { preset_id } => {
                let mut waveforms = self.preferences.custom_waveforms();
                let index = waveforms
                    .iter()
                    .position(|item| item.preset_id == preset_id)
                    .ok_or_else(|| {
                        ControlError::from(HubError::InvalidSourceConfig(
                            "要删除的自定义波形不存在".to_owned(),
                        ))
                    })?;
                waveforms.remove(index);
                let selected = self
                    .preferences
                    .fixed_waveform()
                    .filter(|item| item.preset_id != preset_id);
                self.apply_waveform_state(selected, waveforms).await?;
            }
            ReorderCustomWaveforms { preset_ids } => {
                let waveforms = self.preferences.custom_waveforms();
                if preset_ids.len() != waveforms.len() {
                    return Err(HubError::InvalidSourceConfig(
                        "排序结果必须包含全部自定义波形".to_owned(),
                    )
                    .into());
                }
                let mut remaining = waveforms
                    .into_iter()
                    .map(|waveform| (waveform.preset_id.clone(), waveform))
                    .collect::<std::collections::BTreeMap<_, _>>();
                let reordered = preset_ids
                    .into_iter()
                    .map(|id| {
                        remaining.remove(&id).ok_or_else(|| {
                            ControlError::from(HubError::InvalidSourceConfig(
                                "排序结果包含未知或重复的自定义波形".to_owned(),
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.apply_waveform_state(self.preferences.fixed_waveform(), reordered)
                    .await?;
            }
            SelectDevice { device_id } => self.hub.select_device(device_id).await?,
            SetSyncAllDevices { device_id, enabled } => {
                self.hub.set_sync_all_devices(device_id, enabled).await?
            }
            UpdateSafety {
                connection_timeout_enabled,
                connection_timeout_minutes,
                allow_app_intensity_control,
            } => {
                if !(1..=1440).contains(&connection_timeout_minutes) {
                    return Err(HubError::InvalidConnectionTimeout.into());
                }
                let previous = self.snapshot().safety;
                self.hub
                    .update_safety(
                        connection_timeout_enabled,
                        connection_timeout_minutes,
                        allow_app_intensity_control,
                    )
                    .await?;
                if let Err(error) = self.preferences.set_safety_settings(
                    connection_timeout_enabled,
                    connection_timeout_minutes as u16,
                    allow_app_intensity_control,
                ) {
                    self.hub
                        .update_safety(
                            previous.connection_timeout_enabled,
                            i32::from(previous.connection_timeout_minutes),
                            previous.allow_app_intensity_control,
                        )
                        .await
                        .map_err(|rollback| rollback_error(&error, rollback))?;
                    return Err(error.into());
                }
            }
            SetTouchConfig { config } => {
                return self
                    .configure_source("source-touch".into(), serialize(config)?, None)
                    .await;
            }
            UpdateTouchInput { input } => {
                let device = self
                    .snapshot()
                    .devices
                    .into_iter()
                    .find(|device| device.control_id == input.device_id)
                    .ok_or(HubError::DeviceUnavailable)?;
                if !device.output_active {
                    return Err(HubError::SourceUnavailable("请先开始触控输出".into()).into());
                }
                self.plugins
                    .try_input(
                        "source-touch",
                        InputParams {
                            action: "update_touch_input".into(),
                            value: serialize(&input)?,
                            binding_id: None,
                            owner: input.owner_id,
                            sequence: input.sequence,
                        },
                    )
                    .map_err(plugin_error)?;
            }
            SetAudioConfig {
                device_id,
                channel,
                config,
            } => {
                config
                    .validate()
                    .map_err(|e| ControlError::new("invalid_source_config", e.to_string()))?;
                let device = self
                    .snapshot()
                    .devices
                    .into_iter()
                    .find(|device| device.control_id == device_id)
                    .ok_or(HubError::DeviceUnavailable)?;
                let (source, binding) = match channel {
                    Channel::A => (device.source_id_a, device.binding_id_a),
                    Channel::B => (device.source_id_b, device.binding_id_b),
                };
                if source.as_deref() != Some("source-audio") {
                    return Err(
                        HubError::SourceUnavailable("目标通道未绑定默认音频实例".into()).into(),
                    );
                }
                return self
                    .configure_source("source-audio".into(), serialize(config)?, binding)
                    .await;
            }
            AudioControl { action } => {
                if let AudioAction::LoadFile { path } | AudioAction::SaveRecording { path } =
                    &action
                    && !std::path::Path::new(path).is_absolute()
                {
                    return Err(ControlError::new(
                        "invalid_params",
                        "音频文件和录音保存路径必须为绝对路径",
                    ));
                }
                self.plugins
                    .action(
                        "source-audio",
                        ActionParams {
                            action: "audio_control".into(),
                            value: serde_json::json!({"action":action}),
                            binding_id: None,
                        },
                    )
                    .await
                    .map_err(plugin_error)?;
                self.hub.refresh_plugins().await?;
            }
        }
        Ok(Value::Null)
    }

    async fn configure_source(
        &self,
        source_id: String,
        config: Value,
        binding_id: Option<String>,
    ) -> Result<Value, ControlError> {
        let live_sources = self
            .plugins
            .snapshot()
            .sources
            .into_iter()
            .map(|source| source.spec.id)
            .collect::<std::collections::BTreeSet<_>>();
        if !live_sources.contains(&source_id) {
            return Err(ControlError::new("unknown_source", "输入源实例不存在"));
        }
        let gate = {
            let mut gates = self
                .plugin_configuration
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            gates.retain(|_, gate| Arc::strong_count(gate) > 1);
            if !gates.contains_key(&source_id) && gates.len() >= 64 {
                return Err(HubError::QueueBusy.into());
            }
            gates
                .entry(source_id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _gate = gate
            .try_lock_owned()
            .map_err(|_| ControlError::from(HubError::QueueBusy))?;
        if let Some(binding_id) = binding_id {
            let belongs = self.snapshot().devices.iter().any(|device| {
                (device.binding_id_a.as_ref() == Some(&binding_id)
                    && device.source_id_a.as_ref() == Some(&source_id))
                    || (device.binding_id_b.as_ref() == Some(&binding_id)
                        && device.source_id_b.as_ref() == Some(&source_id))
            });
            if !belongs {
                return Err(ControlError::new("source_unavailable", "通道绑定已变更"));
            }
            let previous = self
                .snapshot()
                .source_bindings
                .into_iter()
                .find(|binding| {
                    binding.source_id == source_id && binding.binding.binding_id == binding_id
                })
                .map(|binding| binding.binding.config)
                .unwrap_or_else(|| serde_json::json!({}));
            let make_action = |config: Value, validate_only: bool| ActionParams {
                action: "configure_binding".into(),
                value: serde_json::json!({"bindingId":binding_id,"config":config,"validateOnly":validate_only}),
                binding_id: Some(binding_id.clone()),
            };
            self.plugins
                .action(&source_id, make_action(config.clone(), true))
                .await
                .map_err(plugin_error)?;
            let value = match self
                .plugins
                .action(&source_id, make_action(config.clone(), false))
                .await
            {
                Ok(value) => value,
                Err(error) => {
                    if let Err(rollback) = self
                        .plugins
                        .action(&source_id, make_action(previous, false))
                        .await
                    {
                        let _ = self.plugins.stop(&source_id).await;
                        let _ = self.hub.refresh_plugins().await;
                        return Err(ControlError::new(
                            "rollback_failed",
                            format!("{error}；恢复旧通道配置失败：{rollback}"),
                        ));
                    }
                    return Err(plugin_error(error));
                }
            };
            if let Err(error) = self
                .hub
                .set_plugin_binding_config(source_id.clone(), binding_id.clone(), config)
                .await
            {
                if let Err(rollback) = self
                    .plugins
                    .action(&source_id, make_action(previous, false))
                    .await
                {
                    let _ = self.plugins.stop(&source_id).await;
                    let _ = self.hub.refresh_plugins().await;
                    return Err(ControlError::new(
                        "rollback_failed",
                        format!("{error}；恢复旧通道配置失败：{rollback}"),
                    ));
                }
                return Err(error.into());
            }
            Ok(value)
        } else {
            let value = self
                .plugins
                .configure(&source_id, config)
                .await
                .map_err(plugin_error)?;
            self.hub.refresh_plugins().await?;
            Ok(value)
        }
    }

    fn bluetooth_device(
        &self,
        device_id: &str,
    ) -> Result<crate::hub::DeviceSnapshot, ControlError> {
        self.snapshot()
            .devices
            .into_iter()
            .find(|device| device.control_id == device_id && device.transport == TransportKind::Ble)
            .ok_or_else(|| {
                ControlError::new("device_unavailable", "指定的蓝牙 controlId 不存在或已断开")
            })
    }

    async fn set_relay_endpoint(
        &self,
        transport: TransportKind,
        endpoint: String,
    ) -> Result<(), ControlError> {
        let previous = self
            .snapshot()
            .connections
            .into_iter()
            .find(|connection| connection.transport == transport)
            .map(|connection| connection.endpoint);
        self.hub
            .transport(TransportAction::SetEndpoint {
                transport,
                endpoint: endpoint.clone(),
            })
            .await?;
        if let Err(error) = self.preferences.set_relay_endpoint(transport, endpoint) {
            if let Some(endpoint) = previous {
                self.hub
                    .transport(TransportAction::SetEndpoint {
                        transport,
                        endpoint,
                    })
                    .await
                    .map_err(|rollback| rollback_error(&error, rollback))?;
            }
            return Err(error.into());
        }
        Ok(())
    }

    fn custom_waveform(&self, preset_id: &str) -> Result<WaveformConfig, ControlError> {
        self.preferences
            .custom_waveforms()
            .into_iter()
            .find(|waveform| waveform.preset_id == preset_id)
            .ok_or_else(|| HubError::InvalidSourceConfig("自定义波形不存在".to_owned()).into())
    }

    async fn import_waveforms(&self, configs: Vec<WaveformConfig>) -> Result<(), ControlError> {
        if configs.is_empty() {
            return Ok(());
        }
        let mut waveforms = self.preferences.custom_waveforms();
        waveforms.extend(configs);
        self.apply_waveform_state(self.preferences.fixed_waveform(), waveforms)
            .await
    }

    async fn apply_waveform_state(
        &self,
        selected: Option<WaveformConfig>,
        waveforms: Vec<WaveformConfig>,
    ) -> Result<(), ControlError> {
        crate::hub::validate_waveform_library(&waveforms, selected.as_ref())?;
        let previous = self.preferences.custom_waveforms();
        let previous_selected = self.preferences.fixed_waveform();
        self.preferences
            .set_waveform_state(selected.clone(), waveforms.clone())?;
        if let Err(error) = self.hub.set_waveform_state(waveforms, selected).await {
            self.preferences
                .set_waveform_state(previous_selected, previous)
                .map_err(|rollback| {
                    ControlError::new(
                        "rollback_error",
                        format!("{error}；偏好回滚失败：{rollback}"),
                    )
                })?;
            return Err(error.into());
        }
        Ok(())
    }
}

fn plugin_error(error: PluginError) -> ControlError {
    ControlError::new(error.code, error.message)
}
fn absolute_path(path: &str) -> Result<PathBuf, ControlError> {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(ControlError::new(
            "invalid_params",
            "插件包路径必须是绝对路径",
        ))
    }
}
struct CorePluginBusiness {
    service: ControlService,
}
impl BusinessHandler for CorePluginBusiness {
    fn call<'a>(&'a self, _source_id: &'a str, command: Value) -> BusinessFuture<'a> {
        Box::pin(async move {
            let command: ControlCommand = serde_json::from_value(command)
                .map_err(|error| PluginError::new("invalid_params", error.to_string()))?;
            if !command.is_business() {
                return Err(PluginError::new(
                    "invalid_command",
                    "插件接口仅开放核心业务命令",
                ));
            }
            self.service
                .execute(command)
                .await
                .map_err(|error| PluginError::new(error.code, error.message))
        })
    }
}

fn serialize(value: impl Serialize) -> Result<Value, ControlError> {
    serde_json::to_value(value)
        .map_err(|error| ControlError::new("internal_error", error.to_string()))
}

fn rollback_error(error: &PreferencesError, rollback: HubError) -> ControlError {
    ControlError::new(
        "rollback_error",
        format!("{error}；运行状态回滚失败：{rollback}"),
    )
}

fn command_description(name: &str) -> &str {
    match name {
        "get_hub_snapshot" => "读取共享核心的连接、设备、输入源、输出状态和运行记录。",
        "connect_relay" => "连接 Socket V4 Relay；连接成功后读取快照中的配对链接。",
        "disconnect_relay" => "停止 Socket V4 所属设备输出并断开 V4 Relay；其他传输继续运行。",
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
        "refresh_pairing" => "刷新配对链接；可能断开已有设备，请先读取状态。",
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
            "校验并应用配置；无 bindingId 时持久保存实例配置，有 bindingId 时更新当前会话通道配置。"
        }
        "get_source_ui" => "按 settings/control surface 读取公开语义界面与动作 Schema。",
        "source_action" => "调用插件声明的动作；configure 使用核心配置事务。",
        "source_input" => "提交带 owner、sequence 的持续输入；处理使用独立有界队列。",
        "update_touch_input" => {
            "提交触控坐标；ownerId、递增 sequence 和一秒租期限制同 GUI，不续租会释放触点。"
        }
        "audio_control" => "控制本机音频播放、采集或录音；文件及录音保存路径必须为绝对路径。",
        "set_sync_all_devices" => {
            "开启或关闭全设备强度同步；deviceId 明确指定开启同步时的基准设备。"
        }
        "list_waveforms" => "读取内置及自定义波形目录。",
        "select_waveform" => "按 presetId 为指定设备通道选择内置或自定义波形。",
        "import_waveform_files" => "解析并导入 .pulse、JSON 或 .pulses 文本，每个文件最多 2 MiB。",
        "parse_waveform_files" => "解析波形文件文本并返回标准波形，不修改波形库。",
        "update_safety" => "更新并持久保存连接超时和手机反向强度控制设置。",
        "set_touch_config" => "校验并持久保存触控配置。",
        "set_audio_config" => "更新指定设备通道的音频映射。",
        "set_device_channel_source" => "为指定设备通道绑定一个输入源。",
        "set_device_channel_source_sync" => "更新指定设备的 A/B 输入源同步设置。",
        "set_default_source" => "更新并保存新设备默认输入源；null 表示每次询问。",
        "set_fixed_waveform" => "为指定设备通道设置完整固定波形配置。",
        "import_custom_waveforms" => "批量校验并导入标准自定义波形配置。",
        "get_custom_waveform" => "读取指定自定义波形完整配置。",
        "select_custom_waveform" => "为指定设备通道选择自定义波形。",
        "delete_custom_waveform" => "删除自定义波形并清理引用它的通道。",
        "reorder_custom_waveforms" => "保存完整自定义波形排序，必须包含全部 ID 且无重复。",
        "select_device" => "显式切换共享 GUI 控制焦点；其他设备写操作均应直接提供 deviceId。",
        _ => "共享核心业务操作。",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dglab::client::DEFAULT_RELAY_ENDPOINT;
    use std::time::Duration;

    #[test]
    fn public_calls_require_explicit_devices_and_describe_all_business_commands() {
        let missing = ControlCommand::from_call(
            "adjust_intensity",
            serde_json::json!({"channel": "a", "delta": 1}),
        )
        .unwrap_err();
        assert_eq!(missing.code, "invalid_params");
        assert!(
            ControlCommand::from_call(
                "start_output",
                serde_json::json!({"deviceId":"test", "typo":true})
            )
            .is_err()
        );
        assert!(ControlCommand::from_call("emergency_stop", serde_json::json!({})).is_err());
        let descriptors = ControlCommand::descriptors();
        let names = descriptors
            .iter()
            .map(|descriptor| descriptor.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(names.len(), descriptors.len());
        assert!(
            names.contains("source_action")
                && names.contains("audio_control")
                && names.contains("import_waveform_files")
        );
        let intensity = descriptors
            .iter()
            .find(|command| command.name == "adjust_intensity")
            .unwrap();
        assert_eq!(
            intensity.input_schema["properties"]["deviceId"]["type"],
            "string"
        );
        assert!(
            intensity.input_schema["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("deviceId"))
        );
        assert!(
            descriptors
                .iter()
                .all(|descriptor| !descriptor.name.contains("tray"))
        );
    }

    #[tokio::test]
    async fn concurrent_waveform_imports_preserve_both_transactions_and_invalid_input_does_not_persist()
     {
        let directory = tempfile::tempdir().unwrap();
        let (service, runtime) = ControlService::create(
            directory.path().to_path_buf(),
            DEFAULT_RELAY_ENDPOINT.to_owned(),
        )
        .unwrap();
        let runtime = tokio::spawn(runtime.run());
        let waveform = |id: &str| WaveformConfig {
            preset_id: id.to_owned(),
            preset_name: id.to_owned(),
            frames: vec!["0A0A0A0A00643200".to_owned()],
        };
        let (first, second) = tokio::join!(
            service.execute(ControlCommand::ImportCustomWaveforms {
                configs: vec![waveform("one")]
            }),
            service.execute(ControlCommand::ImportCustomWaveforms {
                configs: vec![waveform("two")]
            }),
        );
        first.unwrap();
        second.unwrap();
        let saved = std::fs::read(directory.path().join("preferences.json")).unwrap();
        assert_eq!(service.snapshot().custom_waveforms.len(), 2);
        let mut invalid = waveform("invalid");
        invalid.frames = vec!["090A0A0A00643200".to_owned()];
        assert!(
            service
                .execute(ControlCommand::ImportCustomWaveforms {
                    configs: vec![invalid]
                })
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(directory.path().join("preferences.json")).unwrap(),
            saved
        );
        assert_eq!(
            PreferencesState::load(directory.path().to_path_buf())
                .unwrap()
                .custom_waveforms()
                .len(),
            2
        );
        service.shutdown().await.unwrap();
        runtime.await.unwrap();
    }

    #[tokio::test]
    async fn failed_persistence_rolls_back_runtime_safety_and_stop_bypasses_configuration_lock() {
        let directory = tempfile::tempdir().unwrap();
        let (service, runtime) = ControlService::create(
            directory.path().to_path_buf(),
            DEFAULT_RELAY_ENDPOINT.to_owned(),
        )
        .unwrap();
        let runtime = tokio::spawn(runtime.run());
        let previous = service.snapshot().safety;
        std::fs::create_dir(directory.path().join("preferences.json")).unwrap();
        let error = service
            .execute(ControlCommand::UpdateSafety {
                connection_timeout_enabled: true,
                connection_timeout_minutes: 15,
                allow_app_intensity_control: true,
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "preferences_error");
        assert_eq!(service.snapshot().safety, previous);
        let _guard = service.configuration.lock().await;
        tokio::time::timeout(
            Duration::from_secs(1),
            service.execute(ControlCommand::DisconnectRelay),
        )
        .await
        .unwrap()
        .unwrap();
        service.shutdown().await.unwrap();
        runtime.await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_caller_does_not_abandon_persistent_transaction() {
        let directory = tempfile::tempdir().unwrap();
        let (service, runtime) = ControlService::create(
            directory.path().to_path_buf(),
            DEFAULT_RELAY_ENDPOINT.to_owned(),
        )
        .unwrap();
        let pending = service.clone();
        let request = tokio::spawn(async move {
            pending
                .execute(ControlCommand::SetDefaultSource {
                    source_id: Some("source-fixed-waveform".to_owned()),
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while service.configuration.try_lock().is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        request.abort();
        let runtime = tokio::spawn(runtime.run());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(state) = PreferencesState::load(directory.path().to_path_buf())
                    && state.default_source_id().as_deref() == Some("source-fixed-waveform")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            service.snapshot().default_source_id.as_deref(),
            Some("source-fixed-waveform")
        );
        service.shutdown().await.unwrap();
        runtime.await.unwrap();
    }
    #[tokio::test]
    async fn endpoints_are_serialized_and_failed_persistence_restores_the_previous_value() {
        let directory = tempfile::tempdir().unwrap();
        let (service, runtime) = ControlService::create(
            directory.path().to_path_buf(),
            DEFAULT_RELAY_ENDPOINT.to_owned(),
        )
        .unwrap();
        let task = tokio::spawn(runtime.run());
        let (a, b) = tokio::join!(
            service.execute(ControlCommand::SetRelayEndpoint {
                transport: TransportKind::WsV3,
                endpoint: "ws://127.0.0.1:9010/".to_owned()
            }),
            service.execute(ControlCommand::SetRelayEndpoint {
                transport: TransportKind::WsV4,
                endpoint: "ws://127.0.0.1:9011/".to_owned()
            })
        );
        a.unwrap();
        b.unwrap();
        let saved = PreferencesState::load(directory.path().to_path_buf()).unwrap();
        assert_eq!(
            saved.relay_endpoint(TransportKind::WsV3).as_deref(),
            Some("ws://127.0.0.1:9010/")
        );
        assert_eq!(
            saved.relay_endpoint(TransportKind::WsV4).as_deref(),
            Some("ws://127.0.0.1:9011/")
        );
        let previous = service.snapshot().connections;
        std::fs::remove_file(directory.path().join("preferences.json")).unwrap();
        std::fs::create_dir(directory.path().join("preferences.json")).unwrap();
        let error = service
            .execute(ControlCommand::SetRelayEndpoint {
                transport: TransportKind::WsV3,
                endpoint: "ws://127.0.0.1:9012/".to_owned(),
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "preferences_error");
        assert_eq!(service.snapshot().connections, previous);
        service.shutdown().await.unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn stop_invalidates_connection_transactions_waiting_for_the_configuration_lock() {
        let directory = tempfile::tempdir().unwrap();
        let (service, runtime) = ControlService::create(
            directory.path().to_path_buf(),
            DEFAULT_RELAY_ENDPOINT.to_owned(),
        )
        .unwrap();
        let task = tokio::spawn(runtime.run());
        let lock = service.configuration.lock().await;
        let queued_service = service.clone();
        let queued = tokio::spawn(async move {
            queued_service
                .execute(ControlCommand::ConnectTransport {
                    transport: TransportKind::WsV3,
                    endpoint: None,
                })
                .await
        });
        tokio::task::yield_now().await;
        service
            .execute(ControlCommand::DisconnectRelay)
            .await
            .unwrap();
        drop(lock);
        assert_eq!(queued.await.unwrap().unwrap_err().code, "queue_busy");
        assert!(
            service
                .snapshot()
                .connections
                .iter()
                .all(|c| c.state == crate::hub::ConnectionState::Disconnected)
        );
        service.shutdown().await.unwrap();
        task.await.unwrap();
    }
}
