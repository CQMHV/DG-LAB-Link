use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Channel, WaveFrame, WaveSample};

use super::mapping::{MappingPoint, map_curve, period_ms_to_frequency, validate_curve};
use super::preset::MAX_PRESET_FRAMES;
use super::{
    FixedWaveformFactory, SourceDescriptor, SourceError, SourceFactory, WaveSource, WaveformConfig,
};

pub const TOUCH_SOURCE_KIND: &str = "builtin.touch";
pub const TOUCH_INPUT_LEASE: Duration = Duration::from_secs(1);
const MAX_INPUT_ID_LENGTH: usize = 128;

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

pub struct TouchFactory;

impl SourceFactory for TouchFactory {
    fn descriptor(&self) -> SourceDescriptor {
        SourceDescriptor {
            kind: TOUCH_SOURCE_KIND,
            display_name: "触控模式",
            description: "自由触控与律动网格，通过触点生成双通道波形",
        }
    }

    fn default_config(&self) -> Value {
        serde_json::to_value(TouchConfig::default()).expect("built-in config is serializable")
    }

    fn validate(&self, config: &Value) -> Result<(), SourceError> {
        validate_config(&parse_config(config)?)
    }

    fn build(&self, config: &Value) -> Result<Box<dyn WaveSource>, SourceError> {
        self.validate(config)?;
        // The plugin owns binding input; this compiler helper never emits stale touch.
        Ok(Box::new(IdleTouchSource))
    }
}

struct IdleTouchSource;

