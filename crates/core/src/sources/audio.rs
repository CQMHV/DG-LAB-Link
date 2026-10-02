//! Legacy audio command and snapshot DTOs. Audio processing lives in the installed plugin.
use super::SourceError;
use super::mapping::{MappingPoint, validate_curve};
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum AudioInputChannel {
    Left,
    Right,
    #[default]
    Mix,
}

impl AudioInputChannel {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct AudioChannelConfig {
    pub enabled: bool,
    pub input_channel: AudioInputChannel,
    pub gain: f64,
    pub volume_lower: f64,
    pub volume_upper: f64,
    pub adaptive: bool,
    pub adaptive_lower: f64,
    pub adaptive_upper: f64,
    pub hysteresis_ms: u32,
    pub frequency_min: f64,
    pub frequency_max: f64,
    pub period_curve: Vec<MappingPoint>,
}

impl Default for AudioChannelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            input_channel: AudioInputChannel::Mix,
            gain: 2.5,
            volume_lower: 0.05,
            volume_upper: 1.0,
            adaptive: true,
            adaptive_lower: 0.1,
            adaptive_upper: 0.1,
            hysteresis_ms: 300,
            frequency_min: 100.0,
            frequency_max: 1000.0,
            period_curve: vec![
                MappingPoint { x: 0.0, y: 10.0 },
                MappingPoint { x: 0.33, y: 100.0 },
                MappingPoint { x: 0.66, y: 10.0 },
                MappingPoint { x: 1.0, y: 100.0 },
            ],
        }
    }
}

impl AudioChannelConfig {
    pub fn validate(&self) -> Result<(), SourceError> {
        for (name, value, min, max) in [
            ("数据增益", self.gain, 1.0, 10.0),
            ("强度下限", self.volume_lower, 0.0, 1.0),
            ("强度上限", self.volume_upper, 0.0, 1.0),
            ("低适应系数", self.adaptive_lower, 0.0, 0.5),
            ("高适应系数", self.adaptive_upper, 0.0, 0.5),
            ("观察频段下限", self.frequency_min, 50.0, 10_000.0),
            ("观察频段上限", self.frequency_max, 50.0, 10_000.0),
        ] {
            if !value.is_finite() || !(min..=max).contains(&value) {
                return Err(invalid(format!("{name}必须在 {min}..={max}")));
            }
        }
        if self.volume_upper - self.volume_lower < 0.001 {
            return Err(invalid("强度上限必须大于下限，至少相差 0.001"));
        }
        if self.frequency_min >= self.frequency_max {
            return Err(invalid("观察频段上限必须大于下限"));
        }
        if self.hysteresis_ms > 2000 {
            return Err(invalid("迟滞必须在 0..=2000 ms"));
        }
        validate_curve(&self.period_curve, 10.0, 100.0).map_err(invalid)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AudioAction {
    LoadFile {
        path: String,
    },
    Play,
    Pause,
    Stop,
    Seek {
        #[serde(rename = "positionMs")]
        position_ms: u64,
    },
    StartMicrophone,
    StartDesktop,
    StartRecording,
    StopRecording,
    SaveRecording {
        path: String,
    },
    SetPlaybackOptions {
        #[serde(rename = "loop")]
        loop_enabled: bool,
        #[serde(rename = "speakerEnabled")]
        speaker_enabled: bool,
    },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum AudioMode {
    #[default]
    File,
    Microphone,
    Recording,
    Desktop,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum AudioState {
    #[default]
    Idle,
    Loading,
    Playing,
    Paused,
    Capturing,
    Recording,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AudioSnapshot {
    pub mode: AudioMode,
    pub state: AudioState,
    pub file_name: Option<String>,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub level_left: f64,
    pub level_right: f64,
    pub peak_left_hz: f64,
    pub peak_right_hz: f64,
    pub last_error: Option<String>,
    pub has_recording: bool,
    #[serde(rename = "loop")]
    pub loop_enabled: bool,
    pub speaker_enabled: bool,
}

impl Default for AudioSnapshot {
    fn default() -> Self {
        Self {
            mode: AudioMode::File,
            state: AudioState::Idle,
            file_name: None,
            position_ms: 0,
            duration_ms: 0,
            level_left: 0.0,
            level_right: 0.0,
            peak_left_hz: 0.0,
            peak_right_hz: 0.0,
            last_error: None,
            has_recording: false,
            loop_enabled: false,
            speaker_enabled: true,
        }
    }
}

fn invalid(message: impl Into<String>) -> SourceError {
    SourceError::InvalidConfig {
        kind: "builtin.audio",
        message: message.into(),
    }
}
