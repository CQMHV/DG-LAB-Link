//! Socket V4 wire encoding, isolated from the shared Hub.
use crate::model::{Channel, WaveFrame};
use serde_json::{Value, json};
use uuid::Uuid;

pub(crate) fn devices_get_request() -> Value {
    devices_get_request_with_id(&Uuid::new_v4().to_string())
}

pub(crate) fn devices_get_request_with_id(request_id: &str) -> Value {
    json!({
        "t": "req",
        "reqId": request_id,
        "m": "devices.get",
    })
}

pub(crate) fn append_pulse_request(
    request_id: &str,
    slot_id: &str,
    channel: Channel,
    frame_hex: &str,
) -> Value {
    json!({
        "t": "req",
        "reqId": request_id,
        "m": "device.op",
        "data": {
            "s": slot_id,
            "t": 0,
            "c": channel.as_v4(),
            "p": 1,
            "d": 100,
            "ver": 3,
            "v": [frame_hex],
        },
    })
}

pub(crate) fn add_intensity_request(
    request_id: &str,
    slot_id: &str,
    channel: Channel,
    delta: i32,
) -> Value {
    json!({
        "t": "req",
        "reqId": request_id,
        "m": "device.op",
        "data": {
            "s": slot_id,
            "t": 3,
            "c": channel.as_v4(),
            "p": 1,
            "v": delta,
        },
    })
}

pub(crate) fn clear_request(slot_id: &str) -> Value {
    json!({
        "t": "req",
        "reqId": Uuid::new_v4().to_string(),
        "m": "device.op.clear",
        "data": { "s": slot_id },
    })
}

pub(crate) fn clear_channel_request(slot_id: &str, channel: Channel) -> Value {
    json!({
        "t": "req",
        "reqId": Uuid::new_v4().to_string(),
        "m": "device.op.clear",
        "data": { "s": slot_id, "c": channel.as_v4() },
    })
}

pub(crate) fn zero_intensity_request(slot_id: &str, channel: Channel) -> Value {
    json!({
        "t": "req",
        "reqId": Uuid::new_v4().to_string(),
        "m": "device.op",
        "data": {
            "s": slot_id,
            "t": 7,
            "c": channel.as_v4(),
            "p": 1,
            "v": 0,
        },
    })
}

pub(crate) fn stop_operation_requests(slot_id: &str, emergency: bool) -> Vec<Value> {
    let mut requests = vec![clear_request(slot_id)];
    if emergency {
        requests.extend(Channel::ALL.map(|channel| zero_intensity_request(slot_id, channel)));
    }
    requests
}

pub(crate) fn encode_wave_frame(frame: WaveFrame) -> String {
    let mut encoded = String::with_capacity(16);
    for sample in frame.samples() {
        encoded.push_str(&format!("{:02X}", sample.frequency()));
    }
    for sample in frame.samples() {
        encoded.push_str(&format!("{:02X}", sample.pulse_intensity()));
    }
    encoded
}
