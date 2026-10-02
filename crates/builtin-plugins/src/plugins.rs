use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use dg_lab_link_plugin_sdk::{
    ActionDescriptor, ActionParams, Binding, ConfigureParams, Frame, InitializeParams, InputParams,
    Plugin, PluginContext, PluginError, Sample, UiDocument, UiNode, UiNodeKind, UiParams,
    UiSurface, async_trait,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::model::{Channel, WaveFrame};
use crate::sources::audio::{AudioAction, AudioChannelConfig, AudioEngine, AudioMappingRuntime};
use crate::sources::touch::{TouchConfig, TouchFactory, TouchInput, TouchRuntime};
use crate::sources::{SourceError, SourceFactory};

pub const TOUCH_PLUGIN_ID: &str = "cn.dglab.link.touch";
pub const AUDIO_PLUGIN_ID: &str = "cn.dglab.link.audio";
const MAX_CONCURRENT_TOUCH_CLEARS: usize = 8;

type TouchIntent = Option<(i64, Option<usize>)>;

struct PendingTouchClear {
    generation: u64,
    response: Option<tokio::sync::oneshot::Receiver<Result<Value, PluginError>>>,
}

fn source_error(error: SourceError) -> PluginError {
    let code = match error {
        SourceError::InvalidConfig { .. } => "invalid_config",
        SourceError::Runtime(_) => "source_runtime",
    };
    PluginError::new(code, error.to_string())
}

fn parse<T: DeserializeOwned>(value: Value) -> Result<T, PluginError> {
    serde_json::from_value(value)
        .map_err(|error| PluginError::new("invalid_params", error.to_string()))
}

fn channel(channel: dg_lab_link_plugin_sdk::Channel) -> Channel {
    match channel {
        dg_lab_link_plugin_sdk::Channel::A => Channel::A,
        dg_lab_link_plugin_sdk::Channel::B => Channel::B,
    }
}

fn frame(frame: WaveFrame) -> Frame {
    Frame {
        samples: frame.into_samples().map(|sample| Sample {
            frequency: sample.frequency(),
            pulse_intensity: sample.pulse_intensity(),
        }),
    }
}

fn action<T: schemars::JsonSchema>(id: &str, label: &str) -> ActionDescriptor {
    ActionDescriptor {
        id: id.to_owned(),
        label: label.to_owned(),
        description: String::new(),
        params_schema: serde_json::to_value(schemars::schema_for!(T)).expect("schema serializes"),
    }
}

fn node(id: &str, kind: UiNodeKind, label: &str) -> UiNode {
    let mut node = UiNode::new(id, kind);
    node.label = Some(label.to_owned());
    node
}

fn field(config: &Value, id: &str, kind: UiNodeKind, label: &str, props: Value) -> UiNode {
    let mut node = node(id, kind, label);
    node.config_key = Some(id.to_owned());
    node.value = config.get(id).cloned();
    node.props = props;
    node
}

fn binding<'a>(bindings: &'a [Binding], id: &str) -> Result<&'a Binding, PluginError> {
    bindings
        .iter()
        .find(|binding| binding.binding_id == id)
        .ok_or_else(|| PluginError::new("unknown_binding", "输入源通道绑定已失效"))
}

#[derive(Default)]
pub struct TouchPlugin {
    config: TouchConfig,
    bindings: Vec<Binding>,
    runtimes: HashMap<String, TouchRuntime>,
    intents: HashMap<String, TouchIntent>,
    pending_clears: HashMap<String, PendingTouchClear>,
    clear_errors: BTreeMap<String, String>,
}

impl TouchPlugin {
    pub fn state(&self) -> Value {
        json!({"touchConfig": self.config,"inputErrors":self.clear_errors})
    }

    fn validate_config(config: &Value) -> Result<TouchConfig, PluginError> {
        TouchFactory.validate(config).map_err(source_error)?;
        parse(config.clone())
    }

