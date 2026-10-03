use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use dg_lab_link_contracts::ControlError;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const DEFAULT_PORT: u16 = 17845;
pub const DEFAULT_MCP_PORT: u16 = 17846;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalConfig {
    pub port: u16,
    pub mcp_port: u16,
    pub token: String,
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
            Ok(file) => read_config(file),
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
    #[cfg(any(feature = "server", test))]
    pub(crate) fn saved_mcp_url(directory: &Path) -> Result<String, ControlError> {
        Ok(read_config(File::open(directory.join("local-runtime.json"))?)?.mcp_url())
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

fn read_config(file: File) -> Result<LocalConfig, ControlError> {
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(ControlError::new(
            "runtime_config_invalid",
            "本机运行时配置超过 4 KiB",
        ));
    }
    let config: LocalConfig = serde_json::from_slice(&bytes)
        .map_err(|error| ControlError::new("runtime_config_invalid", error.to_string()))?;
    config.validate()?;
    Ok(config)
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
    fn initializes_and_reloads_one_configuration_format() {
        let directory = tempfile::tempdir().unwrap();
        let initial = LocalConfig::load(directory.path()).unwrap();
        assert_eq!(
            (initial.port, initial.mcp_port),
            (DEFAULT_PORT, DEFAULT_MCP_PORT)
        );
        assert_eq!(initial.token.len(), 64);
        initial.validate().unwrap();
        let path = directory.path().join("local-runtime.json");
        let original = fs::read(&path).unwrap();
        let core = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.path().join("core.lock"))
            .unwrap();
        core.try_lock().unwrap();
        let reloaded = LocalConfig::load(directory.path()).unwrap();
        assert_eq!(
            (reloaded.port, reloaded.mcp_port),
            (initial.port, initial.mcp_port)
        );
        assert_eq!(reloaded.token, initial.token);
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn invalid_configuration_is_rejected_without_rewriting_the_file() {
        for invalid in [
            serde_json::json!({"port":17845,"token":"a".repeat(64)}),
            serde_json::json!({"port":17845,"mcpPort":null,"token":"a".repeat(64)}),
            serde_json::json!({"port":17845,"mcpPort":17845,"token":"a".repeat(64)}),
            serde_json::json!({"port":17845,"mcpPort":17846,"token":"short"}),
            serde_json::json!({"port":17845,"mcpPort":17846,"token":"a".repeat(64),"unexpected":true}),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("local-runtime.json");
            let original = serde_json::to_vec(&invalid).unwrap();
            fs::write(&path, &original).unwrap();
            assert_eq!(
                LocalConfig::load(directory.path()).unwrap_err().code,
                "runtime_config_invalid"
            );
            assert_eq!(
                LocalConfig::saved_mcp_url(directory.path())
                    .unwrap_err()
                    .code,
                "runtime_config_invalid"
            );
            assert_eq!(fs::read(path).unwrap(), original);
        }
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
