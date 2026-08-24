use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const PREFERENCES_FILE_NAME: &str = "preferences.json";

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppPreferencesSnapshot {
    pub close_to_tray: bool,
}

impl Default for AppPreferencesSnapshot {
    fn default() -> Self {
        Self {
            close_to_tray: true,
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
    file_path: PathBuf,
}

impl PreferencesState {
    pub fn load(config_dir: PathBuf) -> Result<Self, PreferencesError> {
        let file_path = config_dir.join(PREFERENCES_FILE_NAME);
        let snapshot = match fs::read_to_string(&file_path) {
            Ok(content) => serde_json::from_str(&content).map_err(PreferencesError::Parse)?,
            Err(error) if error.kind() == ErrorKind::NotFound => AppPreferencesSnapshot::default(),
            Err(error) => return Err(PreferencesError::Read(error)),
        };
        Ok(Self::new(file_path, snapshot))
    }

    pub fn with_defaults(config_dir: PathBuf) -> Self {
        Self::new(
            config_dir.join(PREFERENCES_FILE_NAME),
            AppPreferencesSnapshot::default(),
        )
    }

    pub fn snapshot(&self) -> AppPreferencesSnapshot {
        AppPreferencesSnapshot {
            close_to_tray: self.close_to_tray(),
        }
    }

    pub fn close_to_tray(&self) -> bool {
        self.close_to_tray.load(Ordering::Acquire)
    }

    pub fn set_close_to_tray(
        &self,
        enabled: bool,
    ) -> Result<AppPreferencesSnapshot, PreferencesError> {
        let snapshot = AppPreferencesSnapshot {
            close_to_tray: enabled,
        };
        self.persist(snapshot)?;
        self.close_to_tray.store(enabled, Ordering::Release);
        Ok(snapshot)
    }

    fn new(file_path: PathBuf, snapshot: AppPreferencesSnapshot) -> Self {
        Self {
            close_to_tray: AtomicBool::new(snapshot.close_to_tray),
            file_path,
        }
    }

    fn persist(&self, snapshot: AppPreferencesSnapshot) -> Result<(), PreferencesError> {
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
}