    fn set_config(&mut self, config: Value, validate_only: bool) -> Result<Value, PluginError> {
        let config = Self::validate_config(&config)?;
        if !validate_only {
            // Construct every replacement before changing any current runtime.
            let runtimes = self
                .runtimes
                .keys()
                .map(|id| {
                    TouchRuntime::new(config.clone())
                        .map(|runtime| (id.clone(), runtime))
                        .map_err(source_error)
                })
                .collect::<Result<HashMap<_, _>, _>>()?;
            self.config = config;
            self.runtimes = runtimes;
        }
        Ok(self.state())
    }

    fn update_input(&mut self, params: InputParams) -> Result<Value, PluginError> {
        let id = params
            .binding_id
            .ok_or_else(|| PluginError::new("invalid_params", "触控输入必须指定 bindingId"))?;
        let selected = binding(&self.bindings, &id)?;
        let mut pointers: Vec<crate::sources::touch::TouchPointer> =
            parse(params.value.get("pointers").cloned().unwrap_or(json!([])))?;
        for pointer in &mut pointers {
            if pointer.channel.is_none() {
                pointer.channel = Some(channel(selected.channel));
            }
        }
        let input = TouchInput {
            device_id: selected.control_id.clone(),
            owner_id: params.owner,
            sequence: params.sequence,
            pointers,
        };
        self.runtimes
            .get_mut(&id)
            .ok_or_else(|| PluginError::new("unknown_binding", "触控通道绑定已失效"))?
            .update(&input, Instant::now())
            .map_err(source_error)?;
        Ok(self.state())
    }

    fn sync_bindings(&mut self, bindings: Vec<Binding>) -> Result<Value, PluginError> {
        let ids: HashSet<_> = bindings
            .iter()
            .map(|item| item.binding_id.clone())
            .collect();
        for id in &ids {
            if !self.runtimes.contains_key(id) {
                self.runtimes.insert(
                    id.clone(),
                    TouchRuntime::new(self.config.clone()).map_err(source_error)?,
                );
            }
        }
        for previous in &self.bindings {
            let current = bindings
                .iter()
                .find(|item| item.binding_id == previous.binding_id);
            let own_clear = self
                .pending_clears
                .get(&previous.binding_id)
                .is_some_and(|pending| {
                    current.is_some_and(|item| {
                        previous.active && item.active && item.generation > pending.generation
                    })
                });
            if current.is_none_or(|item| {
                item.active != previous.active
                    || (item.generation != previous.generation && !own_clear)
            }) {
                if let Some(runtime) = self.runtimes.get_mut(&previous.binding_id) {
                    runtime.reset_channel(channel(previous.channel));
                }
                self.intents.remove(&previous.binding_id);
                self.pending_clears.remove(&previous.binding_id);
                self.clear_errors.remove(&previous.binding_id);
            }
        }
        self.runtimes.retain(|id, _| ids.contains(id));
        self.intents.retain(|id, _| ids.contains(id));
        self.pending_clears.retain(|id, _| ids.contains(id));
        self.clear_errors.retain(|id, _| ids.contains(id));
        self.bindings = bindings;
        Ok(self.state())
    }

