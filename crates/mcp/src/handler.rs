use dg_lab_link_contracts::hub::HubSnapshot;
use dg_lab_link_contracts::{ControlCommand, ControlError};
use dg_lab_link_runtime::{AcceptedCommandEpoch, Client};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, Resource, ResourceContents, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Value, json};

#[derive(Clone, Copy)]
pub(crate) struct CommandEpoch(pub AcceptedCommandEpoch);

#[derive(Clone)]
pub(crate) struct McpServer {
    client: Client,
    transport: TransportKind,
}

#[derive(Clone, Copy)]
pub(crate) enum TransportKind {
    Http,
    Stdio,
}

impl McpServer {
    pub(crate) fn new(client: Client, transport: TransportKind) -> Self {
        Self { client, transport }
    }

    async fn snapshot(&self) -> Result<HubSnapshot, ErrorData> {
        let value = self
            .client
            .call(ControlCommand::GetHubSnapshot)
            .await
            .map_err(resource_error)?;
        serde_json::from_value(value).map_err(|error| {
            resource_error(ControlError::new(
                "runtime_protocol_error",
                error.to_string(),
            ))
        })
    }

    async fn official_waveforms(&self) -> Result<Value, ErrorData> {
        let value = self
            .client
            .call(ControlCommand::ListWaveforms)
            .await
            .map_err(resource_error)?;
        Ok(value["official"].clone())
    }
}

fn resource_error(error: ControlError) -> ErrorData {
    ErrorData::internal_error(error.message.clone(), Some(json!(error)))
}

pub(crate) fn priority_tool(name: &str, arguments: Value) -> bool {
    ControlCommand::from_call(name, arguments).is_ok_and(|command| command.is_safety())
}

