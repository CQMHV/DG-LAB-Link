use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{WaveFrame, WaveSample};

use super::{SourceDescriptor, SourceError, SourceFactory, WaveSource};

/// DG-LAB-VRCOSC 默认的“呼吸”序列。
///
/// 原序列中的 `(frequency=0, intensity=0)` 是静默采样；本项目领域模型使用
/// 协议允许的最低频率 `10` 配合零脉冲强度表达同一语义。
const BREATH_PATTERN: [[u8; WaveFrame::SAMPLE_COUNT]; 11] = [
    [0, 0, 0, 0],
    [0, 5, 10, 20],
    [20, 25, 30, 40],
    [40, 45, 50, 60],
    [60, 65, 70, 80],
    [100, 100, 100, 100],
    [100, 100, 100, 100],
    [100, 100, 100, 100],
    [0, 0, 0, 0],
    [0, 0, 0, 0],
    [0, 0, 0, 0],
];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestPatternConfig {}

pub struct TestPatternFactory;

impl SourceFactory for TestPatternFactory {
    fn descriptor(&self) -> SourceDescriptor {
        SourceDescriptor {
            kind: "builtin.test_pattern",
            display_name: "测试波形",
            description: "使用 DG-LAB-VRCOSC 默认呼吸波形验证输出链路",
        }
    }

    fn default_config(&self) -> Value {
        serde_json::to_value(TestPatternConfig::default()).expect("built-in config is serializable")
    }

    fn validate(&self, config: &Value) -> Result<(), SourceError> {
        parse_config(config).map(|_| ())
    }

    fn build(&self, config: &Value) -> Result<Box<dyn WaveSource>, SourceError> {
        parse_config(config)?;
        Ok(Box::new(TestPatternSource { frame_index: 0 }))
    }
}

struct TestPatternSource {
    frame_index: usize,
}

impl WaveSource for TestPatternSource {
    fn next_frame(&mut self) -> Result<WaveFrame, SourceError> {
        let intensities = BREATH_PATTERN[self.frame_index];
        let mut samples = [WaveSample::silent(); WaveFrame::SAMPLE_COUNT];
        for (sample, intensity) in samples.iter_mut().zip(intensities) {
            *sample = WaveSample::new(WaveSample::MIN_FREQUENCY, intensity)
                .map_err(|error| SourceError::Runtime(error.to_string()))?;
        }
        self.frame_index = (self.frame_index + 1) % BREATH_PATTERN.len();
        Ok(WaveFrame::new(samples))
    }
}

fn parse_config(config: &Value) -> Result<TestPatternConfig, SourceError> {
    serde_json::from_value(config.clone()).map_err(|error| SourceError::InvalidConfig {
        kind: "builtin.test_pattern",
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_pattern_matches_dglab_vrcosc_breath_envelope_and_repeats() {
        let factory = TestPatternFactory;
        let mut source = factory.build(&serde_json::json!({})).unwrap();

        let frames: Vec<_> = (0..=BREATH_PATTERN.len())
            .map(|_| source.next_frame().unwrap())
            .collect();
        for (frame, expected) in frames.iter().take(BREATH_PATTERN.len()).zip(BREATH_PATTERN) {
            assert_eq!(
                frame.samples().map(|sample| sample.pulse_intensity()),
                expected
            );
            assert!(
                frame
                    .samples()
                    .iter()
                    .all(|sample| sample.frequency() == WaveSample::MIN_FREQUENCY)
            );
        }
        assert_eq!(frames[BREATH_PATTERN.len()], frames[0]);
    }

    #[test]
    fn obsolete_sweep_fields_are_rejected() {
        let factory = TestPatternFactory;
        assert!(
            factory
                .validate(&serde_json::json!({
                    "frequencyStart": 20,
                    "frequencyEnd": 80,
                    "intensity": 20,
                    "step": 5
                }))
                .is_err()
        );
    }
}
