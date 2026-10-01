use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatReader, SeekMode, SeekTo, Track, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::units::{Time, TimeBase};

use super::{
    AUDIO_FILE_EXTENSIONS, MAX_AUDIO_DURATION_MS, MAX_AUDIO_FILE_BYTES, MAX_SAMPLE_RATE,
    MAX_VIDEO_FILE_BYTES, VIDEO_FILE_EXTENSIONS,
};

/// 音频或视频容器只解码选中的音轨，每次保留一个音频包，不缓存整段 PCM。
pub(super) struct AudioFile {
    path: PathBuf,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    pub rate: u32,
    pub duration_ms: u64,
    buffer: Vec<f32>,
    channels: usize,
    cursor: usize,
    decoded_frames: u64,
    eof: bool,
}

impl AudioFile {
    pub fn open(path: &Path) -> Result<Self, String> {
        Self::open_bounded(path)
    }

    pub fn open_recording(path: &Path) -> Result<Self, String> {
        Self::open_bounded(path)
    }

    fn open_bounded(path: &Path) -> Result<Self, String> {
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let is_video = VIDEO_FILE_EXTENSIONS.contains(&extension.as_str());
        if !is_video && !AUDIO_FILE_EXTENSIONS.contains(&extension.as_str()) {
            return Err(
                "支持 MP3、FLAC、WAV、M4A、AAC、OGG 音频及 MP4、M4V、MOV、MKV、WebM 视频"
                    .to_owned(),
            );
        }
        let mut file = File::open(path).map_err(|error| format!("无法打开音视频文件：{error}"))?;
        let length = file.metadata().map_err(|error| error.to_string())?.len();
        let mut header = [0u8; 12];
        file.read_exact(&mut header)
            .map_err(|error| format!("无法读取音视频文件头：{error}"))?;
        validate_file_size(length, &header, is_video)?;
        file.seek(SeekFrom::Start(0))
            .map_err(|error| format!("无法读取音视频文件：{error}"))?;
        let mut hint = Hint::new();
        hint.with_extension(&extension);
        let format = symphonia::default::get_probe()
            .probe(
                &hint,
                MediaSourceStream::new(Box::new(file), Default::default()),
                Default::default(),
                Default::default(),
            )
            .map_err(|error| format!("无法识别音视频格式：{error}"))?;
        let (track, decoder) = select_audio_track(format.as_ref())?;
        let parameters = track
            .codec_params
            .as_ref()
            .and_then(|parameters| parameters.audio())
            .ok_or("不支持此音频编码")?;
        let rate = parameters.sample_rate.ok_or("音频缺少采样率")?;
        if !(8000..=MAX_SAMPLE_RATE).contains(&rate) {
            return Err("音频采样率必须在 8..=192 kHz".to_owned());
        }
        let channels = parameters
            .channels
            .as_ref()
            .map_or(0, |channels| channels.count());
        if !(1..=8).contains(&channels) {
            return Err("音频声道数必须在 1..=8".to_owned());
        }
        if parameters
            .max_frames_per_packet
            .is_some_and(|frames| frames > u64::from(rate) * 2)
        {
            return Err("音频单个解码包超出两秒上限".to_owned());
        }
        let mut duration_ms = match (track.time_base, track.duration) {
            (Some(base), Some(duration)) => base
                .calc_duration(duration)
                .map_or(0, |time| (time.as_secs_f64().max(0.0) * 1000.0) as u64),
            _ => track
                .num_frames
                .map_or(0, |frames| frames.saturating_mul(1000) / u64::from(rate)),
        };
        // MKV/WebM 通常只提供容器时长，使用它展示进度并校验导入时长。
        if duration_ms == 0 {
            let media = format.media_info();
            duration_ms = media
                .time_base
                .zip(media.duration)
                .and_then(|(base, duration)| base.calc_duration(duration))
                .map_or(0, |time| (time.as_secs_f64().max(0.0) * 1000.0) as u64);
        }
        if duration_ms > MAX_AUDIO_DURATION_MS {
            return Err("音频时长不能超过一小时".to_owned());
        }
        let track_id = track.id;
        let time_base = track.time_base;
        Ok(Self {
            path: path.to_owned(),
            format,
            decoder,
            track_id,
            time_base,
            rate,
            duration_ms,
            buffer: Vec::new(),
            channels,
            cursor: 0,
            decoded_frames: 0,
            eof: false,
        })
    }