impl WaveSource for IdleTouchSource {
    fn next_frame(&mut self) -> Result<WaveFrame, SourceError> {
        Ok(WaveFrame::silent())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selection {
    Pad,
    Cell(usize),
}

#[derive(Default)]
struct TouchChannel {
    pointer_id: Option<i64>,
    selection: Option<Selection>,
    cursor: usize,
    gradient_value: f64,
    gradient_zone: i8,
}

struct InputLease {
    owner_id: String,
    sequence: u64,
    updated_at: Instant,
}

/// 按绑定维护触控身份和双通道播放游标；由原生插件的 Rust 节拍驱动。
pub struct TouchRuntime {
    config: TouchConfig,
    free_frames: Vec<Vec<WaveFrame>>,
    rhythm_frames: Vec<Vec<WaveFrame>>,
    background_frames: Option<Vec<WaveFrame>>,
    background_cursor: usize,
    device_id: Option<String>,
    lease: Option<InputLease>,
    pointers: Vec<TouchPointer>,
    channels: [TouchChannel; 2],
    next_alternate_channel: usize,
}

impl TouchRuntime {
    pub fn new(config: TouchConfig) -> Result<Self, SourceError> {
        validate_config(&config)?;
        Ok(Self {
            free_frames: config
                .free_waveforms
                .iter()
                .map(compile_waveform)
                .collect::<Result<_, _>>()?,
            rhythm_frames: config
                .rhythm_waveforms
                .iter()
                .map(compile_waveform)
                .collect::<Result<_, _>>()?,
            background_frames: config
                .background
                .as_ref()
                .map(compile_waveform)
                .transpose()?,
            config,
            background_cursor: 0,
            device_id: None,
            lease: None,
            pointers: Vec::with_capacity(2),
            channels: Default::default(),
            next_alternate_channel: 0,
        })
    }

    pub fn update(&mut self, input: &TouchInput, now: Instant) -> Result<(), SourceError> {
        self.validate_input(input)?;
        let owner_changed = self
            .lease
            .as_ref()
            .is_some_and(|lease| lease.owner_id != input.owner_id);
        if let Some(lease) = &self.lease {
            if input.owner_id == lease.owner_id && input.sequence <= lease.sequence {
                return Err(SourceError::Runtime("已忽略过期的触控状态".to_owned()));
            }
            if self.has_active_input(now) && input.owner_id != lease.owner_id {
                return Err(SourceError::Runtime(
                    "该设备正在由另一个窗口触控".to_owned(),
                ));
            }
        }
        if !self.lease_valid(now) || owner_changed {
            self.clear_touch_state();
            self.next_alternate_channel = 0;
        }
        self.device_id
            .get_or_insert_with(|| input.device_id.clone());
        self.lease = Some(InputLease {
            owner_id: input.owner_id.clone(),
            sequence: input.sequence,
            updated_at: now,
        });
        let previously_explicit = self
            .pointers
            .iter()
            .any(|pointer| pointer.channel.is_some());
        self.pointers.clone_from(&input.pointers);

        if self
            .pointers
            .iter()
            .any(|pointer| pointer.channel.is_some())
        {
            for (index, channel) in self.channels.iter_mut().enumerate() {
                let pointer_id = self
                    .pointers
                    .iter()
                    .find(|pointer| pointer.channel == Some(Channel::ALL[index]))
                    .map(|pointer| pointer.id);
                if channel.pointer_id != pointer_id {
                    *channel = TouchChannel {
                        pointer_id,
                        ..TouchChannel::default()
                    };
                }
            }
        } else {
            if previously_explicit {
                self.channels = Default::default();
            }
            self.assign_routed_pointers();
        }

        for channel in &mut self.channels {
            let Some(pointer) = self
                .pointers
                .iter()
                .find(|pointer| Some(pointer.id) == channel.pointer_id)
            else {
                continue;
            };
            let selection = select_pointer(&self.config, pointer);
            if channel.selection != Some(selection) {
                channel.selection = Some(selection);
                channel.cursor = 0;
                channel.gradient_zone = 0;
                channel.gradient_value = 100.0;
            }
        }
        Ok(())
    }

    fn assign_routed_pointers(&mut self) {
        for channel in &mut self.channels {
            if !self
                .pointers
                .iter()
                .any(|pointer| Some(pointer.id) == channel.pointer_id)
            {
                *channel = TouchChannel::default();
            }
        }
        if self.config.routing == TouchRouting::Separate {
            for pointer in &self.pointers {
                if self
                    .channels
                    .iter()
                    .any(|channel| channel.pointer_id == Some(pointer.id))
                {
                    continue;
                }
                if let Some(channel) = self
                    .channels
                    .iter_mut()
                    .find(|channel| channel.pointer_id.is_none())
                {
                    channel.pointer_id = Some(pointer.id);
                }
            }
        } else if let Some(pointer) = self.pointers.first() {
            match self.config.routing {
                TouchRouting::A => self.channels[0].pointer_id = Some(pointer.id),
                TouchRouting::B => self.channels[1].pointer_id = Some(pointer.id),
                TouchRouting::Sync => {
                    self.channels[0].pointer_id = Some(pointer.id);
                    self.channels[1].pointer_id = Some(pointer.id);
                }
                TouchRouting::Alternate => {
                    if !self
                        .channels
                        .iter()
                        .any(|channel| channel.pointer_id == Some(pointer.id))
                    {
                        self.channels[self.next_alternate_channel].pointer_id = Some(pointer.id);
                        self.next_alternate_channel ^= 1;
                    }
                }
                TouchRouting::Separate => unreachable!(),
            }
        }
    }

    pub fn next_frames(&mut self, now: Instant) -> [WaveFrame; 2] {
        let background = match &self.background_frames {
            Some(frames) => {
                let frame = frames[self.background_cursor];
                self.background_cursor = (self.background_cursor + 1) % frames.len();
                frame
            }
            None => WaveFrame::silent(),
        };
        if !self.lease_valid(now) {
            self.clear_touch_state();
        }
        let expires_at = self
            .lease
            .as_ref()
            .and_then(|lease| lease.updated_at.checked_add(TOUCH_INPUT_LEASE));
        let mut result = [background; 2];
        for (channel_index, channel) in self.channels.iter_mut().enumerate() {
            let Some(pointer) = self
                .pointers
                .iter()
                .find(|pointer| Some(pointer.id) == channel.pointer_id)
            else {
                continue;
            };
            let mut samples = background.into_samples();
            match channel.selection {
                Some(Selection::Pad) => {
                    let (intensity_position, period_position) = if self.config.swap_axes {
                        (pointer.y, pointer.x)
                    } else {
                        (pointer.x, pointer.y)
                    };
                    let frequency = period_ms_to_frequency(map_curve(
                        &self.config.period_curve,
                        period_position,
                    ));
                    for (index, sample) in samples.iter_mut().enumerate() {
                        let sample_at = now + WaveFrame::SAMPLE_PERIOD * index as u32;
                        if expires_at.is_none_or(|expiry| sample_at >= expiry) {
                            continue;
                        }
                        let intensity = match self.config.intensity_mode {
                            TouchIntensityMode::Classic => {
                                map_curve(&self.config.intensity_curve, intensity_position)
                            }
                            TouchIntensityMode::Gradient => advance_gradient(
                                channel,
                                &self.config.intensity_curve,
                                intensity_position,
                                self.config.gradient_direction,
                            ),
                        };
                        *sample =
                            WaveSample::new(frequency, intensity.round().clamp(0.0, 100.0) as u8)
                                .expect("validated touch mappings stay within waveform bounds");
                    }
                }
                Some(Selection::Cell(cell)) => {
                    let frames = match self.config.mode {
                        TouchMode::Free => &self.free_frames[cell],
                        TouchMode::Rhythm => &self.rhythm_frames[cell],
                    };
                    let active = frames[channel.cursor].samples();
                    for (index, sample) in samples.iter_mut().enumerate() {
                        let sample_at = now + WaveFrame::SAMPLE_PERIOD * index as u32;
                        if expires_at.is_some_and(|expiry| sample_at < expiry) {
                            *sample = active[index];
                        }
                    }
                    channel.cursor = (channel.cursor + 1) % frames.len();
                }
                None => {}
            }
            result[channel_index] = WaveFrame::new(samples);
        }
        result
    }

    pub fn has_active_input(&self, now: Instant) -> bool {
        self.active_touch_channels(now)
            .into_iter()
            .any(|active| active)
    }

    pub fn active_touch_channels(&self, now: Instant) -> [bool; 2] {
        if !self.lease_valid(now) {
            return [false; 2];
        }
        self.channels
            .each_ref()
            .map(|channel| channel.pointer_id.is_some())
    }

    pub fn touch_intents(&self, now: Instant) -> [Option<(i64, Option<usize>)>; 2] {
        if !self.lease_valid(now) {
            return [None; 2];
        }
        self.channels.each_ref().map(|channel| {
            channel.pointer_id.map(|id| {
                let cell = match channel.selection {
                    Some(Selection::Cell(cell)) => Some(cell),
                    Some(Selection::Pad) | None => None,
                };
                (id, cell)
            })
        })
    }

    pub fn active_channels(&self, now: Instant) -> [bool; 2] {
        let background_active = self.background_frames.is_some();
        self.active_touch_channels(now)
            .map(|active| active || background_active)
    }

    pub fn release_owner(&mut self, owner_id: &str) {
        if self
            .lease
            .as_ref()
            .is_some_and(|lease| lease.owner_id == owner_id)
        {
            self.clear_touch_state();
            self.next_alternate_channel = 0;
        }
    }

    pub fn reset(&mut self) {
        self.clear_touch_state();
        self.device_id = None;
        self.lease = None;
        self.background_cursor = 0;
        self.next_alternate_channel = 0;
    }

    /// 解绑一个通道时保留另一个通道的触点与播放进度。
    pub fn reset_channel(&mut self, channel: Channel) {
        self.channels[channel.as_v4() as usize] = TouchChannel::default();
        self.pointers
            .retain(|pointer| pointer.channel != Some(channel));
    }

    fn clear_touch_state(&mut self) {
        self.pointers.clear();
        self.channels = Default::default();
    }

    fn lease_valid(&self, now: Instant) -> bool {
        self.lease.as_ref().is_some_and(|lease| {
            now.checked_duration_since(lease.updated_at)
                .is_some_and(|age| age < TOUCH_INPUT_LEASE)
        })
    }

    fn validate_input(&self, input: &TouchInput) -> Result<(), SourceError> {
        if input.device_id.is_empty()
            || input.device_id.len() > MAX_INPUT_ID_LENGTH
            || input.owner_id.is_empty()
            || input.owner_id.len() > MAX_INPUT_ID_LENGTH
        {
            return Err(SourceError::Runtime(
                "触控设备与窗口标识长度必须在 1..=128 个字节".to_owned(),
            ));
        }
        if self
            .device_id
            .as_ref()
            .is_some_and(|device_id| device_id != &input.device_id)
        {
            return Err(SourceError::Runtime("触控状态不属于当前设备".to_owned()));
        }
        validate_pointer_channels(&input.pointers)?;
        let max_pointers = if self.config.routing == TouchRouting::Separate
            || input
                .pointers
                .iter()
                .any(|pointer| pointer.channel.is_some())
        {
            2
        } else {
            1
        };
        if input.pointers.len() > max_pointers {
            return Err(SourceError::Runtime(format!(
                "当前触控分配方式最多允许 {max_pointers} 个触点"
            )));
        }
        let cell_count = match self.config.mode {
            TouchMode::Free => 8,
            TouchMode::Rhythm => self.config.grid_size * self.config.grid_size,
        };
        for pointer in &input.pointers {
            if !pointer.x.is_finite()
                || !pointer.y.is_finite()
                || !(0.0..=1.0).contains(&pointer.x)
                || !(0.0..=1.0).contains(&pointer.y)
            {
                return Err(SourceError::Runtime(
                    "触点坐标必须在 0..=1 范围内".to_owned(),
                ));
            }
            if pointer.cell.is_some_and(|cell| cell >= cell_count) {
                return Err(SourceError::Runtime(
                    "触控波形格超出当前面板范围".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_pointer_channels(pointers: &[TouchPointer]) -> Result<(), SourceError> {
    if pointers.len() > 2 {
        return Err(SourceError::Runtime("触控最多允许 2 个触点".to_owned()));
    }
    let explicit = pointers
        .first()
        .is_some_and(|pointer| pointer.channel.is_some());
    for (index, pointer) in pointers.iter().enumerate() {
        if pointer.channel.is_some() != explicit {
            return Err(SourceError::Runtime(
                "不能混用指定通道和未指定通道的触点".to_owned(),
            ));
        }
        if pointers[..index]
            .iter()
            .any(|existing| existing.id == pointer.id)
        {
            return Err(SourceError::Runtime("触点标识不能重复".to_owned()));
        }
        if pointer.channel.is_some()
            && pointers[..index]
                .iter()
                .any(|existing| existing.channel == pointer.channel)
        {
            return Err(SourceError::Runtime("每个通道最多允许 1 个触点".to_owned()));
        }
    }
    Ok(())
}

fn select_pointer(config: &TouchConfig, pointer: &TouchPointer) -> Selection {
    match (config.mode, pointer.cell) {
        (_, Some(cell)) => Selection::Cell(cell),
        (TouchMode::Free, None) => Selection::Pad,
        (TouchMode::Rhythm, None) => {
            let size = config.grid_size;
            let column = (pointer.x * size as f64).floor() as usize;
            let row = (pointer.y * size as f64).floor() as usize;
            Selection::Cell(row.min(size - 1) * size + column.min(size - 1))
        }
    }
}

/// 选定侧重复下降/上升，其余区域保持满值；离中间越远，变化越快。
/// 每个采样推进 25ms，不依赖前端是否发送移动事件。
fn advance_gradient(
    channel: &mut TouchChannel,
    curve: &[MappingPoint],
    position: f64,
    direction: TouchGradientDirection,
) -> f64 {
    let (left, right) = if curve.len() == 2 {
        (0.5, 0.5)
    } else {
        (curve[1].x, curve[curve.len() - 2].x)
    };
    let (zone, speed) =
        if direction != TouchGradientDirection::Right && position < left && left > 0.0 {
            (-1, (left - position) / left)
        } else if direction != TouchGradientDirection::Left && position > right && right < 1.0 {
            (1, (position - right) / (1.0 - right))
        } else {
            (0, 0.0)
        };
    if channel.gradient_zone != zone {
        channel.gradient_zone = zone;
        channel.gradient_value = if zone > 0 { 0.0 } else { 100.0 };
    }
    if zone == 0 {
        channel.gradient_value = 100.0;
        return 100.0;
    }
    let value = channel.gradient_value;
    let next = value + f64::from(zone) * speed * 100.0 * WaveFrame::SAMPLE_PERIOD.as_secs_f64();
    channel.gradient_value = if zone < 0 && value <= 0.0 {
        100.0
    } else if zone > 0 && value >= 100.0 {
        0.0
    } else {
        next.clamp(0.0, 100.0)
    };
    value
}

fn parse_config(config: &Value) -> Result<TouchConfig, SourceError> {
    serde_json::from_value(config.clone()).map_err(|error| invalid_config(error.to_string()))
}

fn validate_config(config: &TouchConfig) -> Result<(), SourceError> {
    if !(2..=4).contains(&config.grid_size) {
        return Err(invalid_config("律动网格边长必须在 2..=4"));
    }
    if config.free_waveforms.len() != 8 {
        return Err(invalid_config("自由触控必须配置 8 个快捷波形格"));
    }
    if config.rhythm_waveforms.len() < config.grid_size * config.grid_size
        || config.rhythm_waveforms.len() > 16
    {
        return Err(invalid_config(
            "律动波形格数量必须覆盖当前网格，且不能超过 16",
        ));
    }
    validate_curve(&config.intensity_curve, 0.0, 100.0).map_err(invalid_config)?;
    validate_curve(&config.period_curve, 10.0, 100.0).map_err(invalid_config)?;
    let waveforms = config
        .free_waveforms
        .iter()
        .chain(config.rhythm_waveforms.iter())
        .chain(config.background.iter());
    let mut frame_count = 0;
    for waveform in waveforms {
        frame_count += waveform.frames.len();
        if frame_count > MAX_PRESET_FRAMES {
            return Err(invalid_config("触控面板与背景波形总计最多包含 16384 帧"));
        }
        let value =
            serde_json::to_value(waveform).map_err(|error| invalid_config(error.to_string()))?;
        FixedWaveformFactory
            .validate(&value)
            .map_err(|error| invalid_config(error.to_string()))?;
    }
    Ok(())
}

fn compile_waveform(config: &WaveformConfig) -> Result<Vec<WaveFrame>, SourceError> {
    let value = serde_json::to_value(config).map_err(|error| invalid_config(error.to_string()))?;
    let mut source = FixedWaveformFactory.build(&value)?;
    (0..config.frames.len())
        .map(|_| source.next_frame())
        .collect()
}

fn invalid_config(message: impl Into<String>) -> SourceError {
    SourceError::InvalidConfig {
        kind: TOUCH_SOURCE_KIND,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(sequence: u64, pointers: Vec<TouchPointer>) -> TouchInput {
        TouchInput {
            device_id: "device-1".to_owned(),
            owner_id: "window-1".to_owned(),
            sequence,
            pointers,
        }
    }

    fn pointer(id: i64, x: f64, y: f64, cell: Option<usize>) -> TouchPointer {
        TouchPointer {
            id,
            x,
            y,
            cell,
            channel: None,
        }
    }

    fn waveform(frames: &[&str]) -> WaveformConfig {
        WaveformConfig {
            preset_id: "TEST".to_owned(),
            preset_name: "测试".to_owned(),
            frames: frames.iter().map(|frame| (*frame).to_owned()).collect(),
        }
    }

    fn channel_pointer(
        channel: Channel,
        id: i64,
        x: f64,
        y: f64,
        cell: Option<usize>,
    ) -> TouchPointer {
        TouchPointer {
            channel: Some(channel),
            ..pointer(id, x, y, cell)
        }
    }

    #[test]
    fn explicit_channels_ignore_routing_and_start_b_independently_before_a() {
        let now = Instant::now();
        for routing in [
            TouchRouting::A,
            TouchRouting::B,
            TouchRouting::Sync,
            TouchRouting::Separate,
            TouchRouting::Alternate,
        ] {
            let mut runtime = TouchRuntime::new(TouchConfig {
                routing,
                ..TouchConfig::default()
            })
            .unwrap();
            let b = channel_pointer(Channel::B, 1, 0.33, 0.0, None);
            runtime.update(&input(1, vec![b.clone()]), now).unwrap();
            assert_eq!(runtime.active_touch_channels(now), [false, true]);
            assert_eq!(runtime.next_frames(now)[0], WaveFrame::silent());

            let a = channel_pointer(Channel::A, 2, 0.0, 0.33, None);
            runtime.update(&input(2, vec![b, a]), now).unwrap();
            let frames = runtime.next_frames(now);
            assert_eq!(frames[0].samples()[0], WaveSample::new(10, 0).unwrap());
            assert_eq!(frames[1].samples()[0], WaveSample::new(100, 100).unwrap());
        }
    }

    #[test]
    fn independent_cells_preserve_other_channel_cursor_on_down_reorder_and_release() {
        let now = Instant::now();
        let mut config = TouchConfig::default();
        config.free_waveforms[0] =
            waveform(&["0A0A0A0A14141414", "0A0A0A0A1E1E1E1E", "0A0A0A0A28282828"]);
        config.free_waveforms[1] = waveform(&[
            "1414141432323232",
            "141414143C3C3C3C",
            "1414141446464646",
            "1414141450505050",
        ]);
        let mut runtime = TouchRuntime::new(config).unwrap();
        let b = channel_pointer(Channel::B, 1, 0.0, 0.0, Some(1));
        let a = channel_pointer(Channel::A, 2, 0.0, 0.0, Some(0));
        runtime.update(&input(1, vec![b.clone()]), now).unwrap();
        assert_eq!(
            runtime.next_frames(now)[1].samples()[0],
            WaveSample::new(20, 50).unwrap()
        );
        runtime
            .update(&input(2, vec![b.clone(), a.clone()]), now)
            .unwrap();
        let frames = runtime.next_frames(now);
        assert_eq!(frames[0].samples()[0], WaveSample::new(10, 20).unwrap());
        assert_eq!(frames[1].samples()[0], WaveSample::new(20, 60).unwrap());
        runtime.update(&input(3, vec![a, b.clone()]), now).unwrap();
        let frames = runtime.next_frames(now);
        assert_eq!(frames[0].samples()[0].pulse_intensity(), 30);
        assert_eq!(frames[1].samples()[0].pulse_intensity(), 70);
        runtime.update(&input(4, vec![b]), now).unwrap();
        let frames = runtime.next_frames(now);
        assert_eq!(frames[0], WaveFrame::silent());
        assert_eq!(frames[1].samples()[0].pulse_intensity(), 80);
        assert_eq!(runtime.touch_intents(now), [None, Some((1, Some(1)))]);
        runtime.update(&input(5, vec![]), now).unwrap();
        assert_eq!(runtime.next_frames(now), [WaveFrame::silent(); 2]);
    }

    #[test]
    fn retargeting_same_pointer_clears_old_channel_and_restarts_selection() {
        let now = Instant::now();
        let mut config = TouchConfig::default();
        config.free_waveforms[0] = waveform(&["0A0A0A0A14141414", "0A0A0A0A28282828"]);
        let mut runtime = TouchRuntime::new(config).unwrap();
        let mut pointer = channel_pointer(Channel::A, 1, 0.0, 0.0, Some(0));
        runtime
            .update(&input(1, vec![pointer.clone()]), now)
            .unwrap();
        runtime.next_frames(now);
        pointer.channel = Some(Channel::B);
        runtime.update(&input(2, vec![pointer]), now).unwrap();
        let frames = runtime.next_frames(now);
        assert_eq!(frames[0], WaveFrame::silent());
        assert_eq!(frames[1].samples()[0].pulse_intensity(), 20);
        assert_eq!(runtime.touch_intents(now), [None, Some((1, Some(0)))]);
    }

    #[test]
    fn reset_explicit_channel_removes_its_pointer_without_restarting_other_channel() {
        let now = Instant::now();
        let mut config = TouchConfig::default();
        config.free_waveforms[0] = waveform(&["0A0A0A0A14141414", "0A0A0A0A28282828"]);
        let mut runtime = TouchRuntime::new(config).unwrap();
        let a = channel_pointer(Channel::A, 1, 0.0, 0.0, Some(0));
        let b = channel_pointer(Channel::B, 2, 0.0, 0.0, Some(0));
        runtime.update(&input(1, vec![a.clone(), b]), now).unwrap();
        runtime.next_frames(now);
        runtime.reset_channel(Channel::B);
        assert_eq!(runtime.pointers, vec![a.clone()]);
        runtime.update(&input(2, vec![a]), now).unwrap();
        let frames = runtime.next_frames(now);
        assert_eq!(frames[0].samples()[0].pulse_intensity(), 40);
        assert_eq!(frames[1], WaveFrame::silent());
    }

    #[test]
    fn invalid_pointer_channels_and_ids_do_not_mutate_valid_input() {
        let now = Instant::now();
        let mut runtime = TouchRuntime::new(TouchConfig::default()).unwrap();
        let a = channel_pointer(Channel::A, 1, 0.33, 0.33, None);
        let b = channel_pointer(Channel::B, 2, 0.33, 0.33, None);
        runtime
            .update(&input(1, vec![a.clone(), b.clone()]), now)
            .unwrap();
        for invalid in [
            vec![a.clone(), pointer(2, 0.0, 0.0, None)],
            vec![a.clone(), channel_pointer(Channel::A, 2, 0.0, 0.0, None)],
            vec![a.clone(), channel_pointer(Channel::B, 1, 0.0, 0.0, None)],
            vec![a.clone(), b, channel_pointer(Channel::A, 3, 0.0, 0.0, None)],
        ] {
            assert!(runtime.update(&input(2, invalid), now).is_err());
            assert_eq!(
                runtime.touch_intents(now),
                [Some((1, None)), Some((2, None))]
            );
        }
        runtime.update(&input(2, vec![a]), now).unwrap();
        assert_eq!(runtime.active_touch_channels(now), [true, false]);
    }

    #[test]
    fn channel_serialization_keeps_unspecified_inputs_and_routing_supported() {
        let now = Instant::now();
        let legacy: TouchPointer =
            serde_json::from_value(serde_json::json!({"id":1,"x":0.33,"y":0.33,"cell":null}))
                .unwrap();
        assert_eq!(legacy.channel, None);
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("channel")
                .is_none()
        );
        let mut explicit = legacy.clone();
        explicit.channel = Some(Channel::B);
        assert_eq!(serde_json::to_value(&explicit).unwrap()["channel"], "b");
        let mut runtime = TouchRuntime::new(TouchConfig::default()).unwrap();
        runtime.update(&input(1, vec![explicit]), now).unwrap();
        runtime.update(&input(2, vec![legacy]), now).unwrap();
        assert_eq!(runtime.active_touch_channels(now), [true, true]);
    }

    #[test]
    fn classic_pad_maps_relative_intensity_and_period_and_swaps_axes() {
        let now = Instant::now();
        let config = TouchConfig {
            routing: TouchRouting::A,
            ..TouchConfig::default()
        };
        let mut runtime = TouchRuntime::new(config.clone()).unwrap();
        runtime
            .update(&input(1, vec![pointer(1, 0.33, 0.0, None)]), now)
            .unwrap();
        let frames = runtime.next_frames(now);
        assert_eq!(frames[0].samples()[0], WaveSample::new(100, 100).unwrap());
        assert_eq!(frames[1], WaveFrame::silent());
        let mut swapped = TouchRuntime::new(TouchConfig {
            swap_axes: true,
            ..config
        })
        .unwrap();
        swapped
            .update(&input(1, vec![pointer(1, 0.33, 0.0, None)]), now)
            .unwrap();
        assert_eq!(
            swapped.next_frames(now)[0].samples()[0],
            WaveSample::new(10, 0).unwrap()
        );
    }

    #[test]
    fn sync_repeats_same_frame_and_separate_preserves_pointer_identity() {
        let now = Instant::now();
        let mut sync = TouchRuntime::new(TouchConfig::default()).unwrap();
        sync.update(&input(1, vec![pointer(1, 0.33, 0.5, None)]), now)
            .unwrap();
        let frames = sync.next_frames(now);
        assert_eq!(frames[0], frames[1]);

        let mut separate = TouchRuntime::new(TouchConfig {
            routing: TouchRouting::Separate,
            ..TouchConfig::default()
        })
        .unwrap();
        separate
            .update(
                &input(
                    1,
                    vec![pointer(1, 0.33, 0.0, None), pointer(2, 0.0, 0.33, None)],
                ),
                now,
            )
            .unwrap();
        separate
            .update(
                &input(
                    2,
                    vec![pointer(2, 0.0, 0.33, None), pointer(1, 0.33, 0.0, None)],
                ),
                now,
            )
            .unwrap();
        let frames = separate.next_frames(now);
        assert_eq!(frames[0].samples()[0], WaveSample::new(100, 100).unwrap());
        assert_eq!(frames[1].samples()[0], WaveSample::new(10, 0).unwrap());
        separate
            .update(&input(3, vec![pointer(2, 0.0, 0.33, None)]), now)
            .unwrap();
        assert_eq!(separate.active_touch_channels(now), [false, true]);
    }

    #[test]
    fn clearing_one_channel_preserves_the_other_cell_cursor() {
        let now = Instant::now();
        let mut config = TouchConfig::default();
        config.free_waveforms[0] = waveform(&["0A0A0A0A14141414", "0A0A0A0A28282828"]);
        let mut runtime = TouchRuntime::new(config).unwrap();
        runtime
            .update(&input(1, vec![pointer(1, 0.0, 0.0, Some(0))]), now)
            .unwrap();
        runtime.next_frames(now);
        runtime.reset_channel(Channel::B);
        let frames = runtime.next_frames(now + WaveFrame::DURATION);
        assert_eq!(frames[0].samples()[0].pulse_intensity(), 40);
        assert_eq!(frames[1], WaveFrame::silent());
        assert_eq!(runtime.active_touch_channels(now), [true, false]);
    }

    #[test]
    fn alternate_changes_only_on_new_down_and_reset_restarts_at_a() {
        let now = Instant::now();
        let mut runtime = TouchRuntime::new(TouchConfig {
            routing: TouchRouting::Alternate,
            ..TouchConfig::default()
        })
        .unwrap();
        runtime
            .update(&input(1, vec![pointer(1, 0.33, 0.33, None)]), now)
            .unwrap();
        assert_eq!(runtime.active_touch_channels(now), [true, false]);
        runtime
            .update(&input(2, vec![pointer(1, 0.5, 0.5, None)]), now)
            .unwrap();
        assert_eq!(runtime.active_touch_channels(now), [true, false]);
        runtime.update(&input(3, vec![]), now).unwrap();
        runtime
            .update(&input(4, vec![pointer(2, 0.33, 0.33, None)]), now)
            .unwrap();
        assert_eq!(runtime.active_touch_channels(now), [false, true]);
        runtime.reset();
        runtime
            .update(&input(1, vec![pointer(2, 0.33, 0.33, None)]), now)
            .unwrap();
        assert_eq!(runtime.active_touch_channels(now), [true, false]);
    }

    #[test]
    fn holding_same_cell_continues_and_changing_cell_restarts() {
        let now = Instant::now();
        let mut config = TouchConfig::default();
        config.free_waveforms[0] = waveform(&["0A0A0A0A14141414", "0A0A0A0A28282828"]);
        config.free_waveforms[1] = waveform(&["0A0A0A0A3C3C3C3C"]);
        let mut runtime = TouchRuntime::new(config).unwrap();
        runtime
            .update(&input(1, vec![pointer(1, 0.0, 0.0, Some(0))]), now)
            .unwrap();
        assert_eq!(
            runtime.next_frames(now)[0].samples()[0].pulse_intensity(),
            20
        );
        runtime
            .update(&input(2, vec![pointer(1, 0.5, 0.0, Some(0))]), now)
            .unwrap();
        assert_eq!(
            runtime.next_frames(now)[0].samples()[0].pulse_intensity(),
            40
        );
        runtime
            .update(&input(3, vec![pointer(1, 0.5, 0.0, Some(1))]), now)
            .unwrap();
        assert_eq!(
            runtime.next_frames(now)[0].samples()[0].pulse_intensity(),
            60
        );
        runtime
            .update(&input(4, vec![pointer(1, 0.0, 0.0, Some(0))]), now)
            .unwrap();
        assert_eq!(
            runtime.next_frames(now)[0].samples()[0].pulse_intensity(),
            20
        );
    }

    #[test]
    fn release_resumes_background_and_lease_expires_inside_frame() {
        let now = Instant::now();
        let config = TouchConfig {
            background: Some(waveform(&["0A0A0A0A14141414"])),
            ..TouchConfig::default()
        };
        let mut runtime = TouchRuntime::new(config).unwrap();
        runtime
            .update(&input(1, vec![pointer(1, 0.33, 0.33, None)]), now)
            .unwrap();
        let frames = runtime.next_frames(now + Duration::from_millis(950));
        assert_eq!(frames[0].samples()[0].pulse_intensity(), 100);
        assert_eq!(frames[0].samples()[1].pulse_intensity(), 100);
        assert_eq!(frames[0].samples()[2].pulse_intensity(), 20);
        runtime
            .update(&input(2, vec![]), now + Duration::from_millis(975))
            .unwrap();
        assert!(!runtime.has_active_input(now + Duration::from_millis(975)));
        assert_eq!(
            runtime.active_channels(now + Duration::from_millis(975)),
            [true, true]
        );
        assert_eq!(
            runtime.next_frames(now + Duration::from_millis(975))[0].samples()[0].pulse_intensity(),
            20
        );
        assert_eq!(
            runtime.active_touch_channels(now + Duration::from_secs(2)),
            [false, false]
        );
    }

    #[test]
    fn owner_sequence_and_device_are_checked_without_mutating_valid_state() {
        let now = Instant::now();
        let mut runtime = TouchRuntime::new(TouchConfig::default()).unwrap();
        let valid = input(10, vec![pointer(1, 0.33, 0.33, None)]);
        runtime.update(&valid, now).unwrap();
        assert!(runtime.update(&input(9, vec![]), now).is_err());
        let mut contender = input(11, vec![]);
        contender.owner_id = "window-2".to_owned();
        assert!(runtime.update(&contender, now).is_err());
        contender.device_id = "device-2".to_owned();
        assert!(runtime.update(&contender, now + TOUCH_INPUT_LEASE).is_err());
        assert!(runtime.has_active_input(now));
        contender.device_id = "device-1".to_owned();
        runtime.update(&contender, now + TOUCH_INPUT_LEASE).unwrap();
        assert!(!runtime.has_active_input(now + TOUCH_INPUT_LEASE));
    }

    #[test]
    fn released_owner_can_transfer_without_waiting_and_new_owner_restarts_alternate() {
        let now = Instant::now();
        let mut runtime = TouchRuntime::new(TouchConfig {
            routing: TouchRouting::Alternate,
            ..TouchConfig::default()
        })
        .unwrap();
        runtime
            .update(&input(10, vec![pointer(1, 0.33, 0.33, None)]), now)
            .unwrap();
        runtime.update(&input(11, vec![]), now).unwrap();
        assert!(runtime.update(&input(10, vec![]), now).is_err());
        let mut next_owner = input(1, vec![pointer(2, 0.33, 0.33, None)]);
        next_owner.owner_id = "window-2".to_owned();
        runtime.update(&next_owner, now).unwrap();
        assert_eq!(runtime.active_touch_channels(now), [true, false]);
        assert!(
            runtime
                .update(&input(12, vec![pointer(1, 0.33, 0.33, None)]), now)
                .is_err()
        );
    }

    #[test]
    fn touch_intent_changes_only_for_identity_cell_release_or_lease() {
        let now = Instant::now();
        let mut runtime = TouchRuntime::new(TouchConfig::default()).unwrap();
        runtime
            .update(&input(1, vec![pointer(1, 0.33, 0.33, None)]), now)
            .unwrap();
        let intents = [Some((1, None)); 2];
        assert_eq!(runtime.touch_intents(now), intents);
        runtime
            .update(&input(2, vec![pointer(1, 0.5, 0.5, None)]), now)
            .unwrap();
        assert_eq!(runtime.touch_intents(now), intents);
        runtime
            .update(&input(3, vec![pointer(1, 0.5, 0.5, Some(2))]), now)
            .unwrap();
        assert_eq!(runtime.touch_intents(now), [Some((1, Some(2))); 2]);
        assert_eq!(runtime.touch_intents(now + TOUCH_INPUT_LEASE), [None; 2]);
        runtime.update(&input(4, vec![]), now).unwrap();
        assert_eq!(runtime.touch_intents(now), [None; 2]);
    }

    #[test]
    fn factory_and_lost_touch_emit_silent_frames() {
        let factory = TouchFactory;
        let mut source = factory.build(&factory.default_config()).unwrap();
        assert_eq!(source.next_frame().unwrap(), WaveFrame::silent());
        let now = Instant::now();
        let mut runtime = TouchRuntime::new(TouchConfig::default()).unwrap();
        runtime
            .update(&input(1, vec![pointer(1, 0.33, 0.33, None)]), now)
            .unwrap();
        runtime.release_owner("other-window");
        assert!(runtime.has_active_input(now));
        runtime.release_owner("window-1");
        assert_eq!(runtime.next_frames(now), [WaveFrame::silent(); 2]);
        runtime
            .update(&input(2, vec![pointer(1, 0.33, 0.33, None)]), now)
            .unwrap();
        assert_eq!(
            runtime.next_frames(now + TOUCH_INPUT_LEASE),
            [WaveFrame::silent(); 2]
        );
        assert_eq!(
            runtime.active_touch_channels(now + TOUCH_INPUT_LEASE),
            [false; 2]
        );
    }

    #[test]
    fn stationary_gradient_advances_in_each_rust_sample() {
        let now = Instant::now();
        let mut runtime = TouchRuntime::new(TouchConfig {
            intensity_mode: TouchIntensityMode::Gradient,
            ..TouchConfig::default()
        })
        .unwrap();
        runtime
            .update(&input(1, vec![pointer(1, 0.0, 0.33, None)]), now)
            .unwrap();
        let first = runtime.next_frames(now);
        let second = runtime.next_frames(now + WaveFrame::DURATION);
        assert!(first[0].samples()[0].pulse_intensity() > first[0].samples()[3].pulse_intensity());
        assert!(first[0].samples()[3].pulse_intensity() > second[0].samples()[0].pulse_intensity());
    }

    #[test]
    fn gradient_direction_enables_only_its_selected_sides() {
        let now = Instant::now();
        for (direction, left_changes, right_changes) in [
            (TouchGradientDirection::Left, true, false),
            (TouchGradientDirection::Right, false, true),
            (TouchGradientDirection::Both, true, true),
        ] {
            let config = TouchConfig {
                intensity_mode: TouchIntensityMode::Gradient,
                gradient_direction: direction,
                ..TouchConfig::default()
            };
            let mut left = TouchRuntime::new(config.clone()).unwrap();
            left.update(&input(1, vec![pointer(1, 0.0, 0.33, None)]), now)
                .unwrap();
            let samples = left.next_frames(now)[0].into_samples();
            assert_eq!(samples[0].pulse_intensity(), 100);
            assert_eq!(samples[3].pulse_intensity() < 100, left_changes);
            let mut right = TouchRuntime::new(config).unwrap();
            right
                .update(&input(1, vec![pointer(1, 1.0, 0.33, None)]), now)
                .unwrap();
            let samples = right.next_frames(now)[0].into_samples();
            if right_changes {
                assert_eq!(samples[0].pulse_intensity(), 0);
                assert!(samples[3].pulse_intensity() > 0);
            } else {
                assert!(samples.iter().all(|sample| sample.pulse_intensity() == 100));
            }
        }
    }

    #[test]
    fn gradient_direction_serializes_readably_and_defaults_for_saved_config() {
        let value = TouchFactory.default_config();
        assert_eq!(value["gradientDirection"], "both");
        let mut old = value;
        old.as_object_mut().unwrap().remove("gradientDirection");
        assert_eq!(
            parse_config(&old).unwrap().gradient_direction,
            TouchGradientDirection::Both
        );
    }

    #[test]
    fn rhythm_coordinates_select_grid_cells_and_reject_invalid_input() {
        let now = Instant::now();
        let mut config = TouchConfig {
            mode: TouchMode::Rhythm,
            grid_size: 2,
            ..TouchConfig::default()
        };
        config.rhythm_waveforms[3] = waveform(&["0A0A0A0A28282828"]);
        let mut runtime = TouchRuntime::new(config).unwrap();
        runtime
            .update(&input(1, vec![pointer(1, 1.0, 1.0, None)]), now)
            .unwrap();
        assert_eq!(
            runtime.next_frames(now)[0].samples()[0].pulse_intensity(),
            40
        );
        assert!(
            runtime
                .update(&input(2, vec![pointer(1, 0.5, 0.5, Some(4))]), now)
                .is_err()
        );
        assert!(
            runtime
                .update(&input(2, vec![pointer(1, f64::NAN, 0.5, None)]), now)
                .is_err()
        );
        assert!(
            runtime
                .update(
                    &input(
                        2,
                        vec![pointer(1, 0.5, 0.5, None), pointer(2, 0.5, 0.5, None)]
                    ),
                    now
                )
                .is_err()
        );
    }

    #[test]
    fn config_validates_grid_waveforms_curves_and_total_frames() {
        let mut config = TouchConfig::default();
        assert!(TouchRuntime::new(config.clone()).is_ok());
        config.grid_size = 5;
        assert!(TouchRuntime::new(config.clone()).is_err());
        config.grid_size = 4;
        config.free_waveforms.pop();
        assert!(TouchRuntime::new(config.clone()).is_err());
        config.free_waveforms.push(WaveformConfig::default());
        config.period_curve[1].y = 101.0;
        assert!(TouchRuntime::new(config.clone()).is_err());
        config.period_curve[1].y = 10.0;
        config.free_waveforms[0].frames = vec!["0A0A0A0A00000000".to_owned(); MAX_PRESET_FRAMES];
        assert!(TouchRuntime::new(config).is_err());
    }
}
