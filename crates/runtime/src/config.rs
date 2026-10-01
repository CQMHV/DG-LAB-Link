use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use dg_lab_link_core::ControlError;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const DEFAULT_PORT: u16 = 17846;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalConfig {
    pub port: u16,
    pub token: String,
}

impl LocalConfig {
    pub fn load(directory: &Path) -> Result<Self, ControlError> {
        fs::create_dir_all(directory)?;
        let _lock = configuration_lock(directory)?;
        let path = directory.join("local-runtime.json");
        match File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(4097).read_to_end(&mut bytes)?;
                if bytes.len() > 4096 {
                    return Err(ControlError::new(
                        "runtime_config_invalid",
                        "本机运行时配置超过 4 KiB",
                    ));
                }
                let config: Self = serde_json::from_slice(&bytes).map_err(|error| {
                    ControlError::new("runtime_config_invalid", error.to_string())
                })?;
                config.validate()?;
                Ok(config)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let config = Self {
                    port: DEFAULT_PORT,
                    token: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
                };
                config.save_unlocked(directory)?;
                Ok(config)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Port changes take effect on the next core launch. The token is not rotated.
    pub fn save(&self, directory: &Path) -> Result<(), ControlError> {
        self.validate()?;
        fs::create_dir_all(directory)?;
        let _lock = configuration_lock(directory)?;
        self.save_unlocked(directory)
    }

    pub fn mcp_url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.port)
    }

    fn validate(&self) -> Result<(), ControlError> {
        if self.port == 0
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
