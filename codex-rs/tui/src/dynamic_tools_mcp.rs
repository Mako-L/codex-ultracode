//! Approval-gated MCP transport for task tools owned by a local-daemon TUI.

use crate::app_event_sender::AppEventSender;
use crate::dynamic_tools;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::Request;
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::middleware;
use axum::middleware::Next;
use axum::response::Response;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::DynamicToolCallOutputContentItem;
use codex_app_server_protocol::DynamicToolCallParams;
use codex_app_server_protocol::DynamicToolNamespaceTool;
use codex_app_server_protocol::DynamicToolSpec;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStatusChangedNotification;
use codex_config::McpServerConfig;
use codex_config::McpServerRequirement;
use codex_config::RawMcpServerConfig;
use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResult;
use rmcp::model::ContentBlock;
use rmcp::model::JsonObject;
use rmcp::model::ListToolsResult;
use rmcp::model::PaginatedRequestParams;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerInfo;
use rmcp::model::Tool;
use rmcp::model::ToolAnnotations;
use rmcp::service::RequestContext;
use rmcp::service::RoleServer;
use rmcp::transport::StreamableHttpServerConfig;
use rmcp::transport::StreamableHttpService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use serde_json::Value;
use serde_json::json;
use std::borrow::Cow;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::RwLock;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use uuid::Uuid;

#[derive(Clone)]
pub(crate) enum ThreadToolTransport {
    Disabled,
    Workflow,
    Tasks,
    Dynamic,
    Mcp(Arc<DynamicToolMcpServer>),
}

impl ThreadToolTransport {
    pub(crate) fn configure(&self, params: &mut ThreadStartParams) {
        match self {
            Self::Disabled => params.dynamic_tools = None,
            Self::Workflow => {
                params.dynamic_tools = Some(dynamic_tools::workflow_tool_specs());
            }
            Self::Tasks => {
                params.dynamic_tools = Some(dynamic_tools::non_delegation_task_tool_specs());
            }
            Self::Dynamic => {
                params.dynamic_tools = Some(dynamic_tools::non_delegation_tool_specs());
            }
            Self::Mcp(server) => {
                params.dynamic_tools = server
                    .native_workflow_tool
                    .then(dynamic_tools::workflow_tool_specs);
                self.configure_mcp(&mut params.config);
            }
        }
    }

    pub(crate) fn configure_mcp(&self, config: &mut Option<HashMap<String, Value>>) {
        if let Self::Mcp(server) = self {
            config.get_or_insert_default().insert(
                format!("mcp_servers.{}", dynamic_tools::NAMESPACE),
                server.config.clone(),
            );
        }
    }
}

type ToolConnection = Arc<RwLock<Option<(AppServerRequestHandle, AppEventSender)>>>;

#[cfg(test)]
#[path = "dynamic_tools_native_workflow_transport_tests.rs"]
mod native_workflow_transport_tests;

pub(crate) trait WorkflowMcpHandler: Send + Sync {
    fn call(
        &self,
        params: DynamicToolCallParams,
    ) -> Pin<Box<dyn Future<Output = codex_app_server_protocol::DynamicToolCallResponse> + Send>>;
}

pub(crate) struct DynamicToolMcpServer {
    // Local interactive daemons use native dynamic-tool requests for workflow consent.
    native_workflow_tool: bool,
    connection: ToolConnection,
    config: Value,
    task: JoinHandle<()>,
    pub(crate) supervisor: Option<crate::ultracode_bridge::UltracodeBridge>,
}

impl DynamicToolMcpServer {
    pub(crate) fn configuration(&self) -> Value {
        self.config.clone()
    }