    fn service_clears(&mut self, context: &PluginContext, now: Instant) -> Result<(), PluginError> {
        for binding in &self.bindings {
            let intent = self
                .runtimes
                .get(&binding.binding_id)
                .map(|runtime| {
                    runtime.touch_intents(now)[channel(binding.channel).as_v4() as usize]
                })
                .unwrap_or(None);
            let previous = self
                .intents
                .insert(binding.binding_id.clone(), intent)
                .flatten();
            if binding.active
                && previous.is_some()
                && previous != intent
                && !self.clear_errors.contains_key(&binding.binding_id)
            {
                self.pending_clears
                    .entry(binding.binding_id.clone())
                    .or_insert(PendingTouchClear {
                        generation: binding.generation,
                        response: None,
                    });
            }
        }

        let mut completed = Vec::new();
        for (id, pending) in &mut self.pending_clears {
            let Some(response) = &mut pending.response else {
                continue;
            };
            match response.try_recv() {
                Ok(result) => completed.push((id.clone(), result)),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => completed.push((
                    id.clone(),
                    Err(PluginError::new(
                        "plugin_disconnected",
                        "通道清理请求已中断",
                    )),
                )),
            }
        }
        let mut errors_changed = false;
        for (id, result) in completed {
            let pending = self
                .pending_clears
                .remove(&id)
                .expect("pending clear exists");
            match result.and_then(|value| {
                let generation = value
                    .get("generation")
                    .and_then(Value::as_u64)
                    .filter(|generation| *generation > pending.generation);
                if value.get("bindingId").and_then(Value::as_str) != Some(id.as_str())
                    || generation.is_none()
                {
                    return Err(PluginError::new(
                        "invalid_response",
                        "通道清理未返回有效绑定代次",
                    ));
                }
                Ok(generation.expect("checked generation"))
            }) {
                Ok(generation) => {
                    if let Some(binding) = self
                        .bindings
                        .iter_mut()
                        .find(|binding| binding.binding_id == id)
                    {
                        binding.generation = binding.generation.max(generation);
                    }
                }
                Err(error) => {
                    self.clear_errors.insert(id, error.message);
                    errors_changed = true;
                }
            }
        }

        let mut available = MAX_CONCURRENT_TOUCH_CLEARS.saturating_sub(
            self.pending_clears
                .values()
                .filter(|pending| pending.response.is_some())
                .count(),
        );
        for (id, pending) in &mut self.pending_clears {
            if available == 0 {
                break;
            }
            if pending.response.is_some() {
                continue;
            }
            let Some(binding) = self
                .bindings
                .iter()
                .find(|binding| binding.binding_id == *id && binding.active)
            else {
                continue;
            };
            let command = json!({"command":"clear_device_channel","params":{
                "deviceId":binding.control_id,"channel":binding.channel
            }});
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let context = context.clone();
            tokio::spawn(async move {
                let _ = sender.send(context.business_call(command).await);
            });
            pending.response = Some(receiver);
            available -= 1;
        }
        if errors_changed {
            context.status(self.state())?;
        }
        Ok(())
    }

    fn document(&self, params: UiParams) -> Result<UiDocument, PluginError> {
        let actions = vec![action::<TouchConfig>("configure", "设置触控参数")];
        if matches!(params.surface, UiSurface::Control) {
            let selected = params
                .binding_id
                .as_deref()
                .map(|id| binding(&self.bindings, id))
                .transpose()?;
            let is_grid = matches!(self.config.mode, crate::sources::touch::TouchMode::Rhythm);
            let mut pad = node(
                "touch-input",
                if is_grid {
                    UiNodeKind::Grid
                } else {
                    UiNodeKind::XyPad
                },
                if is_grid {
                    "律动网格"
                } else {
                    "自由触控"
                },
            );
            pad.input = Some("update_touch_input".to_owned());
            pad.props = json!({
                "gridSize": self.config.grid_size,
                "columns": self.config.grid_size,
                "rows": self.config.grid_size,
                "swapAxes": self.config.swap_axes,
                "channel": selected.map(|item| item.channel),
                "leaseMs": 1000,
                "touchConfig": self.config,
                "labels": self.config.rhythm_waveforms.iter().map(|wave| &wave.preset_name).collect::<Vec<_>>(),
            });
            return Ok(UiDocument {
                title: "触控".to_owned(),
                nodes: vec![pad],
                actions,
                revision: 0,
            });
        }
        let value = serde_json::to_value(&self.config).expect("touch config serializes");
        let mut form = node("touch-config", UiNodeKind::Form, "触控设置");
        form.action = Some("configure".to_owned());
        form.children = vec![
            field(
                &value,
                "mode",
                UiNodeKind::Select,
                "模式",
                json!({"options":[{"value":"free","label":"自由"},{"value":"rhythm","label":"律动"}]}),
            ),
            field(
                &value,
                "routing",
                UiNodeKind::Select,
                "通道分配",
                json!({"options":[{"value":"a","label":"A"},{"value":"b","label":"B"},{"value":"sync","label":"同步"},{"value":"separate","label":"独立"},{"value":"alternate","label":"交替"}]}),
            ),
            field(
                &value,
                "gridSize",
                UiNodeKind::IntegerField,
                "网格边长",
                json!({"min":2,"max":4}),
            ),
            field(
                &value,
                "swapAxes",
                UiNodeKind::Switch,
                "交换坐标轴",
                json!({}),
            ),
            field(
                &value,
                "intensityMode",
                UiNodeKind::Select,
                "强度映射",
                json!({"options":[{"value":"classic","label":"经典"},{"value":"gradient","label":"渐变"}]}),
            ),
            field(
                &value,
                "gradientDirection",
                UiNodeKind::Select,
                "渐变方向",
                json!({"options":[{"value":"left","label":"向左"},{"value":"right","label":"向右"},{"value":"both","label":"双向"}]}),
            ),
            field(
                &value,
                "intensityCurve",
                UiNodeKind::Curve,
                "强度曲线",
                json!({"min":0,"max":100}),
            ),
            field(
                &value,
                "periodCurve",
                UiNodeKind::Curve,
                "周期曲线",
                json!({"min":10,"max":100}),
            ),
            field(
                &value,
                "freeWaveforms",
                UiNodeKind::WaveformPicker,
                "自由网格波形",
                json!({"multiple":true,"count":8}),
            ),
            field(
                &value,
                "rhythmWaveforms",
                UiNodeKind::WaveformPicker,
                "律动网格波形",
                json!({"multiple":true,"count":16}),
            ),
            field(
                &value,
                "background",
                UiNodeKind::WaveformPicker,
                "背景波形",
                json!({"nullable":true}),
            ),
        ];
        Ok(UiDocument {
            title: "触控输入源".to_owned(),
            nodes: vec![form],
            actions,
            revision: 0,
        })
    }
}

