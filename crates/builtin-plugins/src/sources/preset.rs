use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{WaveFrame, WaveSample};

use super::{SourceDescriptor, SourceError, SourceFactory, WaveSource};

pub const MAX_PRESET_ID_LENGTH: usize = 64;
pub const MAX_PRESET_NAME_LENGTH: usize = 64;
pub const MAX_PRESET_FRAMES: usize = 16_384;

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

pub struct FixedWaveformFactory;

impl SourceFactory for FixedWaveformFactory {
    fn descriptor(&self) -> SourceDescriptor {
        SourceDescriptor {
            kind: "builtin.fixed_waveform",
            display_name: "固定波形",
            description: "循环输出选定的内置或自定义固定波形",
        }
    }

    fn default_config(&self) -> Value {
        serialized_default_config()
    }

    fn validate(&self, config: &Value) -> Result<(), SourceError> {
        validate_config(config, "builtin.fixed_waveform")
    }

    fn build(&self, config: &Value) -> Result<Box<dyn WaveSource>, SourceError> {
        build_source(config, "builtin.fixed_waveform")
    }
}

struct PresetWaveformSource {
    frames: Vec<WaveFrame>,
    cursor: usize,
}

impl WaveSource for PresetWaveformSource {
    fn next_frame(&mut self) -> Result<WaveFrame, SourceError> {
        let frame = self.frames[self.cursor];
        self.cursor = (self.cursor + 1) % self.frames.len();
        Ok(frame)
    }
}

fn serialized_default_config() -> Value {
    serde_json::to_value(WaveformConfig::default()).expect("built-in config is serializable")
}

fn validate_config(config: &Value, kind: &'static str) -> Result<(), SourceError> {
    frames_from_config(&parse_config(config, kind)?, kind).map(|_| ())
}

fn build_source(config: &Value, kind: &'static str) -> Result<Box<dyn WaveSource>, SourceError> {
    let config = parse_config(config, kind)?;
    Ok(Box::new(PresetWaveformSource {
        frames: frames_from_config(&config, kind)?,
        cursor: 0,
    }))
}

fn parse_config(config: &Value, kind: &'static str) -> Result<WaveformConfig, SourceError> {
    serde_json::from_value(config.clone()).map_err(|error| invalid_config(kind, error.to_string()))
}

fn frames_from_config(
    config: &WaveformConfig,
    kind: &'static str,
) -> Result<Vec<WaveFrame>, SourceError> {
    if config.preset_id.is_empty() || config.preset_id.len() > MAX_PRESET_ID_LENGTH {
        return Err(invalid_config(kind, "波形标识长度必须在 1..=64 个字节"));
    }
    if config.preset_name.trim().is_empty() || config.preset_name.len() > MAX_PRESET_NAME_LENGTH {
        return Err(invalid_config(kind, "波形名称长度必须在 1..=64 个字节"));
    }
    if config.frames.is_empty() || config.frames.len() > MAX_PRESET_FRAMES {
        return Err(invalid_config(kind, "波形必须包含 1..=16384 帧"));
    }

    config
        .frames
        .iter()
        .enumerate()
        .map(|(index, encoded)| {
            let bytes = decode_frame(encoded).map_err(|error| {
                invalid_config(kind, format!("第 {} 帧无效：{error}", index + 1))
            })?;
            let mut samples = [WaveSample::silent(); WaveFrame::SAMPLE_COUNT];
            for sample_index in 0..WaveFrame::SAMPLE_COUNT {
                samples[sample_index] = WaveSample::new(
                    bytes[sample_index],
                    bytes[sample_index + WaveFrame::SAMPLE_COUNT],
                )
                .map_err(|error| {
                    invalid_config(kind, format!("第 {} 帧无效：{error}", index + 1))
                })?;
            }
            Ok(WaveFrame::new(samples))
        })
        .collect()
}

fn decode_frame(encoded: &str) -> Result<[u8; 8], String> {
    if encoded.len() != 16 || !encoded.is_ascii() {
        return Err("波形必须由 16 个十六进制字符组成".to_owned());
    }
    let mut bytes = [0; 8];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&encoded[index * 2..index * 2 + 2], 16)
            .map_err(|_| "波形包含非法十六进制字符".to_owned())?;
    }
    Ok(bytes)
}
fn invalid_config(kind: &'static str, message: impl Into<String>) -> SourceError {
    SourceError::InvalidConfig {
        kind,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_source_cycles_through_selected_frames() {
        let factory = FixedWaveformFactory;
        let config = serde_json::json!({
            "presetId": "BUBBLE",
            "presetName": "气泡",
            "frames": ["2D2D2D2D00000000", "2D2D2D2D64646464"]
        });
        let mut source = factory.build(&config).unwrap();

        let silent = source.next_frame().unwrap();
        let active = source.next_frame().unwrap();
        let repeated = source.next_frame().unwrap();
        assert_eq!(silent.samples()[0], WaveSample::new(45, 0).unwrap());
        assert_eq!(active.samples()[2], WaveSample::new(45, 100).unwrap());
        assert_eq!(repeated, silent);
    }

    #[test]
    fn default_source_uses_official_breathing_waveform() {
        let factory = FixedWaveformFactory;
        let mut source = factory.build(&factory.default_config()).unwrap();

        assert_eq!(source.next_frame().unwrap(), WaveFrame::silent());
        assert_eq!(
            source.next_frame().unwrap().samples()[0],
            WaveSample::new(10, 20).unwrap()
        );
    }

    #[test]
    fn fixed_factory_rejects_invalid_frames() {
        let invalid = serde_json::json!({
            "presetId": "INVALID",
            "presetName": "无效",
            "frames": ["090A0A0A00000000"]
        });

        assert!(matches!(
            FixedWaveformFactory.validate(&invalid),
            Err(SourceError::InvalidConfig { .. })
        ));
    }
}
