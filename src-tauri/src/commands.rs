use serde::{Serialize, de::DeserializeOwned};
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;

use dg_lab_link_core::hub::HubSnapshot;
use dg_lab_link_core::model::Channel;
use dg_lab_link_core::preferences::AppPreferencesSnapshot;
use dg_lab_link_core::sources::WaveformConfig;
use dg_lab_link_core::sources::audio::{AudioAction, AudioChannelConfig};
use dg_lab_link_core::sources::touch::{TouchConfig, TouchInput};
use dg_lab_link_core::transport::{
    BleParameters, BluetoothDevice, TransportConnectionSnapshot, TransportKind,
};
use dg_lab_link_core::waveforms::WaveformFile;
use dg_lab_link_core::{ControlCommand, ControlError};
use dg_lab_link_runtime::{Client, LocalConfig, RuntimeInfo};

use crate::{DesktopPreferences, RuntimeConfigDir};

pub type CommandError = ControlError;

async fn call<T: DeserializeOwned>(
    client: &Client,
    command: ControlCommand,
) -> Result<T, CommandError> {
    serde_json::from_value(client.call(command).await?)
        .map_err(|error| ControlError::new("invalid_response", error.to_string()))
}

async fn app_preferences(
    app: &AppHandle,
    client: &Client,
    cache: &DesktopPreferences,
    command: ControlCommand,
) -> Result<AppPreferencesSnapshot, CommandError> {
    if !matches!(command, ControlCommand::GetAppPreferences) {
        call::<()>(client, command).await?;
    }
    let mut preferences: AppPreferencesSnapshot =
        call(client, ControlCommand::GetAppPreferences).await?;
    cache.update(preferences);
    preferences.auto_start = app.autolaunch().is_enabled().map_err(autostart_error)?;
    Ok(preferences)
}

fn autostart_error(error: tauri_plugin_autostart::Error) -> CommandError {
    ControlError::new("autostart_error", format!("无法更新开机自启设置：{error}"))
}

fn selected_device(client: &Client, device_id: Option<String>) -> Result<String, CommandError> {
    device_id
        .or_else(|| client.snapshot().selected_device_id)
        .ok_or_else(|| ControlError::new("not_connected", "设备尚未连接"))
}

#[tauri::command]
pub async fn get_app_preferences(
    app: AppHandle,
    client: State<'_, Client>,
    cache: State<'_, DesktopPreferences>,
) -> Result<AppPreferencesSnapshot, CommandError> {
    app_preferences(&app, &client, &cache, ControlCommand::GetAppPreferences).await
}

#[tauri::command]
pub async fn set_close_to_tray(
    app: AppHandle,
    client: State<'_, Client>,
    cache: State<'_, DesktopPreferences>,
    enabled: bool,
) -> Result<AppPreferencesSnapshot, CommandError> {
    app_preferences(
        &app,
        &client,
        &cache,
        ControlCommand::SetCloseToTray { enabled },
    )
    .await
}

#[tauri::command]
pub async fn set_start_minimized(
    app: AppHandle,
    client: State<'_, Client>,
    cache: State<'_, DesktopPreferences>,
    enabled: bool,
) -> Result<AppPreferencesSnapshot, CommandError> {
    app_preferences(
        &app,
        &client,
        &cache,
        ControlCommand::SetStartMinimized { enabled },
    )
    .await
}

#[tauri::command]
pub async fn set_auto_start(
    app: AppHandle,
    client: State<'_, Client>,
    cache: State<'_, DesktopPreferences>,
    enabled: bool,
) -> Result<AppPreferencesSnapshot, CommandError> {
    if enabled {
        app.autolaunch().enable().map_err(autostart_error)?;
    } else {
        app.autolaunch().disable().map_err(autostart_error)?;
    }
    app_preferences(&app, &client, &cache, ControlCommand::GetAppPreferences).await
}

#[tauri::command]
pub async fn get_hub_snapshot(client: State<'_, Client>) -> Result<HubSnapshot, CommandError> {
    call(&client, ControlCommand::GetHubSnapshot).await
}

