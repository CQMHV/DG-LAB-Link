use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::io::Read;
use std::path::PathBuf;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::sources::WaveformConfig;
use crate::transport::{BleParameters, TransportKind};

const PREFERENCES_FILE_NAME: &str = "preferences.json";
const MAX_PREFERENCES_BYTES: u64 = 4 * 1024 * 1024;

pub use dg_lab_link_contracts::preferences::AppPreferencesSnapshot;

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct StoredPreferences {
    relay_endpoints: BTreeMap<TransportKind, String>,
    ble_parameters: BTreeMap<String, BleParameters>,
    close_to_tray: bool,
    start_minimized: bool,
    connection_timeout_enabled: bool,
    connection_timeout_minutes: u16,
    allow_app_intensity_control: bool,
    default_source_id: Option<String>,
    fixed_waveform: Option<WaveformConfig>,
    custom_waveforms: Vec<WaveformConfig>,
}

impl Default for StoredPreferences {
    fn default() -> Self {
        Self {
            relay_endpoints: BTreeMap::new(),
            ble_parameters: BTreeMap::new(),
            close_to_tray: true,
            start_minimized: true,
            connection_timeout_enabled: false,
            connection_timeout_minutes: 60,
            allow_app_intensity_control: false,
            default_source_id: None,
            fixed_waveform: Some(WaveformConfig::default()),
            custom_waveforms: Vec::new(),
        }
    }
}

