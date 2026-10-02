//! Shared waveform catalogue and bounded .pulse / JSON import.
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::control::ControlError;
use crate::sources::{WaveformConfig, builtin_registry};

pub const MAX_IMPORT_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_IMPORT_FILES: usize = 128;
const MAX_FRAMES: usize = 16_384;

pub use dg_lab_link_contracts::waveforms::WaveformFile;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficialWaveform {
    #[serde(flatten)]
    pub config: WaveformConfig,
    pub english_name: String,
    pub duration_ms: usize,
}

pub fn official_waveforms() -> &'static [OfficialWaveform] {
    static CATALOGUE: OnceLock<Vec<OfficialWaveform>> = OnceLock::new();
    CATALOGUE.get_or_init(|| {
        serde_json::from_str(include_str!("../../../shared/official-waveforms.json"))
            .expect("内置波形目录必须有效")
    })
}

pub fn validate(config: &WaveformConfig) -> Result<(), ControlError> {
    builtin_registry()
        .validate(
            "builtin.fixed_waveform",
            &serde_json::to_value(config).map_err(json_error)?,
        )
        .map_err(|error| import_error(error.to_string()))
}

pub fn parse_files(files: &[WaveformFile]) -> Result<Vec<WaveformConfig>, ControlError> {
    if files.len() > MAX_IMPORT_FILES {
        return Err(import_error("一次最多导入 128 个文件"));
    }
    let mut configs = Vec::new();
    let mut frame_count = 0usize;
    for file in files {
        if file.name.is_empty() || file.name.len() > 1024 {
            return Err(import_error("波形文件名长度无效"));
        }
        if file.content.len() > MAX_IMPORT_FILE_BYTES {
            return Err(import_error(format!("{} 超过 2 MB，无法导入", file.name)));
        }
        let extension = file
            .name
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let imported = if extension == "pulse" {
            vec![parse_pulse(file)?]
        } else {
            parse_json(file)?
        };
        for config in imported {
            validate(&config)?;
            frame_count += config.frames.len();
            if configs.len() >= MAX_IMPORT_FILES || frame_count > MAX_FRAMES {
                return Err(import_error(
                    "一次导入最多 128 项波形，且总帧数不能超过 16384",
                ));
            }
            configs.push(config);
        }
    }
    Ok(configs)
}

fn parse_json(file: &WaveformFile) -> Result<Vec<WaveformConfig>, ControlError> {
    let value: Value = serde_json::from_str(&file.content)
        .map_err(|_| import_error(format!("{} 不是有效的 JSON 波形文件", file.name)))?;
    let fallback = file
        .name
        .rsplit_once('.')
        .map_or(file.name.as_str(), |(stem, _)| stem);
    if let Some(array) = value.as_array() {
        if array.iter().all(Value::is_string) {
            return Ok(vec![parse_json_waveform(&value, fallback, &file.name)?]);
        }
        if array.len() > MAX_IMPORT_FILES {
            return Err(import_error("一次导入最多 128 项波形"));
        }
        return array
            .iter()
            .enumerate()
            .map(|(index, item)| {
                parse_json_waveform(item, &format!("{fallback} {}", index + 1), &file.name)
            })
            .collect();
    }
    Ok(vec![parse_json_waveform(&value, fallback, &file.name)?])
}

