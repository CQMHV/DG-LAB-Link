use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const PREFERENCES_FILE_NAME: &str = "preferences.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppPreferencesSnapshot {
    pub close_to_tray: bool,
    pub auto_start: bool,
    pub start_minimized: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default, rename_all = "camelCase")]
struct StoredPreferences {
    close_to_tray: bool,
    start_minimized: bool,
    default_source_id: Option<String>,
}

impl Default for StoredPreferences {
    fn default() -> Self {
        Self {
            close_to_tray: true,
            start_minimized: true,
            default_source_id: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum PreferencesError {
    #[error("无法读取应用偏好设置：{0}")]
    Read(#[source] std::io::Error),
    #[error("应用偏好设置格式无效：{0}")]
    Parse(#[source] serde_json::Error),
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
    pub fn load(config_dir: PathBuf) -> Result<Self, PreferencesError> {
        let file_path = config_dir.join(PREFERENCES_FILE_NAME);
        let snapshot = match fs::read_to_string(&file_path) {
            Ok(content) => serde_json::from_str(&content).map_err(PreferencesError::Parse)?,
            Err(error) if error.kind() == ErrorKind::NotFound => StoredPreferences::default(),
            Err(error) => return Err(PreferencesError::Read(error)),
        };
        Ok(Self::new(file_path, snapshot))
    }

    pub fn with_defaults(config_dir: PathBuf) -> Self {
        Self::new(
            config_dir.join(PREFERENCES_FILE_NAME),
            StoredPreferences::default(),
        )
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

    pub fn set_close_to_tray(&self, enabled: bool) -> Result<(), PreferencesError> {
        self.update(|stored| stored.close_to_tray = enabled)
    }

    pub fn set_start_minimized(&self, enabled: bool) -> Result<(), PreferencesError> {
        self.update(|stored| stored.start_minimized = enabled)
    }

    pub fn set_default_source_id(&self, source_id: Option<String>) -> Result<(), PreferencesError> {
        self.update(|stored| stored.default_source_id = source_id)
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
        self.persist(&next)?;
        *stored = next;
        Ok(())
    }

    fn persist(&self, snapshot: &StoredPreferences) -> Result<(), PreferencesError> {
        if let Some(parent) = self.file_path.parent() {
            fs::create_dir_all(parent).map_err(PreferencesError::Write)?;
        }
        let content = serde_json::to_vec_pretty(&snapshot).map_err(PreferencesError::Serialize)?;
        fs::write(&self.file_path, content).map_err(PreferencesError::Write)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_config_dir() -> PathBuf {
        std::env::temp_dir().join(format!("dg-lab-link-preferences-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn close_to_tray_is_enabled_by_default() {
        let config_dir = temporary_config_dir();
        let state = PreferencesState::load(config_dir).unwrap();

        assert!(state.close_to_tray());
        assert!(state.start_minimized());
        assert_eq!(state.default_source_id(), None);
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
    fn older_preferences_files_receive_the_new_default() {
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

        fs::remove_dir_all(config_dir).unwrap();
    }

    #[test]
    fn default_source_preference_is_persisted_and_can_be_cleared() {
        let config_dir = temporary_config_dir();
        let state = PreferencesState::load(config_dir.clone()).unwrap();
        state
            .set_default_source_id(Some("source-manual".to_owned()))
            .unwrap();

        let reloaded = PreferencesState::load(config_dir.clone()).unwrap();
        assert_eq!(
            reloaded.default_source_id().as_deref(),
            Some("source-manual")
        );
        reloaded.set_default_source_id(None).unwrap();

        let cleared = PreferencesState::load(config_dir.clone()).unwrap();
        assert_eq!(cleared.default_source_id(), None);

        fs::remove_dir_all(config_dir).unwrap();
    }
}