impl StoredPreferences {
    fn validate(&self) -> Result<(), PreferencesError> {
        if !(1..=1440).contains(&self.connection_timeout_minutes) {
            return Err(PreferencesError::Invalid(
                "连接超时须为 1..1440 分钟".into(),
            ));
        }
        crate::hub::validate_waveform_library(&self.custom_waveforms, self.fixed_waveform.as_ref())
            .map_err(|error| PreferencesError::Invalid(error.to_string()))?;
        for (transport, endpoint) in &self.relay_endpoints {
            let url = url::Url::parse(endpoint)
                .map_err(|error| PreferencesError::Invalid(error.to_string()))?;
            if *transport == TransportKind::Ble
                || !matches!(url.scheme(), "ws" | "wss")
                || url.host_str().is_none()
            {
                return Err(PreferencesError::Invalid(
                    "Relay 端点须为 WS/WSS 地址".into(),
                ));
            }
        }
        for parameters in self.ble_parameters.values() {
            parameters
                .validate()
                .map_err(|error| PreferencesError::Invalid(error.to_string()))?;
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum PreferencesError {
    #[error("无法读取应用偏好设置：{0}")]
    Read(#[source] std::io::Error),
    #[error("应用偏好设置格式无效：{0}")]
    Parse(#[source] serde_json::Error),
    #[error("应用偏好设置无效：{0}")]
    Invalid(String),
    #[error("无法保存应用偏好设置：{0}")]
    Write(#[source] std::io::Error),
    #[error("无法序列化应用偏好设置：{0}")]
    Serialize(#[source] serde_json::Error),
}

pub struct PreferencesState {
    stored: RwLock<StoredPreferences>,
    file_path: PathBuf,
}

impl PreferencesState {
    pub fn relay_endpoint(&self, transport: TransportKind) -> Option<String> {
        self.stored
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .relay_endpoints
            .get(&transport)
            .cloned()
    }
    pub fn set_relay_endpoint(
        &self,
        transport: TransportKind,
        endpoint: String,
    ) -> Result<(), PreferencesError> {
        self.update(|stored| {
            stored.relay_endpoints.insert(transport, endpoint);
        })
    }
    pub fn ble_parameters(&self, device_id: &str) -> BleParameters {
        self.stored
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .ble_parameters
            .get(device_id)
            .cloned()
            .unwrap_or_default()
    }
    pub fn set_ble_parameters(
        &self,
        device_id: String,
        parameters: BleParameters,
    ) -> Result<(), PreferencesError> {
        self.update(|stored| {
            stored.ble_parameters.insert(device_id, parameters);
        })
    }
    pub fn load(config_dir: PathBuf) -> Result<Self, PreferencesError> {
        let file_path = config_dir.join(PREFERENCES_FILE_NAME);
        let snapshot: StoredPreferences = match fs::File::open(&file_path) {
            Ok(file) => {
                let mut content = Vec::new();
                file.take(MAX_PREFERENCES_BYTES + 1)
                    .read_to_end(&mut content)
                    .map_err(PreferencesError::Read)?;
                if content.len() as u64 > MAX_PREFERENCES_BYTES {
                    return Err(PreferencesError::Read(std::io::Error::new(
                        ErrorKind::InvalidData,
                        "偏好设置超过 4 MiB",
                    )));
                }
                serde_json::from_slice(&content).map_err(PreferencesError::Parse)?
            }
            Err(error) if error.kind() == ErrorKind::NotFound => StoredPreferences::default(),
            Err(error) => return Err(PreferencesError::Read(error)),
        };
        snapshot.validate()?;
        Ok(Self::new(file_path, snapshot))
    }

    pub fn snapshot(&self, auto_start: bool) -> AppPreferencesSnapshot {
        let stored = self
            .stored
            .read()
            .unwrap_or_else(|error| error.into_inner());
        AppPreferencesSnapshot {
            close_to_tray: stored.close_to_tray,
            auto_start,
            start_minimized: stored.start_minimized,
        }
    }

    pub fn close_to_tray(&self) -> bool {
        self.stored
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .close_to_tray
    }

    pub fn start_minimized(&self) -> bool {
        self.stored
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .start_minimized
    }

    pub fn default_source_id(&self) -> Option<String> {
        self.stored
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .default_source_id
            .clone()
    }

    pub fn fixed_waveform(&self) -> Option<WaveformConfig> {
        self.stored
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .fixed_waveform
            .clone()
    }

    pub fn custom_waveforms(&self) -> Vec<WaveformConfig> {
        self.stored
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .custom_waveforms
            .clone()
    }

    pub fn safety_settings(&self) -> (bool, u16, bool) {
        let stored = self
            .stored
            .read()
            .unwrap_or_else(|error| error.into_inner());
        (
            stored.connection_timeout_enabled,
            stored.connection_timeout_minutes,
            stored.allow_app_intensity_control,
        )
    }

    pub fn set_safety_settings(
        &self,
        connection_timeout_enabled: bool,
        connection_timeout_minutes: u16,
        allow_app_intensity_control: bool,
    ) -> Result<(), PreferencesError> {
        self.update(|stored| {
            stored.connection_timeout_enabled = connection_timeout_enabled;
            stored.connection_timeout_minutes = connection_timeout_minutes;
            stored.allow_app_intensity_control = allow_app_intensity_control;
        })
    }

    pub fn set_close_to_tray(&self, enabled: bool) -> Result<(), PreferencesError> {
        self.update(|stored| stored.close_to_tray = enabled)
    }

    pub fn set_start_minimized(&self, enabled: bool) -> Result<(), PreferencesError> {
        self.update(|stored| stored.start_minimized = enabled)
    }

    pub fn set_default_source_id(&self, source_id: Option<String>) -> Result<(), PreferencesError> {
        self.update(|stored| stored.default_source_id = source_id)
    }

    pub fn set_waveform_state(
        &self,
        selected: Option<WaveformConfig>,
        waveforms: Vec<WaveformConfig>,
    ) -> Result<(), PreferencesError> {
        self.update(|stored| {
            stored.fixed_waveform = selected;
            stored.custom_waveforms = waveforms;
        })
    }

    fn new(file_path: PathBuf, snapshot: StoredPreferences) -> Self {
        Self {
            stored: RwLock::new(snapshot),
            file_path,
        }
    }

    fn update(&self, update: impl FnOnce(&mut StoredPreferences)) -> Result<(), PreferencesError> {
        let mut stored = self
            .stored
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let mut next = stored.clone();
        update(&mut next);
        next.validate()?;
        self.persist(&next)?;
        *stored = next;
        Ok(())
    }

    fn persist(&self, snapshot: &StoredPreferences) -> Result<(), PreferencesError> {
        if let Some(parent) = self.file_path.parent() {
            fs::create_dir_all(parent).map_err(PreferencesError::Write)?;
        }
        let content = serde_json::to_vec_pretty(&snapshot).map_err(PreferencesError::Serialize)?;
        let temporary = self
            .file_path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(PreferencesError::Write)?;
            std::io::Write::write_all(&mut file, &content).map_err(PreferencesError::Write)?;
            file.sync_all().map_err(PreferencesError::Write)?;
            drop(file);
            fs::rename(&temporary, &self.file_path).map_err(PreferencesError::Write)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_config_dir() -> PathBuf {
        std::env::temp_dir().join(format!("dg-lab-link-preferences-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn invalid_persisted_values_are_rejected_without_modifying_the_file() {
        for invalid in [
            serde_json::json!({"connectionTimeoutMinutes":0}),
            serde_json::json!({"fixedWaveform":{"presetId":"bad","presetName":"Bad","frames":["FFFFFFFFFFFFFFFF"]}}),
            serde_json::json!({"relayEndpoints":{"ws_v3":"https://example.test"}}),
            serde_json::json!({"bleParameters":{"device":{"maxStrengthA":201}}}),
            serde_json::json!({"unexpected":{}}),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(PREFERENCES_FILE_NAME);
            let original = serde_json::to_vec(&invalid).unwrap();
            fs::write(&path, &original).unwrap();
            assert!(
                PreferencesState::load(directory.path().to_owned()).is_err(),
                "{invalid}"
            );
            assert_eq!(fs::read(path).unwrap(), original);
        }
    }

    #[test]
    fn invalid_direct_update_preserves_memory_and_persisted_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let state = PreferencesState::load(directory.path().to_owned()).unwrap();
        state.set_close_to_tray(false).unwrap();
        let path = directory.path().join(PREFERENCES_FILE_NAME);
        let original = fs::read(&path).unwrap();
        assert!(state.set_safety_settings(true, 0, true).is_err());
        assert_eq!(state.safety_settings(), (false, 60, false));
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn oversized_preferences_are_rejected_before_parsing() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join(PREFERENCES_FILE_NAME),
            vec![b' '; MAX_PREFERENCES_BYTES as usize + 1],
        )
        .unwrap();
        let result = PreferencesState::load(directory.path().to_path_buf());
        assert!(
            matches!(result, Err(PreferencesError::Read(error)) if error.kind() == ErrorKind::InvalidData)
        );
    }

    #[test]
    fn close_to_tray_is_enabled_by_default() {
        let config_dir = temporary_config_dir();
        let state = PreferencesState::load(config_dir).unwrap();

        assert!(state.close_to_tray());
        assert!(state.start_minimized());
        assert_eq!(state.default_source_id(), None);
        assert_eq!(
            state.fixed_waveform().unwrap().preset_id,
            crate::sources::DEFAULT_WAVEFORM_ID
        );
        assert_eq!(state.custom_waveforms(), Vec::new());
    }

    #[test]
    fn close_to_tray_preference_is_persisted() {
        let config_dir = temporary_config_dir();
        let state = PreferencesState::load(config_dir.clone()).unwrap();
        state.set_close_to_tray(false).unwrap();

        let reloaded = PreferencesState::load(config_dir.clone()).unwrap();
        assert!(!reloaded.close_to_tray());

        fs::remove_dir_all(config_dir).unwrap();
    }

    #[test]
    fn connection_timeout_and_reverse_control_are_persisted() {
        let config_dir = temporary_config_dir();
        let state = PreferencesState::load(config_dir.clone()).unwrap();
        assert_eq!(state.safety_settings(), (false, 60, false));
        state.set_safety_settings(true, 90, true).unwrap();
        let reloaded = PreferencesState::load(config_dir.clone()).unwrap();
        assert_eq!(reloaded.safety_settings(), (true, 90, true));
        fs::remove_dir_all(config_dir).unwrap();
    }

    #[test]
    fn start_minimized_preference_is_persisted() {
        let config_dir = temporary_config_dir();
        let state = PreferencesState::load(config_dir.clone()).unwrap();
        state.set_start_minimized(false).unwrap();

        let reloaded = PreferencesState::load(config_dir.clone()).unwrap();
        assert!(!reloaded.start_minimized());
        assert!(reloaded.close_to_tray());

        fs::remove_dir_all(config_dir).unwrap();
    }

    #[test]
    fn omitted_optional_preferences_use_defaults() {
        let config_dir = temporary_config_dir();
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join(PREFERENCES_FILE_NAME),
            r#"{"closeToTray":false}"#,
        )
        .unwrap();

        let state = PreferencesState::load(config_dir.clone()).unwrap();
        assert!(!state.close_to_tray());
        assert!(state.start_minimized());
        assert_eq!(state.default_source_id(), None);
        assert_eq!(
            state.fixed_waveform().unwrap().preset_id,
            crate::sources::DEFAULT_WAVEFORM_ID
        );

        fs::remove_dir_all(config_dir).unwrap();
    }

    #[test]
    fn default_source_preference_is_persisted_and_can_be_cleared() {
        let config_dir = temporary_config_dir();
        let state = PreferencesState::load(config_dir.clone()).unwrap();
        state
            .set_default_source_id(Some("source-fixed-waveform".to_owned()))
            .unwrap();

        let reloaded = PreferencesState::load(config_dir.clone()).unwrap();
        assert_eq!(
            reloaded.default_source_id().as_deref(),
            Some("source-fixed-waveform")
        );
        reloaded.set_default_source_id(None).unwrap();

        let cleared = PreferencesState::load(config_dir.clone()).unwrap();
        assert_eq!(cleared.default_source_id(), None);

        fs::remove_dir_all(config_dir).unwrap();
    }

    #[test]
    fn waveform_preferences_are_persisted() {
        let config_dir = temporary_config_dir();
        let state = PreferencesState::load(config_dir.clone()).unwrap();
        let custom = WaveformConfig {
            preset_id: "custom-1".to_owned(),
            preset_name: "自定义".to_owned(),
            frames: vec!["0A0A0A0A64646464".to_owned()],
        };
        state
            .set_waveform_state(Some(custom.clone()), vec![custom.clone()])
            .unwrap();

        let reloaded = PreferencesState::load(config_dir.clone()).unwrap();
        assert_eq!(reloaded.fixed_waveform(), Some(custom.clone()));
        assert_eq!(reloaded.custom_waveforms(), vec![custom]);

        fs::remove_dir_all(config_dir).unwrap();
    }

    #[test]
    fn transport_configuration_persists_only_durable_parameters() {
        let config_dir = temporary_config_dir();
        let state = PreferencesState::load(config_dir.clone()).unwrap();
        assert_eq!(state.relay_endpoint(TransportKind::WsV3), None);
        assert_eq!(state.ble_parameters("peripheral"), BleParameters::default());
        state
            .set_relay_endpoint(TransportKind::WsV3, "ws://127.0.0.1:9000/".to_owned())
            .unwrap();
        let parameters = BleParameters {
            max_strength_a: 75,
            ..BleParameters::default()
        };
        state
            .set_ble_parameters("peripheral".to_owned(), parameters.clone())
            .unwrap();
        let reloaded = PreferencesState::load(config_dir.clone()).unwrap();
        assert_eq!(reloaded.ble_parameters("peripheral"), parameters);
        assert_eq!(
            reloaded.relay_endpoint(TransportKind::WsV3).as_deref(),
            Some("ws://127.0.0.1:9000/")
        );
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(config_dir.join(PREFERENCES_FILE_NAME)).unwrap())
                .unwrap();
        assert!(saved.get("connections").is_none());
        assert!(saved.get("output").is_none());
        assert!(saved.get("intensityA").is_none());
        fs::remove_dir_all(config_dir).unwrap();
    }
}