    pub fn seek(&mut self, position_ms: u64) -> Result<(), String> {
        // ISO MP4 解析器在读到容器尾部后可能丢失 atom 状态，重新打开再跳转。
        // EOF 也缓存于 next_frame，播放重采样器的重复探测不会继续读取解析器。
        if self.eof {
            let mut reopened = Self::open_bounded(&self.path)?;
            if reopened.duration_ms == 0 {
                reopened.duration_ms = self.duration_ms;
            }
            *self = reopened;
        }
        let time = Time::try_from_secs_f64(position_ms as f64 / 1000.0).ok_or("播放位置无效")?;
        let seeked = self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|error| format!("无法跳转播放位置：{error}"))?;
        self.decoder.reset();
        self.buffer.clear();
        self.cursor = 0;
        self.decoded_frames = position_ms.saturating_mul(u64::from(self.rate)) / 1000;
        // Accurate 模式落在目标之前，丢弃目标之前的 PCM，防止时间轴偏移。
        if let Some(base) = self.time_base {
            let actual_ms = base
                .calc_time(seeked.actual_ts)
                .map_or(position_ms, |time| {
                    (time.as_secs_f64().max(0.0) * 1000.0) as u64
                });
            self.decoded_frames = actual_ms.saturating_mul(u64::from(self.rate)) / 1000;
            let discard = position_ms
                .saturating_sub(actual_ms)
                .saturating_mul(u64::from(self.rate))
                / 1000;
            for _ in 0..discard {
                if self.next_frame()?.is_none() {
                    break;
                }
            }
            self.decoded_frames = position_ms.saturating_mul(u64::from(self.rate)) / 1000;
        }
        Ok(())
    }

    pub fn next_frame(&mut self) -> Result<Option<[f32; 2]>, String> {
        if self.eof {
            return Ok(None);
        }
        while self.cursor >= self.buffer.len() {
            let Some(packet) = self
                .format
                .next_packet()
                .map_err(|error| format!("读取音频失败：{error}"))?
            else {
                self.eof = true;
                if self.duration_ms == 0 {
                    self.duration_ms = self.decoded_frames * 1000 / u64::from(self.rate);
                }
                return Ok(None);
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let audio = self
                .decoder
                .decode(&packet)
                .map_err(|error| format!("解码音频失败：{error}"))?;
            if audio.spec().rate() != self.rate || audio.spec().channels().count() != self.channels
            {
                return Err("播放中音频格式发生变化，请重新导入".to_owned());
            }
            if audio.frames() > self.rate as usize * 2 {
                return Err("音频解码包超出两秒内存上限".to_owned());
            }
            self.buffer.resize(audio.samples_interleaved(), 0.0);
            audio.copy_to_slice_interleaved(&mut self.buffer);
            self.cursor = 0;
        }
        if self.decoded_frames >= u64::from(self.rate) * MAX_AUDIO_DURATION_MS / 1000 {
            return Err("音频时长不能超过一小时".to_owned());
        }
        let left = finite_sample(self.buffer[self.cursor]);
        let right = if self.channels == 1 {
            left
        } else {
            finite_sample(self.buffer[self.cursor + 1])
        };
        self.cursor += self.channels;
        self.decoded_frames += 1;
        Ok(Some([left, right]))
    }
}

/// 优先使用容器的默认音轨；其编码不受支持时选择第一个可解码音轨。
/// 视频和字幕包由 next_frame 跳过，无需视频解码器或中间提取文件。
fn select_audio_track(
    format: &dyn FormatReader,
) -> Result<(&Track, Box<dyn AudioDecoder>), String> {
    let default = format.default_track(TrackType::Audio);
    let default_id = default.map(|track| track.id);
    let candidates = default.into_iter().chain(
        format
            .tracks()
            .iter()
            .filter(|track| Some(track.id) != default_id),
    );
    let mut has_audio = false;
    for track in candidates {
        let Some(parameters) = track
            .codec_params
            .as_ref()
            .and_then(|parameters| parameters.audio())
        else {
            continue;
        };
        has_audio = true;
        if let Ok(decoder) = symphonia::default::get_codecs()
            .make_audio_decoder(parameters, &AudioDecoderOptions::default())
        {
            return Ok((track, decoder));
        }
    }
    Err(if has_audio {
        "文件中没有可解码的音轨；支持 AAC、MP3、FLAC、ALAC、PCM、Vorbis 等编码，暂不支持 Opus、AC-3/E-AC-3"
            .to_owned()
    } else {
        "文件中没有音轨，请选择包含音频的视频或音频文件".to_owned()
    })
}

