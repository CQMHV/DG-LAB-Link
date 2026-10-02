use std::path::PathBuf;
use std::sync::Arc;

use dg_lab_link_plugin_runtime::{BusinessFuture, BusinessHandler, PluginManager};
use dg_lab_link_plugin_sdk::{ActionParams, PluginError, SourceSpec};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex, Semaphore, watch};

use crate::hub::{
    HubError, HubHandle, HubRuntime, HubSnapshot, SafetySnapshot,
    create_hub_with_source_preferences,
};
use crate::preferences::{PreferencesError, PreferencesState};
use crate::sources::WaveformConfig;
use crate::transport::{DEFAULT_V3_ENDPOINT, TransportAction, TransportKind};

pub use dg_lab_link_contracts::{ControlCommand, ControlError};

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
        let preferences = PreferencesState::load(config_dir.clone())?;
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
        let operation_hub = hub.clone();
        plugins.set_operation_validator(Arc::new(move |epoch| {
            if epoch != operation_hub.command_epoch() {
                return Err(PluginError::new(
                    "queue_busy",
                    "操作上下文已被停止撤销，请重新发起操作",
                ));
            }
            Ok(())
        }));
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
                serde_json::json!({}),
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
        let epoch = self.accept_command(command.is_safety());
        self.execute_received(command, epoch).await
    }

    /// Capture the core's operation epoch when an entry point receives a command.
    /// Safety commands revoke older operations immediately, before scheduling.
    pub fn accept_command(&self, safety: bool) -> u64 {
        self.hub.accept_command(safety)
    }

    pub fn command_epoch(&self) -> u64 {
        self.hub.command_epoch()
    }

    pub fn subscribe_command_epoch(&self) -> watch::Receiver<u64> {
        self.hub.subscribe_command_epoch()
    }

    /// Forward the same command with its original acceptance epoch. Never sample
    /// a new epoch after a queued operation has been revoked by another entry.
    pub async fn execute_received(
        &self,
        command: ControlCommand,
        epoch: u64,
    ) -> Result<Value, ControlError> {
        self.execute_scoped(command, Some(epoch)).await
    }

    async fn execute_scoped(
        &self,
        command: ControlCommand,
        operation_epoch: Option<u64>,
    ) -> Result<Value, ControlError> {
        let epoch = operation_epoch.unwrap_or_else(|| self.command_epoch());
        if command.may_resume_output()
            && (operation_epoch.is_none() || epoch != self.command_epoch())
        {
            return Err(ControlError::new(
                "queue_busy",
                "操作上下文已被停止撤销，请重新发起操作",
            ));
        }
        dg_lab_link_plugin_sdk::OPERATION_EPOCH
            .scope(epoch, self.execute_accepted(command))
            .await
    }

    async fn execute_accepted(&self, command: ControlCommand) -> Result<Value, ControlError> {
        let plugin_transaction = matches!(
            &command,
            ControlCommand::SetSourceConfig { .. }
                | ControlCommand::InstallPlugin { .. }
                | ControlCommand::UpdatePlugin { .. }
                | ControlCommand::UninstallPlugin { .. }
                | ControlCommand::CreateSource { .. }
                | ControlCommand::DeleteSource { .. }
                | ControlCommand::SetSourceEnabled { .. }
        );
        if plugin_transaction {
            let permit = self
                .configuration_slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| ControlError::from(HubError::QueueBusy))?;
            let service = self.clone();
            let epoch = dg_lab_link_plugin_sdk::OPERATION_EPOCH.with(|epoch| *epoch);
            return tokio::spawn(dg_lab_link_plugin_sdk::OPERATION_EPOCH.scope(
                epoch,
                async move {
                    let _permit = permit;
                    if command.may_resume_output() && epoch != service.command_epoch() {
                        return Err(HubError::QueueBusy.into());
                    }
                    service.execute_inner(command).await
                },
            ))
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
            let accepted_epoch = dg_lab_link_plugin_sdk::OPERATION_EPOCH.with(|epoch| *epoch);
            let may_reconnect = matches!(
                command,
                ControlCommand::ConnectTransport { .. } | ControlCommand::SetBluetoothConfig { .. }
            );
            // A disconnected caller must not cancel a partially persisted transaction.
            return tokio::spawn(dg_lab_link_plugin_sdk::OPERATION_EPOCH.scope(
                accepted_epoch,
                async move {
                    let _permit = permit;
                    let _transaction = service.configuration.lock().await;
                    if may_reconnect && accepted_epoch != service.command_epoch() {
                        return Err(HubError::QueueBusy.into());
                    }
                    service.execute_inner(command).await
                },
            ))
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
                            self.snapshot()
                                .connections
                                .into_iter()
                                .find(|connection| connection.transport == transport)
                                .map(|connection| connection.endpoint)
                                .unwrap_or_else(|| {
                                    crate::dglab::client::DEFAULT_RELAY_ENDPOINT.to_owned()
                                })
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
                self.hub
                    .enqueue_disconnect_connection(connection_id)
                    .await?
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
                self.hub
                    .enqueue_disconnect_connection(connection_id)
                    .await?;
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
                let epoch = dg_lab_link_plugin_sdk::OPERATION_EPOCH.with(|epoch| *epoch);
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
                if epoch != self.command_epoch() {
                    return Err(HubError::QueueBusy.into());
                }
                self.hub.start_output(device_id).await?;
            }
            ClearDeviceChannel { device_id, channel } => {
                let (binding_id, generation) = self
                    .hub
                    .clear_device_channel(device_id.clone(), channel)
                    .await?;
                return Ok(serde_json::json!({"bindingId":binding_id,"generation":generation}));
            }
            StopOutput { device_id } => self.hub.enqueue_stop_output(device_id).await?,
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
                expected_revision,
            } => {
                return self
                    .configure_source(source_id, config, binding_id, expected_revision)
                    .await;
            }
            GetSourceUi { source_id, params } => {
                return serialize(
                    self.plugins
                        .ui(&source_id, params)
                        .await
                        .map_err(plugin_error)?,
                );
            }
            SourceAction { source_id, params } => {
                if matches!(params.action.as_str(), "configure" | "configure_binding") {
                    return Err(ControlError::new(
                        "invalid_command",
                        "配置写入请使用带 expectedRevision 的 set_source_config",
                    ));
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
        }
        Ok(Value::Null)
    }

    async fn configure_source(
        &self,
        source_id: String,
        config: Value,
        binding_id: Option<String>,
        expected_revision: u64,
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
            let binding = self
                .snapshot()
                .source_bindings
                .into_iter()
                .find(|binding| {
                    binding.source_id == source_id && binding.binding.binding_id == binding_id
                })
                .ok_or_else(|| ControlError::new("source_unavailable", "通道绑定已变更"))?;
            if binding.revision != expected_revision {
                return Err(ControlError::new(
                    "config_conflict",
                    "通道配置已由其他操作修改，请重新读取",
                ));
            }
            let previous = binding.binding.config;
            let make_action = |config: Value, validate_only: bool| ActionParams {
                action: "configure_binding".into(),
                value: serde_json::json!({"bindingId":binding_id,"config":config,"validateOnly":validate_only}),
                binding_id: Some(binding_id.clone()),
            };
            let actions = [
                make_action(config.clone(), true),
                make_action(config.clone(), false),
                make_action(previous, false),
            ];
            let accepted_epoch = dg_lab_link_plugin_sdk::OPERATION_EPOCH.with(|epoch| *epoch);
            let hub = self.hub.clone();
            let commit_source = source_id.clone();
            let result = self
                .plugins
                .configure_binding_transaction(
                    &source_id,
                    actions,
                    || {
                        if accepted_epoch != self.command_epoch() {
                            return Err(PluginError::new(
                                "queue_busy",
                                "操作上下文已被停止撤销，请重新发起操作",
                            ));
                        }
                        Ok(())
                    },
                    move || async move {
                        hub.set_plugin_binding_config(
                            commit_source,
                            binding_id,
                            config,
                            expected_revision,
                        )
                        .await
                        .map_err(|error| PluginError::new(error.code(), error.to_string()))
                    },
                )
                .await;
            if result
                .as_ref()
                .is_err_and(|error| error.code == "rollback_failed")
            {
                let _ = self.hub.refresh_plugins().await;
            }
            result.map_err(plugin_error)
        } else {
            let value = self
                .plugins
                .configure(&source_id, config, expected_revision)
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
    fn begin_operation<'a>(&'a self, _source_id: &'a str) -> BusinessFuture<'a> {
        Box::pin(async move {
            Ok(serde_json::json!({ "operationEpoch": self.service.accept_command(false) }))
        })
    }

    fn call<'a>(
        &'a self,
        _source_id: &'a str,
        command: Value,
        operation_epoch: Option<u64>,
    ) -> BusinessFuture<'a> {
        Box::pin(async move {
            let command: ControlCommand = serde_json::from_value(command)
                .map_err(|error| PluginError::new("invalid_params", error.to_string()))?;
            if !command.is_business() {
                return Err(PluginError::new(
                    "invalid_command",
                    "插件接口仅开放核心业务命令",
                ));
            }
            let operation_epoch = if command.is_safety() {
                Some(self.service.accept_command(true))
            } else {
                operation_epoch
            };
            self.service
                .execute_scoped(command, operation_epoch)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dglab::client::DEFAULT_RELAY_ENDPOINT;
    use std::time::Duration;

    #[test]
    fn malformed_preferences_fail_without_replacing_recoverable_data() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let original = br#"{"connectionTimeoutMinutes":"broken","customWaveforms":[{"presetId":"recoverable","presetName":"Recoverable","frames":["0A0A0A0A64646464"]}]}"#;
        std::fs::write(&path, original).unwrap();
        let result =
            ControlService::create(directory.path().to_owned(), "ws://127.0.0.1:1/v4".into());
        assert!(matches!(result, Err(error) if error.code == "preferences_error"));
        assert_eq!(std::fs::read(path).unwrap(), original);
        assert!(!directory.path().join("plugins/registry.json").exists());
    }

    #[tokio::test]
    async fn plugin_callbacks_keep_their_original_stop_context() {
        let directory = tempfile::tempdir().unwrap();
        let (service, runtime) =
            ControlService::create(directory.path().to_owned(), "ws://127.0.0.1:1/v4".into())
                .unwrap();
        let hub = tokio::spawn(runtime.run());
        let callback = CorePluginBusiness {
            service: service.clone(),
        };
        let old_epoch = service.accept_command(false);
        let mut epochs = service.subscribe_command_epoch();
        let queued_start = service.execute_received(
            ControlCommand::StartOutput {
                device_id: "device".into(),
            },
            old_epoch,
        );
        // A plugin stop enters directly through CorePluginBusiness, outside the
        // WebSocket dispatcher, and must revoke commands accepted there too.
        callback
            .call(
                "plugin",
                serde_json::json!({"command":"disconnect_connection","params":{"connectionId":"ws-v4"}}),
                None,
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), epochs.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*epochs.borrow_and_update(), old_epoch + 1);
        assert_eq!(service.command_epoch(), old_epoch + 1);
        assert_eq!(queued_start.await.unwrap_err().code, "queue_busy");
        let start = serde_json::json!({"command":"start_output","params":{"deviceId":"device"}});
        assert_eq!(
            callback
                .call("plugin", start.clone(), Some(old_epoch))
                .await
                .unwrap_err()
                .code,
            "queue_busy"
        );
        assert_eq!(
            callback
                .call("plugin", start.clone(), None)
                .await
                .unwrap_err()
                .code,
            "queue_busy"
        );
        let new_context = callback.begin_operation("plugin").await.unwrap();
        let new_epoch = new_context["operationEpoch"].as_u64().unwrap();
        assert_eq!(new_epoch, service.command_epoch());
        assert_ne!(new_epoch, old_epoch);
        assert_eq!(
            callback
                .call("plugin", start.clone(), Some(new_epoch))
                .await
                .unwrap_err()
                .code,
            "device_unavailable"
        );
        assert_eq!(
            callback
                .call("plugin", start, Some(old_epoch))
                .await
                .unwrap_err()
                .code,
            "queue_busy"
        );
        assert!(
            callback
                .call(
                    "plugin",
                    serde_json::json!({"command":"get_hub_snapshot"}),
                    Some(old_epoch)
                )
                .await
                .is_ok()
        );
        let before_source_stop = service.command_epoch();
        assert_eq!(
            callback
                .call(
                    "plugin",
                    serde_json::json!({"command":"stop_source","params":{"sourceId":"missing"}}),
                    Some(old_epoch),
                )
                .await
                .unwrap_err()
                .code,
            "source_not_found"
        );
        // A safety callback gets a fresh epoch even with an obsolete context,
        // and its source lifecycle branch must not revoke it a second time.
        assert_eq!(service.command_epoch(), before_source_stop + 1);
        assert_eq!(
            service
                .execute_received(
                    ControlCommand::GetSourceUi {
                        source_id: "missing".into(),
                        params: Default::default(),
                    },
                    before_source_stop,
                )
                .await
                .unwrap_err()
                .code,
            "queue_busy"
        );
        service.shutdown().await.unwrap();
        hub.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn source_stop_revokes_configuration_spawned_before_its_first_poll() {
        let directory = tempfile::tempdir().unwrap();
        let plugins = directory.path().join("plugins");
        std::fs::create_dir(&plugins).unwrap();
        // A preserved instance remains stoppable after uninstalling its package.
        // It must not reach configure/start after this successful lifecycle stop.
        std::fs::write(
            plugins.join("registry.json"),
            serde_json::to_vec(&serde_json::json!({
                "sources": {
                    "source-test": {
                        "id":"source-test", "pluginId":"example.pulse-source",
                        "name":"Stopped instance", "enabled":true, "config":{}
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let (service, runtime) =
            ControlService::create(directory.path().to_owned(), "ws://127.0.0.1:1/v4".into())
                .unwrap();
        let hub = tokio::spawn(runtime.run());
        let old_epoch = service.accept_command(false);
        let configure = service.execute_received(
            ControlCommand::SetSourceConfig {
                source_id: "source-test".into(),
                config: serde_json::json!({"intensity":90}),
                binding_id: None,
                expected_revision: 0,
            },
            old_epoch,
        );
        let mut configure = std::pin::pin!(configure);
        // This runs the outer check and creates the transaction task. On this
        // current-thread executor the task cannot run before we yield below.
        assert!(futures_util::poll!(configure.as_mut()).is_pending());
        service
            .execute(ControlCommand::StopSource {
                source_id: "source-test".into(),
            })
            .await
            .unwrap();
        assert_eq!(service.command_epoch(), old_epoch + 1);
        assert_eq!(configure.await.unwrap_err().code, "queue_busy");
        let source = service.plugins.snapshot().sources.remove(0);
        assert_eq!(
            source.status,
            dg_lab_link_plugin_runtime::SourceStatus::Stopped
        );
        assert_eq!(source.revision, 0);
        assert_eq!(source.spec.config, serde_json::json!({}));
        service.shutdown().await.unwrap();
        hub.await.unwrap();
    }

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
                && !names.contains("audio_control")
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
            service.execute(ControlCommand::DisconnectConnection {
                connection_id: crate::transport::V4_CONNECTION_ID.into(),
            }),
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
            .execute(ControlCommand::DisconnectConnection {
                connection_id: crate::transport::V4_CONNECTION_ID.into(),
            })
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