#[async_trait]
impl Plugin for TouchPlugin {
    async fn initialize(
        &mut self,
        params: InitializeParams,
        context: &PluginContext,
    ) -> Result<Value, PluginError> {
        let config = if params
            .source
            .config
            .as_object()
            .is_some_and(|value| value.is_empty())
        {
            serde_json::to_value(TouchConfig::default()).expect("touch defaults serialize")
        } else {
            params.source.config
        };
        let state = self.set_config(config, false)?;
        context.status(state.clone())?;
        Ok(state)
    }

    async fn configure(
        &mut self,
        params: ConfigureParams,
        context: &PluginContext,
    ) -> Result<Value, PluginError> {
        let state = self.set_config(params.config, params.validate_only)?;
        if !params.validate_only {
            context.status(state.clone())?;
        }
        Ok(state)
    }

    async fn bindings(
        &mut self,
        bindings: Vec<Binding>,
        context: &PluginContext,
    ) -> Result<Value, PluginError> {
        let state = self.sync_bindings(bindings)?;
        context.status(state.clone())?;
        Ok(state)
    }

    async fn action(
        &mut self,
        params: ActionParams,
        context: &PluginContext,
    ) -> Result<Value, PluginError> {
        let state = match params.action.as_str() {
            "release_owner" => {
                let owner = params
                    .value
                    .get("ownerId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| PluginError::new("invalid_params", "缺少 ownerId"))?;
                if let Some(id) = params.binding_id {
                    binding(&self.bindings, &id)?;
                    if let Some(runtime) = self.runtimes.get_mut(&id) {
                        runtime.release_owner(owner);
                    }
                } else {
                    for runtime in self.runtimes.values_mut() {
                        runtime.release_owner(owner);
                    }
                }
                self.state()
            }
            _ => return Err(PluginError::new("unsupported_action", "未知触控动作")),
        };
        self.service_clears(context, Instant::now())?;
        context.status(state.clone())?;
        Ok(state)
    }

    async fn ui(
        &mut self,
        params: UiParams,
        _context: &PluginContext,
    ) -> Result<UiDocument, PluginError> {
        self.document(params)
    }

    async fn input(
        &mut self,
        params: InputParams,
        context: &PluginContext,
    ) -> Result<Value, PluginError> {
        if params.action != "update_touch_input" {
            return Err(PluginError::new("unsupported_action", "未知触控输入"));
        }
        let state = self.update_input(params)?;
        self.service_clears(context, Instant::now())?;
        Ok(state)
    }

    async fn tick(&mut self, context: &PluginContext) -> Result<(), PluginError> {
        let now = Instant::now();
        self.service_clears(context, now)?;
        for binding in self.bindings.iter().filter(|item| {
            item.active
                && !self.pending_clears.contains_key(&item.binding_id)
                && !self.clear_errors.contains_key(&item.binding_id)
        }) {
            if let Some(runtime) = self.runtimes.get_mut(&binding.binding_id) {
                let frames = runtime.next_frames(now);
                context.emit_frame(
                    &binding.binding_id,
                    binding.generation,
                    frame(frames[channel(binding.channel).as_v4() as usize]),
                )?;
            }
        }
        Ok(())
    }

    async fn shutdown(&mut self, _context: &PluginContext) {
        for runtime in self.runtimes.values_mut() {
            runtime.reset();
        }
    }
}

pub struct AudioPlugin {
    engine: AudioEngine,
    config: Value,
    bindings: Vec<Binding>,
    mappings: HashMap<String, AudioMappingRuntime>,
    channel_configs: HashMap<(String, Channel), AudioChannelConfig>,
}

impl Default for AudioPlugin {
    fn default() -> Self {
        Self {
            engine: AudioEngine::new(),
            config: json!({}),
            bindings: Vec::new(),
            mappings: HashMap::new(),
            channel_configs: HashMap::new(),
        }
    }
}

impl AudioPlugin {
    pub fn state(&self) -> Value {
        let mut bindings: Vec<_> = self
            .channel_configs
            .iter()
            .map(|((id, channel), config)| json!({"deviceId":id,"channel":channel,"config":config}))
            .collect();
        bindings.sort_by_key(|value| format!("{}:{}", value["deviceId"], value["channel"]));
        json!({"audio": self.engine.snapshot(), "audioBindings": bindings})
    }