fn tools() -> &'static [Tool] {
    static TOOLS: std::sync::OnceLock<Vec<Tool>> = std::sync::OnceLock::new();
    TOOLS.get_or_init(|| {
        ControlCommand::descriptors()
            .iter()
            .map(|descriptor| {
                let schema = descriptor
                    .input_schema
                    .as_object()
                    .expect("业务参数 Schema 是对象")
                    .clone();
                Tool::new(
                    descriptor.name.clone(),
                    descriptor.description.clone(),
                    schema,
                )
                .with_annotations(ToolAnnotations::new().read_only(descriptor.read_only))
            })
            .collect()
    })
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        let lifecycle = match self.transport {
            TransportKind::Http => {
                "HTTP 请求不持续持有核心。先通过 GUI 或 CLI serve --background 建立持有者；任务结束释放自己创建的 CLI holderId。"
            }
            TransportKind::Stdio => {
                "stdio 进程已自动连接或启动核心并持有它。关闭 stdio 会释放本进程的持有者；无需另行启动后台 CLI。"
            }
        };
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().build())
            .with_server_info(Implementation::new("dg-lab-link", env!("CARGO_PKG_VERSION")))
            .with_instructions(format!("DG-LAB Link 本机共享核心。{lifecycle}设备写操作显式提供 deviceId（快照的 controlId），先读取状态和安全设置。触控必须按租期续传。写操作超时后先读取状态，不自动重试。GUI、CLI 和两种 MCP 接入共享同一设备会话。"))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools().to_vec()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().iter().find(|tool| tool.name == name).cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if !ControlCommand::descriptors()
            .iter()
            .any(|descriptor| descriptor.name == request.name)
        {
            return Err(ErrorData::invalid_params(
                "未知业务工具",
                Some(json!({"name": request.name})),
            ));
        }
        let params = request
            .arguments
            .map(Value::Object)
            .unwrap_or_else(|| json!({}));
        let command = match ControlCommand::from_call(&request.name, params) {
            Ok(command) if command.is_business() => command,
            Ok(_) => {
                return Err(ErrorData::invalid_params(
                    "GUI 偏好不作为 MCP 业务工具",
                    None,
                ));
            }
            Err(error) => return Ok(CallToolResult::structured_error(json!(error)).into()),
        };
        let epoch = context
            .extensions
            .get::<CommandEpoch>()
            .copied()
            .or_else(|| {
                context
                    .extensions
                    .get::<axum::http::request::Parts>()
                    .and_then(|parts| parts.extensions.get::<CommandEpoch>())
                    .copied()
            });
        let executed = match epoch {
            Some(epoch) => self.client.call_received(command, epoch.0).await,
            None if command.is_safety() => self.client.call(command).await,
            None => Err(ControlError::new(
                "runtime_protocol_error",
                "MCP 请求缺少停止代次",
            )),
        };
        let result = match executed {
            Ok(value) => CallToolResult::structured(if value.is_object() {
                value
            } else {
                json!({"result": value})
            }),
            Err(error) => CallToolResult::structured_error(json!(error)),
        };
        Ok(result.into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let snapshot = self.snapshot().await?;
        let mut resources: Vec<Resource> = [
            ("status", "共享状态", "共享 Hub 完整最新快照"),
            ("devices", "设备", "当前配对设备及 controlId"),
            ("connections", "连接", "V4、V3 和蓝牙连接状态与配对信息"),
            (
                "bluetooth",
                "蓝牙发现",
                "最近一次主动扫描的郊狼 3.0 设备；读取资源不触发扫描",
            ),
            ("sources", "输入源", "输入源分配及可用波形目录"),
            ("plugins", "插件", "已安装本地插件包、版本与发布者"),
            ("logs", "运行记录", "最近的有界运行记录"),
        ]
        .into_iter()
        .map(|(name, title, description)| {
            Resource::new(format!("dglab://{name}"), name)
                .with_title(title)
                .with_description(description)
                .with_mime_type("application/json")
        })
        .collect();
        resources.extend(snapshot.sources.iter().map(|source| {
            Resource::new(format!("dglab://sources/{}", source.id), source.id.clone())
                .with_title(source.name.clone())
                .with_description("输入源实例元数据、配置、运行状态和插件发布状态；读取不启动插件")
                .with_mime_type("application/json")
        }));
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let snapshot = self.snapshot().await?;
        let value = match request.uri.as_str() {
            "dglab://status" => json!(snapshot),
            "dglab://devices" => json!(snapshot.devices),
            "dglab://connections" => json!(snapshot.connections),
            "dglab://bluetooth" => json!(snapshot.bluetooth),
            "dglab://sources" => {
                json!({ "sources": snapshot.sources, "bindings": snapshot.source_bindings, "officialWaveforms": self.official_waveforms().await?, "customWaveforms": snapshot.custom_waveforms, "defaultSourceId": snapshot.default_source_id })
            }
            "dglab://plugins" => json!(snapshot.plugins),
            "dglab://logs" => json!(snapshot.logs),
            uri if uri.starts_with("dglab://sources/") => {
                let source_id = uri.trim_start_matches("dglab://sources/");
                let source = snapshot
                    .sources
                    .iter()
                    .find(|source| source.id == source_id)
                    .ok_or_else(|| {
                        ErrorData::resource_not_found("输入源实例不存在", Some(json!({"uri":uri})))
                    })?;
                json!(source)
            }
            _ => {
                return Err(ErrorData::resource_not_found(
                    "资源不存在",
                    Some(json!({"uri": request.uri})),
                ));
            }
        };
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(value.to_string(), request.uri)
                .with_mime_type("application/json"),
        ])
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_dispatch_uses_validated_core_commands() {
        assert!(priority_tool(
            "stop_output",
            json!({"deviceId":"explicit-device"})
        ));
        assert!(!priority_tool("disconnect_relay", json!({})));
        assert!(priority_tool(
            "disconnect_connection",
            json!({"connectionId":"ws-v3"})
        ));
        assert!(!priority_tool("stop_output", json!({})));
        assert!(!priority_tool("emergency_stop", json!({})));
        assert!(!priority_tool(
            "set_source_config",
            json!({"sourceId":"source-test","config":{}})
        ));
    }
}