/// 本应用保存的双声道 16 位 WAV 可大于压缩音频的 200 MB 导入上限。
/// 只有实际 RIFF/WAVE 头启用有界的一小时 PCM 容量，随后仍检查时长和解码参数。
fn validate_file_size(length: u64, header: &[u8; 12], is_video: bool) -> Result<(), String> {
    let is_wave = &header[..4] == b"RIFF" && &header[8..] == b"WAVE";
    let maximum_bytes = if is_wave {
        u64::from(MAX_SAMPLE_RATE) * 4 * MAX_AUDIO_DURATION_MS / 1000 + 44
    } else if is_video {
        MAX_VIDEO_FILE_BYTES
    } else {
        MAX_AUDIO_FILE_BYTES
    };
    if length == 0 || length > maximum_bytes {
        return Err(if is_wave {
            "WAV 文件超出一小时双声道 PCM 容量上限".to_owned()
        } else if is_video {
            "视频文件必须大于零且不超过 2 GB".to_owned()
        } else {
            "音频文件必须大于零且不超过 200 MB".to_owned()
        });
    }
    Ok(())
}

pub(super) fn finite_sample(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// 16 位双声道 WAV 按块写入磁盘，录音缓存不随时长增长。
pub(super) struct Recording {
    pub path: PathBuf,
    writer: Option<BufWriter<File>>,
    pub rate: u32,
    pub frames: u64,
}

impl Recording {
    pub fn new(rate: u32) -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!(
            "dg-lab-link-recording-{}.wav",
            uuid::Uuid::new_v4()
        ));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| format!("无法创建录音临时文件：{error}"))?;
        let mut recording = Self {
            path,
            writer: Some(BufWriter::new(file)),
            rate,
            frames: 0,
        };
        recording.write_header()?;
        Ok(recording)
    }

    pub fn write(&mut self, samples: &[[f32; 2]]) -> Result<(), String> {
        if self.frames + samples.len() as u64 > u64::from(self.rate) * MAX_AUDIO_DURATION_MS / 1000
        {
            return Err("录音已达到一小时上限".to_owned());
        }
        let writer = self.writer.as_mut().ok_or("录音已经结束")?;
        for sample in samples {
            for channel in sample {
                let value = (finite_sample(*channel) * i16::MAX as f32).round() as i16;
                writer
                    .write_all(&value.to_le_bytes())
                    .map_err(|error| format!("录音写入失败：{error}"))?;
            }
        }
        self.frames += samples.len() as u64;
        Ok(())
    }

    fn write_header(&mut self) -> Result<(), String> {
        let data_len = (self.frames * 4) as u32;
        let writer = self.writer.as_mut().ok_or("录音已经结束")?;
        writer
            .seek(SeekFrom::Start(0))
            .map_err(|error| error.to_string())?;
        for bytes in [
            b"RIFF".as_slice(),
            &(36 + data_len).to_le_bytes(),
            b"WAVEfmt ".as_slice(),
            &16u32.to_le_bytes(),
            &1u16.to_le_bytes(),
            &2u16.to_le_bytes(),
            &self.rate.to_le_bytes(),
            &(self.rate * 4).to_le_bytes(),
            &4u16.to_le_bytes(),
            &16u16.to_le_bytes(),
            b"data".as_slice(),
            &data_len.to_le_bytes(),
        ] {
            writer.write_all(bytes).map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), String> {
        if self.writer.is_some() {
            self.write_header()?;
            self.writer
                .take()
                .expect("writer checked")
                .flush()
                .map_err(|error| format!("无法保存录音：{error}"))?;
        }
        Ok(())
    }

    pub fn duration_ms(&self) -> u64 {
        self.frames * 1000 / u64::from(self.rate)
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        self.writer.take();
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/sources/audio/testdata")
            .join(name)
    }

    #[test]
    fn video_containers_decode_only_audio_and_support_seeking() {
        for name in [
            "video-aac.mp4",
            "video-pcm.mov",
            "video-fallback.mkv",
            "video-vorbis.webm",
        ] {
            let mut file = AudioFile::open(&fixture(name)).unwrap();
            assert_eq!(file.rate, 48_000, "{name}");
            assert!((400..=650).contains(&file.duration_ms), "{name}");
            let mut samples = Vec::new();
            while let Some(frame) = file.next_frame().unwrap() {
                assert!(frame.iter().all(|value| value.is_finite()), "{name}");
                samples.push(frame);
            }
            assert!((24_000..=32_000).contains(&samples.len()), "{name}");
            assert!(samples.iter().any(|frame| frame[0].abs() > 0.1), "{name}");
            if name == "video-aac.mp4" || name == "video-vorbis.webm" {
                let features = super::super::analyze(&samples[4800..9600], file.rate, true);
                let channels = &features.windows[0].channels;
                assert!((channels[0].peak_in_band(100.0, 1000.0) - 440.0).abs() < 30.0);
                assert!((channels[1].peak_in_band(100.0, 1000.0) - 880.0).abs() < 30.0);
            }
            assert!(file.next_frame().unwrap().is_none(), "{name}");
            assert!(file.next_frame().unwrap().is_none(), "{name}");
            file.seek(250)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert!(file.next_frame().unwrap().is_some(), "{name}");
            file.seek(0).unwrap();
            assert!(file.next_frame().unwrap().is_some(), "{name}");
        }
    }

    #[test]
    fn unsupported_default_track_falls_back_to_decodable_audio() {
        let mut file = AudioFile::open(&fixture("video-fallback.mkv")).unwrap();
        let frame = file.next_frame().unwrap().unwrap();
        assert!((frame[0] - 0.25).abs() < 0.001);
        assert!((frame[1] + 0.5).abs() < 0.001);
    }

    #[test]
    fn absent_and_unsupported_audio_have_distinct_errors() {
        let missing = AudioFile::open(&fixture("video-no-audio.mp4"))
            .err()
            .unwrap();
        assert!(missing.contains("没有音轨"), "{missing}");
        let unsupported = AudioFile::open(&fixture("video-opus.webm")).err().unwrap();
        assert!(unsupported.contains("没有可解码的音轨"), "{unsupported}");
    }

    #[test]
    fn video_size_limit_is_separate_and_bounded() {
        let mp4 = b"\0\0\0\x20ftypisom";
        assert!(validate_file_size(MAX_AUDIO_FILE_BYTES + 1, mp4, true).is_ok());
        assert!(validate_file_size(MAX_AUDIO_FILE_BYTES + 1, mp4, false).is_err());
        assert!(validate_file_size(MAX_VIDEO_FILE_BYTES, mp4, true).is_ok());
        assert!(validate_file_size(MAX_VIDEO_FILE_BYTES + 1, mp4, true).is_err());
        assert!(validate_file_size(0, mp4, true).is_err());
    }

    #[test]
    fn recording_roundtrips_and_removes_its_temporary_file() {
        let path;
        {
            let mut recording = Recording::new(48_000).unwrap();
            path = recording.path.clone();
            recording.write(&vec![[0.25, -0.5]; 4800]).unwrap();
            recording.finish().unwrap();
            assert_eq!(recording.duration_ms(), 100);
            let mut decoder = AudioFile::open(&path).unwrap();
            assert_eq!(decoder.duration_ms, 100);
            let frame = decoder.next_frame().unwrap().unwrap();
            assert!((frame[0] - 0.25).abs() < 0.001);
            assert!((frame[1] + 0.5).abs() < 0.001);
            decoder.seek(50).unwrap();
            assert!(decoder.next_frame().unwrap().is_some());
        }
        assert!(!path.exists());
    }

    #[test]
    fn recording_is_bounded_and_nonfinite_samples_are_silent() {
        let mut recording = Recording::new(8000).unwrap();
        recording.frames = u64::from(recording.rate) * MAX_AUDIO_DURATION_MS / 1000;
        assert!(recording.write(&[[0.0, 0.0]]).is_err());
        assert_eq!(finite_sample(f32::NAN), 0.0);
        assert_eq!(finite_sample(f32::INFINITY), 0.0);
    }

    #[test]
    fn long_wav_recordings_can_be_reimported_without_relaxing_other_formats() {
        let long_recording_bytes = 48_000 * 4 * MAX_AUDIO_DURATION_MS / 1000 + 44;
        assert!(long_recording_bytes > MAX_AUDIO_FILE_BYTES);
        assert!(validate_file_size(long_recording_bytes, b"RIFF\0\0\0\0WAVE", false).is_ok());
        assert!(validate_file_size(long_recording_bytes, b"fLaC\0\0\0\0\0\0\0\0", false).is_err());
        assert!(validate_file_size(long_recording_bytes, b"RIFF\0\0\0\0AVI ", false).is_err());
        let pcm_limit = u64::from(MAX_SAMPLE_RATE) * 4 * MAX_AUDIO_DURATION_MS / 1000 + 44;
        assert!(validate_file_size(pcm_limit + 1, b"RIFF\0\0\0\0WAVE", false).is_err());
    }
}