    fn config_defaults(config: &Value) -> Result<AudioChannelConfig, PluginError> {
        if !config.is_object() {
            return Err(PluginError::new("invalid_config", "音频实例配置必须是对象"));
        }
        let defaults = config
            .get("defaultChannelConfig")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let defaults: AudioChannelConfig = parse(defaults)?;
        defaults.validate().map_err(source_error)?;
        Ok(defaults)
    }

    fn set_config(&mut self, config: Value, validate_only: bool) -> Result<Value, PluginError> {
        Self::config_defaults(&config)?;
        if !validate_only {
            self.config = config;
        }
        Ok(self.state())
    }

    fn sync_bindings(&mut self, bindings: Vec<Binding>) -> Result<Value, PluginError> {
        let defaults = Self::config_defaults(&self.config)?;
        let mut replacements = HashMap::new();
        let devices: HashSet<_> = bindings
            .iter()
            .map(|binding| binding.control_id.as_str())
            .collect();
        let mut configs = self.channel_configs.clone();
        configs.retain(|(id, _), _| devices.contains(id.as_str()));
        for binding in &bindings {
            let key = (binding.control_id.clone(), channel(binding.channel));
            let config = if binding
                .config
                .as_object()
                .is_some_and(|value| value.is_empty())
            {
                configs
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| defaults.clone())
            } else {
                parse(binding.config.clone())?
            };
            config.validate().map_err(source_error)?;
            configs.insert(key, config.clone());
            let unchanged = self
                .bindings
                .iter()
                .find(|item| item.binding_id == binding.binding_id)
                .is_some_and(|item| {
                    item.generation == binding.generation && item.active == binding.active
                })
                && self
                    .mappings
                    .get(&binding.binding_id)
                    .is_some_and(|mapping| mapping.config == config);
            if !unchanged {
                replacements.insert(
                    binding.binding_id.clone(),
                    AudioMappingRuntime::new(config).map_err(source_error)?,
                );
            }
        }
        self.mappings
            .retain(|id, _| bindings.iter().any(|item| item.binding_id == *id));
        self.mappings.extend(replacements);
        self.channel_configs = configs;
        self.bindings = bindings;
        Ok(self.state())
    }