#[tauri::command]
pub async fn get_runtime_info(client: State<'_, Client>) -> Result<RuntimeInfo, CommandError> {
    client.runtime_info().await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConfig {
    url: String,
    token: String,
}

#[tauri::command]
pub async fn get_mcp_config(
    client: State<'_, Client>,
    config_dir: State<'_, RuntimeConfigDir>,
) -> Result<McpConfig, CommandError> {
    let info = client.runtime_info().await?;
    let config = LocalConfig::load(&config_dir.0)?;
    Ok(McpConfig {
        url: info.mcp_url,
        token: config.token,
    })
}

#[tauri::command]
pub async fn update_touch_input(
    client: State<'_, Client>,
    input: TouchInput,
) -> Result<(), CommandError> {
    call(&client, ControlCommand::UpdateTouchInput { input }).await
}

#[tauri::command]
pub async fn set_touch_config(
    client: State<'_, Client>,
    config: TouchConfig,
) -> Result<(), CommandError> {
    call(&client, ControlCommand::SetTouchConfig { config }).await
}

#[tauri::command]
pub async fn set_audio_config(
    client: State<'_, Client>,
    device_id: String,
    channel: Channel,
    config: AudioChannelConfig,
) -> Result<(), CommandError> {
    call(
        &client,
        ControlCommand::SetAudioConfig {
            device_id,
            channel,
            config,
        },
    )
    .await
}

#[tauri::command]
pub async fn audio_control(
    client: State<'_, Client>,
    action: AudioAction,
) -> Result<(), CommandError> {
    call(&client, ControlCommand::AudioControl { action }).await
}

#[tauri::command]
pub async fn get_custom_waveform(
    client: State<'_, Client>,
    preset_id: String,
) -> Result<WaveformConfig, CommandError> {
    call(&client, ControlCommand::GetCustomWaveform { preset_id }).await
}

#[tauri::command]
pub async fn parse_waveform_files(
    client: State<'_, Client>,
    files: Vec<WaveformFile>,
) -> Result<Vec<WaveformConfig>, CommandError> {
    call(&client, ControlCommand::ParseWaveformFiles { files }).await
}

#[tauri::command]
pub async fn choose_audio_file() -> Result<Option<String>, CommandError> {
    choose_plugin_file().await
}

#[tauri::command]
pub async fn choose_recording_destination() -> Result<Option<String>, CommandError> {
    tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("保存录音")
            .set_file_name("DG-LAB录音.wav")
            .add_filter("WAV 音频", &["wav"])
            .save_file()
            .map(|path| path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|error| ControlError::new("dialog_error", error.to_string()))
}

#[tauri::command]
pub async fn connect_relay(client: State<'_, Client>) -> Result<(), CommandError> {
    call(&client, ControlCommand::ConnectRelay).await
}

#[tauri::command]
pub async fn disconnect_relay(client: State<'_, Client>) -> Result<(), CommandError> {
    call(&client, ControlCommand::DisconnectRelay).await
}

#[tauri::command]
pub async fn refresh_pairing(client: State<'_, Client>) -> Result<(), CommandError> {
    call(&client, ControlCommand::RefreshPairing).await
}

#[tauri::command]
pub async fn get_connections(
    client: State<'_, Client>,
) -> Result<Vec<TransportConnectionSnapshot>, CommandError> {
    call(&client, ControlCommand::GetConnections).await
}

#[tauri::command]
pub async fn connect_transport(
    client: State<'_, Client>,
    transport: TransportKind,
    endpoint: Option<String>,
) -> Result<(), CommandError> {
    call::<serde_json::Value>(
        &client,
        ControlCommand::ConnectTransport {
            transport,
            endpoint,
        },
    )
    .await
    .map(|_| ())
}

#[tauri::command]
pub async fn disconnect_connection(
    client: State<'_, Client>,
    connection_id: String,
) -> Result<(), CommandError> {
    call::<serde_json::Value>(
        &client,
        ControlCommand::DisconnectConnection { connection_id },
    )
    .await
    .map(|_| ())
}

#[tauri::command]
pub async fn refresh_connection_pairing(
    client: State<'_, Client>,
    connection_id: String,
) -> Result<(), CommandError> {
    call::<serde_json::Value>(
        &client,
        ControlCommand::RefreshConnectionPairing { connection_id },
    )
    .await
    .map(|_| ())
}

#[tauri::command]
pub async fn set_relay_endpoint(
    client: State<'_, Client>,
    transport: TransportKind,
    endpoint: String,
) -> Result<(), CommandError> {
    call::<serde_json::Value>(
        &client,
        ControlCommand::SetRelayEndpoint {
            transport,
            endpoint,
        },
    )
    .await
    .map(|_| ())
}

#[tauri::command]
pub async fn scan_bluetooth(
    client: State<'_, Client>,
    duration_ms: u64,
) -> Result<Vec<BluetoothDevice>, CommandError> {
    call(&client, ControlCommand::ScanBluetooth { duration_ms }).await
}

#[tauri::command]
pub async fn connect_bluetooth(
    client: State<'_, Client>,
    device_id: String,
) -> Result<(), CommandError> {
    call::<serde_json::Value>(&client, ControlCommand::ConnectBluetooth { device_id })
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn disconnect_bluetooth(
    client: State<'_, Client>,
    device_id: String,
) -> Result<(), CommandError> {
    call::<serde_json::Value>(&client, ControlCommand::DisconnectBluetooth { device_id })
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn get_bluetooth_config(
    client: State<'_, Client>,
    device_id: String,
) -> Result<BleParameters, CommandError> {
    call(&client, ControlCommand::GetBluetoothConfig { device_id }).await
}

#[tauri::command]
pub async fn set_bluetooth_config(
    client: State<'_, Client>,
    device_id: String,
    config: BleParameters,
) -> Result<(), CommandError> {
    call::<serde_json::Value>(
        &client,
        ControlCommand::SetBluetoothConfig { device_id, config },
    )
    .await
    .map(|_| ())
}

#[tauri::command]
pub async fn adjust_intensity(
    client: State<'_, Client>,
    device_id: Option<String>,
    channel: Channel,
    delta: i32,
) -> Result<(), CommandError> {
    let device_id = selected_device(&client, device_id)?;
    call(
        &client,
        ControlCommand::AdjustIntensity {
            device_id,
            channel,
            delta,
        },
    )
    .await
}

#[tauri::command]
pub async fn start_output(
    client: State<'_, Client>,
    device_id: String,
) -> Result<(), CommandError> {
    call(&client, ControlCommand::StartOutput { device_id }).await
}

#[tauri::command]
pub async fn stop_output(client: State<'_, Client>, device_id: String) -> Result<(), CommandError> {
    call(&client, ControlCommand::StopOutput { device_id }).await
}

#[tauri::command]
pub async fn set_device_channel_source(
    client: State<'_, Client>,
    device_id: String,
    channel: Channel,
    source_id: String,
) -> Result<(), CommandError> {
    call(
        &client,
        ControlCommand::SetDeviceChannelSource {
            device_id,
            channel,
            source_id,
        },
    )
    .await
}

#[tauri::command]
pub async fn set_device_channel_source_sync(
    client: State<'_, Client>,
    device_id: String,
    enabled: bool,
) -> Result<(), CommandError> {
    call(
        &client,
        ControlCommand::SetDeviceChannelSourceSync { device_id, enabled },
    )
    .await
}

#[tauri::command]
pub async fn set_default_source(
    client: State<'_, Client>,
    source_id: Option<String>,
) -> Result<(), CommandError> {
    call(&client, ControlCommand::SetDefaultSource { source_id }).await
}

#[tauri::command]
pub async fn set_fixed_waveform(
    client: State<'_, Client>,
    device_id: String,
    channel: Channel,
    config: WaveformConfig,
) -> Result<(), CommandError> {
    call(
        &client,
        ControlCommand::SetFixedWaveform {
            device_id,
            channel,
            config,
        },
    )
    .await
}

#[tauri::command]
pub async fn import_custom_waveforms(
    client: State<'_, Client>,
    configs: Vec<WaveformConfig>,
) -> Result<(), CommandError> {
    call(&client, ControlCommand::ImportCustomWaveforms { configs }).await
}

#[tauri::command]
pub async fn select_custom_waveform(
    client: State<'_, Client>,
    device_id: String,
    channel: Channel,
    preset_id: String,
) -> Result<(), CommandError> {
    call(
        &client,
        ControlCommand::SelectCustomWaveform {
            device_id,
            channel,
            preset_id,
        },
    )
    .await
}

#[tauri::command]
pub async fn delete_custom_waveform(
    client: State<'_, Client>,
    preset_id: String,
) -> Result<(), CommandError> {
    call(&client, ControlCommand::DeleteCustomWaveform { preset_id }).await
}

#[tauri::command]
pub async fn reorder_custom_waveforms(
    client: State<'_, Client>,
    preset_ids: Vec<String>,
) -> Result<(), CommandError> {
    call(
        &client,
        ControlCommand::ReorderCustomWaveforms { preset_ids },
    )
    .await
}

#[tauri::command]
pub async fn select_device(
    client: State<'_, Client>,
    device_id: String,
) -> Result<(), CommandError> {
    call(&client, ControlCommand::SelectDevice { device_id }).await
}

#[tauri::command]
pub async fn set_sync_all_devices(
    client: State<'_, Client>,
    enabled: bool,
    device_id: Option<String>,
) -> Result<(), CommandError> {
    let device_id = if enabled {
        selected_device(&client, device_id)?
    } else {
        client.snapshot().selected_device_id.unwrap_or_default()
    };
    call(
        &client,
        ControlCommand::SetSyncAllDevices { device_id, enabled },
    )
    .await
}

#[tauri::command]
pub async fn update_safety(
    client: State<'_, Client>,
    connection_timeout_enabled: bool,
    connection_timeout_minutes: i32,
    allow_app_intensity_control: bool,
) -> Result<(), CommandError> {
    call(
        &client,
        ControlCommand::UpdateSafety {
            connection_timeout_enabled,
            connection_timeout_minutes,
            allow_app_intensity_control,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_errors_preserve_shared_error_format() {
        let json =
            serde_json::to_value(ControlError::new("not_connected", "设备尚未连接")).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "code": "not_connected", "message": "设备尚未连接" })
        );
    }

    #[test]
    fn every_tauri_command_is_declared_and_allowed() {
        let commands_source = include_str!("commands.rs");
        let build_script = include_str!("../build.rs");
        let capability = include_str!("../capabilities/default.json");
        let command_attribute = ["#[tauri", "::command]"].concat();
        for command_block in commands_source.split(&command_attribute).skip(1) {
            let function = command_block
                .split("fn ")
                .nth(1)
                .and_then(|rest| rest.split('(').next())
                .expect("tauri command must be a function")
                .trim();
            assert!(
                build_script.contains(&format!("\"{function}\"")),
                "{function} is missing from build.rs AppManifest"
            );
            let permission = format!("allow-{}", function.replace('_', "-"));
            assert!(
                capability.contains(&format!("\"{permission}\"")),
                "{permission} is missing from the main window capability"
            );
        }
    }
}

#[tauri::command]
pub async fn plugin_call(
    client: State<'_, Client>,
    command: ControlCommand,
) -> Result<serde_json::Value, CommandError> {
    if !command.is_business() {
        return Err(ControlError::new(
            "invalid_command",
            "插件调用仅支持核心业务命令",
        ));
    }
    client.call(command).await
}

#[tauri::command]
pub async fn choose_plugin_package() -> Result<Option<String>, CommandError> {
    tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("选择输入源插件包")
            .add_filter("DG-LAB Link 插件", &["dglabplugin"])
            .pick_file()
            .map(|path| path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|error| ControlError::new("file_dialog_error", error.to_string()))
}

#[tauri::command]
pub async fn choose_plugin_file() -> Result<Option<String>, CommandError> {
    tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("选择插件文件")
            .pick_file()
            .map(|path| path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|error| ControlError::new("file_dialog_error", error.to_string()))
}
