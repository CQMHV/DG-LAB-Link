//! Host-side native plugin management. No Tauri, WebView or JavaScript runtime.

mod frames;
mod manager;
pub mod package;
mod process;

pub use dg_lab_link_plugin_sdk::*;
pub use frames::LatestFrameStore;
pub use manager::{
    InstalledPlugin, PluginManager, PluginRuntimeSnapshot, SourceState, SourceStatus,
};
pub use process::{BusinessFuture, BusinessHandler};
