use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 2;
pub const MAX_BINDINGS: usize = 64;
pub const FRAME_INTERVAL_MS: u64 = 100;
pub const MAX_INSTANCES: usize = 32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub id: String,
    pub version: String,
    pub protocol_version: u32,
    pub name: String,
    pub publisher: String,
    pub license: String,
    pub executable: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPlugin {
    pub manifest: PluginManifest,
    pub digest: String,
    pub preinstalled: bool,
    #[serde(skip)]
    pub directory: std::path::PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceSpec {
    pub id: String,
    pub plugin_id: String,
    pub name: String,
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    #[serde(default = "empty_object")]
    pub config: Value,
}

const fn enabled_default() -> bool {
    true
}

pub fn empty_object() -> Value {
    serde_json::json!({})
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    A,
    B,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    pub binding_id: String,
    pub control_id: String,
    pub channel: Channel,
    pub generation: u64,
    #[serde(default = "empty_object")]
    pub config: Value,
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    pub frequency: u8,
    pub pulse_intensity: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Frame {
    pub samples: [Sample; 4],
}

impl Frame {
    pub const fn silent() -> Self {
        Self {
            samples: [Sample {
                frequency: 10,
                pulse_intensity: 0,
            }; 4],
        }
    }

    pub fn validate(&self) -> Result<(), PluginError> {
        if self
            .samples
            .iter()
            .any(|sample| !(10..=240).contains(&sample.frequency) || sample.pulse_intensity > 100)
        {
            return Err(PluginError::new("invalid_frame", "波形采样值超出有效范围"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameNotification {
    pub binding_id: String,
    pub generation: u64,
    pub sequence: u64,
    pub frame: Frame,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: u32,
    pub source: SourceSpec,
    pub data_directory: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigureParams {
    pub config: Value,
    #[serde(default)]
    pub validate_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrateParams {
    pub from_version: String,
    pub config: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionParams {
    pub action: String,
    #[serde(default = "empty_object")]
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InputParams {
    pub action: String,
    #[serde(default = "empty_object")]
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<String>,
    pub owner: String,
    pub sequence: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum UiSurface {
    #[default]
    Settings,
    Control,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UiParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<String>,
    #[serde(default)]
    pub surface: UiSurface,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionDescriptor {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "empty_object")]
    pub params_schema: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UiNodeKind {
    Page,
    Section,
    Stack,
    Group,
    Form,
    List,
    Text,
    Status,
    KeyValue,
    Progress,
    Divider,
    Button,
    Switch,
    TextField,
    IntegerField,
    NumberField,
    Select,
    Slider,
    XyPad,
    Grid,
    AudioPlayer,
    Meter,
    Curve,
    WaveformPicker,
    FileField,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UiNode {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: UiNodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(default = "empty_object")]
    pub props: Value,
    #[serde(default)]
    pub children: Vec<UiNode>,
}

impl UiNode {
    pub fn new(id: impl Into<String>, kind: UiNodeKind) -> Self {
        Self {
            id: id.into(),
            kind,
            label: None,
            value: None,
            config_key: None,
            action: None,
            input: None,
            props: empty_object(),
            children: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UiDocument {
    pub title: String,
    #[serde(default)]
    pub nodes: Vec<UiNode>,
    #[serde(default)]
    pub actions: Vec<ActionDescriptor>,
    #[serde(default)]
    pub revision: u64,
}

impl UiDocument {
    /// Reject documents too deeply nested or too large for a bounded renderer.
    pub fn validate(&self) -> Result<(), PluginError> {
        fn visit(
            nodes: &[UiNode],
            depth: usize,
            count: &mut usize,
            ids: &mut std::collections::HashSet<String>,
        ) -> Result<(), PluginError> {
            if depth > 16 {
                return Err(PluginError::new("invalid_ui", "界面层级超过 16 层"));
            }
            for node in nodes {
                *count += 1;
                if *count > 512
                    || node.id.is_empty()
                    || node.id.len() > 128
                    || !ids.insert(node.id.clone())
                {
                    return Err(PluginError::new("invalid_ui", "界面节点数量或标识无效"));
                }
                visit(&node.children, depth + 1, count, ids)?;
            }
            Ok(())
        }
        let mut count = 0;
        let mut ids = std::collections::HashSet::new();
        visit(&self.nodes, 0, &mut count, &mut ids)?;
        let mut actions = std::collections::HashSet::new();
        if self.actions.len() > 128
            || self.actions.iter().any(|action| {
                action.id.is_empty() || action.id.len() > 128 || !actions.insert(action.id.clone())
            })
        {
            return Err(PluginError::new("invalid_ui", "界面动作数量或标识无效"));
        }
        Ok(())
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, thiserror::Error,
)]
#[error("{message}")]
pub struct PluginError {
    pub code: String,
    pub message: String,
}

impl PluginError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