    fn set_channel_config(&mut self, params: &ActionParams) -> Result<Value, PluginError> {
        let selected_id = params
            .binding_id
            .as_deref()
            .or_else(|| params.value.get("bindingId").and_then(Value::as_str));
        let (device_id, selected_channel) =
            if let Some(id) = selected_id {
                let item = binding(&self.bindings, id)?;
                (item.control_id.clone(), channel(item.channel))
            } else {
                let device = params
                    .value
                    .get("deviceId")
                    .or_else(|| params.value.get("controlId"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| PluginError::new("invalid_params", "音频配置需要指定设备"))?;
                let selected =
                    parse(params.value.get("channel").cloned().ok_or_else(|| {
                        PluginError::new("invalid_params", "音频配置需要指定通道")
                    })?)?;
                (device.to_owned(), selected)
            };
        let config: AudioChannelConfig = parse(
            params
                .value
                .get("config")
                .cloned()
                .unwrap_or_else(|| params.value.clone()),
        )?;
        config.validate().map_err(source_error)?;
        if params.value.get("validateOnly").and_then(Value::as_bool) == Some(true) {
            return Ok(self.state());
        }
        let replacements: HashMap<_, _> = self
            .bindings
            .iter()
            .filter(|item| {
                item.control_id == device_id && channel(item.channel) == selected_channel
            })
            .map(|item| {
                AudioMappingRuntime::new(config.clone())
                    .map(|runtime| (item.binding_id.clone(), runtime))
                    .map_err(source_error)
            })
            .collect::<Result<_, _>>()?;
        self.mappings.extend(replacements);
        self.channel_configs
            .insert((device_id, selected_channel), config);
        Ok(self.state())
    }

    fn control(&self, value: Value) -> Result<Value, PluginError> {
        let mut action: AudioAction = parse(value.get("action").cloned().unwrap_or(value))?;
        if let AudioAction::LoadFile { path } | AudioAction::SaveRecording { path } = &mut action
            && !Path::new(path).is_absolute()
        {
            *path = std::env::current_dir()
                .map_err(|error| PluginError::new("audio_path", error.to_string()))?
                .join(&*path)
                .to_string_lossy()
                .into_owned();
        }
        self.engine.control(action).map_err(source_error)?;
        Ok(self.state())
    }

    fn document(&self, params: UiParams) -> Result<UiDocument, PluginError> {
        let actions = vec![
            action::<AudioAction>("audio_control", "音频控制"),
            action::<AudioChannelConfig>("configure", "设置通道映射"),
        ];
        let snapshot =
            serde_json::to_value(self.engine.snapshot()).expect("audio snapshot serializes");
        let mut player = node("audio-player", UiNodeKind::AudioPlayer, "音频输入");
        player.value = Some(snapshot.clone());
        player.action = Some("audio_control".to_owned());
        player.props = json!({"description":"支持音频与视频音轨、麦克风、录音和 Windows 桌面音频。音频文件不超过 200 MiB，视频不超过 2 GiB，时长不超过 1 小时。","modes":["file","microphone","recording","desktop"],"extensions":["mp3","flac","wav","m4a","aac","ogg","mp4","m4v","mov","mkv","webm"]});
        let mut meter = node("audio-level", UiNodeKind::Meter, "输入电平");
        meter.value = Some(json!([snapshot["levelLeft"], snapshot["levelRight"]]));
        meter.props = json!({"min":0,"max":1,"labels":["L","R"]});
        let mut nodes = vec![player, meter];
        let selected = params
            .binding_id
            .as_deref()
            .map(|id| binding(&self.bindings, id))
            .transpose()?;
        let config = if let Some(binding) = selected {
            self.channel_configs
                .get(&(binding.control_id.clone(), channel(binding.channel)))
                .cloned()
                .unwrap_or_default()
        } else {
            Self::config_defaults(&self.config)?
        };
        let value = serde_json::to_value(config).expect("audio mapping serializes");
        let mut form = node(
            "audio-config",
            UiNodeKind::Form,
            if selected.is_some() {
                "此通道音频映射"
            } else {
                "新通道默认映射"
            },
        );
        form.action = Some("configure".to_owned());
        form.props = json!({"configScope":if selected.is_some() { "binding" } else { "instance" },"configPrefix":if selected.is_some() { "" } else { "defaultChannelConfig" }});
        form.children = vec![
            field(&value, "enabled", UiNodeKind::Switch, "启用映射", json!({})),
            field(
                &value,
                "inputChannel",
                UiNodeKind::Select,
                "音频声道",
                json!({"options":[{"value":"left","label":"左"},{"value":"right","label":"右"},{"value":"mix","label":"混合"}]}),
            ),
            field(
                &value,
                "gain",
                UiNodeKind::Slider,
                "增益",
                json!({"min":1,"max":10,"step":0.1}),
            ),
            field(
                &value,
                "volumeLower",
                UiNodeKind::NumberField,
                "强度下限",
                json!({"min":0,"max":1,"step":0.01}),
            ),
            field(
                &value,
                "volumeUpper",
                UiNodeKind::NumberField,
                "强度上限",
                json!({"min":0,"max":1,"step":0.01}),
            ),
            field(&value, "adaptive", UiNodeKind::Switch, "自适应", json!({})),
            field(
                &value,
                "adaptiveLower",
                UiNodeKind::NumberField,
                "低适应系数",
                json!({"min":0,"max":0.5,"step":0.01}),
            ),
            field(
                &value,
                "adaptiveUpper",
                UiNodeKind::NumberField,
                "高适应系数",
                json!({"min":0,"max":0.5,"step":0.01}),
            ),
            field(
                &value,
                "hysteresisMs",
                UiNodeKind::IntegerField,
                "迟滞（ms）",
                json!({"min":0,"max":2000}),
            ),
            field(
                &value,
                "frequencyMin",
                UiNodeKind::NumberField,
                "观察频段下限（Hz）",
                json!({"min":50,"max":10000}),
            ),
            field(
                &value,
                "frequencyMax",
                UiNodeKind::NumberField,
                "观察频段上限（Hz）",
                json!({"min":50,"max":10000}),
            ),
            field(
                &value,
                "periodCurve",
                UiNodeKind::Curve,
                "周期曲线",
                json!({"min":10,"max":100}),
            ),
        ];
        nodes.push(form);
        Ok(UiDocument {
            title: "音频输入源".to_owned(),
            nodes,
            actions,
            revision: 0,
        })
    }
}

#[async_trait]
impl Plugin for AudioPlugin {
    async fn initialize(
        &mut self,
        params: InitializeParams,
        context: &PluginContext,
    ) -> Result<Value, PluginError> {
        let state = self.set_config(params.source.config, false)?;
        context.status(state.clone())?;
        Ok(state)
    }

