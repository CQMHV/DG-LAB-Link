use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;

use crate::hub::{HubError, HubHandle, HubSnapshot};
use crate::model::Channel;
use crate::preferences::{AppPreferencesSnapshot, PreferencesError, PreferencesState};
use crate::sources::WaveformConfig;

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
pub async fn start_output(
    hub: State<'_, HubHandle>,
    device_id: String,
) -> Result<(), CommandError> {
    hub.start_output(device_id).await.map_err(Into::into)
}

#[tauri::command]
pub async fn stop_output(hub: State<'_, HubHandle>, device_id: String) -> Result<(), CommandError> {
    hub.stop_output(device_id).await.map_err(Into::into)
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
pub async fn set_fixed_waveform(
    hub: State<'_, HubHandle>,
    device_id: String,
    channel: Channel,
    config: WaveformConfig,
) -> Result<(), CommandError> {
    hub.set_fixed_waveform(device_id, channel, Some(config))
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn import_custom_waveforms(
    hub: State<'_, HubHandle>,
    preferences: State<'_, PreferencesState>,
    configs: Vec<WaveformConfig>,
) -> Result<(), CommandError> {
    if configs.is_empty() {
        return Ok(());
    }
    let previous_waveforms = preferences.custom_waveforms();
    let previous_selected = preferences.fixed_waveform();
    let mut next_waveforms = previous_waveforms.clone();
    next_waveforms.extend(configs);
    apply_waveform_state(
        &hub,
        &preferences,
        previous_selected.clone(),
        previous_waveforms,
        previous_selected,
        next_waveforms,
    )
    .await
}

#[tauri::command]
pub async fn select_custom_waveform(
    hub: State<'_, HubHandle>,
    preferences: State<'_, PreferencesState>,
    device_id: String,
    channel: Channel,
    preset_id: String,
) -> Result<(), CommandError> {
    let waveforms = preferences.custom_waveforms();
    let selected = waveforms
        .iter()
        .find(|waveform| waveform.preset_id == preset_id)
        .cloned()
        .ok_or_else(|| {
            CommandError::from(HubError::InvalidSourceConfig(
                "选择的自定义波形不存在".to_owned(),
            ))
        })?;
    hub.set_fixed_waveform(device_id, channel, Some(selected))
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn delete_custom_waveform(
    hub: State<'_, HubHandle>,
    preferences: State<'_, PreferencesState>,
    preset_id: String,
) -> Result<(), CommandError> {
    let previous_waveforms = preferences.custom_waveforms();
    let previous_selected = preferences.fixed_waveform();
    let removed_index = previous_waveforms
        .iter()
        .position(|waveform| waveform.preset_id == preset_id)
        .ok_or_else(|| {
            CommandError::from(HubError::InvalidSourceConfig(
                "要删除的自定义波形不存在".to_owned(),
            ))
        })?;
    let mut next_waveforms = previous_waveforms.clone();
    next_waveforms.remove(removed_index);
    let next_selected = if previous_selected
        .as_ref()
        .map(|item| item.preset_id.as_str())
        == Some(preset_id.as_str())
    {
        None
    } else {
        previous_selected.clone()
    };
    apply_waveform_state(
        &hub,
        &preferences,
        previous_selected,
        previous_waveforms,
        next_selected,
        next_waveforms,
    )
    .await
}

#[tauri::command]
pub async fn reorder_custom_waveforms(
    hub: State<'_, HubHandle>,
    preferences: State<'_, PreferencesState>,
    preset_ids: Vec<String>,
) -> Result<(), CommandError> {
    let previous_waveforms = preferences.custom_waveforms();
    let selected = preferences.fixed_waveform();
    if preset_ids.len() != previous_waveforms.len() {
        return Err(
            HubError::InvalidSourceConfig("排序结果必须包含全部自定义波形".to_owned()).into(),
        );
    }
    let mut remaining = previous_waveforms
        .iter()
        .cloned()
        .map(|waveform| (waveform.preset_id.clone(), waveform))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut next_waveforms = Vec::with_capacity(preset_ids.len());
    for preset_id in preset_ids {
        let waveform = remaining.remove(&preset_id).ok_or_else(|| {
            CommandError::from(HubError::InvalidSourceConfig(
                "排序结果包含未知或重复的自定义波形".to_owned(),
            ))
        })?;
        next_waveforms.push(waveform);
    }
    if !remaining.is_empty() {
        return Err(
            HubError::InvalidSourceConfig("排序结果必须包含全部自定义波形".to_owned()).into(),
        );
    }
    apply_waveform_state(
        &hub,
        &preferences,
        selected.clone(),
        previous_waveforms,
        selected,
        next_waveforms,
    )
    .await
}

async fn apply_waveform_state(
    hub: &HubHandle,
    preferences: &PreferencesState,
    previous_selected: Option<WaveformConfig>,
    previous_waveforms: Vec<WaveformConfig>,
    next_selected: Option<WaveformConfig>,
    next_waveforms: Vec<WaveformConfig>,
) -> Result<(), CommandError> {
    preferences.set_waveform_state(next_selected.clone(), next_waveforms.clone())?;
    if let Err(error) = hub.set_waveform_state(next_waveforms, next_selected).await {
        let _ = preferences.set_waveform_state(previous_selected, previous_waveforms);
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
pub async fn update_safety(
    hub: State<'_, HubHandle>,
    preferences: State<'_, PreferencesState>,
    connection_timeout_enabled: bool,
    connection_timeout_minutes: i32,
    allow_app_intensity_control: bool,
) -> Result<(), CommandError> {
    if !(1..=1440).contains(&connection_timeout_minutes) {
        return Err(HubError::InvalidConnectionTimeout.into());
    }
    let previous = hub.snapshot().safety;
    hub.update_safety(
        connection_timeout_enabled,
        connection_timeout_minutes,
        allow_app_intensity_control,
    )
    .await?;
    if let Err(error) = preferences.set_safety_settings(
        connection_timeout_enabled,
        connection_timeout_minutes as u16,
        allow_app_intensity_control,
    ) {
        let _ = hub
            .update_safety(
                previous.connection_timeout_enabled,
                i32::from(previous.connection_timeout_minutes),
                previous.allow_app_intensity_control,
            )
            .await;
        return Err(error.into());
    }
    Ok(())
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