    #[cfg(unix)]
    pub(crate) async fn connect_supervisor(
        home: &std::path::Path,
        request_handle: AppServerRequestHandle,
        thread_start_params: ThreadStartParams,
        events: AppEventSender,
        managed_requirement: Option<&McpServerRequirement>,
    ) -> std::io::Result<Self> {
        let plugin_root = crate::workflow_runtime::WorkflowRuntime::bundled()?.root;
        let supervisor = crate::ultracode_host::connect(home).await?;
        let config = supervisor
            .request(
                "configure",
                json!({"threadStartParams":thread_start_params,"pluginRoot":plugin_root}),
                std::time::Duration::from_secs(30),
            )
            .await
            .map_err(std::io::Error::other)?;
        if let Some(requirement) = managed_requirement {
            let raw: RawMcpServerConfig = serde_json::from_value(config.clone())?;
            let configured = McpServerConfig::try_from(raw).map_err(std::io::Error::other)?;
            if !configured.matches_requirement(requirement) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "managed MCP requirements do not permit the native workflow host",
                ));
            }
        }
        Ok(Self {
            connection: Arc::new(RwLock::new(Some((request_handle, events)))),
            native_workflow_tool: true,
            config,
            task: tokio::spawn(async {}),
            supervisor: Some(supervisor),
        })
    }

    pub(crate) fn suspend(&self) {
        *self
            .connection
            .write()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    pub(crate) fn reconnect(&self, handle: AppServerRequestHandle, events: AppEventSender) {
        *self
            .connection
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some((handle, events));
    }

    pub(crate) async fn start(
        request_handle: AppServerRequestHandle,
        mut thread_start_params: ThreadStartParams,
        app_event_tx: AppEventSender,
        status_updates: broadcast::Sender<ThreadStatusChangedNotification>,
        managed_requirement: Option<&McpServerRequirement>,
        workflow_handler: Option<Arc<dyn WorkflowMcpHandler>>,
        workflows_enabled: bool,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let authorization = Arc::new(format!("Bearer {}", Uuid::new_v4()));
        let server_config = json!({
            "url": format!("http://{address}/mcp"),
            "http_headers": {"Authorization": authorization.as_str()},
            "default_tools_approval_mode": "approve",
            "tools": {
                "create_thread": {"approval_mode": "prompt"},
                "send_message_to_thread": {"approval_mode": "prompt"},
                "fork_thread": {"approval_mode": "prompt"}
            }
        });
        if let Some(requirement) = managed_requirement {
            let raw_config = serde_json::from_value::<RawMcpServerConfig>(server_config.clone())
                .map_err(|error| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
                })?;
            let configured_server = McpServerConfig::try_from(raw_config)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            if !configured_server.matches_requirement(requirement) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "managed MCP requirements do not permit the TUI task-tools server",
                ));
            }
        }
        if let Some(overrides) = thread_start_params.config.as_mut() {
            overrides.remove("web_search");
        }
        let connection = Arc::new(RwLock::new(Some((request_handle, app_event_tx))));
        let handler = DynamicToolMcpHandler {
            connection: Arc::clone(&connection),
            thread_start_params,
            status_updates,
            server_config: server_config.clone(),
            workflow_handler,
            workflows_enabled,
        };
        let service = StreamableHttpService::new(
            move || Ok(handler.clone()),
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig::default(),
        );
        let router =
            Router::new()
                .nest_service("/mcp", service)
                .layer(middleware::from_fn_with_state(
                    authorization,
                    require_authorization,
                ));
        let task = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                tracing::warn!(%error, "TUI task-tools MCP server stopped");
            }
        });
        Ok(Self {
            connection,
            native_workflow_tool: false,
            config: server_config,
            task,
            supervisor: None,
        })
    }
}

