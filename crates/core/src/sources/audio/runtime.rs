use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::{Deserialize, Serialize};

use crate::sources::SourceError;

use super::files::{AudioFile, Recording, finite_sample};
use super::{AudioFeatures, MAX_AUDIO_DURATION_MS, MAX_SAMPLE_RATE, analyze};

const CONTROL_CAPACITY: usize = 8;
const PCM_CAPACITY: usize = 4;
const FEATURE_LEASE: Duration = Duration::from_millis(500);

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

struct LatestFeatures {
    features: AudioFeatures,
    received_at: Instant,
    generation: u64,
    stop_epoch: u64,
}

/// 控制和 PCM 队列均有界。创建时不请求麦克风，明确动作才打开设备。
pub struct AudioEngine {
    controls: Mutex<mpsc::SyncSender<(AudioAction, u64, u64)>>,
    latest: Arc<Mutex<LatestFeatures>>,
    snapshot: Arc<Mutex<AudioSnapshot>>,
    running: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    stop_epoch: Arc<AtomicU64>,
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioEngine {
    pub fn new() -> Self {
        let (controls, receiver) = mpsc::sync_channel(CONTROL_CAPACITY);
        let latest = Arc::new(Mutex::new(LatestFeatures {
            features: AudioFeatures::silent(),
            received_at: Instant::now(),
            generation: 0,
            stop_epoch: 0,
        }));
        let snapshot = Arc::new(Mutex::new(AudioSnapshot::default()));
        let running = Arc::new(AtomicBool::new(true));
        let generation = Arc::new(AtomicU64::new(0));
        let stop_epoch = Arc::new(AtomicU64::new(0));
        let worker = Worker::new(
            latest.clone(),
            snapshot.clone(),
            running.clone(),
            generation.clone(),
            stop_epoch.clone(),
        );
        if let Err(error) = thread::Builder::new()
            .name("dglab-audio".to_owned())
            .spawn(move || worker.run(receiver))
        {
            running.store(false, Ordering::Release);
            let mut snapshot = lock(&snapshot);
            snapshot.state = AudioState::Error;
            snapshot.last_error = Some(format!("无法创建音频线程：{error}"));
        }
        Self {
            controls: Mutex::new(controls),
            latest,
            snapshot,
            running,
            generation,
            stop_epoch,
        }
    }

    pub fn control(&self, action: AudioAction) -> Result<(), SourceError> {
        if !self.running.load(Ordering::Acquire) {
            return Err(SourceError::Runtime("音频运行时已经关闭".to_owned()));
        }
        if let AudioAction::LoadFile { path } | AudioAction::SaveRecording { path } = &action
            && (path.is_empty() || path.len() > 4096)
        {
            return Err(SourceError::Runtime(
                "音频路径必须在 1..=4096 字节".to_owned(),
            ));
        }
        if matches!(&action, AudioAction::Seek { position_ms } if *position_ms > MAX_AUDIO_DURATION_MS)
        {
            return Err(SourceError::Runtime("播放位置不能超过一小时".to_owned()));
        }
        let controls = lock(&self.controls);
        let stop_epoch = self.stop_epoch.load(Ordering::Acquire);
        let preserves_stream = matches!(
            action,
            AudioAction::SaveRecording { .. } | AudioAction::SetPlaybackOptions { .. }
        );
        let generation = if preserves_stream {
            self.generation.load(Ordering::Acquire)
        } else {
            self.generation.load(Ordering::Acquire).wrapping_add(1)
        };
        controls
            .try_send((action.clone(), generation, stop_epoch))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    SourceError::Runtime("音频控制队列已满，请稍后重试".to_owned())
                }
                mpsc::TrySendError::Disconnected(_) => {
                    SourceError::Runtime("音频运行时已退出".to_owned())
                }
            })?;
        if !preserves_stream {
            // IPC 完成即撤销旧特征，worker 的旧 PCM 也由代次排除。
            self.generation.store(generation, Ordering::Release);
            lock(&self.latest).features = AudioFeatures::silent();
        }
        Ok(())
    }

    /// 紧急停止不获取普通控制锁，也不占用有界控制队列。
    /// 立即撤销旧特征和回调；worker 优先关闭流并丢弃之前排队的动作。
    pub fn emergency_stop(&self) {
        self.stop_epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub fn latest(&self) -> AudioFeatures {
        let latest = lock(&self.latest);
        if latest.received_at.elapsed() <= FEATURE_LEASE
            && latest.generation == self.generation.load(Ordering::Acquire)
            && latest.stop_epoch == self.stop_epoch.load(Ordering::Acquire)
        {
            latest.features.clone()
        } else {
            AudioFeatures::silent()
        }
    }

    pub fn snapshot(&self) -> AudioSnapshot {
        lock(&self.snapshot).clone()
    }

    pub fn shutdown(&self) {
        self.emergency_stop();
        self.running.store(false, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        lock(&self.latest).features = AudioFeatures::silent();
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct AudioBlock {
    generation: u64,
    stop_epoch: u64,
    rate: u32,
    samples: Vec<[f32; 2]>,
    received_at: Instant,
}

struct StreamFailure {
    generation: u64,
    stop_epoch: u64,
    message: String,
}

struct OutputBuffer {
    queue: VecDeque<[f32; 2]>,
    consumed: u64,
    speaker_enabled: bool,
}

struct Playback {
    file: AudioFile,
    rate: u32,
    phase: f64,
    current: Option<[f32; 2]>,
    next: Option<[f32; 2]>,
    buffer: Arc<Mutex<OutputBuffer>>,
    eof: bool,
    start_ms: u64,
    loop_enabled: bool,
    has_looped: bool,
}

impl Playback {
    fn refill(&mut self) -> Result<(), String> {
        let target = self.rate as usize / 5;
        let needed = target.saturating_sub(lock(&self.buffer).queue.len());
        let mut output = Vec::with_capacity(needed);
        for _ in 0..needed {
            if self.current.is_none() {
                self.current = self.file.next_frame()?;
                self.next = self.file.next_frame()?;
            }
            if self.current.is_none() && self.loop_enabled && self.file.duration_ms > 0 {
                self.file.seek(0)?;
                self.current = self.file.next_frame()?;
                self.next = self.file.next_frame()?;
                self.phase = 0.0;
                self.has_looped = true;
                self.eof = false;
            }
            let Some(current) = self.current else {
                self.eof = true;
                break;
            };
            let next = self.next.unwrap_or(current);
            let ratio = self.phase as f32;
            output.push([
                current[0] + (next[0] - current[0]) * ratio,
                current[1] + (next[1] - current[1]) * ratio,
            ]);
            self.phase += f64::from(self.file.rate) / f64::from(self.rate);
            while self.phase >= 1.0 {
                self.phase -= 1.0;
                self.current = self.next;
                self.next = self.file.next_frame()?;
                if self.current.is_none() && !self.loop_enabled {
                    self.eof = true;
                    break;
                }
            }
            if self.eof {
                break;
            }
        }
        lock(&self.buffer).queue.extend(output);
        Ok(())
    }

    fn position_ms(&self) -> u64 {
        let position = self.start_ms + lock(&self.buffer).consumed * 1000 / u64::from(self.rate);
        if (self.loop_enabled || self.has_looped) && self.file.duration_ms > 0 {
            position % self.file.duration_ms
        } else {
            position
        }
    }
}

struct Worker {
    latest: Arc<Mutex<LatestFeatures>>,
    snapshot: Arc<Mutex<AudioSnapshot>>,
    running: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    stop_epoch: Arc<AtomicU64>,
    observed_stop_epoch: u64,
    active_generation: u64,
    active_stop_epoch: u64,
    block_tx: mpsc::SyncSender<AudioBlock>,
    blocks: mpsc::Receiver<AudioBlock>,
    error_tx: mpsc::SyncSender<StreamFailure>,
    errors: mpsc::Receiver<StreamFailure>,
    stream: Option<cpal::Stream>,
    playback: Option<Playback>,
    recording: Option<Recording>,
    file_path: Option<PathBuf>,
    last_received: Instant,
}

impl Worker {
    fn new(
        latest: Arc<Mutex<LatestFeatures>>,
        snapshot: Arc<Mutex<AudioSnapshot>>,
        running: Arc<AtomicBool>,
        generation: Arc<AtomicU64>,
        stop_epoch: Arc<AtomicU64>,
    ) -> Self {
        let (block_tx, blocks) = mpsc::sync_channel(PCM_CAPACITY);
        let (error_tx, errors) = mpsc::sync_channel(1);
        Self {
            latest,
            snapshot,
            running,
            generation,
            stop_epoch,
            observed_stop_epoch: 0,
            active_generation: 0,
            active_stop_epoch: 0,
            block_tx,
            blocks,
            error_tx,
            errors,
            stream: None,
            playback: None,
            recording: None,
            file_path: None,
            last_received: Instant::now(),
        }
    }

    fn run(mut self, controls: mpsc::Receiver<(AudioAction, u64, u64)>) {
        while self.running.load(Ordering::Acquire) {
            self.apply_emergency_stop();
            match controls.recv_timeout(Duration::from_millis(5)) {
                Ok((action, generation, stop_epoch)) => {
                    self.handle_control(action, generation, stop_epoch);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            self.apply_emergency_stop();
            while let Ok(error) = self.errors.try_recv() {
                if error.generation == self.generation.load(Ordering::Acquire)
                    && error.stop_epoch == self.stop_epoch.load(Ordering::Acquire)
                {
                    self.fail(error.message);
                }
            }
            while let Ok(block) = self.blocks.try_recv() {
                if block.generation != self.generation.load(Ordering::Acquire)
                    || block.stop_epoch != self.stop_epoch.load(Ordering::Acquire)
                    || block.received_at.elapsed() > FEATURE_LEASE
                {
                    continue;
                }
                self.last_received = Instant::now();
                if let Err(error) = self.consume(block) {
                    self.fail(error);
                }
            }
            if let Some(playback) = &mut self.playback {
                if let Err(error) = playback.refill() {
                    self.fail(error);
                    continue;
                }
                let position_ms = playback.position_ms();
                let complete = playback.eof && lock(&playback.buffer).queue.is_empty();
                let mut snapshot = lock(&self.snapshot);
                snapshot.position_ms = position_ms;
                if snapshot.duration_ms == 0 {
                    snapshot.duration_ms = playback.file.duration_ms;
                }
                drop(snapshot);
                if complete {
                    self.close_stream();
                    let mut snapshot = lock(&self.snapshot);
                    if snapshot.duration_ms == 0 {
                        snapshot.duration_ms = position_ms;
                    }
                    snapshot.position_ms = snapshot.duration_ms;
                    snapshot.state = AudioState::Idle;
                }
            }
            if self.stream.is_some() {
                self.check_input_lease();
            }
        }
        self.close_stream();
        if let Some(recording) = &mut self.recording {
            let _ = recording.finish();
        }
    }

    fn apply_emergency_stop(&mut self) {
        let stop_epoch = self.stop_epoch.load(Ordering::Acquire);
        if stop_epoch == self.observed_stop_epoch {
            return;
        }
        self.observed_stop_epoch = stop_epoch;
        // 先关闭设备；录音收尾失败也不能保留正在播放或采集的流。
        self.close_stream();
        if let Err(error) = self.finish_recording() {
            self.fail(error);
        } else {
            let mut snapshot = lock(&self.snapshot);
            snapshot.state = AudioState::Idle;
            snapshot.position_ms = 0;
        }
    }

    fn handle_control(&mut self, action: AudioAction, generation: u64, stop_epoch: u64) {
        self.apply_emergency_stop();
        if stop_epoch != self.stop_epoch.load(Ordering::Acquire) {
            return;
        }
        self.active_generation = generation;
        self.active_stop_epoch = stop_epoch;
        if let Err(error) = self.handle(action) {
            self.fail(error);
        }
        // 动作可能阻塞在文件或设备初始化上；返回后仍须执行期间到达的停止。
        self.apply_emergency_stop();
    }

    fn handle(&mut self, action: AudioAction) -> Result<(), String> {
        lock(&self.snapshot).last_error = None;
        match action {
            AudioAction::LoadFile { path } => {
                self.finish_recording()?;
                self.close_stream();
                lock(&self.snapshot).state = AudioState::Loading;
                let file = AudioFile::open(Path::new(&path))?;
                self.file_path = Some(PathBuf::from(&path));
                let mut snapshot = lock(&self.snapshot);
                snapshot.mode = AudioMode::File;
                snapshot.state = AudioState::Idle;
                snapshot.file_name = Path::new(&path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned());
                snapshot.position_ms = 0;
                snapshot.duration_ms = file.duration_ms;
            }
            AudioAction::Play => {
                let mode = lock(&self.snapshot).mode;
                if matches!(mode, AudioMode::Microphone | AudioMode::Desktop) {
                    self.start_input(mode)?;
                } else {
                    self.finish_recording()?;
                    self.start_playback()?;
                }
            }
            AudioAction::Pause => {
                self.finish_recording()?;
                self.close_stream();
                lock(&self.snapshot).state = AudioState::Paused;
            }
            AudioAction::Stop => {
                self.finish_recording()?;
                self.close_stream();
                let mut snapshot = lock(&self.snapshot);
                snapshot.state = AudioState::Idle;
                snapshot.position_ms = 0;
            }
            AudioAction::Seek { position_ms } => {
                if matches!(
                    lock(&self.snapshot).state,
                    AudioState::Recording | AudioState::Capturing
                ) {
                    return Err("实时采集期间不能跳转播放位置".to_owned());
                }
                let playing = lock(&self.snapshot).state == AudioState::Playing;
                self.close_stream();
                let duration = lock(&self.snapshot).duration_ms;
                lock(&self.snapshot).position_ms = if duration > 0 {
                    position_ms.min(duration)
                } else {
                    position_ms
                };
                if playing {
                    self.start_playback()?;
                }
            }
            AudioAction::StartMicrophone => {
                self.finish_recording()?;
                self.start_input(AudioMode::Microphone)?;
            }
            AudioAction::StartDesktop => {
                self.finish_recording()?;
                self.start_input(AudioMode::Desktop)?;
            }
            AudioAction::StartRecording => {
                self.finish_recording()?;
                self.start_input(AudioMode::Recording)?;
            }
            AudioAction::StopRecording => {
                self.finish_recording()?;
                self.close_stream();
                lock(&self.snapshot).state = AudioState::Idle;
            }
            AudioAction::SaveRecording { path } => {
                if lock(&self.snapshot).state == AudioState::Recording {
                    return Err("请先结束录音再保存".to_owned());
                }
                let recording = self
                    .recording
                    .as_ref()
                    .filter(|recording| recording.frames > 0)
                    .ok_or("暂无可保存的录音")?;
                std::fs::copy(&recording.path, &path)
                    .map_err(|error| format!("保存 WAV 录音失败：{error}"))?;
            }
            AudioAction::SetPlaybackOptions {
                loop_enabled,
                speaker_enabled,
            } => {
                let mut snapshot = lock(&self.snapshot);
                snapshot.loop_enabled = loop_enabled;
                snapshot.speaker_enabled = speaker_enabled;
                if let Some(playback) = &mut self.playback {
                    playback.loop_enabled = loop_enabled;
                    lock(&playback.buffer).speaker_enabled = speaker_enabled;
                }
            }
        }
        Ok(())
    }

    fn finish_recording(&mut self) -> Result<(), String> {
        if lock(&self.snapshot).state != AudioState::Recording {
            return Ok(());
        }
        self.close_stream();
        if let Some(recording) = &mut self.recording {
            recording.finish()?;
            if recording.frames == 0 {
                return Err("录音中没有收到音频数据".to_owned());
            }
            self.file_path = Some(recording.path.clone());
            let mut snapshot = lock(&self.snapshot);
            snapshot.mode = AudioMode::Recording;
            snapshot.state = AudioState::Idle;
            snapshot.has_recording = true;
            snapshot.file_name = Some("麦克风录音.wav".to_owned());
            snapshot.position_ms = 0;
            snapshot.duration_ms = recording.duration_ms();
        }
        Ok(())
    }

    fn close_stream(&mut self) {
        if let Some(playback) = &self.playback {
            lock(&self.snapshot).position_ms = playback.position_ms();
        }
        self.stream.take();
        self.playback.take();
        self.clear_features();
    }

    fn clear_features(&self) {
        lock(&self.latest).features = AudioFeatures::silent();
        let mut snapshot = lock(&self.snapshot);
        snapshot.level_left = 0.0;
        snapshot.level_right = 0.0;
        snapshot.peak_left_hz = 0.0;
        snapshot.peak_right_hz = 0.0;
    }

    fn check_input_lease(&mut self) {
        if self.last_received.elapsed() <= FEATURE_LEASE {
            return;
        }
        let snapshot = lock(&self.snapshot);
        let quiet_desktop =
            snapshot.mode == AudioMode::Desktop && snapshot.state == AudioState::Capturing;
        drop(snapshot);
        if quiet_desktop {
            // WASAPI 回环在电脑未播放声音时可以不提供 PCM，静默不意味着设备故障。
            // 撤销旧特征和表头，但保留流；设备错误仍走 fail 关闭采集。
            self.clear_features();
        } else {
            self.fail("音频设备超过 500 ms 未提供数据，已停止音频输入".to_owned());
        }
    }

    fn fail(&mut self, error: String) {
        if lock(&self.snapshot).state == AudioState::Recording {
            let _ = self.finish_recording();
        }
        self.close_stream();
        let mut snapshot = lock(&self.snapshot);
        snapshot.state = AudioState::Error;
        snapshot.last_error = Some(error);
    }

    fn consume(&mut self, block: AudioBlock) -> Result<(), String> {
        if block.stop_epoch != self.stop_epoch.load(Ordering::Acquire) {
            return Ok(());
        }
        let state = lock(&self.snapshot).state;
        if !matches!(
            state,
            AudioState::Playing | AudioState::Capturing | AudioState::Recording
        ) {
            return Ok(());
        }
        if state == AudioState::Recording
            && let Some(recording) = &mut self.recording
        {
            recording.write(&block.samples)?;
            lock(&self.snapshot).position_ms = recording.duration_ms();
        }
        let features = analyze(&block.samples, block.rate, state != AudioState::Recording);
        let mut snapshot = lock(&self.snapshot);
        snapshot.level_left = features
            .windows
            .iter()
            .map(|window| window.channels[0].rms)
            .sum::<f64>()
            / 4.0;
        snapshot.level_right = features
            .windows
            .iter()
            .map(|window| window.channels[1].rms)
            .sum::<f64>()
            / 4.0;
        snapshot.peak_left_hz = features.windows[3].channels[0].peak_hz;
        snapshot.peak_right_hz = features.windows[3].channels[1].peak_hz;
        drop(snapshot);
        *lock(&self.latest) = LatestFeatures {
            features,
            received_at: block.received_at,
            generation: block.generation,
            stop_epoch: block.stop_epoch,
        };
        Ok(())
    }

    fn start_input(&mut self, mode: AudioMode) -> Result<(), String> {
        let recording = mode == AudioMode::Recording;
        let desktop = mode == AudioMode::Desktop;
        let source_name = if desktop { "桌面音频" } else { "麦克风" };
        self.close_stream();
        let (device, supported) = capture_device(mode)?;
        let rate = supported.sample_rate();
        let channels = usize::from(supported.channels());
        validate_device(rate, channels)?;
        if recording {
            self.recording = Some(Recording::new(rate)?);
            let mut snapshot = lock(&self.snapshot);
            snapshot.has_recording = false;
            snapshot.duration_ms = 0;
            snapshot.position_ms = 0;
            snapshot.file_name = Some("麦克风录音.wav".to_owned());
        }
        let mut blocks = BlockCollector::new(
            rate,
            self.active_generation,
            self.active_stop_epoch,
            self.block_tx.clone(),
            self.error_tx.clone(),
        );
        let error_tx = self.error_tx.clone();
        let generation = self.active_generation;
        let stream_epoch = self.active_stop_epoch;
        let stop_epoch = self.stop_epoch.clone();
        let config = supported.config();
        macro_rules! build {
            ($type:ty) => {
                device.build_input_stream(
                    config,
                    move |data: &[$type], _| {
                        if stop_epoch.load(Ordering::Acquire) != stream_epoch {
                            return;
                        }
                        blocks.begin_callback();
                        for frame in data.chunks_exact(channels) {
                            blocks.push(capture_frame(frame, desktop));
                        }
                    },
                    move |error| {
                        if let Some(message) = capture_error(mode, &error) {
                            let _ = error_tx.try_send(StreamFailure {
                                generation,
                                stop_epoch: stream_epoch,
                                message,
                            });
                        }
                    },
                    Some(Duration::from_secs(2)),
                )
            };
        }
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => build!(f32),
            cpal::SampleFormat::F64 => build!(f64),
            cpal::SampleFormat::I8 => build!(i8),
            cpal::SampleFormat::I16 => build!(i16),
            cpal::SampleFormat::I32 => build!(i32),
            cpal::SampleFormat::I64 => build!(i64),
            cpal::SampleFormat::U8 => build!(u8),
            cpal::SampleFormat::U16 => build!(u16),
            cpal::SampleFormat::U32 => build!(u32),
            cpal::SampleFormat::U64 => build!(u64),
            _ => return Err(format!("{source_name}的采样格式不受支持")),
        }
        .map_err(|error| format!("无法启动{source_name}：{error}"))?;
        stream
            .play()
            .map_err(|error| format!("无法启动{source_name}：{error}"))?;
        self.last_received = Instant::now();
        self.stream = Some(stream);
        let mut snapshot = lock(&self.snapshot);
        snapshot.mode = mode;
        snapshot.state = if recording {
            AudioState::Recording
        } else {
            AudioState::Capturing
        };
        if !recording {
            snapshot.file_name = None;
            snapshot.position_ms = 0;
            snapshot.duration_ms = 0;
        }
        Ok(())
    }

    fn start_playback(&mut self) -> Result<(), String> {
        self.close_stream();
        let path = self
            .file_path
            .as_ref()
            .ok_or("请先导入音频或视频文件，或完成录音")?;
        let mut file = if self
            .recording
            .as_ref()
            .is_some_and(|recording| &recording.path == path)
        {
            AudioFile::open_recording(path)?
        } else {
            AudioFile::open(path)?
        };
        let mut position_ms = lock(&self.snapshot).position_ms;
        if file.duration_ms > 0 && position_ms >= file.duration_ms {
            position_ms = 0;
        }
        if position_ms > 0 {
            file.seek(position_ms)?;
        }
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("系统没有可用的默认扬声器")?;
        let supported = device
            .default_output_config()
            .map_err(|error| format!("无法读取扬声器配置：{error}"))?;
        let rate = supported.sample_rate();
        let channels = usize::from(supported.channels());
        validate_device(rate, channels)?;
        let buffer = Arc::new(Mutex::new(OutputBuffer {
            queue: VecDeque::with_capacity(rate as usize / 5),
            consumed: 0,
            speaker_enabled: lock(&self.snapshot).speaker_enabled,
        }));
        let mut playback = Playback {
            file,
            rate,
            phase: 0.0,
            current: None,
            next: None,
            buffer: buffer.clone(),
            eof: false,
            start_ms: position_ms,
            loop_enabled: lock(&self.snapshot).loop_enabled,
            has_looped: false,
        };
        playback.refill()?;
        let mut blocks = BlockCollector::new(
            rate,
            self.active_generation,
            self.active_stop_epoch,
            self.block_tx.clone(),
            self.error_tx.clone(),
        );
        let error_tx = self.error_tx.clone();
        let generation = self.active_generation;
        let stream_epoch = self.active_stop_epoch;
        let stop_epoch = self.stop_epoch.clone();
        let config = supported.config();
        macro_rules! build {
            ($type:ty) => {
                device.build_output_stream(
                    config,
                    move |data: &mut [$type], _| {
                        if mute_cancelled_output(data, &stop_epoch, stream_epoch) {
                            return;
                        }
                        let mut queue = match buffer.try_lock() {
                            Ok(queue) => queue,
                            Err(_) => {
                                data.fill(<$type as cpal::Sample>::EQUILIBRIUM);
                                return;
                            }
                        };
                        for output in data.chunks_exact_mut(channels) {
                            if mute_cancelled_output(output, &stop_epoch, stream_epoch) {
                                continue;
                            }
                            let sample = queue.queue.pop_front();
                            if sample.is_some() {
                                queue.consumed += 1;
                            }
                            let frame = sample.unwrap_or([0.0, 0.0]);
                            for (index, output_sample) in output.iter_mut().enumerate() {
                                let value = if channels == 1 {
                                    (frame[0] + frame[1]) * 0.5
                                } else if index < 2 {
                                    frame[index]
                                } else {
                                    0.0
                                };
                                *output_sample = <$type as cpal::FromSample<f32>>::from_sample_(
                                    if queue.speaker_enabled { value } else { 0.0 },
                                );
                            }
                            blocks.push(frame);
                        }
                    },
                    move |error| {
                        let _ = error_tx.try_send(StreamFailure {
                            generation,
                            stop_epoch: stream_epoch,
                            message: format!("扬声器错误：{error}"),
                        });
                    },
                    Some(Duration::from_secs(2)),
                )
            };
        }
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => build!(f32),
            cpal::SampleFormat::F64 => build!(f64),
            cpal::SampleFormat::I8 => build!(i8),
            cpal::SampleFormat::I16 => build!(i16),
            cpal::SampleFormat::I32 => build!(i32),
            cpal::SampleFormat::I64 => build!(i64),
            cpal::SampleFormat::U8 => build!(u8),
            cpal::SampleFormat::U16 => build!(u16),
            cpal::SampleFormat::U32 => build!(u32),
            cpal::SampleFormat::U64 => build!(u64),
            _ => return Err("扬声器的采样格式不受支持".to_owned()),
        }
        .map_err(|error| format!("无法创建音频播放：{error}"))?;
        stream
            .play()
            .map_err(|error| format!("无法启动音频播放：{error}"))?;
        self.last_received = Instant::now();
        self.stream = Some(stream);
        self.playback = Some(playback);
        let mut snapshot = lock(&self.snapshot);
        snapshot.state = AudioState::Playing;
        snapshot.position_ms = position_ms;
        Ok(())
    }
}

fn mute_cancelled_output<T: cpal::Sample>(
    output: &mut [T],
    stop_epoch: &AtomicU64,
    stream_epoch: u64,
) -> bool {
    if stop_epoch.load(Ordering::Acquire) == stream_epoch {
        return false;
    }
    output.fill(T::EQUILIBRIUM);
    true
}

fn capture_device(mode: AudioMode) -> Result<(cpal::Device, cpal::SupportedStreamConfig), String> {
    if mode == AudioMode::Desktop {
        #[cfg(target_os = "windows")]
        {
            // WASAPI 将播放设备的 input stream 自动初始化为共享模式回环采集。
            // 必须使用播放配置，default_input_config 在播放设备上不受支持。
            let host = cpal::host_from_id(cpal::HostId::Wasapi)
                .map_err(|error| format!("无法初始化 Windows 桌面音频：{error}"))?;
            let device = host
                .default_output_device()
                .ok_or("系统没有可用的默认播放设备")?;
            let config = device
                .default_output_config()
                .map_err(|error| format!("无法读取桌面音频配置：{error}"))?;
            return Ok((device, config));
        }
        #[cfg(not(target_os = "windows"))]
        return Err("桌面音频目前仅支持 Windows".to_owned());
    }
    if !matches!(mode, AudioMode::Microphone | AudioMode::Recording) {
        return Err("文件模式不能启动实时采集".to_owned());
    }
    let device = cpal::default_host()
        .default_input_device()
        .ok_or("系统没有可用的默认麦克风")?;
    let config = device
        .default_input_config()
        .map_err(|error| format!("无法读取麦克风配置：{error}"))?;
    Ok((device, config))
}

fn capture_frame<T: cpal::Sample>(frame: &[T], desktop: bool) -> [f32; 2]
where
    f32: cpal::FromSample<T>,
{
    let sample = |index: usize| finite_sample(frame[index].to_sample::<f32>());
    let left = sample(0);
    let right = if frame.len() == 1 { left } else { sample(1) };
    if desktop && frame.len() > 2 {
        // 保留左右声道差异，将其他播放声道平均混入两路，避免漏掉环绕声中的对白。
        let shared = (2..frame.len()).map(sample).sum::<f32>() / (frame.len() - 2) as f32;
        [finite_sample(left + shared), finite_sample(right + shared)]
    } else {
        [left, right]
    }
}

fn capture_error(mode: AudioMode, error: &cpal::Error) -> Option<String> {
    if mode == AudioMode::Desktop {
        match error.kind() {
            // 静音后的恢复可以出现 discontinuity；它不代表回环流已经停止。
            cpal::ErrorKind::Xrun
            | cpal::ErrorKind::RealtimeDenied
            | cpal::ErrorKind::DeviceChanged => return None,
            cpal::ErrorKind::StreamInvalidated => {
                return Some("播放设备已更改或配置失效，请重新开启桌面音频".to_owned());
            }
            _ => {}
        }
        Some(format!("桌面音频错误：{error}"))
    } else {
        Some(format!("麦克风错误：{error}"))
    }
}

fn validate_device(rate: u32, channels: usize) -> Result<(), String> {
    if !(8000..=MAX_SAMPLE_RATE).contains(&rate) || !(1..=32).contains(&channels) {
        Err("音频设备需要 8..=192 kHz 采样率和 1..=32 声道".to_owned())
    } else {
        Ok(())
    }
}

struct BlockCollector {
    samples: Vec<[f32; 2]>,
    size: usize,
    rate: u32,
    generation: u64,
    stop_epoch: u64,
    sender: mpsc::SyncSender<AudioBlock>,
    errors: mpsc::SyncSender<StreamFailure>,
    last_callback: Instant,
}

impl BlockCollector {
    fn new(
        rate: u32,
        generation: u64,
        stop_epoch: u64,
        sender: mpsc::SyncSender<AudioBlock>,
        errors: mpsc::SyncSender<StreamFailure>,
    ) -> Self {
        let size = (rate / 10) as usize;
        Self {
            samples: Vec::with_capacity(size),
            size,
            rate,
            generation,
            stop_epoch,
            sender,
            errors,
            last_callback: Instant::now(),
        }
    }

    fn begin_callback(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_callback) > FEATURE_LEASE {
            // 桌面音频从静默恢复时丢弃旧的未满块，避免把旧声音混入新特征。
            self.samples.clear();
        }
        self.last_callback = now;
    }

    fn push(&mut self, sample: [f32; 2]) {
        self.samples.push(sample);
        if self.samples.len() == self.size {
            let samples = std::mem::replace(&mut self.samples, Vec::with_capacity(self.size));
            if self
                .sender
                .try_send(AudioBlock {
                    generation: self.generation,
                    stop_epoch: self.stop_epoch,
                    rate: self.rate,
                    samples,
                    received_at: Instant::now(),
                })
                .is_err()
            {
                let _ = self.errors.try_send(StreamFailure {
                    generation: self.generation,
                    stop_epoch: self.stop_epoch,
                    message: "音频处理队列拥塞，已停止输入以避免延迟输出".to_owned(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queued_engine() -> (AudioEngine, mpsc::Receiver<(AudioAction, u64, u64)>, Worker) {
        let (sender, receiver) = mpsc::sync_channel(CONTROL_CAPACITY);
        let engine = AudioEngine {
            controls: Mutex::new(sender),
            latest: Arc::new(Mutex::new(LatestFeatures {
                features: AudioFeatures {
                    active: true,
                    ..Default::default()
                },
                received_at: Instant::now(),
                generation: 0,
                stop_epoch: 0,
            })),
            snapshot: Arc::new(Mutex::new(AudioSnapshot {
                state: AudioState::Playing,
                ..Default::default()
            })),
            running: Arc::new(AtomicBool::new(true)),
            generation: Arc::new(AtomicU64::new(0)),
            stop_epoch: Arc::new(AtomicU64::new(0)),
        };
        let worker = Worker::new(
            engine.latest.clone(),
            engine.snapshot.clone(),
            engine.running.clone(),
            engine.generation.clone(),
            engine.stop_epoch.clone(),
        );
        (engine, receiver, worker)
    }

    #[test]
    fn emergency_stop_bypasses_full_queue_and_busy_locks_and_discards_old_starts() {
        let (engine, receiver, mut worker) = queued_engine();
        let engine = Arc::new(engine);
        // 文件未加载；即使取消过滤回归，Play 也会在查找设备之前失败，不打开音频设备。
        for _ in 0..CONTROL_CAPACITY {
            engine.control(AudioAction::Play).unwrap();
        }
        assert!(engine.control(AudioAction::Stop).is_err());
        *lock(&engine.latest) = LatestFeatures {
            features: AudioFeatures {
                active: true,
                ..Default::default()
            },
            received_at: Instant::now(),
            generation: CONTROL_CAPACITY as u64,
            stop_epoch: 0,
        };
        assert!(engine.latest().active);

        let controls_guard = lock(&engine.controls);
        let features_guard = lock(&engine.latest);
        let snapshot_guard = lock(&engine.snapshot);
        let (finished, completion) = mpsc::channel();
        let stop_engine = engine.clone();
        let stopping = thread::spawn(move || {
            stop_engine.emergency_stop();
            let _ = finished.send(());
        });
        let result = completion.recv_timeout(Duration::from_secs(2));
        drop(snapshot_guard);
        drop(features_guard);
        drop(controls_guard);
        stopping.join().unwrap();
        assert!(result.is_ok(), "紧急停止不得等待控制、特征或快照锁");
        assert!(
            !engine.latest().active,
            "worker 处理停止之前就必须撤销旧特征"
        );

        while let Ok((action, generation, stop_epoch)) = receiver.try_recv() {
            worker.handle_control(action, generation, stop_epoch);
            assert_eq!(engine.snapshot().state, AudioState::Idle);
            assert_eq!(engine.snapshot().last_error, None);
        }
        assert!(worker.stream.is_none());
        assert!(worker.playback.is_none());
        assert!(!engine.latest().active);
    }

    #[test]
    fn stopped_epoch_drops_stale_pcm_and_new_controls_keep_fifo() {
        let (engine, receiver, mut worker) = queued_engine();
        engine.emergency_stop();
        worker.apply_emergency_stop();
        assert_eq!(engine.snapshot().state, AudioState::Idle);
        engine.control(AudioAction::Pause).unwrap();
        engine
            .control(AudioAction::Seek { position_ms: 125 })
            .unwrap();
        engine
            .control(AudioAction::SetPlaybackOptions {
                loop_enabled: true,
                speaker_enabled: false,
            })
            .unwrap();
        let (action, generation, stop_epoch) = receiver.try_recv().unwrap();
        assert!(matches!(action, AudioAction::Pause));
        assert_eq!(stop_epoch, 1);
        worker.handle_control(action, generation, stop_epoch);
        assert_eq!(engine.snapshot().state, AudioState::Paused);
        let (action, generation, stop_epoch) = receiver.try_recv().unwrap();
        assert!(matches!(action, AudioAction::Seek { position_ms: 125 }));
        worker.handle_control(action, generation, stop_epoch);
        assert_eq!(engine.snapshot().position_ms, 125);
        let (action, generation, stop_epoch) = receiver.try_recv().unwrap();
        worker.handle_control(action, generation, stop_epoch);
        assert!(engine.snapshot().loop_enabled);
        assert!(!engine.snapshot().speaker_enabled);

        // 模拟停止后显式重启产生的新流；旧 PCM 即使普通代次相同也不能进入新流。
        lock(&engine.snapshot).state = AudioState::Playing;
        let generation = engine.generation.load(Ordering::Acquire);
        worker
            .consume(AudioBlock {
                generation,
                stop_epoch: 0,
                rate: 48_000,
                samples: vec![[0.5, 0.5]; 4800],
                received_at: Instant::now(),
            })
            .unwrap();
        assert!(!engine.latest().active);
        assert_eq!(engine.snapshot().level_left, 0.0);
        worker
            .consume(AudioBlock {
                generation,
                stop_epoch: 1,
                rate: 48_000,
                samples: vec![[0.5, 0.5]; 4800],
                received_at: Instant::now(),
            })
            .unwrap();
        assert!(engine.latest().active);
        assert_eq!(engine.snapshot().level_left, 0.5);
    }

    #[test]
    fn emergency_stop_mutes_speaker_callback_without_waiting_for_worker() {
        let stop_epoch = AtomicU64::new(0);
        let mut output = [0.75f32, -0.25];
        assert!(!mute_cancelled_output(&mut output, &stop_epoch, 0));
        assert_eq!(output, [0.75, -0.25]);
        stop_epoch.fetch_add(1, Ordering::AcqRel);
        assert!(mute_cancelled_output(&mut output, &stop_epoch, 0));
        assert_eq!(output, [0.0, 0.0]);
        let mut signed_output = [20_000i16, -12_000];
        assert!(mute_cancelled_output(&mut signed_output, &stop_epoch, 0));
        assert_eq!(signed_output, [0, 0]);
        let mut unsigned_output = [0u16, u16::MAX];
        assert!(mute_cancelled_output(&mut unsigned_output, &stop_epoch, 0));
        assert_eq!(unsigned_output, [<u16 as cpal::Sample>::EQUILIBRIUM; 2]);
    }

    fn capture_worker(mode: AudioMode) -> Worker {
        Worker::new(
            Arc::new(Mutex::new(LatestFeatures {
                features: AudioFeatures::silent(),
                received_at: Instant::now(),
                generation: 0,
                stop_epoch: 0,
            })),
            Arc::new(Mutex::new(AudioSnapshot {
                mode,
                state: AudioState::Capturing,
                ..Default::default()
            })),
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        )
    }

    #[test]
    fn desktop_silence_clears_features_without_stopping_and_sound_resumes() {
        let mut worker = capture_worker(AudioMode::Desktop);
        worker
            .consume(AudioBlock {
                generation: 0,
                stop_epoch: 0,
                rate: 48_000,
                samples: vec![[0.5, -0.25]; 4800],
                received_at: Instant::now(),
            })
            .unwrap();
        assert!(lock(&worker.latest).features.active);
        assert_eq!(lock(&worker.snapshot).level_left, 0.5);
        worker.last_received = Instant::now() - Duration::from_secs(1);
        worker.check_input_lease();
        assert_eq!(lock(&worker.snapshot).state, AudioState::Capturing);
        assert_eq!(lock(&worker.snapshot).mode, AudioMode::Desktop);
        assert_eq!(lock(&worker.snapshot).level_left, 0.0);
        assert!(!lock(&worker.latest).features.active);
        worker
            .consume(AudioBlock {
                generation: 0,
                stop_epoch: 0,
                rate: 48_000,
                samples: vec![[0.25, -0.5]; 4800],
                received_at: Instant::now(),
            })
            .unwrap();
        assert!(lock(&worker.latest).features.active);
        assert_eq!(lock(&worker.snapshot).level_right, 0.5);
        worker.handle(AudioAction::Stop).unwrap();
        assert_eq!(lock(&worker.snapshot).state, AudioState::Idle);
        assert!(!lock(&worker.latest).features.active);
        worker
            .consume(AudioBlock {
                generation: 0,
                stop_epoch: 0,
                rate: 48_000,
                samples: vec![[1.0, 1.0]; 4800],
                received_at: Instant::now(),
            })
            .unwrap();
        assert!(!lock(&worker.latest).features.active);
    }

    #[test]
    fn microphone_timeout_remains_an_error() {
        let mut worker = capture_worker(AudioMode::Microphone);
        worker.last_received = Instant::now() - Duration::from_secs(1);
        worker.check_input_lease();
        assert_eq!(lock(&worker.snapshot).state, AudioState::Error);
        assert!(!lock(&worker.latest).features.active);
    }

    #[test]
    fn desktop_callback_gap_discards_old_partial_audio() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let (errors, _) = mpsc::sync_channel(1);
        let mut collector = BlockCollector::new(8000, 7, 0, sender, errors);
        collector.push([1.0, 1.0]);
        collector.last_callback = Instant::now() - Duration::from_secs(1);
        collector.begin_callback();
        assert!(collector.samples.is_empty());
        for _ in 0..800 {
            collector.push([0.25, -0.5]);
        }
        let block = receiver.try_recv().unwrap();
        assert!(block.samples.iter().all(|sample| *sample == [0.25, -0.5]));
    }

    #[test]
    fn desktop_pcm_includes_surround_channels_and_filters_invalid_samples() {
        assert_eq!(capture_frame(&[0.25f32, -0.5], true), [0.25, -0.5]);
        assert_eq!(capture_frame(&[0.5f32], true), [0.5, 0.5]);
        assert_eq!(
            capture_frame(&[0.0f32, 0.0, 0.8, 0.0, 0.0, 0.0], true),
            [0.2, 0.2]
        );
        assert_eq!(capture_frame(&[0.0f32, 0.0, 0.8], false), [0.0, 0.0]);
        assert_eq!(capture_frame(&[f32::NAN, f32::INFINITY], true), [0.0, 0.0]);
        assert_eq!(capture_frame(&[32768u16, 49152], true), [0.0, 0.5]);
    }

    #[test]
    fn desktop_discontinuity_is_nonfatal_but_device_failure_is_reported() {
        let xrun = cpal::Error::new(cpal::ErrorKind::Xrun);
        assert!(capture_error(AudioMode::Desktop, &xrun).is_none());
        assert!(capture_error(AudioMode::Microphone, &xrun).is_some());
        let missing = cpal::Error::new(cpal::ErrorKind::DeviceNotAvailable);
        assert!(
            capture_error(AudioMode::Desktop, &missing)
                .unwrap()
                .contains("桌面音频")
        );
        let changed = cpal::Error::new(cpal::ErrorKind::StreamInvalidated);
        assert!(
            capture_error(AudioMode::Desktop, &changed)
                .unwrap()
                .contains("重新开启")
        );
    }

    #[test]
    fn desktop_wire_contract_matches_frontend() {
        assert_eq!(serde_json::to_value(AudioMode::Desktop).unwrap(), "desktop");
        assert_eq!(
            serde_json::to_value(AudioAction::StartDesktop).unwrap()["type"],
            "startDesktop"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "需要本机默认播放设备；显式运行，不连接 DG-LAB 设备也不保存录音"]
    fn windows_desktop_loopback_opens_waits_and_stops() {
        let engine = AudioEngine::new();
        engine.control(AudioAction::StartDesktop).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = engine.snapshot();
            assert_ne!(
                snapshot.state,
                AudioState::Error,
                "{:?}",
                snapshot.last_error
            );
            if snapshot.state == AudioState::Capturing {
                break;
            }
            assert!(Instant::now() < deadline, "桌面音频未启动");
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(750));
        assert_eq!(engine.snapshot().mode, AudioMode::Desktop);
        assert_eq!(engine.snapshot().state, AudioState::Capturing);
        engine.control(AudioAction::Stop).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while engine.snapshot().state != AudioState::Idle {
            assert!(Instant::now() < deadline, "桌面音频未停止");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!engine.latest().active);
        engine.shutdown();
    }

    #[test]
    fn expired_features_never_produce_output() {
        let engine = AudioEngine::new();
        *lock(&engine.latest) = LatestFeatures {
            features: AudioFeatures {
                active: true,
                ..Default::default()
            },
            received_at: Instant::now() - Duration::from_secs(1),
            generation: 0,
            stop_epoch: 0,
        };
        assert!(!engine.latest().active);
        engine.shutdown();
        assert!(engine.control(AudioAction::Play).is_err());
    }

    #[test]
    fn pcm_queue_is_bounded_and_reports_overflow() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let (errors, error_receiver) = mpsc::sync_channel(1);
        let mut collector = BlockCollector::new(8000, 7, 0, sender, errors);
        for _ in 0..1600 {
            collector.push([0.0, 0.0]);
        }
        assert_eq!(receiver.try_recv().unwrap().generation, 7);
        assert!(receiver.try_recv().is_err());
        assert!(error_receiver.try_recv().unwrap().message.contains("拥塞"));
        assert!(collector.samples.len() < collector.size);
    }

    #[test]
    fn rejected_controls_do_not_invalidate_the_current_generation() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let engine = AudioEngine {
            controls: Mutex::new(sender),
            latest: Arc::new(Mutex::new(LatestFeatures {
                features: AudioFeatures {
                    active: true,
                    ..Default::default()
                },
                received_at: Instant::now(),
                generation: 7,
                stop_epoch: 0,
            })),
            snapshot: Arc::new(Mutex::new(AudioSnapshot::default())),
            running: Arc::new(AtomicBool::new(true)),
            generation: Arc::new(AtomicU64::new(7)),
            stop_epoch: Arc::new(AtomicU64::new(0)),
        };
        assert!(engine.latest().active);
        engine.control(AudioAction::Pause).unwrap();
        assert_eq!(engine.generation.load(Ordering::Acquire), 8);
        assert!(!engine.latest().active);
        lock(&engine.latest).features.active = true;
        lock(&engine.latest).generation = 8;
        assert!(engine.control(AudioAction::Play).is_err());
        assert_eq!(engine.generation.load(Ordering::Acquire), 8);
        assert!(engine.latest().active);
        receiver.try_recv().unwrap();
        engine
            .control(AudioAction::SetPlaybackOptions {
                loop_enabled: true,
                speaker_enabled: false,
            })
            .unwrap();
        assert_eq!(engine.generation.load(Ordering::Acquire), 8);
        assert!(engine.latest().active);
    }

    #[test]
    fn recording_collects_sound_without_producing_device_output() {
        let latest = Arc::new(Mutex::new(LatestFeatures {
            features: AudioFeatures::silent(),
            received_at: Instant::now(),
            generation: 0,
            stop_epoch: 0,
        }));
        let snapshot = Arc::new(Mutex::new(AudioSnapshot {
            state: AudioState::Recording,
            mode: AudioMode::Recording,
            ..Default::default()
        }));
        let mut worker = Worker::new(
            latest.clone(),
            snapshot.clone(),
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        );
        worker.recording = Some(Recording::new(48_000).unwrap());
        worker
            .consume(AudioBlock {
                generation: 0,
                stop_epoch: 0,
                rate: 48_000,
                samples: vec![[0.5, 0.5]; 4800],
                received_at: Instant::now(),
            })
            .unwrap();
        assert!(!lock(&latest).features.active);
        assert_eq!(lock(&snapshot).position_ms, 100);
        assert_eq!(lock(&snapshot).level_left, 0.5);
        worker.finish_recording().unwrap();
        assert!(lock(&snapshot).has_recording);
        assert_eq!(lock(&snapshot).state, AudioState::Idle);
        assert_eq!(lock(&snapshot).duration_ms, 100);
    }

    #[test]
    fn video_audio_loops_across_container_eof_in_the_existing_buffer() {
        for name in [
            "video-aac.mp4",
            "video-pcm.mov",
            "video-fallback.mkv",
            "video-vorbis.webm",
        ] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/sources/audio/testdata")
                .join(name);
            let buffer = Arc::new(Mutex::new(OutputBuffer {
                queue: VecDeque::new(),
                consumed: 0,
                speaker_enabled: true,
            }));
            let mut playback = Playback {
                file: AudioFile::open(&path).unwrap(),
                rate: 48_000,
                phase: 0.0,
                current: None,
                next: None,
                buffer: buffer.clone(),
                eof: false,
                start_ms: 0,
                loop_enabled: true,
                has_looped: false,
            };
            for _ in 0..8 {
                playback
                    .refill()
                    .unwrap_or_else(|error| panic!("{name}: {error}"));
                let mut output = lock(&buffer);
                assert_eq!(output.queue.len(), 9600, "{name}");
                assert!(
                    output.queue.iter().any(|frame| frame[0].abs() > 0.1),
                    "{name}"
                );
                output.consumed += output.queue.len() as u64;
                output.queue.clear();
            }
            assert!(playback.has_looped, "{name}");
            assert!(!playback.eof, "{name}");
        }
    }

    #[test]
    fn loop_refills_the_existing_stream_buffer_and_wraps_position() {
        let mut recording = Recording::new(48_000).unwrap();
        recording.write(&vec![[0.25, -0.5]; 4800]).unwrap();
        recording.finish().unwrap();
        let buffer = Arc::new(Mutex::new(OutputBuffer {
            queue: VecDeque::new(),
            consumed: 0,
            speaker_enabled: true,
        }));
        let mut playback = Playback {
            file: AudioFile::open_recording(&recording.path).unwrap(),
            rate: 48_000,
            phase: 0.0,
            current: None,
            next: None,
            buffer: buffer.clone(),
            eof: false,
            start_ms: 0,
            loop_enabled: true,
            has_looped: false,
        };
        playback.refill().unwrap();
        assert_eq!(lock(&buffer).queue.len(), 9600);
        assert!(
            lock(&buffer)
                .queue
                .iter()
                .all(|sample| (sample[0] - 0.25).abs() < 0.001)
        );
        assert!(!playback.eof);
        lock(&buffer).consumed = 4320;
        assert_eq!(playback.position_ms(), 90);
        lock(&buffer).consumed = 5280;
        assert_eq!(playback.position_ms(), 10);
        assert!(AudioSnapshot::default().speaker_enabled);
        assert!(!AudioSnapshot::default().loop_enabled);
    }
}