    async fn configure(
        &mut self,
        params: ConfigureParams,
        context: &PluginContext,
    ) -> Result<Value, PluginError> {
        let state = self.set_config(params.config, params.validate_only)?;
        if !params.validate_only {
            context.status(state.clone())?;
        }
        Ok(state)
    }

    async fn bindings(
        &mut self,
        bindings: Vec<Binding>,
        context: &PluginContext,
    ) -> Result<Value, PluginError> {
        let state = self.sync_bindings(bindings)?;
        context.status(state.clone())?;
        Ok(state)
    }

    async fn action(
        &mut self,
        params: ActionParams,
        context: &PluginContext,
    ) -> Result<Value, PluginError> {
        let state = match params.action.as_str() {
            "configure_binding" => self.set_channel_config(&params)?,
            "audio_control" => self.control(params.value)?,
            _ => return Err(PluginError::new("unsupported_action", "未知音频动作")),
        };
        context.status(state.clone())?;
        Ok(state)
    }

    async fn ui(
        &mut self,
        params: UiParams,
        _context: &PluginContext,
    ) -> Result<UiDocument, PluginError> {
        self.document(params)
    }

    async fn tick(&mut self, context: &PluginContext) -> Result<(), PluginError> {
        let features = self.engine.latest();
        for binding in self.bindings.iter().filter(|item| item.active) {
            if let Some(mapping) = self.mappings.get_mut(&binding.binding_id) {
                context.emit_frame(
                    &binding.binding_id,
                    binding.generation,
                    frame(mapping.next_frame(&features)),
                )?;
            }
        }
        context.status(self.state())
    }

