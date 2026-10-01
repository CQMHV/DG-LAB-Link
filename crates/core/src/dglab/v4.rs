use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};
use thiserror::Error;
use url::Url;

use crate::model::{Channel, WaveFrame, WaveSample};

pub const DEFAULT_RELAY_URL: &str = "wss://trex.dungeon-lab.cn/v4";
pub const APP_LINK_URL: &str = "https://dungeon-lab.cn/s/";
pub const WAVE_FRAME_DURATION_MS: u64 = 100;
pub const CLOSE_CONTROLLER_DISCONNECTED: u16 = 4000;
pub const CLOSE_CONTROLLER_NOT_FOUND: u16 = 4001;
pub const CLOSE_IDLE_TIMEOUT: u16 = 4002;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("无效 JSON：{0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("协议帧必须是 JSON 对象")]
    ExpectedObject,
    #[error("协议字段 {field} 缺失或类型错误")]
    InvalidField { field: &'static str },
    #[error("RPC 响应必须且只能包含 result 或 error 之一")]
    InvalidResponse,
    #[error("{field} 不能为空")]
    EmptyIdentifier { field: &'static str },
    #[error("V4 操作优先级必须在 0..=2，当前为 {0}")]
    InvalidPriority(u8),
    #[error("V3 波形帧必须恰好为 16 个十六进制字符，当前长度为 {0}")]
    InvalidHexLength(usize),
    #[error("V3 波形帧第 {index} 个字节不是有效十六进制")]
    InvalidHex { index: usize },
    #[error("V3 波形帧的第 {index} 个频率编码必须在 10..=240，当前为 {actual}")]
    InvalidFrequency { index: usize, actual: u8 },
    #[error("V3 波形帧的第 {index} 个波形强度必须在 0..=100，当前为 {actual}")]
    InvalidPulseIntensity { index: usize, actual: u8 },
    #[error("AppendPulseData 至少需要一帧波形")]
    EmptyPulseFrames,
    #[error("URL 无效：{0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("WebSocket 地址必须使用 ws 或 wss scheme")]
    InvalidWebSocketScheme,
}

/// V4 Relay 层入站帧。未知 `type` 会保留为 `Unknown`，以允许服务端前向扩展。
#[derive(Debug, Clone, PartialEq)]
pub enum RelayFrame {
    Hello {
        client_id: String,
    },
    ClientAttached {
        client_id: String,
    },
    ClientDisconnected {
        client_id: String,
    },
    ControllerAttached {
        client_id: String,
    },
    ControllerDisconnected {
        client_id: String,
    },
    Heartbeat,
    Pong {
        timestamp: Option<i64>,
    },
    Message {
        client_id: Option<String>,
        data: Value,
    },
    IdleTimeout,
    Error {
        code: String,
        client_id: Option<String>,
        message: Option<String>,
    },
    Unknown {
        frame_type: Option<String>,
        raw: Value,
    },
}

/// 解析 Relay 文本帧。已知帧会校验必要字段，未知帧不会导致连接中止。
pub fn parse_relay_text(text: &str) -> Result<RelayFrame, ProtocolError> {
    let value = serde_json::from_str(text)?;
    parse_relay_frame(&value)
}

pub fn parse_relay_frame(value: &Value) -> Result<RelayFrame, ProtocolError> {
    let object = value.as_object().ok_or(ProtocolError::ExpectedObject)?;
    let frame_type = optional_string(object, "type")?;

    let frame = match frame_type.as_deref() {
        Some("hello") => RelayFrame::Hello {
            client_id: required_string(object, "clientId")?,
        },
        Some("client_attached") => RelayFrame::ClientAttached {
            client_id: required_string(object, "clientId")?,
        },
        Some("client_disconnected") => RelayFrame::ClientDisconnected {
            client_id: required_string(object, "clientId")?,
        },
        Some("controller_attached") => RelayFrame::ControllerAttached {
            client_id: required_string(object, "clientId")?,
        },
        Some("controller_disconnected") => RelayFrame::ControllerDisconnected {
            client_id: required_string(object, "clientId")?,
        },
        Some("heartbeat") => RelayFrame::Heartbeat,
        Some("pong") => RelayFrame::Pong {
            timestamp: optional_i64(object, "ts")?,
        },
        Some("message") => RelayFrame::Message {
            // APP 收到的下行帧没有 clientId；控制方收到的上行帧有 clientId。
            client_id: optional_string(object, "clientId")?,
            data: object
                .get("data")
                .cloned()
                .ok_or(ProtocolError::InvalidField { field: "data" })?,
        },
        Some("idle_timeout") => RelayFrame::IdleTimeout,
        Some("error") => RelayFrame::Error {
            code: required_string(object, "code")?,
            client_id: optional_string(object, "clientId")?,
            message: optional_string(object, "message")?,
        },
        _ => RelayFrame::Unknown {
            frame_type,
            raw: value.clone(),
        },
    };

    Ok(frame)
}

/// V4 APP 应用层消息。未知 `t` 或未知事件名称可由调用方安全忽略。
#[derive(Debug, Clone, PartialEq)]
pub enum AppMessage {
    Request {
        request_id: String,
        method: String,
        data: Option<Value>,
    },
    Response {
        request_id: String,
        result: Option<Value>,
        error: Option<String>,
    },
    Event {
        name: String,
        body: Map<String, Value>,
    },
    Unknown {
        message_type: Option<String>,
        raw: Value,
    },
}

pub fn parse_app_message(value: &Value) -> Result<AppMessage, ProtocolError> {
    let object = value.as_object().ok_or(ProtocolError::ExpectedObject)?;
    let message_type = optional_string(object, "t")?;

    match message_type.as_deref() {
        Some("req") => Ok(AppMessage::Request {
            request_id: required_string(object, "reqId")?,
            method: required_string(object, "m")?,
            data: object.get("data").cloned(),
        }),
        Some("resp") => {
            let request_id = required_string(object, "reqId")?;
            let result = object.get("result").cloned();
            let error = optional_string(object, "error")?;
            if result.is_some() == error.is_some() {
                return Err(ProtocolError::InvalidResponse);
            }
            Ok(AppMessage::Response {
                request_id,
                result,
                error,
            })
        }
        Some("ev") => {
            let name = required_string(object, "ev")?;
            let mut body = object.clone();
            body.remove("t");
            body.remove("ev");
            Ok(AppMessage::Event { name, body })
        }
        _ => Ok(AppMessage::Unknown {
            message_type,
            raw: value.clone(),
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDevice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub slot_id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub device_type: String,
    #[serde(default)]
    pub props: Map<String, Value>,
    #[serde(default)]
    pub slot_state: Map<String, Value>,
    /// 保留未来设备字段，避免协议升级时丢失信息。
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceUpdate {
    NoChange,
    Snapshot {
        count: usize,
    },
    DevicesPatched {
        added: Vec<String>,
        removed: Vec<String>,
    },
    SlotsPatched {
        updated: Vec<String>,
        missing: Vec<String>,
    },
}

#[derive(Debug, Default, Clone)]
pub struct DeviceStore {
    devices: HashMap<String, RemoteDevice>,
}

impl DeviceStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn get(&self, slot_id: &str) -> Option<&RemoteDevice> {
        self.devices.get(slot_id)
    }

    pub fn values(&self) -> impl Iterator<Item = &RemoteDevice> {
        self.devices.values()
    }

    pub fn clear(&mut self) {
        self.devices.clear();
    }

    /// 应用 APP 消息中的设备全量或增量。未知事件返回 `NoChange`。
    pub fn apply_app_message(&mut self, value: &Value) -> Result<DeviceUpdate, ProtocolError> {
        match parse_app_message(value)? {
            AppMessage::Event { name, body } if name == "devices.snapshot" => {
                self.apply_snapshot(body.get("devices"))
            }
            AppMessage::Event { name, body } if name == "devices.patch" => {
                self.apply_devices_patch(&body)
            }
            AppMessage::Event { name, body } if name == "slots.patch" => {
                self.apply_slots_patch(body.get("slots"))
            }
            // `devices.get` 的响应也能作为全量设备列表使用；关联 reqId 仍由 session 管理。
            AppMessage::Response {
                result: Some(result),
                ..
            } if result.get("devices").is_some() => self.apply_snapshot(result.get("devices")),
            _ => Ok(DeviceUpdate::NoChange),
        }
    }

    fn apply_snapshot(&mut self, devices: Option<&Value>) -> Result<DeviceUpdate, ProtocolError> {
        let devices = parse_devices(devices, "devices")?;
        self.devices = devices
            .into_iter()
            .map(|device| (device.slot_id.clone(), device))
            .collect();
        Ok(DeviceUpdate::Snapshot {
            count: self.devices.len(),
        })
    }

    fn apply_devices_patch(
        &mut self,
        body: &Map<String, Value>,
    ) -> Result<DeviceUpdate, ProtocolError> {
        let added = match body.get("added") {
            Some(value) => serde_json::from_value::<Vec<RemoteDevice>>(value.clone())?,
            None => Vec::new(),
        };
        let removed = match body.get("removed") {
            Some(value) => serde_json::from_value::<Vec<String>>(value.clone())?,
            None => Vec::new(),
        };

        for slot_id in &removed {
            self.devices.remove(slot_id);
        }
        let added_ids = added
            .into_iter()
            .map(|device| {
                let slot_id = device.slot_id.clone();
                self.devices.insert(slot_id.clone(), device);
                slot_id
            })
            .collect();

        Ok(DeviceUpdate::DevicesPatched {
            added: added_ids,
            removed,
        })
    }

    fn apply_slots_patch(&mut self, slots: Option<&Value>) -> Result<DeviceUpdate, ProtocolError> {
        let slots = slots
            .ok_or(ProtocolError::InvalidField { field: "slots" })?
            .as_array()
            .ok_or(ProtocolError::InvalidField { field: "slots" })?;
        let mut updated = Vec::new();
        let mut missing = Vec::new();

        for slot in slots {
            let patch = slot
                .as_object()
                .ok_or(ProtocolError::InvalidField { field: "slots[]" })?;
            let slot_id = required_string(patch, "slotId")?;
            let Some(device) = self.devices.get_mut(&slot_id) else {
                missing.push(slot_id);
                continue;
            };

            if let Some(props) = optional_object(patch, "props")? {
                deep_merge_map(&mut device.props, props);
            }
            if let Some(slot_state) = optional_object(patch, "slotState")? {
                deep_merge_map(&mut device.slot_state, slot_state);
            }

            // 未知 top-level 增量同样保留并深合并。
            for (key, patch_value) in patch {
                if matches!(key.as_str(), "slotId" | "props" | "slotState") {
                    continue;
                }
                match device.extra.get_mut(key) {
                    Some(target) => deep_merge(target, patch_value),
                    None => {
                        device.extra.insert(key.clone(), patch_value.clone());
                    }
                }
            }
            updated.push(slot_id);
        }

        Ok(DeviceUpdate::SlotsPatched { updated, missing })
    }
}

/// 对 JSON 对象递归合并；对象以外的值（包括数组与 null）整体替换。
pub fn deep_merge(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(target), Value::Object(patch)) => deep_merge_map(target, patch),
        (target, patch) => *target = patch.clone(),
    }
}

fn deep_merge_map(target: &mut Map<String, Value>, patch: &Map<String, Value>) {
    for (key, patch_value) in patch {
        match target.get_mut(key) {
            Some(target_value) => deep_merge(target_value, patch_value),
            None => {
                target.insert(key.clone(), patch_value.clone());
            }
        }
    }
}

/// 一个合法的郊狼 V3 100ms 波形帧：四个频率字节后跟四个波形强度字节。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct V3WaveFrame([u8; 8]);

/// 更明确的别名，供只处理十六进制 wire 格式的调用方使用。
pub type V3HexFrame = V3WaveFrame;

impl V3WaveFrame {
    pub const HEX_LENGTH: usize = 16;

    pub fn new(frequencies: [u8; 4], intensities: [u8; 4]) -> Result<Self, ProtocolError> {
        for (index, frequency) in frequencies.iter().copied().enumerate() {
            if !(WaveSample::MIN_FREQUENCY..=WaveSample::MAX_FREQUENCY).contains(&frequency) {
                return Err(ProtocolError::InvalidFrequency {
                    index,
                    actual: frequency,
                });
            }
        }
        for (index, intensity) in intensities.iter().copied().enumerate() {
            if intensity > WaveSample::MAX_PULSE_INTENSITY {
                return Err(ProtocolError::InvalidPulseIntensity {
                    index,
                    actual: intensity,
                });
            }
        }

        let mut bytes = [0_u8; 8];
        bytes[..4].copy_from_slice(&frequencies);
        bytes[4..].copy_from_slice(&intensities);
        Ok(Self(bytes))
    }

    pub fn from_wave_frame(frame: &WaveFrame) -> Self {
        let mut frequencies = [0_u8; 4];
        let mut intensities = [0_u8; 4];
        for (index, sample) in frame.samples().iter().enumerate() {
            frequencies[index] = sample.frequency();
            intensities[index] = sample.pulse_intensity();
        }

        // WaveFrame 只能由已验证的 WaveSample 构造，因此这里不会失败。
        Self::new(frequencies, intensities).expect("WaveFrame invariants guarantee valid V3 frame")
    }

    pub fn from_hex(hex: &str) -> Result<Self, ProtocolError> {
        let encoded = hex.as_bytes();
        if encoded.len() != Self::HEX_LENGTH {
            return Err(ProtocolError::InvalidHexLength(encoded.len()));
        }

        let mut bytes = [0_u8; 8];
        for (index, byte) in bytes.iter_mut().enumerate() {
            let offset = index * 2;
            let high =
                decode_hex_nibble(encoded[offset]).ok_or(ProtocolError::InvalidHex { index })?;
            let low = decode_hex_nibble(encoded[offset + 1])
                .ok_or(ProtocolError::InvalidHex { index })?;
            *byte = (high << 4) | low;
        }

        let frequencies: [u8; 4] = bytes[..4].try_into().expect("slice has fixed length");
        let intensities: [u8; 4] = bytes[4..].try_into().expect("slice has fixed length");
        Self::new(frequencies, intensities)
    }

    pub const fn bytes(self) -> [u8; 8] {
        self.0
    }

    pub fn to_hex(self) -> String {
        format!(
            "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5], self.0[6], self.0[7]
        )
    }
}

impl From<&WaveFrame> for V3WaveFrame {
    fn from(value: &WaveFrame) -> Self {
        Self::from_wave_frame(value)
    }
}

impl FromStr for V3WaveFrame {
    type Err = ProtocolError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_hex(value)
    }
}

impl fmt::Display for V3WaveFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

impl Serialize for V3WaveFrame {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for V3WaveFrame {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_hex(&value).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationOptions {
    pub priority: u8,
    pub immediate: bool,
}

impl Default for OperationOptions {
    fn default() -> Self {
        Self {
            priority: 1,
            immediate: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClearTarget {
    All,
    Device { slot_id: String },
    Channel { slot_id: String, channel: Channel },
}

/// 构造 Relay 级 ping，不会被转发到 APP。
pub fn build_relay_ping() -> Value {
    json!({ "type": "ping" })
}

/// 构造统一的 Relay `message` + APP RPC 请求。
pub fn build_rpc_message(
    client_id: &str,
    request_id: &str,
    method: &str,
    data: Option<Value>,
) -> Result<Value, ProtocolError> {
    ensure_non_empty(client_id, "clientId")?;
    ensure_non_empty(request_id, "reqId")?;
    ensure_non_empty(method, "m")?;

    let mut app = Map::new();
    app.insert("t".to_owned(), Value::String("req".to_owned()));
    app.insert("reqId".to_owned(), Value::String(request_id.to_owned()));
    app.insert("m".to_owned(), Value::String(method.to_owned()));
    if let Some(data) = data {
        app.insert("data".to_owned(), data);
    }

    Ok(json!({
        "type": "message",
        "clientId": client_id,
        "data": Value::Object(app)
    }))
}

pub fn build_devices_get(client_id: &str, request_id: &str) -> Result<Value, ProtocolError> {
    build_rpc_message(client_id, request_id, "devices.get", None)
}

pub fn build_app_ping(client_id: &str, request_id: &str) -> Result<Value, ProtocolError> {
    build_rpc_message(client_id, request_id, "ping", None)
}

/// 下发调用方提供的波形帧。`duration_ms` 是最长播放时间；本函数不会根据它
/// 自动重复帧，APP 仍会在 `frames` 消费完后结束任务。
pub fn build_append_pulse(
    client_id: &str,
    request_id: &str,
    slot_id: &str,
    channel: Channel,
    duration_ms: u64,
    frames: &[V3WaveFrame],
    options: OperationOptions,
) -> Result<Value, ProtocolError> {
    if frames.is_empty() {
        return Err(ProtocolError::EmptyPulseFrames);
    }
    let mut data = operation_base(slot_id, channel, 0, options)?;
    data.insert("d".to_owned(), Value::from(duration_ms));
    data.insert("ver".to_owned(), Value::from(3));
    data.insert(
        "v".to_owned(),
        Value::Array(
            frames
                .iter()
                .map(|frame| Value::String(frame.to_hex()))
                .collect(),
        ),
    );
    build_rpc_message(
        client_id,
        request_id,
        "device.op",
        Some(Value::Object(data)),
    )
}

pub fn build_add_intensity(
    client_id: &str,
    request_id: &str,
    slot_id: &str,
    channel: Channel,
    delta: i64,
    options: OperationOptions,
) -> Result<Value, ProtocolError> {
    let mut data = operation_base(slot_id, channel, 3, options)?;
    data.insert("v".to_owned(), Value::from(delta));
    build_rpc_message(
        client_id,
        request_id,
        "device.op",
        Some(Value::Object(data)),
    )
}

pub fn build_set_temp_intensity(
    client_id: &str,
    request_id: &str,
    slot_id: &str,
    channel: Channel,
    value: u64,
    duration_ms: u64,
    options: OperationOptions,
) -> Result<Value, ProtocolError> {
    let mut data = operation_base(slot_id, channel, 4, options)?;
    data.insert("v".to_owned(), Value::from(value));
    data.insert("d".to_owned(), Value::from(duration_ms));
    build_rpc_message(
        client_id,
        request_id,
        "device.op",
        Some(Value::Object(data)),
    )
}

pub fn build_set_mute(
    client_id: &str,
    request_id: &str,
    slot_id: &str,
    channel: Channel,
    muted: bool,
    options: OperationOptions,
) -> Result<Value, ProtocolError> {
    let mut data = operation_base(slot_id, channel, 5, options)?;
    data.insert("v".to_owned(), Value::Bool(muted));
    build_rpc_message(
        client_id,
        request_id,
        "device.op",
        Some(Value::Object(data)),
    )
}

/// V4 的 SetIntensity (`t=7`) 只允许归零，因此 API 不接受任意目标值。
pub fn build_reset_intensity(
    client_id: &str,
    request_id: &str,
    slot_id: &str,
    channel: Channel,
    options: OperationOptions,
) -> Result<Value, ProtocolError> {
    let mut data = operation_base(slot_id, channel, 7, options)?;
    data.insert("v".to_owned(), Value::from(0));
    build_rpc_message(
        client_id,
        request_id,
        "device.op",
        Some(Value::Object(data)),
    )
}

pub fn build_clear_operations(
    client_id: &str,
    request_id: &str,
    target: ClearTarget,
) -> Result<Value, ProtocolError> {
    let data = match target {
        ClearTarget::All => None,
        ClearTarget::Device { slot_id } => {
            ensure_non_empty(&slot_id, "s")?;
            Some(json!({ "s": slot_id }))
        }
        ClearTarget::Channel { slot_id, channel } => {
            ensure_non_empty(&slot_id, "s")?;
            Some(json!({ "s": slot_id, "c": channel.as_v4() }))
        }
    };
    build_rpc_message(client_id, request_id, "device.op.clear", data)
}

fn operation_base(
    slot_id: &str,
    channel: Channel,
    action_type: u8,
    options: OperationOptions,
) -> Result<Map<String, Value>, ProtocolError> {
    ensure_non_empty(slot_id, "s")?;
    if options.priority > 2 {
        return Err(ProtocolError::InvalidPriority(options.priority));
    }

    let mut data = Map::new();
    data.insert("s".to_owned(), Value::String(slot_id.to_owned()));
    data.insert("t".to_owned(), Value::from(action_type));
    data.insert("c".to_owned(), Value::from(channel.as_v4()));
    data.insert("p".to_owned(), Value::from(options.priority));
    if options.immediate {
        data.insert("im".to_owned(), Value::Bool(true));
    }
    Ok(data)
}

/// 在 WebSocket URL 上安全写入 `tid`，保留其他查询参数并替换旧 `tid`。
pub fn build_app_socket_url(base_ws_url: &str, target_id: &str) -> Result<Url, ProtocolError> {
    ensure_non_empty(target_id, "tid")?;
    let mut url = Url::parse(base_ws_url)?;
    if !matches!(url.scheme(), "ws" | "wss") {
        return Err(ProtocolError::InvalidWebSocketScheme);
    }

    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "tid")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.set_query(None);
    {
        let mut query = url.query_pairs_mut();
        for (key, value) in retained {
            query.append_pair(&key, &value);
        }
        query.append_pair("tid", target_id);
    }
    Ok(url)
}

/// 构造 DG-LAB APP 可扫描的二维码 URL；嵌套 WebSocket URL 由 `url` crate
/// 进行 percent-encode，不做易错的手工字符串拼接。
pub fn build_pairing_url(base_ws_url: &str, target_id: &str) -> Result<Url, ProtocolError> {
    let app_socket_url = build_app_socket_url(base_ws_url, target_id)?;
    let mut pairing_url = Url::parse(APP_LINK_URL)?;
    pairing_url
        .query_pairs_mut()
        .append_pair("v", "1")
        .append_pair("action", "socket")
        .append_pair("url", app_socket_url.as_str());
    Ok(pairing_url)
}

pub fn app_socket_url(base_ws_url: &str, target_id: &str) -> Result<String, ProtocolError> {
    Ok(build_app_socket_url(base_ws_url, target_id)?.into())
}

pub fn pairing_url(base_ws_url: &str, target_id: &str) -> Result<String, ProtocolError> {
    Ok(build_pairing_url(base_ws_url, target_id)?.into())
}

fn ensure_non_empty(value: &str, field: &'static str) -> Result<(), ProtocolError> {
    if value.is_empty() {
        Err(ProtocolError::EmptyIdentifier { field })
    } else {
        Ok(())
    }
}

fn decode_hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn required_string(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<String, ProtocolError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(ProtocolError::InvalidField { field })
}

fn optional_string(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<String>, ProtocolError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(ProtocolError::InvalidField { field }),
    }
}

fn optional_i64(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<i64>, ProtocolError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_i64()
            .map(Some)
            .ok_or(ProtocolError::InvalidField { field }),
        Some(_) => Err(ProtocolError::InvalidField { field }),
    }
}

fn optional_object<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
) -> Result<Option<&'a Map<String, Value>>, ProtocolError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(value)) => Ok(Some(value)),
        Some(_) => Err(ProtocolError::InvalidField { field }),
    }
}

fn parse_devices(
    devices: Option<&Value>,
    field: &'static str,
) -> Result<Vec<RemoteDevice>, ProtocolError> {
    let devices = devices.ok_or(ProtocolError::InvalidField { field })?;
    serde_json::from_value(devices.clone()).map_err(ProtocolError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device_snapshot() -> Value {
        json!({
            "t": "ev",
            "ev": "devices.snapshot",
            "futureEventField": { "ignored": true },
            "devices": [{
                "id": 0,
                "slotId": "slot-a",
                "name": "郊狼",
                "type": "COYOTE_030",
                "futureDeviceField": { "kept": true },
                "props": {
                    "power": 87,
                    "intensityA": 11,
                    "intensityB": 7
                },
                "slotState": {
                    "markLight": "green",
                    "hasDevice": true,
                    "channelA": {
                        "isMuted": false,
                        "intensityMax": 100
                    }
                }
            }]
        })
    }

    #[test]
    fn relay_parser_accepts_known_frames_with_extra_fields() {
        let frame =
            parse_relay_text(r#"{"type":"hello","clientId":"a1b2c3d4","future":true}"#).unwrap();
        assert_eq!(
            frame,
            RelayFrame::Hello {
                client_id: "a1b2c3d4".to_owned()
            }
        );
    }

    #[test]
    fn relay_parser_preserves_unknown_frame_instead_of_failing() {
        let raw = json!({ "type": "future.frame", "payload": [1, 2, 3] });
        let frame = parse_relay_frame(&raw).unwrap();
        assert_eq!(
            frame,
            RelayFrame::Unknown {
                frame_type: Some("future.frame".to_owned()),
                raw
            }
        );
    }

    #[test]
    fn response_requires_exactly_one_result_or_error() {
        assert!(matches!(
            parse_app_message(&json!({ "t": "resp", "reqId": "1" })),
            Err(ProtocolError::InvalidResponse)
        ));
        assert!(matches!(
            parse_app_message(&json!({
                "t": "resp",
                "reqId": "1",
                "result": {},
                "error": "bad"
            })),
            Err(ProtocolError::InvalidResponse)
        ));
    }

    #[test]
    fn device_snapshot_replaces_store_and_preserves_unknown_device_fields() {
        let mut store = DeviceStore::new();
        assert_eq!(
            store.apply_app_message(&device_snapshot()).unwrap(),
            DeviceUpdate::Snapshot { count: 1 }
        );
        let device = store.get("slot-a").unwrap();
        assert_eq!(device.extra["futureDeviceField"]["kept"], true);

        store
            .apply_app_message(&json!({
                "t": "ev",
                "ev": "devices.snapshot",
                "devices": []
            }))
            .unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn slots_patch_deep_merges_nested_state() {
        let mut store = DeviceStore::new();
        store.apply_app_message(&device_snapshot()).unwrap();

        let update = store
            .apply_app_message(&json!({
                "t": "ev",
                "ev": "slots.patch",
                "slots": [{
                    "slotId": "slot-a",
                    "props": { "intensityA": 12 },
                    "slotState": { "channelA": { "isMuted": true } },
                    "futureDeviceField": { "revision": 2 }
                }]
            }))
            .unwrap();

        assert_eq!(
            update,
            DeviceUpdate::SlotsPatched {
                updated: vec!["slot-a".to_owned()],
                missing: vec![]
            }
        );
        let device = store.get("slot-a").unwrap();
        assert_eq!(device.props["power"], 87);
        assert_eq!(device.props["intensityA"], 12);
        assert_eq!(device.props["intensityB"], 7);
        assert_eq!(device.slot_state["channelA"]["isMuted"], true);
        assert_eq!(device.slot_state["channelA"]["intensityMax"], 100);
        assert_eq!(device.extra["futureDeviceField"]["kept"], true);
        assert_eq!(device.extra["futureDeviceField"]["revision"], 2);
    }

    #[test]
    fn devices_patch_adds_replaces_and_removes_by_slot_id() {
        let mut store = DeviceStore::new();
        store.apply_app_message(&device_snapshot()).unwrap();

        let update = store
            .apply_app_message(&json!({
                "t": "ev",
                "ev": "devices.patch",
                "removed": ["slot-a"],
                "added": [{
                    "slotId": "slot-b",
                    "name": "新设备",
                    "type": "FUTURE_DEVICE",
                    "props": {},
                    "slotState": {}
                }]
            }))
            .unwrap();

        assert!(store.get("slot-a").is_none());
        assert_eq!(store.get("slot-b").unwrap().device_type, "FUTURE_DEVICE");
        assert_eq!(
            update,
            DeviceUpdate::DevicesPatched {
                added: vec!["slot-b".to_owned()],
                removed: vec!["slot-a".to_owned()]
            }
        );
    }

    #[test]
    fn v3_hex_round_trip_is_exact_and_uppercase() {
        let frame = V3WaveFrame::from_hex("0a141e2864645a50").unwrap();
        assert_eq!(frame.to_hex(), "0A141E2864645A50");
        assert_eq!(
            serde_json::to_string(&frame).unwrap(),
            r#""0A141E2864645A50""#
        );
        assert_eq!(
            serde_json::from_str::<V3WaveFrame>(r#""0A141E2864645A50""#).unwrap(),
            frame
        );
    }

    #[test]
    fn v3_hex_rejects_wrong_length_invalid_hex_and_invalid_ranges() {
        assert!(matches!(
            V3WaveFrame::from_hex("ABC"),
            Err(ProtocolError::InvalidHexLength(3))
        ));
        assert!(matches!(
            V3WaveFrame::from_hex("0A0A0A0A0000000Z"),
            Err(ProtocolError::InvalidHex { index: 7 })
        ));
        assert!(matches!(
            V3WaveFrame::from_hex("中文中文中文中文"),
            Err(ProtocolError::InvalidHexLength(_))
        ));
        assert!(matches!(
            V3WaveFrame::from_hex("090A0A0A00000000"),
            Err(ProtocolError::InvalidFrequency {
                index: 0,
                actual: 9
            })
        ));
        assert!(matches!(
            V3WaveFrame::from_hex("0A0A0A0A00000065"),
            Err(ProtocolError::InvalidPulseIntensity {
                index: 3,
                actual: 101
            })
        ));
    }

    #[test]
    fn append_pulse_serializes_each_frame_once_even_if_duration_is_longer() {
        let frame = V3WaveFrame::from_hex("0A0A0A0A00000000").unwrap();
        let message = build_append_pulse(
            "app00001",
            "pulse-1",
            "slot-a",
            Channel::A,
            1000,
            &[frame],
            OperationOptions::default(),
        )
        .unwrap();

        assert_eq!(
            message,
            json!({
                "type": "message",
                "clientId": "app00001",
                "data": {
                    "t": "req",
                    "reqId": "pulse-1",
                    "m": "device.op",
                    "data": {
                        "s": "slot-a",
                        "t": 0,
                        "c": 0,
                        "p": 1,
                        "d": 1000,
                        "ver": 3,
                        "v": ["0A0A0A0A00000000"]
                    }
                }
            })
        );
        assert_eq!(message["data"]["data"]["v"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn reset_intensity_can_only_build_t7_v0() {
        let message = build_reset_intensity(
            "app00001",
            "req-4",
            "slot-a",
            Channel::B,
            OperationOptions::default(),
        )
        .unwrap();
        assert_eq!(message["data"]["data"]["t"], 7);
        assert_eq!(message["data"]["data"]["v"], 0);
        assert_eq!(message["data"]["data"]["c"], 1);
    }

    #[test]
    fn clear_channel_always_includes_slot_id() {
        let message = build_clear_operations(
            "app00001",
            "clear-a",
            ClearTarget::Channel {
                slot_id: "slot-a".to_owned(),
                channel: Channel::A,
            },
        )
        .unwrap();
        assert_eq!(message["data"]["data"], json!({ "s": "slot-a", "c": 0 }));
    }

    #[test]
    fn pairing_url_uses_tid_and_percent_encodes_nested_websocket_url() {
        let app_url = app_socket_url("wss://relay.example/v4?region=cn", "id +/中文").unwrap();
        assert_eq!(
            app_url,
            "wss://relay.example/v4?region=cn&tid=id+%2B%2F%E4%B8%AD%E6%96%87"
        );

        let pairing = pairing_url("wss://relay.example/v4", "a1b2c3d4").unwrap();
        assert_eq!(
            pairing,
            "https://dungeon-lab.cn/s/?v=1&action=socket&url=wss%3A%2F%2Frelay.example%2Fv4%3Ftid%3Da1b2c3d4"
        );

        let official = pairing_url(DEFAULT_RELAY_URL, "a1b2c3d4").unwrap();
        assert_eq!(
            official,
            "https://dungeon-lab.cn/s/?v=1&action=socket&url=wss%3A%2F%2Ftrex.dungeon-lab.cn%2Fv4%3Ftid%3Da1b2c3d4"
        );
    }

    #[test]
    fn existing_tid_is_replaced_not_duplicated() {
        let url = build_app_socket_url("wss://relay.example/v4?tid=old&x=1", "new").unwrap();
        assert_eq!(url.as_str(), "wss://relay.example/v4?x=1&tid=new");
    }
}
