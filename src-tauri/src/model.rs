use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

/// 输入源实例的稳定标识。
pub type SourceId = Uuid;

/// DG-LAB 设备的两个输出通道。
///
/// 领域层使用可读的 `a` / `b` 表示；V4 的 `0` / `1` wire 值由协议层转换。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    A,
    B,
}

impl Channel {
    pub const ALL: [Self; 2] = [Self::A, Self::B];

    pub const fn as_v4(self) -> u8 {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }

    pub const fn from_v4(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::A),
            1 => Some(Self::B),
            _ => None,
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::A => "A",
            Self::B => "B",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ModelError {
    #[error("波形频率编码必须在 {min}..={max}，当前为 {actual}")]
    FrequencyOutOfRange { min: u8, max: u8, actual: u8 },
    #[error("波形强度必须在 {min}..={max}，当前为 {actual}")]
    PulseIntensityOutOfRange { min: u8, max: u8, actual: u8 },
    #[error("波形块必须至少包含一帧")]
    EmptyWaveChunk,
    #[error("单个波形块最多包含 {max} 帧，当前为 {actual}")]
    WaveChunkTooLarge { max: usize, actual: usize },
}

/// 郊狼 V3 波形中的一组 25ms 采样。
///
/// `frequency` 是设备协议的频率编码而非 Hz。官方有效范围为 10..=240；
/// `pulse_intensity` 是波形脉宽相对值，范围为 0..=100，与通道绝对强度不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "WaveSampleWire", into = "WaveSampleWire")]
pub struct WaveSample {
    frequency: u8,
    pulse_intensity: u8,
}

impl WaveSample {
    pub const MIN_FREQUENCY: u8 = 10;
    pub const MAX_FREQUENCY: u8 = 240;
    pub const MIN_PULSE_INTENSITY: u8 = 0;
    pub const MAX_PULSE_INTENSITY: u8 = 100;

    pub fn new(frequency: u8, pulse_intensity: u8) -> Result<Self, ModelError> {
        if !(Self::MIN_FREQUENCY..=Self::MAX_FREQUENCY).contains(&frequency) {
            return Err(ModelError::FrequencyOutOfRange {
                min: Self::MIN_FREQUENCY,
                max: Self::MAX_FREQUENCY,
                actual: frequency,
            });
        }
        if !(Self::MIN_PULSE_INTENSITY..=Self::MAX_PULSE_INTENSITY).contains(&pulse_intensity) {
            return Err(ModelError::PulseIntensityOutOfRange {
                min: Self::MIN_PULSE_INTENSITY,
                max: Self::MAX_PULSE_INTENSITY,
                actual: pulse_intensity,
            });
        }

        Ok(Self {
            frequency,
            pulse_intensity,
        })
    }

    pub const fn frequency(self) -> u8 {
        self.frequency
    }

    pub const fn pulse_intensity(self) -> u8 {
        self.pulse_intensity
    }

    /// 使用最低合法频率和零波形强度构造安全的无输出采样。
    pub const fn silent() -> Self {
        Self {
            frequency: Self::MIN_FREQUENCY,
            pulse_intensity: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct WaveSampleWire {
    frequency: u8,
    pulse_intensity: u8,
}

impl TryFrom<WaveSampleWire> for WaveSample {
    type Error = ModelError;

    fn try_from(value: WaveSampleWire) -> Result<Self, Self::Error> {
        Self::new(value.frequency, value.pulse_intensity)
    }
}

impl From<WaveSample> for WaveSampleWire {
    fn from(value: WaveSample) -> Self {
        Self {
            frequency: value.frequency,
            pulse_intensity: value.pulse_intensity,
        }
    }
}

/// 一帧 100ms 波形，由四组连续的 25ms 采样组成。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaveFrame {
    samples: [WaveSample; Self::SAMPLE_COUNT],
}

impl WaveFrame {
    pub const SAMPLE_COUNT: usize = 4;
    pub const SAMPLE_PERIOD: Duration = Duration::from_millis(25);
    pub const DURATION: Duration = Duration::from_millis(100);

    pub const fn new(samples: [WaveSample; Self::SAMPLE_COUNT]) -> Self {
        Self { samples }
    }

    pub const fn repeat(sample: WaveSample) -> Self {
        Self {
            samples: [sample; Self::SAMPLE_COUNT],
        }
    }

    pub const fn silent() -> Self {
        Self::repeat(WaveSample::silent())
    }

    pub const fn samples(&self) -> &[WaveSample; Self::SAMPLE_COUNT] {
        &self.samples
    }

    pub const fn into_samples(self) -> [WaveSample; Self::SAMPLE_COUNT] {
        self.samples
    }
}

/// 输入源在一次调度中产生的有界波形块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaveChunk {
    pub source_id: SourceId,
    pub channel: Channel,
    pub sequence: u64,
    frames: Vec<WaveFrame>,
}

impl WaveChunk {
    /// 与旧 Socket 协议单包上限一致；内部队列仍应另外设置容量限制。
    pub const MAX_FRAMES: usize = 100;

    pub fn new(
        source_id: SourceId,
        channel: Channel,
        sequence: u64,
        frames: Vec<WaveFrame>,
    ) -> Result<Self, ModelError> {
        if frames.is_empty() {
            return Err(ModelError::EmptyWaveChunk);
        }
        if frames.len() > Self::MAX_FRAMES {
            return Err(ModelError::WaveChunkTooLarge {
                max: Self::MAX_FRAMES,
                actual: frames.len(),
            });
        }

        Ok(Self {
            source_id,
            channel,
            sequence,
            frames,
        })
    }

    pub fn frames(&self) -> &[WaveFrame] {
        &self.frames
    }

    pub fn into_frames(self) -> Vec<WaveFrame> {
        self.frames
    }

    pub fn duration(&self) -> Duration {
        WaveFrame::DURATION.saturating_mul(self.frames.len() as u32)
    }
}

/// 可持久化的输入源实例定义。每种 factory 自行解释并校验 `config`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSpec {
    pub id: SourceId,
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    #[serde(default)]
    pub config: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_uses_readable_domain_json_and_v4_conversion() {
        assert_eq!(serde_json::to_string(&Channel::A).unwrap(), "\"a\"");
        assert_eq!(Channel::A.as_v4(), 0);
        assert_eq!(Channel::B.as_v4(), 1);
        assert_eq!(Channel::from_v4(2), None);
    }

    #[test]
    fn sample_rejects_values_that_make_device_discard_the_frame() {
        assert!(matches!(
            WaveSample::new(9, 20),
            Err(ModelError::FrequencyOutOfRange { .. })
        ));
        assert!(matches!(
            WaveSample::new(10, 101),
            Err(ModelError::PulseIntensityOutOfRange { .. })
        ));
    }

    #[test]
    fn deserialization_preserves_sample_invariants() {
        let error = serde_json::from_str::<WaveSample>(r#"{"frequency":10,"pulse_intensity":101}"#)
            .unwrap_err();

        assert!(error.to_string().contains("0..=100"));
    }

    #[test]
    fn chunk_is_non_empty_and_bounded() {
        let source_id = Uuid::new_v4();
        assert_eq!(
            WaveChunk::new(source_id, Channel::A, 0, vec![]).unwrap_err(),
            ModelError::EmptyWaveChunk
        );

        let frames = vec![WaveFrame::silent(); WaveChunk::MAX_FRAMES + 1];
        assert!(matches!(
            WaveChunk::new(source_id, Channel::A, 0, frames),
            Err(ModelError::WaveChunkTooLarge { .. })
        ));
    }
}
