//! 音频采集与分析在专用 Rust worker 上运行；Hub 只读取最新的有界特征帧。
//!
//! 这里明确采用 25ms RMS、Hann 窗 FFT 频谱峰值和分段线性映射。
//! 官方 APP 的 WASM 数值算法未公开，本实现只对应其功能与参数语义。

mod files;
mod runtime;

use std::sync::Arc;

use rustfft::{FftPlanner, num_complex::Complex};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{WaveFrame, WaveSample};

use super::mapping::{MappingPoint, map_curve, period_ms_to_frequency, validate_curve};
use super::{SourceDescriptor, SourceError, SourceFactory, WaveSource};

pub use runtime::{AudioAction, AudioEngine, AudioMode, AudioSnapshot, AudioState};

pub const AUDIO_KIND: &str = "builtin.audio";
pub const MAX_AUDIO_FILE_BYTES: u64 = 200 * 1024 * 1024;
pub(super) const MAX_VIDEO_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_AUDIO_DURATION_MS: u64 = 60 * 60 * 1000;
pub(super) const MAX_SAMPLE_RATE: u32 = 192_000;
pub const AUDIO_FILE_EXTENSIONS: &[&str] = &["mp3", "flac", "wav", "m4a", "aac", "ogg"];
pub const VIDEO_FILE_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "mkv", "webm"];

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

impl AudioInputChannel {
    fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Mix => 2,
        }
    }
}

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

#[derive(Debug, Clone, Default)]
pub struct AudioFeatureSample {
    pub rms: f64,
    pub peak_hz: f64,
    spectrum: Arc<Vec<(f64, f64)>>,
}

impl AudioFeatureSample {
    fn peak_in_band(&self, minimum: f64, maximum: f64) -> f64 {
        self.spectrum
            .iter()
            .filter(|(frequency, _)| (*frequency >= minimum) && (*frequency <= maximum))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .filter(|(_, amplitude)| *amplitude > 1e-10)
            .map_or(0.0, |(frequency, _)| *frequency)
    }
}

#[derive(Debug, Clone, Default)]
pub struct AudioWindow {
    /// 左声道、右声道以及 PCM 均值混合声道。
    pub channels: [AudioFeatureSample; 3],
}

#[derive(Debug, Clone, Default)]
pub struct AudioFeatures {
    pub active: bool,
    pub windows: [AudioWindow; WaveFrame::SAMPLE_COUNT],
}

impl AudioFeatures {
    pub fn silent() -> Self {
        Self::default()
    }
}

/// 独立的每绑定映射状态；采集数据共享，自适应阈值不共享。
pub struct AudioMappingRuntime {
    pub config: AudioChannelConfig,
    lower: f64,
    upper: f64,
    below_upper_ms: u32,
    above_lower_ms: u32,
}

impl AudioMappingRuntime {
    pub fn new(config: AudioChannelConfig) -> Result<Self, SourceError> {
        config.validate()?;
        Ok(Self {
            lower: config.volume_lower,
            upper: config.volume_upper,
            config,
            below_upper_ms: 0,
            above_lower_ms: 0,
        })
    }

    pub fn reset(&mut self) {
        self.lower = self.config.volume_lower;
        self.upper = self.config.volume_upper;
        self.below_upper_ms = 0;
        self.above_lower_ms = 0;
    }

    pub fn next_frame(&mut self, features: &AudioFeatures) -> WaveFrame {
        if !features.active || !self.config.enabled {
            self.reset();
            return WaveFrame::silent();
        }
        let mut samples = [WaveSample::silent(); WaveFrame::SAMPLE_COUNT];
        for (index, window) in features.windows.iter().enumerate() {
            let feature = &window.channels[self.config.input_channel.index()];
            let level = (feature.rms * self.config.gain).clamp(0.0, 1.0);
            self.adapt(level);
            let pulse = ((level - self.lower) / (self.upper - self.lower)).clamp(0.0, 1.0);
            let peak = feature.peak_in_band(self.config.frequency_min, self.config.frequency_max);
            if peak == 0.0 || level <= 1e-8 {
                continue;
            }
            let position = (peak / self.config.frequency_min).ln()
                / (self.config.frequency_max / self.config.frequency_min).ln();
            let period = map_curve(&self.config.period_curve, position.clamp(0.0, 1.0));
            // 周期 10..100ms 对应协议同值编码；这里不把音频 Hz 误写入协议。
            samples[index] = WaveSample::new(
                period_ms_to_frequency(period),
                (pulse * 100.0).round() as u8,
            )
            .expect("validated mapping keeps protocol values in range");
        }
        WaveFrame::new(samples)
    }

