use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;

use crate::hub::{HubError, HubHandle, HubSnapshot};
use crate::model::Channel;
use crate::preferences::{AppPreferencesSnapshot, PreferencesError, PreferencesState};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub code: &'static str,
    pub message: String,
}

impl From<HubError> for CommandError {
    fn from(error: HubError) -> Self {
        Self {
            code: error.code(),
            message: error.to_string(),
        }
    }
}

impl From<PreferencesError> for CommandError {
    fn from(error: PreferencesError) -> Self {
        Self {
            code: "preferences_error",
            message: error.to_string(),
        }
    }
}

impl From<tauri_plugin_autostart::Error> for CommandError {
    fn from(error: tauri_plugin_autostart::Error) -> Self {
        Self {
            code: "autostart_error",
            message: format!("无法更新开机自启设置：{error}"),
        }
    }
}

fn app_preferences(
    app: &AppHandle,
    preferences: &PreferencesState,
) -> Result<AppPreferencesSnapshot, CommandError> {
    let auto_start = app.autolaunch().is_enabled()?;
    Ok(preferences.snapshot(auto_start))
}

#[tauri::command]
pub fn get_app_preferences(
    app: AppHandle,
    preferences: State<'_, PreferencesState>,
) -> Result<AppPreferencesSnapshot, CommandError> {
    app_preferences(&app, &preferences)
}

#[tauri::command]
pub fn set_close_to_tray(
    app: AppHandle,
    preferences: State<'_, PreferencesState>,
    enabled: bool,
) -> Result<AppPreferencesSnapshot, CommandError> {
    preferences.set_close_to_tray(enabled)?;
    app_preferences(&app, &preferences)
}

#[tauri::command]
pub fn set_auto_start(
    app: AppHandle,
    preferences: State<'_, PreferencesState>,
    enabled: bool,
) -> Result<AppPreferencesSnapshot, CommandError> {
    if enabled {
        app.autolaunch().enable()?;
    } else {
        app.autolaunch().disable()?;
    }
    app_preferences(&app, &preferences)
}

#[tauri::command]
pub fn set_start_minimized(
    app: AppHandle,
    preferences: State<'_, PreferencesState>,
    enabled: bool,
) -> Result<AppPreferencesSnapshot, CommandError> {
    preferences.set_start_minimized(enabled)?;
    app_preferences(&app, &preferences)
}

#[tauri::command]
pub fn get_hub_snapshot(hub: State<'_, HubHandle>) -> HubSnapshot {
    hub.snapshot()
}

#[tauri::command]
pub async fn connect_relay(hub: State<'_, HubHandle>) -> Result<(), CommandError> {
    hub.connect_relay().await.map_err(Into::into)
}

#[tauri::command]
pub async fn disconnect_relay(hub: State<'_, HubHandle>) -> Result<(), CommandError> {
    hub.disconnect_relay().await.map_err(Into::into)
}

#[tauri::command]
pub async fn refresh_pairing(hub: State<'_, HubHandle>) -> Result<(), CommandError> {
    hub.refresh_pairing().await.map_err(Into::into)
}

#[tauri::command]
pub async fn adjust_intensity(
    hub: State<'_, HubHandle>,
    device_id: Option<String>,
    channel: Channel,
    delta: i32,
) -> Result<(), CommandError> {
    hub.adjust_device_intensity(device_id, channel, delta)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn start_output(hub: State<'_, HubHandle>) -> Result<(), CommandError> {
    hub.start_output().await.map_err(Into::into)
}

#[tauri::command]
pub async fn stop_output(hub: State<'_, HubHandle>) -> Result<(), CommandError> {
    hub.stop_output().await.map_err(Into::into)
}

#[tauri::command]
pub async fn emergency_stop(hub: State<'_, HubHandle>) -> Result<(), CommandError> {
    hub.emergency_stop().await.map_err(Into::into)
}

#[tauri::command]
pub async fn set_device_channel_source(
    hub: State<'_, HubHandle>,
    device_id: String,
    channel: Channel,
    source_id: String,
) -> Result<(), CommandError> {
    hub.set_device_channel_source(device_id, channel, source_id)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn set_device_channel_source_sync(
    hub: State<'_, HubHandle>,
    device_id: String,
    enabled: bool,
) -> Result<(), CommandError> {
    hub.set_device_channel_source_sync(device_id, enabled)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn set_default_source(
    hub: State<'_, HubHandle>,
    preferences: State<'_, PreferencesState>,
    source_id: Option<String>,
) -> Result<(), CommandError> {
    let previous_source_id = hub.snapshot().default_source_id;
    hub.set_default_source(source_id.clone()).await?;
    if let Err(error) = preferences.set_default_source_id(source_id) {
        let _ = hub.set_default_source(previous_source_id).await;
        return Err(error.into());
    }
    Ok(())
}

#[tauri::command]
pub async fn select_device(
    hub: State<'_, HubHandle>,
    device_id: String,
) -> Result<(), CommandError> {
    hub.select_device(device_id).await.map_err(Into::into)
}

#[tauri::command]
pub async fn set_sync_all_devices(
    hub: State<'_, HubHandle>,
    enabled: bool,
) -> Result<(), CommandError> {
    hub.set_sync_all_devices(enabled).await.map_err(Into::into)
}

#[tauri::command]
pub async fn set_channel_limit(hub: State<'_, HubHandle>, limit: i32) -> Result<(), CommandError> {
    hub.set_channel_limit(limit).await.map_err(Into::into)
}

#[tauri::command]
pub async fn update_safety(
    hub: State<'_, HubHandle>,
    channel_limit: i32,
    max_duration_minutes: i32,
    allow_app_intensity_control: bool,
) -> Result<(), CommandError> {
    hub.update_safety(
        channel_limit,
        max_duration_minutes,
        allow_app_intensity_control,
    )
    .await
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_errors_are_serializable_and_chinese() {
        let error = CommandError::from(HubError::NotConnected);
        let json = serde_json::to_value(error).unwrap();
        assert_eq!(json["code"], "not_connected");
        assert!(json["message"].as_str().unwrap().contains("连接"));
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