fn parse_json_waveform(
    value: &Value,
    fallback: &str,
    file_name: &str,
) -> Result<WaveformConfig, ControlError> {
    let frames = if value.is_array() {
        value
    } else {
        value
            .get("frames")
            .or_else(|| value.get("pulseData"))
            .ok_or_else(|| {
                import_error(format!("{file_name} 中的波形缺少 frames 或 pulseData 数组"))
            })?
    };
    let frames = frames
        .as_array()
        .ok_or_else(|| import_error(format!("{file_name} 中的波形必须是对象或十六进制帧数组")))?;
    if frames.is_empty() || frames.len() > MAX_FRAMES {
        return Err(import_error(format!(
            "{file_name} 中的波形必须包含 1–16384 帧"
        )));
    }
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| value.get("presetName").and_then(Value::as_str))
        .unwrap_or(fallback)
        .trim();
    let name = if name.is_empty() { fallback } else { name };
    let frames = frames
        .iter()
        .map(|frame| {
            frame
                .as_str()
                .map(|text| text.trim().to_ascii_uppercase())
                .ok_or_else(|| import_error(format!("{file_name} 中的帧必须为十六进制字符串")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(custom_config(name, frames))
}

// Matches the existing MIT-licensed @dg-kit/waveforms parser's 25 ms grid.
fn parse_pulse(file: &WaveformFile) -> Result<WaveformConfig, ControlError> {
    const PREFIX: &str = "Dungeonlab+pulse:";
    const FREQUENCIES: [f64; 19] = [
        10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0, 200.0, 300.0, 400.0, 500.0,
        600.0, 700.0, 800.0, 900.0, 1000.0,
    ];
    const DURATIONS: [usize; 13] = [1, 2, 3, 4, 5, 8, 10, 15, 20, 30, 40, 50, 60];
    let text = file.content.trim();
    if !text
        .get(..PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(PREFIX))
    {
        return Err(import_error(
            "脉冲格式无效，必须以 'Dungeonlab+pulse:' 开头",
        ));
    }
    let mut parts = text[PREFIX.len()..].split("+section+");
    let first = parts.next().unwrap_or_default();
    let (name, first_section) = first
        .split_once('=')
        .ok_or_else(|| import_error("脉冲格式无效，缺少 '=' 分隔符"))?;
    let mut samples: Vec<(u8, u8)> = Vec::new();
    for (section_index, section) in std::iter::once(first_section)
        .chain(parts)
        .take(10)
        .enumerate()
    {
        if section.is_empty() {
            continue;
        }
        let (header, shape) = section
            .split_once('/')
            .ok_or_else(|| import_error(format!("第 {} 段缺少 '/' 分隔符", section_index + 1)))?;
        let values: Vec<_> = header.split(',').take(5).collect();
        let number = |index: usize| values.get(index).map_or(0.0, |value| js_number(value));
        let start = FREQUENCIES[dataset_index(number(0), FREQUENCIES.len())];
        let end = FREQUENCIES[dataset_index(number(1), FREQUENCIES.len())];
        let duration = DURATIONS[dataset_index(number(2), DURATIONS.len())];
        let mode = number(3);
        let shape: Vec<u8> = shape
            .split(',')
            .filter(|part| !part.is_empty())
            .map(|part| {
                js_number(part.split('-').next().unwrap_or(""))
                    .round()
                    .clamp(0.0, 100.0) as u8
            })
            .collect();
        if shape.len() < 2 {
            return Err(import_error(format!(
                "第 {} 段至少需要 2 个形状点",
                section_index + 1
            )));
        }
        if values.get(4) == Some(&"0") {
            continue;
        }
        let count = duration.div_ceil(shape.len()).max(1);
        let section_len = count * shape.len();
        if samples.len() + section_len > MAX_FRAMES * 4 {
            return Err(import_error("解析后的波形不能超过 16384 帧"));
        }
        for element in 0..count {
            for (index, intensity) in shape.iter().enumerate() {
                let progress = if mode == 2.0 {
                    (element * shape.len() + index) as f64 / section_len as f64
                } else if mode == 3.0 {
                    index as f64 / shape.len() as f64
                } else if mode == 4.0 && count > 1 {
                    element as f64 / (count - 1) as f64
                } else {
                    0.0
                };
                samples.push((
                    encode_frequency(start + (end - start) * progress),
                    *intensity,
                ));
            }
        }
    }
    if samples.is_empty() {
        return Err(import_error("脉冲数据无效，没有启用的分段"));
    }
    let frames = samples
        .chunks(4)
        .map(|chunk| {
            let mut group = [*chunk.last().expect("分组非空"); 4];
            group[..chunk.len()].copy_from_slice(chunk);
            group
                .iter()
                .map(|(frequency, _)| format!("{frequency:02X}"))
                .chain(
                    group
                        .iter()
                        .map(|(_, intensity)| format!("{intensity:02X}")),
                )
                .collect()
        })
        .collect();
    let fallback = file
        .name
        .rsplit_once('.')
        .map_or(file.name.as_str(), |(stem, _)| stem);
    Ok(custom_config(
        if name.trim().is_empty() {
            fallback
        } else {
            name.trim()
        },
        frames,
    ))
}

fn js_number(value: &str) -> f64 {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
        .unwrap_or(0.0)
}

fn dataset_index(value: f64, length: usize) -> usize {
    value.floor().clamp(0.0, (length - 1) as f64) as usize
}

fn encode_frequency(value: f64) -> u8 {
    let encoded = if value <= 100.0 {
        value
    } else if value <= 600.0 {
        (value - 100.0) / 5.0 + 100.0
    } else {
        (value - 600.0) / 10.0 + 200.0
    };
    encoded.round().clamp(10.0, 240.0) as u8
}

fn custom_config(name: &str, frames: Vec<String>) -> WaveformConfig {
    WaveformConfig {
        preset_id: format!("custom-{}", uuid::Uuid::new_v4()),
        preset_name: name.to_owned(),
        frames,
    }
}

fn import_error(message: impl Into<String>) -> ControlError {
    ControlError::new("invalid_source_config", message)
}
fn json_error(error: serde_json::Error) -> ControlError {
    import_error(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_is_unique_and_every_frame_is_valid() {
        let catalogue = official_waveforms();
        let mut ids = std::collections::BTreeSet::new();
        assert_eq!(catalogue.len(), 24);
        for waveform in catalogue {
            assert!(ids.insert(&waveform.config.preset_id));
            assert_eq!(waveform.duration_ms, waveform.config.frames.len() * 100);
            validate(&waveform.config).unwrap();
        }
    }

    #[test]
    fn pulse_matches_gui_sampling_and_pads_incomplete_groups() {
        let files = [WaveformFile {
            name: "呼吸.pulse".to_owned(),
            content: "Dungeonlab+pulse:自定义呼吸=0,0,0,1,1/0-0,100-0,50-0,0-0".to_owned(),
        }];
        let parsed = parse_files(&files).unwrap();
        assert_eq!(parsed[0].preset_name, "自定义呼吸");
        assert_eq!(parsed[0].frames, ["0A0A0A0A00643200"]);
        let parsed = parse_files(&[WaveformFile {
            name: "三点.pulse".to_owned(),
            content: "dungeonlab+pulse:=0,0,0,1,1/0-0,50-0,100-0".to_owned(),
        }])
        .unwrap();
        assert_eq!(parsed[0].frames, ["0A0A0A0A00326464"]);
    }

    #[test]
    fn json_supports_gui_formats_and_rejects_out_of_range_frames() {
        let configs = parse_files(&[WaveformFile { name: "组合.json".to_owned(), content: r#"[{"name":"波形一","frames":["0a0a0a0a00643200"]},{"presetName":"波形二","pulseData":["2D2D2D2D64646464"]}]"#.to_owned() }]).unwrap();
        assert_eq!(configs[0].frames, ["0A0A0A0A00643200"]);
        assert_ne!(configs[0].preset_id, configs[1].preset_id);
        assert!(
            parse_files(&[WaveformFile {
                name: "错误.json".to_owned(),
                content: r#"["090A0A0A00643200"]"#.to_owned()
            }])
            .is_err()
        );
    }

    #[test]
    fn pulse_import_matches_existing_frontend_reference_corpus() {
        let cases: Vec<Value> = serde_json::from_str(include_str!(
            "../../../shared/waveform-import-fixtures.json"
        ))
        .unwrap();
        for case in cases {
            let file = WaveformFile {
                name: case["name"].as_str().unwrap().to_owned(),
                content: case["content"].as_str().unwrap().to_owned(),
            };
            let parsed = parse_files(std::slice::from_ref(&file)).unwrap();
            assert_eq!(
                serde_json::to_value(&parsed[0].frames).unwrap(),
                case["frames"],
                "{}",
                file.name
            );
            assert_eq!(parsed[0].preset_name, case["presetName"].as_str().unwrap());
        }
    }
}
