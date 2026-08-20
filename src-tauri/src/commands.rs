use serde::Serialize;
use tauri::State;

use crate::hub::{HubError, HubHandle, HubSnapshot};
use crate::model::Channel;

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
pub async fn set_active_source(
    hub: State<'_, HubHandle>,
    source_id: String,
) -> Result<(), CommandError> {
    hub.set_active_source(source_id).await.map_err(Into::into)
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
