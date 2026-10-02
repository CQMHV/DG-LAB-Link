//! Native input-source plugin protocol and authoring SDK.
//! stdout is reserved for framed IPC. Log to stderr instead.

pub mod protocol;
mod runner;
mod types;

pub use async_trait::async_trait;
pub use runner::{Plugin, PluginContext, run_plugin};
pub use types::*;
