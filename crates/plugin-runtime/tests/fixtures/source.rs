use std::time::Duration;

use dg_lab_link_plugin_sdk::*;
use serde_json::{Value, json};

#[derive(Default)]
struct Fixture {
    config: Value,
    external_context: Option<PluginContext>,
}

#[async_trait]
impl Plugin for Fixture {
    async fn initialize(
        &mut self,
        params: InitializeParams,
        context: &PluginContext,
    ) -> Result<Value, PluginError> {
        self.config = params.source.config;
        self.external_context = Some(context.clone());
        context.status(json!({"initialized":true}))?;
        Ok(Value::Null)
    }

    async fn migrate(
        &mut self,
        params: MigrateParams,
        _: &PluginContext,
    ) -> Result<Value, PluginError> {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut config = params.config;
        config["migrated"] = json!(true);
        Ok(config)
    }

    async fn configure(
        &mut self,
        params: ConfigureParams,
        _: &PluginContext,
    ) -> Result<Value, PluginError> {
        if !params.validate_only {
            self.config = params.config;
        }
        Ok(self.config.clone())
    }

    async fn bindings(&mut self, _: Vec<Binding>, _: &PluginContext) -> Result<Value, PluginError> {
        Ok(Value::Null)
    }

    async fn action(
        &mut self,
        params: ActionParams,
        context: &PluginContext,
    ) -> Result<Value, PluginError> {
        if params.action == "external_business" {
            let original = self.external_context.as_ref().expect("initialized context");
            let fresh = original.begin_operation().await?;
            let command = json!({"command":"start_output","params":{"deviceId":"fixture"}});
            let result = fresh.business_call(command.clone()).await?;
            let original_code = original
                .business_call(command)
                .await
                .map_or_else(|error| error.code, |_| "ok".into());
            return Ok(json!({"result":result,"originalCode":original_code}));
        }
        if params.action == "deferred_business" {
            let context = context.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let result = context
                    .business_call(
                        json!({"command":"start_output","params":{"deviceId":"fixture"}}),
                    )
                    .await;
                let code = result.map_or_else(|error| error.code, |_| "ok".into());
                let _ = context.status(json!({"deferredResult":code}));
            });
            return Ok(json!({"queued":true}));
        }
        Ok(self.config.clone())
    }

    async fn ui(&mut self, _: UiParams, _: &PluginContext) -> Result<UiDocument, PluginError> {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok(UiDocument {
            title: "Fixture".into(),
            nodes: vec![],
            actions: vec![],
            revision: 0,
        })
    }

    async fn shutdown(&mut self, context: &PluginContext) {
        let _ = context.status(json!({"shutdownStarted":true}));
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run_plugin(Fixture::default()).await {
        eprintln!("{}: {}", error.code, error.message);
        std::process::exit(1);
    }
}
