//! Legacy touch configuration and input DTOs. Processing lives in the installed plugin.
use super::WaveformConfig;
use super::mapping::MappingPoint;
use crate::model::Channel;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum TouchMode {
    #[default]
    Free,
    Rhythm,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum TouchRouting {
    A,
    B,
    #[default]
    Sync,
    Separate,
    Alternate,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum TouchIntensityMode {
    #[default]
    Classic,
    Gradient,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum TouchGradientDirection {
    Left,
    Right,
    #[default]
    Both,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct TouchConfig {
    pub mode: TouchMode,
    pub routing: TouchRouting,
    pub grid_size: usize,
    pub swap_axes: bool,
    pub intensity_mode: TouchIntensityMode,
    pub gradient_direction: TouchGradientDirection,
    pub intensity_curve: Vec<MappingPoint>,
    pub period_curve: Vec<MappingPoint>,
    pub free_waveforms: Vec<WaveformConfig>,
    pub rhythm_waveforms: Vec<WaveformConfig>,
    pub background: Option<WaveformConfig>,
}

impl Default for TouchConfig {
    fn default() -> Self {
        static DEFAULT_WAVEFORMS: OnceLock<[WaveformConfig; 16]> = OnceLock::new();
        let waveforms = DEFAULT_WAVEFORMS.get_or_init(|| {
            serde_json::from_str(include_str!("touch-defaults.json"))
                .expect("built-in touch waveform library is valid")
        });
        Self {
            mode: TouchMode::Free,
            routing: TouchRouting::Sync,
            grid_size: 4,
            swap_axes: false,
            intensity_mode: TouchIntensityMode::Classic,
            gradient_direction: TouchGradientDirection::Both,
            intensity_curve: vec![
                MappingPoint { x: 0.0, y: 0.0 },
                MappingPoint { x: 0.33, y: 100.0 },
                MappingPoint { x: 0.66, y: 100.0 },
                MappingPoint { x: 1.0, y: 0.0 },
            ],
            period_curve: vec![
                MappingPoint { x: 0.0, y: 100.0 },
                MappingPoint { x: 0.33, y: 10.0 },
                MappingPoint { x: 0.66, y: 10.0 },
                MappingPoint { x: 1.0, y: 100.0 },
            ],
            free_waveforms: waveforms[..8].to_vec(),
            rhythm_waveforms: waveforms.to_vec(),
            background: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TouchPointer {
    pub id: i64,
    pub x: f64,
    pub y: f64,
    pub cell: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<Channel>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TouchInput {
    pub device_id: String,
    pub owner_id: String,
    pub sequence: u64,
    pub pointers: Vec<TouchPointer>,
}