    fn adapt(&mut self, level: f64) {
        if !self.config.adaptive {
            return;
        }
        self.below_upper_ms = if level < self.upper {
            self.below_upper_ms.saturating_add(25)
        } else {
            0
        };
        self.above_lower_ms = if level > self.lower {
            self.above_lower_ms.saturating_add(25)
        } else {
            0
        };
        // 系数定义为每 100ms 收敛比例，换算到四个 25ms 采样后与节拍无关。
        if self.below_upper_ms > self.config.hysteresis_ms {
            let ratio = 1.0 - (1.0 - self.config.adaptive_upper).powf(0.25);
            self.upper += (level - self.upper) * ratio;
        }
        if self.above_lower_ms > self.config.hysteresis_ms {
            let ratio = 1.0 - (1.0 - self.config.adaptive_lower).powf(0.25);
            self.lower += (level - self.lower) * ratio;
        }
        self.lower = self
            .lower
            .clamp(self.config.volume_lower, self.config.volume_upper - 0.001);
        self.upper = self
            .upper
            .clamp(self.lower + 0.001, self.config.volume_upper);
    }
}

pub struct AudioFactory;

impl SourceFactory for AudioFactory {
    fn descriptor(&self) -> SourceDescriptor {
        SourceDescriptor {
            kind: AUDIO_KIND,
            display_name: "音频模式",
            description: "本地音频与视频音轨、麦克风、录音回放和桌面音频，按各通道独立映射波形",
        }
    }

    fn default_config(&self) -> Value {
        serde_json::to_value(AudioChannelConfig::default())
            .expect("audio defaults are serializable")
    }

    fn validate(&self, config: &Value) -> Result<(), SourceError> {
        parse_config(config)?.validate()
    }

    fn build(&self, config: &Value) -> Result<Box<dyn WaveSource>, SourceError> {
        self.validate(config)?;
        Ok(Box::new(IdleAudioSource))
    }
}

struct IdleAudioSource;

impl WaveSource for IdleAudioSource {
    fn next_frame(&mut self) -> Result<WaveFrame, SourceError> {
        Ok(WaveFrame::silent())
    }
}

fn parse_config(config: &Value) -> Result<AudioChannelConfig, SourceError> {
    serde_json::from_value(config.clone()).map_err(|error| invalid(error.to_string()))
}

fn invalid(message: impl Into<String>) -> SourceError {
    SourceError::InvalidConfig {
        kind: AUDIO_KIND,
        message: message.into(),
    }
}

