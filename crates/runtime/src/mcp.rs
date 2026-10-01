use std::sync::Arc;
use std::sync::atomic::Ordering;

use dg_lab_link_core::{ControlCommand, ControlError};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, Resource, ResourceContents, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Value, json};

use crate::MAX_REQUEST_BYTES;
use crate::server::{CommandEpoch, Shared};

pub(crate) fn mcp_service(
    state: Arc<Shared>,
) -> StreamableHttpService<McpServer, LocalSessionManager> {
    let mut config = StreamableHttpServerConfig::default();
    config.legacy_session_mode = false;
    config.json_response = true;
    config.max_request_body_bytes = MAX_REQUEST_BYTES;
    config.cancellation_token = state.terminated.clone();
    // Header authentication and the exact local Host/Origin allowlist are also
    // enforced before entering the SDK. Stateless HTTP requests are not holders.
    StreamableHttpService::new(
        move || {
            Ok(McpServer {
                state: state.clone(),
            })
        },
        Arc::new(LocalSessionManager::default()),
        config,
    )
}

#[derive(Clone)]
pub(crate) struct McpServer {
    state: Arc<Shared>,
}

fn tools() -> Vec<Tool> {
    ControlCommand::descriptors()
        .into_iter()
        .map(|descriptor| {
            let schema = descriptor
                .input_schema
                .as_object()
                .expect("业务参数 Schema 是对象")
                .clone();
            Tool::new(descriptor.name, descriptor.description, schema)
                .with_annotations(ToolAnnotations::new().read_only(descriptor.read_only))
        })
        .collect()
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().build())
            .with_server_info(Implementation::new("dg-lab-link", env!("CARGO_PKG_VERSION")))
            .with_instructions("DG-LAB Link 本机共享核心。先通过 CLI serve --background 建立持有者；MCP 请求不保持核心运行。设备写操作显式提供 deviceId（快照的 controlId），先读取状态和安全设置。触控必须按租期续传；任务结束调用 CLI holders release <id>。写操作超时后先读取状态，不自动重试。GUI、CLI 和 MCP 共享同一设备会话。")
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().find(|tool| tool.name == name)
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
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<CommandEpoch>())
            .copied()
            .unwrap_or_else(|| self.state.accept_command(command.is_safety()));
        let result = match self.state.execute_received(command, epoch).await {
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
        let resources = [
            ("status", "共享状态", "共享 Hub 完整最新快照"),
            ("devices", "设备", "当前配对设备及 controlId"),
            ("sources", "输入源", "输入源分配及可用波形目录"),
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
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if self.state.stopping.load(Ordering::Acquire) {
            return Err(ErrorData::internal_error(
                "共享核心正在安全关闭",
                Some(json!(ControlError::new(
                    "runtime_stopping",
                    "共享核心正在安全关闭"
                ))),
            ));
        }
        let snapshot = self.state.service.snapshot();
        let value = match request.uri.as_str() {
            "dglab://status" => json!(snapshot),
            "dglab://devices" => json!(snapshot.devices),
            "dglab://sources" => {
                json!({ "sources": snapshot.sources, "officialWaveforms": dg_lab_link_core::waveforms::official_waveforms(), "customWaveforms": snapshot.custom_waveforms, "defaultSourceId": snapshot.default_source_id })
            }
            "dglab://logs" => json!(snapshot.logs),
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
