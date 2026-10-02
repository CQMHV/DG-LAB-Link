use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use dg_lab_link_contracts::ControlError;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const DEFAULT_PORT: u16 = 17845;
pub const DEFAULT_MCP_PORT: u16 = 17846;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalConfig {
    pub port: u16,
    pub mcp_port: u16,
    pub token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredConfig {
    port: u16,
    mcp_port: Option<u16>,
    token: String,
}

impl LocalConfig {
    pub fn load(directory: &Path) -> Result<Self, ControlError> {
        fs::create_dir_all(directory)?;
        let _lock = configuration_lock(directory)?;
        Self::load_unlocked(directory)
    }

    fn load_unlocked(directory: &Path) -> Result<Self, ControlError> {
        let path = directory.join("local-runtime.json");
        match File::open(&path) {
            Ok(file) => {
                let stored = read_stored(file)?;
                let legacy = stored.mcp_port.is_none();
                let config = Self {
                    port: if legacy {
                        if stored.port == DEFAULT_PORT {
                            DEFAULT_MCP_PORT
                        } else {
                            DEFAULT_PORT
                        }
                    } else {
                        stored.port
                    },
                    mcp_port: stored.mcp_port.unwrap_or(stored.port),
                    token: stored.token,
                };
                config.validate()?;
                if legacy {
                    let lock = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .truncate(false)
                        .open(directory.join("core.lock"))?;
                    match lock.try_lock() {
                        Ok(()) => config.save_unlocked(directory)?,
                        Err(std::fs::TryLockError::WouldBlock) => {
                            return Err(ControlError::new(
                                "runtime_config_migration_required",
                                "请先退出旧版 GUI 和 CLI 并等待共享核心关闭，再迁移独立 MCP 端口配置",
                            ));
                        }
                        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
                    }
                }
                Ok(config)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let config = Self {
                    port: DEFAULT_PORT,
                    mcp_port: DEFAULT_MCP_PORT,
                    token: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
                };
                config.save_unlocked(directory)?;
                Ok(config)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Save both local interface ports. The token is not rotated.
    pub fn save(&self, directory: &Path) -> Result<(), ControlError> {
        self.validate()?;
        fs::create_dir_all(directory)?;
        let _lock = configuration_lock(directory)?;
        self.save_unlocked(directory)
    }

    pub fn mcp_url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.mcp_port)
    }

    // Saves replace the whole file atomically. Runtime info can read the latest
    // address without waiting for a configuration transaction on its WS loop.
    #[cfg(feature = "server")]
    pub(crate) fn saved_mcp_url(directory: &Path) -> Result<String, ControlError> {
        let stored = read_stored(File::open(directory.join("local-runtime.json"))?)?;
        let config = Self {
            port: stored.port,
            mcp_port: stored
                .mcp_port
                .ok_or_else(|| ControlError::new("runtime_config_invalid", "本机配置尚未迁移"))?,
            token: stored.token,
        };
        config.validate()?;
        Ok(config.mcp_url())
    }

    /// Change only the MCP HTTP port while its server is offline. The core may
    /// keep running; a concurrent core-port update cannot be overwritten.
    pub fn save_mcp_port(directory: &Path, port: u16) -> Result<(), ControlError> {
        fs::create_dir_all(directory)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("mcp-http.lock"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(ControlError::new(
                    "mcp_already_running",
                    "HTTP MCP 正在运行；请先结束它再修改端口",
                ));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
        let _configuration = configuration_lock(directory)?;
        let mut config = Self::load_unlocked(directory)?;
        config.mcp_port = port;
        config.validate()?;
        config.save_unlocked(directory)
    }

    #[cfg(any(feature = "server", test))]
    pub(crate) fn save_core_port(directory: &Path, port: u16) -> Result<Self, ControlError> {
        let _configuration = configuration_lock(directory)?;
        let mut config = Self::load_unlocked(directory)?;
        config.port = port;
        config.validate()?;
        config.save_unlocked(directory)?;
        Ok(config)
    }

    pub(crate) fn validate(&self) -> Result<(), ControlError> {
        if self.port == 0
            || self.mcp_port == 0
            || self.port == self.mcp_port
            || self.token.len() < 32
            || self.token.len() > 256
            || !self.token.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(ControlError::new(
                "runtime_config_invalid",
                "本机端口或令牌配置无效",
            ));
        }
        Ok(())
    }

    fn save_unlocked(&self, directory: &Path) -> Result<(), ControlError> {
        let temporary = directory.join(format!("local-runtime-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            file.write_all(&serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, directory.join("local-runtime.json"))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(Into::into)
    }
}

fn read_stored(file: File) -> Result<StoredConfig, ControlError> {
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(ControlError::new(
            "runtime_config_invalid",
            "本机运行时配置超过 4 KiB",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| ControlError::new("runtime_config_invalid", error.to_string()))
}

fn configuration_lock(directory: &Path) -> Result<File, ControlError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("local-runtime-config.lock"))?;
    file.lock()?;
    Ok(file)
}

pub fn config_dir() -> Result<PathBuf, ControlError> {
    dirs::config_dir()
        .map(|directory| directory.join("cn.dglab.link"))
        .ok_or_else(|| ControlError::new("config_dir_missing", "无法找到当前用户的配置目录"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn legacy_configuration_preserves_mcp_port_and_token() {
        for old_port in [17846, 19046, DEFAULT_PORT] {
            let directory = tempfile::tempdir().unwrap();
            let token = "a".repeat(64);
            fs::write(
                directory.path().join("local-runtime.json"),
                serde_json::to_vec(&serde_json::json!({"port": old_port, "token": token})).unwrap(),
            )
            .unwrap();
            let config = LocalConfig::load(directory.path()).unwrap();
            assert_eq!(config.mcp_port, old_port);
            assert_eq!(config.token, token);
            assert_ne!(config.port, config.mcp_port);
            assert_eq!(
                LocalConfig::load(directory.path()).unwrap().port,
                config.port
            );
            let persisted: serde_json::Value = serde_json::from_slice(
                &fs::read(directory.path().join("local-runtime.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(persisted["mcpPort"], old_port);
        }
    }

    #[test]
    fn live_legacy_core_prevents_migration_without_changing_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let original =
            serde_json::to_vec(&serde_json::json!({"port":17846,"token":"b".repeat(64)})).unwrap();
        let path = directory.path().join("local-runtime.json");
        fs::write(&path, &original).unwrap();
        let core = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.path().join("core.lock"))
            .unwrap();
        core.try_lock().unwrap();
        assert_eq!(
            LocalConfig::load(directory.path()).unwrap_err().code,
            "runtime_config_migration_required"
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        drop(core);
        assert_eq!(LocalConfig::load(directory.path()).unwrap().mcp_port, 17846);
    }

    #[test]
    fn independent_port_updates_serialize_and_failed_updates_roll_back() {
        let directory = tempfile::tempdir().unwrap();
        let original = LocalConfig::load(directory.path()).unwrap();
        std::thread::scope(|scope| {
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let first = barrier.clone();
            let path = directory.path();
            let core = scope.spawn(move || {
                first.wait();
                LocalConfig::save_core_port(path, 20001).unwrap();
            });
            let mcp = scope.spawn(move || {
                barrier.wait();
                LocalConfig::save_mcp_port(path, 20002).unwrap();
            });
            core.join().unwrap();
            mcp.join().unwrap();
        });
        let config = LocalConfig::load(directory.path()).unwrap();
        assert_eq!((config.port, config.mcp_port), (20001, 20002));
        assert_eq!(config.token, original.token);
        let before = fs::read(directory.path().join("local-runtime.json")).unwrap();
        for invalid in [0, config.port] {
            assert_eq!(
                LocalConfig::save_mcp_port(directory.path(), invalid)
                    .unwrap_err()
                    .code,
                "runtime_config_invalid"
            );
            assert_eq!(
                fs::read(directory.path().join("local-runtime.json")).unwrap(),
                before
            );
        }
        // A core runtime-info read must not wait on the persistent-config lock.
        let _configuration = configuration_lock(directory.path()).unwrap();
        assert_eq!(
            LocalConfig::saved_mcp_url(directory.path()).unwrap(),
            config.mcp_url()
        );
    }

    #[test]
    fn running_http_mcp_rejects_port_changes_without_mutating_config() {
        let directory = tempfile::tempdir().unwrap();
        let config = LocalConfig::load(directory.path()).unwrap();
        let before = fs::read(directory.path().join("local-runtime.json")).unwrap();
        let http = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.path().join("mcp-http.lock"))
            .unwrap();
        http.try_lock().unwrap();
        assert_eq!(
            LocalConfig::save_mcp_port(directory.path(), 19046)
                .unwrap_err()
                .code,
            "mcp_already_running"
        );
        assert_eq!(
            fs::read(directory.path().join("local-runtime.json")).unwrap(),
            before
        );
        drop(http);
        LocalConfig::save_mcp_port(directory.path(), 19046).unwrap();
        let after = LocalConfig::load(directory.path()).unwrap();
        assert_eq!(after.port, config.port);
        assert_eq!(after.mcp_port, 19046);
        assert_eq!(after.token, config.token);
    }
}
