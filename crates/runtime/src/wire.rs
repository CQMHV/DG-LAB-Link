use dg_lab_link_core::control::{ControlCommand, ControlError};
use dg_lab_link_core::hub::HubSnapshot;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HolderInfo {
    pub id: String,
    pub label: String,
    pub pid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInfo {
    pub instance_id: String,
    pub pid: u32,
    pub holder_count: usize,
    pub mcp_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Request {
    pub id: u64,
    pub operation: Operation,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "params", rename_all = "snake_case")]
pub(crate) enum Operation {
    Hello(HolderInfo),
    Call(ControlCommand),
    RuntimeInfo,
    Holders,
    ReleaseHolder { id: String },
    Release,
    Heartbeat,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Response {
    Hello {
        holder: HolderInfo,
        runtime: RuntimeInfo,
        snapshot: Box<HubSnapshot>,
    },
    Result {
        id: u64,
        result: Option<Value>,
        error: Option<ControlError>,
    },
    Snapshot {
        snapshot: Box<HubSnapshot>,
    },
}

impl Response {
    pub fn result(id: u64, result: Result<Value, ControlError>) -> Self {
        match result {
            Ok(value) => Self::Result {
                id,
                result: Some(value),
                error: None,
            },
            Err(error) => Self::Result {
                id,
                result: None,
                error: Some(error),
            },
        }
    }
}
