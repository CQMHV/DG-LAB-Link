use crate::hub::{ChannelStatus, ConnectionState};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const V4_CONNECTION_ID: &str = "ws-v4";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    WsV4,
    WsV3,
    Ble,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeviceAddress {
    pub connection_id: String,
    pub client_id: String,
    pub slot_id: String,
}
impl DeviceAddress {
    pub fn control_id(&self) -> String {
        let legacy = format!(
            "{}:{}{}",
            self.client_id.len(),
            self.client_id,
            self.slot_id
        );
        if self.connection_id == V4_CONNECTION_ID {
            legacy
        } else {
            format!("{}:{legacy}", self.connection_id)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransportConnectionSnapshot {
    pub connection_id: String,
    pub transport: TransportKind,
    pub state: ConnectionState,
    pub endpoint: String,
    pub controller_id: Option<String>,
    pub pairing_url: Option<String>,
    pub app_count: usize,
    pub last_error: Option<String>,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum InitializationState {
    Initializing,
    #[default]
    Ready,
    Fault,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct DeviceCapabilities {
    pub battery: bool,
    pub load_status: bool,
    pub soft_limits: bool,
    pub balance: bool,
    pub wheel_protection: bool,
    pub standard_mode: bool,
    pub operation_confirmation: bool,
}
impl Default for DeviceCapabilities {
    fn default() -> Self {
        Self {
            battery: true,
            load_status: true,
            soft_limits: false,
            balance: false,
            wheel_protection: false,
            standard_mode: false,
            operation_confirmation: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct BleParameters {
    pub max_strength_a: u16,
    pub max_strength_b: u16,
    pub frequency_balance_a: u8,
    pub frequency_balance_b: u8,
    pub strength_balance_a: u8,
    pub strength_balance_b: u8,
    pub wheel_protection_enabled: bool,
    pub wheel_protection_value: u8,
}
impl Default for BleParameters {
    fn default() -> Self {
        Self {
            max_strength_a: 100,
            max_strength_b: 100,
            frequency_balance_a: 160,
            frequency_balance_b: 160,
            strength_balance_a: 0,
            strength_balance_b: 0,
            wheel_protection_enabled: true,
            wheel_protection_value: 10,
        }
    }
}
impl BleParameters {
    pub fn validate(&self) -> Result<(), TransportError> {
        if self.max_strength_a > 200
            || self.max_strength_b > 200
            || !(1..=50).contains(&self.wheel_protection_value)
        {
            return Err(TransportError::new(
                "invalid_ble_parameters",
                "BLE 上限须为 0..200，旋钮保护值须为 1..50",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BluetoothDevice {
    pub device_id: String,
    pub name: String,
    pub rssi: Option<i16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDevice {
    pub id: String,
    pub slot_id: String,
    pub name: String,
    pub device_type: String,
    pub power: Option<u16>,
    pub intensity_a: u16,
    pub intensity_b: u16,
    pub intensity_limit_a: u16,
    pub intensity_limit_b: u16,
    pub channel_a_status: ChannelStatus,
    pub channel_b_status: ChannelStatus,
    pub initialization: InitializationState,
    pub capabilities: DeviceCapabilities,
    pub ble_parameters: Option<BleParameters>,
    /// BF has no device acknowledgement: "sent" differs from "confirmed".
    pub configuration_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct TransportError {
    pub code: String,
    pub message: String,
}
impl TransportError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
    pub fn stopped() -> Self {
        Self::new("transport_stopped", "设备连接已停止")
    }
    pub fn busy() -> Self {
        Self::new("queue_busy", "设备队列繁忙或操作已被停止取代")
    }
}