/// 输入块限定为 100ms，FFT 内存随采样率有界；每段去直流后使用 Hann 窗。
pub(super) fn analyze(samples: &[[f32; 2]], rate: u32, active: bool) -> AudioFeatures {
    let mut features = AudioFeatures {
        active,
        ..AudioFeatures::default()
    };
    if samples.is_empty() || !(8000..=MAX_SAMPLE_RATE).contains(&rate) {
        return features;
    }
    let window_len = (rate / 40) as usize;
    let mut planner = FftPlanner::<f64>::new();
    let fft_len = window_len.next_power_of_two();
    let fft = planner.plan_fft_forward(fft_len);
    for (window_index, output) in features.windows.iter_mut().enumerate() {
        let start = window_index * window_len;
        let end = (start + window_len).min(samples.len());
        if start >= end {
            continue;
        }
        let input = &samples[start..end];
        for channel in 0..3 {
            let values: Vec<f64> = input
                .iter()
                .map(|sample| match channel {
                    0 => f64::from(sample[0]),
                    1 => f64::from(sample[1]),
                    _ => f64::from((sample[0] + sample[1]) * 0.5),
                })
                .collect();
            let rms = (values.iter().map(|sample| sample * sample).sum::<f64>()
                / values.len() as f64)
                .sqrt();
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            let mut buffer = vec![Complex::default(); fft_len];
            for (index, value) in values.iter().enumerate() {
                let hann = 0.5
                    * (1.0
                        - (std::f64::consts::TAU * index as f64 / (window_len - 1) as f64).cos());
                buffer[index].re = (value - mean) * hann;
            }
            fft.process(&mut buffer);
            let spectrum: Vec<_> = buffer
                .iter()
                .enumerate()
                .take(fft_len / 2 + 1)
                .skip(1)
                .map(|(index, bin)| {
                    (
                        index as f64 * f64::from(rate) / fft_len as f64,
                        bin.norm_sqr(),
                    )
                })
                .filter(|(frequency, _)| (50.0..=10_000.0).contains(frequency))
                .collect();
            let peak_hz = spectrum
                .iter()
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .filter(|(_, amplitude)| *amplitude > 1e-10)
                .map_or(0.0, |(frequency, _)| *frequency);
            output.channels[channel] = AudioFeatureSample {
                rms,
                peak_hz,
                spectrum: Arc::new(spectrum),
            };
        }
    }
    features
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(left: f32, right: f32) -> AudioFeatures {
        let samples: Vec<_> = (0..4800)
            .map(|index| {
                let value = (std::f32::consts::TAU * 440.0 * index as f32 / 48_000.0).sin();
                [value * left, value * right]
            })
            .collect();
        analyze(&samples, 48_000, true)
    }

    #[test]
    fn rms_fft_and_independent_channel_selection() {
        let features = sine(0.5, 0.0);
        assert!((features.windows[0].channels[0].rms - 0.5 / 2.0f64.sqrt()).abs() < 0.005);
        assert!((features.windows[0].channels[0].peak_hz - 440.0).abs() < 25.0);
        let mut left = AudioMappingRuntime::new(AudioChannelConfig {
            input_channel: AudioInputChannel::Left,
            gain: 1.0,
            adaptive: false,
            ..Default::default()
        })
        .unwrap();
        let mut right = AudioMappingRuntime::new(AudioChannelConfig {
            input_channel: AudioInputChannel::Right,
            ..left.config.clone()
        })
        .unwrap();
        assert!(left.next_frame(&features).samples()[0].pulse_intensity() > 0);
        assert_eq!(right.next_frame(&features), WaveFrame::silent());
    }

    #[test]
    fn fixed_thresholds_and_log_frequency_mapping() {
        let features = sine(0.1, 0.1);
        let mut runtime = AudioMappingRuntime::new(AudioChannelConfig {
            gain: 1.0,
            adaptive: false,
            volume_lower: 0.1,
            ..Default::default()
        })
        .unwrap();
        assert!(
            runtime
                .next_frame(&features)
                .samples()
                .iter()
                .all(|sample| sample.pulse_intensity() == 0)
        );
        runtime.config.frequency_min = 800.0;
        runtime.config.frequency_max = 1000.0;
        // Hann 泄漏仍可能产生带内峰，但相对强度必须保持为零。
        assert!(
            runtime
                .next_frame(&features)
                .samples()
                .iter()
                .all(|sample| sample.pulse_intensity() == 0)
        );
    }

    #[test]
    fn adaptive_thresholds_wait_for_hysteresis_and_reset_on_silence() {
        let features = sine(0.2, 0.2);
        let mut runtime = AudioMappingRuntime::new(AudioChannelConfig::default()).unwrap();
        for _ in 0..3 {
            runtime.next_frame(&features);
        }
        assert_eq!(runtime.lower, 0.05);
        assert_eq!(runtime.upper, 1.0);
        runtime.next_frame(&features);
        assert!(runtime.lower > 0.05);
        assert!(runtime.upper < 1.0);
        assert_eq!(
            runtime.next_frame(&AudioFeatures::silent()),
            WaveFrame::silent()
        );
        assert_eq!(runtime.lower, 0.05);
        assert_eq!(runtime.upper, 1.0);
    }

    #[test]
    fn mapping_rejects_invalid_ranges_and_nan() {
        for config in [
            AudioChannelConfig {
                gain: f64::NAN,
                ..Default::default()
            },
            AudioChannelConfig {
                volume_lower: 1.0,
                volume_upper: 1.0,
                ..Default::default()
            },
            AudioChannelConfig {
                frequency_min: 1000.0,
                frequency_max: 100.0,
                ..Default::default()
            },
            AudioChannelConfig {
                hysteresis_ms: 2001,
                ..Default::default()
            },
        ] {
            assert!(config.validate().is_err());
        }
    }

    #[test]
    fn each_mapping_selects_the_spectral_peak_inside_its_own_band() {
        let samples: Vec<_> = (0..4800)
            .map(|index| {
                let time = index as f32 / 48_000.0;
                let signal = (std::f32::consts::TAU * 440.0 * time).sin() * 0.4
                    + (std::f32::consts::TAU * 1500.0 * time).sin() * 0.2;
                [signal, signal]
            })
            .collect();
        let features = analyze(&samples, 48_000, true);
        for window in &features.windows {
            let spectrum = &window.channels[0];
            assert!((spectrum.peak_in_band(100.0, 1000.0) - 440.0).abs() < 25.0);
            assert!((spectrum.peak_in_band(1000.0, 2000.0) - 1500.0).abs() < 25.0);
        }
    }
}
