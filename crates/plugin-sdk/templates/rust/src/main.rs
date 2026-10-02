use dg_lab_link_plugin_sdk::*;
use serde_json::{Value, json};

#[derive(Default)]
struct PulseSource {
    frequency: u8,
    intensity: u8,
    bindings: Vec<Binding>,
}

impl PulseSource {
    fn parse(config: &Value) -> Result<(u8, u8), PluginError> {
        let frequency = config
            .get("frequency")
            .and_then(Value::as_u64)
            .unwrap_or(100);
        let intensity = config
            .get("intensity")
            .and_then(Value::as_u64)
            .unwrap_or(20);
        if !(10..=240).contains(&frequency) || intensity > 100 {
            return Err(PluginError::new(
                "invalid_config",
                "frequency 必须为 10..240，intensity 必须为 0..100",
            ));
        }
        Ok((frequency as u8, intensity as u8))
    }

    fn state(&self) -> Value {
        json!({"frequency":self.frequency,"intensity":self.intensity,"bindingCount":self.bindings.len()})
    }
}

#[async_trait]
impl Plugin for PulseSource {
    async fn initialize(
        &mut self,
        params: InitializeParams,
        context: &PluginContext,
    ) -> Result<Value, PluginError> {
        (self.frequency, self.intensity) = Self::parse(&params.source.config)?;
        context.status(self.state())?;
        Ok(self.state())
    }

    async fn configure(
        &mut self,
        params: ConfigureParams,
        context: &PluginContext,
    ) -> Result<Value, PluginError> {
        let values = Self::parse(&params.config)?;
        if !params.validate_only {
            (self.frequency, self.intensity) = values;
            context.status(self.state())?;
        }
        Ok(json!({"frequency":values.0,"intensity":values.1}))
    }

    async fn bindings(
        &mut self,
        bindings: Vec<Binding>,
        context: &PluginContext,
    ) -> Result<Value, PluginError> {
        self.bindings = bindings;
        context.status(self.state())?;
        Ok(Value::Null)
    }

    async fn action(
        &mut self,
        params: ActionParams,
        context: &PluginContext,
    ) -> Result<Value, PluginError> {
        match params.action.as_str() {
            "core_snapshot" => {
                context
                    .business_call(json!({"command":"get_hub_snapshot"}))
                    .await
            }
            "state" => Ok(self.state()),
            _ => Err(PluginError::new("unsupported_action", "未知示例动作")),
        }
    }

    async fn ui(
        &mut self,
        _params: UiParams,
        _context: &PluginContext,
    ) -> Result<UiDocument, PluginError> {
        let mut frequency = UiNode::new("frequency", UiNodeKind::Slider);
        frequency.label = Some("频率编码".into());
        frequency.config_key = Some("frequency".into());
        frequency.value = Some(json!(self.frequency));
        frequency.props = json!({"min":10,"max":240,"step":1});

        let mut intensity = UiNode::new("intensity", UiNodeKind::Slider);
        intensity.label = Some("波形强度".into());
        intensity.config_key = Some("intensity".into());
        intensity.value = Some(json!(self.intensity));
        intensity.props = json!({"min":0,"max":100,"step":1});

        let mut form = UiNode::new("settings", UiNodeKind::Form);
        form.label = Some("配置示例".into());
        form.action = Some("configure".into());
        form.children = vec![frequency, intensity];

        let mut snapshot = UiNode::new("snapshot", UiNodeKind::Button);
        snapshot.label = Some("读取核心快照".into());
        snapshot.action = Some("core_snapshot".into());
        Ok(UiDocument {
            title: "第三方输入源示例".into(),
            nodes: vec![form, snapshot],
            actions: vec![ActionDescriptor {
                id: "core_snapshot".into(),
                label: "读取核心快照".into(),
                description: "演示插件调用完整核心业务接口".into(),
                params_schema: json!({"type":"object"}),
            }],
            revision: 0,
        })
    }

    async fn tick(&mut self, context: &PluginContext) -> Result<(), PluginError> {
        let frame = Frame {
            samples: [Sample {
                frequency: self.frequency,
                pulse_intensity: self.intensity,
            }; 4],
        };
        context.emit_for_bindings(&self.bindings, frame)
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run_plugin(PulseSource::default()).await {
        eprintln!(
            "{}",
            serde_json::to_string(&error).expect("error serialization")
        );
        std::process::exit(1);
    }
}
