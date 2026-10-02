use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppPreferencesSnapshot {
    pub close_to_tray: bool,
    pub auto_start: bool,
    pub start_minimized: bool,
}
