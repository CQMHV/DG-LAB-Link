use serde::{Deserialize, Serialize};

pub const DEFAULT_WAVEFORM_ID: &str = "BREATHING";
pub const DEFAULT_WAVEFORM_NAME: &str = "呼吸";

const DEFAULT_WAVEFORM_FRAMES: [&str; 12] = [
    "0A0A0A0A00000000",
    "0A0A0A0A14141414",
    "0A0A0A0A28282828",
    "0A0A0A0A3C3C3C3C",
    "0A0A0A0A50505050",
    "0A0A0A0A64646464",
    "0A0A0A0A64646464",
    "0A0A0A0A64646464",
    "0A0A0A0A00000000",
    "0A0A0A0A00000000",
    "0A0A0A0A00000000",
    "0A0A0A0A00000000",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct WaveformConfig {
    pub preset_id: String,
    pub preset_name: String,
    pub frames: Vec<String>,
}

impl Default for WaveformConfig {
    fn default() -> Self {
        Self {
            preset_id: DEFAULT_WAVEFORM_ID.to_owned(),
            preset_name: DEFAULT_WAVEFORM_NAME.to_owned(),
            frames: DEFAULT_WAVEFORM_FRAMES
                .iter()
                .map(|frame| (*frame).to_owned())
                .collect(),
        }
    }
}
