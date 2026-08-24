use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

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

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default, rename_all = "camelCase")]
struct StoredPreferences {
    pub close_to_tray: bool,
    pub start_minimized: bool,
}

impl Default for StoredPreferences {
    fn default() -> Self {
        Self {
            close_to_tray: true,
            start_minimized: true,
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
    close_to_tray: AtomicBool,
    start_minimized: AtomicBool,
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
        AppPreferencesSnapshot {
            close_to_tray: self.close_to_tray(),
            auto_start,
            start_minimized: self.start_minimized(),
        }
    }

    pub fn close_to_tray(&self) -> bool {
        self.close_to_tray.load(Ordering::Acquire)
    }

    pub fn start_minimized(&self) -> bool {
        self.start_minimized.load(Ordering::Acquire)
    }

    pub fn set_close_to_tray(&self, enabled: bool) -> Result<(), PreferencesError> {
        let snapshot = StoredPreferences {
            close_to_tray: enabled,
            start_minimized: self.start_minimized(),
        };
        self.persist(snapshot)?;
        self.close_to_tray.store(enabled, Ordering::Release);
        Ok(())
    }

    pub fn set_start_minimized(&self, enabled: bool) -> Result<(), PreferencesError> {
        let snapshot = StoredPreferences {
            close_to_tray: self.close_to_tray(),
            start_minimized: enabled,
        };
        self.persist(snapshot)?;
        self.start_minimized.store(enabled, Ordering::Release);
        Ok(())
    }

    fn new(file_path: PathBuf, snapshot: StoredPreferences) -> Self {
        Self {
            close_to_tray: AtomicBool::new(snapshot.close_to_tray),
            start_minimized: AtomicBool::new(snapshot.start_minimized),
            file_path,
        }
    }

    fn persist(&self, snapshot: StoredPreferences) -> Result<(), PreferencesError> {
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

        fs::remove_dir_all(config_dir).unwrap();
    }
}