    async fn shutdown(&mut self, _context: &PluginContext) {
        self.engine.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_binding(device: &str, channel: dg_lab_link_plugin_sdk::Channel) -> Binding {
        Binding {
            binding_id: format!("{device}:{channel:?}"),
            control_id: device.to_owned(),
            channel,
            generation: 1,
            config: json!({}),
            active: true,
        }
    }

    #[test]
    fn touch_config_validation_does_not_replace_live_config() {
        let mut plugin = TouchPlugin::default();
        let original = plugin.config.clone();
        let mut candidate = serde_json::to_value(&original).unwrap();
        candidate["swapAxes"] = json!(true);
        plugin.set_config(candidate, true).unwrap();
        assert_eq!(plugin.config, original);
        assert!(plugin.set_config(json!({"gridSize":8}), false).is_err());
        assert_eq!(plugin.config, original);
    }

    #[test]
    fn touch_stop_generation_clears_only_changed_channel() {
        let mut plugin = TouchPlugin::default();
        let a = test_binding("device", dg_lab_link_plugin_sdk::Channel::A);
        let b = test_binding("device", dg_lab_link_plugin_sdk::Channel::B);
        plugin.sync_bindings(vec![a.clone(), b.clone()]).unwrap();
        for selected in [&a, &b] {
            plugin
                .update_input(InputParams {
                    action: "update_touch_input".into(),
                    binding_id: Some(selected.binding_id.clone()),
                    owner: "window".into(),
                    sequence: 1,
                    value: json!({"pointers":[{"id":1,"x":0.5,"y":0.5}]}),
                })
                .unwrap();
        }
        let mut stopped = a;
        stopped.generation += 1;
        stopped.active = false;
        plugin.sync_bindings(vec![stopped, b]).unwrap();
        assert!(!plugin.runtimes["device:A"].active_touch_channels(Instant::now())[0]);
        assert!(plugin.runtimes["device:B"].active_touch_channels(Instant::now())[1]);
    }

    #[test]
    fn touch_input_requires_an_explicit_live_binding() {
        let mut plugin = TouchPlugin::default();
        let a = test_binding("device", dg_lab_link_plugin_sdk::Channel::A);
        plugin.sync_bindings(vec![a]).unwrap();
        let input = InputParams {
            action: "update_touch_input".into(),
            binding_id: None,
            owner: "window".into(),
            sequence: 1,
            value: json!({"pointers":[{"id":1,"x":0.5,"y":0.5}]}),
        };
        assert_eq!(
            plugin.update_input(input.clone()).unwrap_err().code,
            "invalid_params"
        );
        assert_eq!(
            plugin
                .update_input(InputParams {
                    binding_id: Some("expired".into()),
                    ..input
                })
                .unwrap_err()
                .code,
            "unknown_binding"
        );
        assert!(!plugin.runtimes["device:A"].active_touch_channels(Instant::now())[0]);
    }

    #[test]
    fn invalid_audio_binding_update_is_atomic_and_channels_are_independent() {
        let mut plugin = AudioPlugin::default();
        let a = test_binding("device", dg_lab_link_plugin_sdk::Channel::A);
        let b = test_binding("device", dg_lab_link_plugin_sdk::Channel::B);
        plugin.sync_bindings(vec![a.clone(), b.clone()]).unwrap();
        let mut changed = b.clone();
        changed.config = json!({"gain":4.0});
        plugin.sync_bindings(vec![a.clone(), changed]).unwrap();
        assert_eq!(plugin.mappings[&a.binding_id].config.gain, 2.5);
        assert_eq!(plugin.mappings[&b.binding_id].config.gain, 4.0);
        let mut invalid = b.clone();
        invalid.config = json!({"gain":20.0});
        assert!(plugin.sync_bindings(vec![a, invalid]).is_err());
        assert_eq!(plugin.mappings[&b.binding_id].config.gain, 4.0);
    }

    #[test]
    fn builtin_documents_use_public_widget_contracts() {
        let touch = TouchPlugin::default();
        touch
            .document(UiParams::default())
            .unwrap()
            .validate()
            .unwrap();
        let touch_control = touch
            .document(UiParams {
                surface: UiSurface::Control,
                ..UiParams::default()
            })
            .unwrap();
        assert!(matches!(touch_control.nodes[0].kind, UiNodeKind::XyPad));
        let audio = AudioPlugin::default();
        let audio_document = audio.document(UiParams::default()).unwrap();
        audio_document.validate().unwrap();
        assert!(matches!(
            audio_document.nodes[0].kind,
            UiNodeKind::AudioPlayer
        ));
    }
}
