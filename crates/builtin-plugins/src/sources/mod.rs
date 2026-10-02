pub mod audio;
pub mod mapping;
mod preset;
pub mod touch;

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::model::WaveFrame;

pub use preset::{FixedWaveformFactory, WaveformConfig};

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDescriptor {
    pub kind: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("输入源 {kind} 的配置无效：{message}")]
    InvalidConfig { kind: &'static str, message: String },
    #[error("输入源运行失败：{0}")]
    Runtime(String),
}

// Algorithm-local helpers, not a host registration mechanism. Keeping the
// waveform compiler here lets touch ship its bundled presets independently.
pub trait WaveSource: Send {
    fn next_frame(&mut self) -> Result<WaveFrame, SourceError>;
}

pub trait SourceFactory: Send + Sync {
    fn descriptor(&self) -> SourceDescriptor;
    fn default_config(&self) -> Value;
    fn validate(&self, config: &Value) -> Result<(), SourceError>;
    fn build(&self, config: &Value) -> Result<Box<dyn WaveSource>, SourceError>;
}
