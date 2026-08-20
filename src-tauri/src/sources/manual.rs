use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{WaveFrame, WaveSample};

use super::{SourceDescriptor, SourceError, SourceFactory, WaveSource};

/// 固定输出一帧用户给定波形，适合 UI 手动预览和端到端链路测试。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ManualConfig {
    pub frequencies: [u8; WaveFrame::SAMPLE_COUNT],
    pub intensities: [u8; WaveFrame::SAMPLE_COUNT],
}

impl Default for ManualConfig {
    fn default() -> Self {
        Self {
            frequencies: [20, 20, 20, 20],
            intensities: [0, 0, 0, 0],
        }
    }
}

pub struct ManualFactory;

impl SourceFactory for ManualFactory {
    fn descriptor(&self) -> SourceDescriptor {
        SourceDescriptor {
            kind: "builtin.manual",
            display_name: "手动波形",
            description: "持续输出一帧由频率和波形强度定义的固定波形",
        }
    }

    fn default_config(&self) -> Value {
        serde_json::to_value(ManualConfig::default()).expect("built-in config is serializable")
    }

    fn validate(&self, config: &Value) -> Result<(), SourceError> {
        frame_from_config(parse_config(config)?).map(|_| ())
    }

    fn build(&self, config: &Value) -> Result<Box<dyn WaveSource>, SourceError> {
        Ok(Box::new(ManualSource {
            frame: frame_from_config(parse_config(config)?)?,
        }))
    }
}

struct ManualSource {
    frame: WaveFrame,
}

impl WaveSource for ManualSource {
    fn next_frame(&mut self) -> Result<WaveFrame, SourceError> {
        Ok(self.frame)
    }
}

fn parse_config(config: &Value) -> Result<ManualConfig, SourceError> {
    serde_json::from_value(config.clone()).map_err(|error| SourceError::InvalidConfig {
        kind: "builtin.manual",
        message: error.to_string(),
    })
}

fn frame_from_config(config: ManualConfig) -> Result<WaveFrame, SourceError> {
    let mut samples = [WaveSample::silent(); WaveFrame::SAMPLE_COUNT];
    for (index, sample) in samples.iter_mut().enumerate() {
        *sample = WaveSample::new(config.frequencies[index], config.intensities[index]).map_err(
            |error| SourceError::InvalidConfig {
                kind: "builtin.manual",
                message: format!("第 {} 组采样：{error}", index + 1),
            },
        )?;
    }

    Ok(WaveFrame::new(samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_repeats_exact_manual_frame() {
        let factory = ManualFactory;
        let config = serde_json::json!({
            "frequencies": [10, 20, 30, 40],
            "intensities": [0, 25, 50, 100]
        });
        let mut source = factory.build(&config).unwrap();

        let first = source.next_frame().unwrap();
        let second = source.next_frame().unwrap();
        assert_eq!(first, second);
        assert_eq!(first.samples()[2], WaveSample::new(30, 50).unwrap());
    }

    #[test]
    fn source_rejects_invalid_sample_instead_of_silently_clamping() {
        let factory = ManualFactory;
        let config = serde_json::json!({
            "frequencies": [10, 20, 30, 9],
            "intensities": [0, 25, 50, 100]
        });

        assert!(matches!(
            factory.validate(&config),
            Err(SourceError::InvalidConfig { .. })
        ));
        assert!(matches!(
            factory.build(&config),
            Err(SourceError::InvalidConfig { .. })
        ));
    }
}
