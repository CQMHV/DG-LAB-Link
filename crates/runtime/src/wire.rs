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
    #[serde(default)]
    pub command_epoch: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "params", rename_all = "snake_case")]
pub(crate) enum Operation {
    Hello(HolderInfo),
    Observe(HolderInfo),
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
        #[serde(rename = "commandEpoch")]
        command_epoch: u64,
    },
    Result {
        id: u64,
        result: Option<Value>,
        error: Option<ControlError>,
        #[serde(rename = "commandEpoch")]
        command_epoch: u64,
    },
    Snapshot {
        snapshot: Box<HubSnapshot>,
    },
    CommandEpoch {
        epoch: u64,
    },
}

impl Response {
    pub fn result(id: u64, result: Result<Value, ControlError>) -> Self {
        match result {
            Ok(value) => Self::Result {
                id,
                result: Some(value),
                error: None,
                command_epoch: 0,
            },
            Err(error) => Self::Result {
                id,
                result: None,
                error: Some(error),
                command_epoch: 0,
            },
        }
    }
}