impl Drop for DynamicToolMcpServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn require_authorization(
    State(expected): State<Arc<String>>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    if request
        .headers()
        .get(AUTHORIZATION)
        .is_some_and(|value| value.as_bytes() == expected.as_bytes())
    {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

#[derive(Clone)]
struct DynamicToolMcpHandler {
    connection: ToolConnection,
    thread_start_params: ThreadStartParams,
    status_updates: broadcast::Sender<ThreadStatusChangedNotification>,
    server_config: Value,
    workflow_handler: Option<Arc<dyn WorkflowMcpHandler>>,
    workflows_enabled: bool,
}

impl ServerHandler for DynamicToolMcpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let mut tools = Vec::new();
        let specs = if self.workflows_enabled {
            dynamic_tools::tool_specs()
        } else {
            dynamic_tools::task_tool_specs()
        };
        for spec in specs {
            let functions = match spec {
                DynamicToolSpec::Function(function) => vec![function],
                DynamicToolSpec::Namespace(namespace) => namespace
                    .tools
                    .into_iter()
                    .map(|tool| match tool {
                        DynamicToolNamespaceTool::Function(function) => function,
                    })
                    .collect(),
            };
            for function in functions {
                let description = if function.name == "workflow" {
                    function
                        .description
                        .replace("tools.workflow", "tools.mcp__codex_tui__workflow")
                } else {
                    function.description
                };
                let schema = serde_json::from_value::<JsonObject>(function.input_schema)
                    .map_err(|error| McpError::internal_error(error.to_string(), None))?;
                let mut tool = Tool::new(
                    Cow::Owned(function.name),
                    Cow::Owned(description),
                    Arc::new(schema),
                );
                tool.annotations = Some(ToolAnnotations::new().read_only(matches!(
                    tool.name.as_ref(),
                    "list_threads" | "list_archived_threads" | "read_thread" | "wait_threads"
                )));
                tools.push(tool);
            }
        }
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, McpError> {
        if !self.workflows_enabled && request.name == "workflow" {
            return Err(McpError::invalid_params(
                "workflow tools are disabled",
                None,
            ));
        }
        let metadata = &context.meta.0.0;
        let turn_metadata = metadata
            .get("x-codex-turn-metadata")
            .and_then(|value| match value {
                Value::Object(_) => Some(value.clone()),
                Value::String(value) => serde_json::from_str(value).ok(),
                _ => None,
            });
        let thread_id = metadata
            .get("threadId")
            .and_then(Value::as_str)
            .or_else(|| turn_metadata.as_ref()?.get("thread_id")?.as_str())
            .filter(|thread_id| !thread_id.is_empty())
            .ok_or_else(|| McpError::invalid_params("missing task metadata", None))?;
        let turn_id = metadata
            .get("turnId")
            .and_then(Value::as_str)
            .or_else(|| turn_metadata.as_ref()?.get("turn_id")?.as_str())
            .map_or_else(|| format!("mcp-turn-{}", Uuid::new_v4()), str::to_string);
        let call_id = metadata
            .get("callId")
            .and_then(Value::as_str)
            .map_or_else(|| format!("mcp-call-{}", Uuid::new_v4()), str::to_string);
        let params = DynamicToolCallParams {
            thread_id: thread_id.to_string(),
            turn_id,
            call_id,
            namespace: Some(dynamic_tools::NAMESPACE.to_string()),
            tool: request.name.into_owned(),
            arguments: Value::Object(request.arguments.unwrap_or_default()),
        };
        if params.tool == "workflow" {
            let handler = self.workflow_handler.as_ref().ok_or_else(|| {
                McpError::internal_error("workflow supervisor is unavailable", None)
            })?;
            return Ok(dynamic_response_to_mcp(handler.call(params).await));
        }
        let mut thread_start_params = self.thread_start_params.clone();
        thread_start_params.config.get_or_insert_default().insert(
            format!("mcp_servers.{}", dynamic_tools::NAMESPACE),
            self.server_config.clone(),
        );
        // Snapshot once: an in-flight call must never switch connections or replay a mutation.
        let (request_handle, app_event_tx) = self
            .connection
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| {
                McpError::internal_error("TUI is reconnecting; tool was not sent", None)
            })?;
        let response = dynamic_tools::execute(
            request_handle,
            params,
            thread_start_params,
            self.status_updates.subscribe(),
            Some(&app_event_tx),
        )
        .await;
        Ok(dynamic_response_to_mcp(response))
    }
}

fn dynamic_response_to_mcp(
    response: codex_app_server_protocol::DynamicToolCallResponse,
) -> rmcp::model::CallToolResponse {
    let content = response
        .content_items
        .into_iter()
        .map(|item| match item {
            DynamicToolCallOutputContentItem::InputText { text } => ContentBlock::text(text),
            DynamicToolCallOutputContentItem::InputImage { image_url } => {
                ContentBlock::text(image_url)
            }
            DynamicToolCallOutputContentItem::InputAudio { audio_url } => {
                ContentBlock::text(audio_url)
            }
        })
        .collect();
    if response.success {
        CallToolResult::success(content)
    } else {
        CallToolResult::error(content)
    }
    .into()
}
