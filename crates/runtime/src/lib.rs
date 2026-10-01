mod client;
mod config;
mod server;
mod wire;

pub use client::{AcceptedCommandEpoch, Client, connect_or_spawn, core_executable};
pub use config::{DEFAULT_MCP_PORT, DEFAULT_PORT, LocalConfig, config_dir};
pub use server::run_core;
pub use wire::{HolderInfo, RuntimeInfo};

pub const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
pub(crate) const HEARTBEAT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub(crate) const SOCKET_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
