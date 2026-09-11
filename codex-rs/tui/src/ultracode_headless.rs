//! Explicit headless frontend for the native workflow supervisor.
use crate::ultracode_bridge::UltracodeBridge;
use codex_app_server_client::AppServerClient;
use codex_app_server_client::RemoteAppServerEndpoint;
use codex_app_server_client::legacy_core::config::Config;
use codex_app_server_protocol::ThreadStartParams;
use serde_json::Value;
use serde_json::json;
use std::io;
use std::time::Duration;

/// Authenticated control connection for a CLI-selected headless workflow route.
#[derive(Clone)]
pub struct HeadlessWorkflowController(UltracodeBridge);

impl HeadlessWorkflowController {
    /// Inspect workflow completion state or deliberately stop this parent's workflows.
    pub async fn control(&self, parent_thread_id: String, action: String) -> io::Result<Value> {
        self.0
            .request(
                "headless",
                json!({"parentThreadId":parent_thread_id,"action":action}),
                Duration::from_secs(30),
            )
            .await
            .map_err(io::Error::other)
    }
}

/// Prepare the daemon and workflow listener before an explicitly opted-in exec parent starts.
pub async fn prepare(
    config: Config,
    thread_start_params: ThreadStartParams,
) -> io::Result<(AppServerClient, Value, HeadlessWorkflowController)> {
    if config.disable_workflows {
        return Err(io::Error::other("Workflows are disabled by configuration."));
    }
    if config
        .mcp_servers
        .get()
        .contains_key(crate::dynamic_tools::NAMESPACE)
    {
        return Err(io::Error::other(
            "a user-configured MCP server already owns the codex_tui namespace",
        ));
    }
    let socket_path = codex_app_server_daemon::ensure_workflow_backend(
        &config.codex_home,
        &std::env::current_exe()?,
    )
    .await
    .map_err(io::Error::other)?;
    let socket_path = codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(socket_path)?;
    let root = std::env::var_os("ULTRACODE_PLUGIN_ROOT")
        .or_else(|| std::env::var_os("CODEX_PLUGIN_ROOT"))
        .ok_or_else(|| io::Error::other("frontend plugin root is unavailable"))?;
    let root = std::path::PathBuf::from(root).canonicalize()?;
    let supervisor = crate::ultracode_host::connect(&config.codex_home).await?;
    let mcp = supervisor.request("configure", json!({"threadStartParams":thread_start_params,"pluginRoot":root,"frontend":"headless"}), Duration::from_secs(30))
        .await.map_err(io::Error::other)?;
    if let Some(requirements) = config
        .config_layer_stack
        .requirements()
        .mcp_servers
        .as_ref()
    {
        let requirement = requirements
            .value
            .get(crate::dynamic_tools::NAMESPACE)
            .ok_or_else(|| {
                io::Error::other("managed MCP requirements do not permit the native workflow host")
            })?;
        let raw: codex_config::RawMcpServerConfig = serde_json::from_value(mcp.clone())?;
        let configured = codex_config::McpServerConfig::try_from(raw).map_err(io::Error::other)?;
        if !configured.matches_requirement(requirement) {
            return Err(io::Error::other(
                "managed MCP requirements do not permit the native workflow host",
            ));
        }
    }
    let client = AppServerClient::Remote(
        codex_app_server_client::RemoteAppServerClient::connect(
            codex_app_server_client::RemoteAppServerConnectArgs {
                endpoint: RemoteAppServerEndpoint::UnixSocket { socket_path },
                client_name: "codex_exec".into(),
                client_version: env!("CARGO_PKG_VERSION").into(),
                experimental_api: true,
                mcp_server_openai_form_elicitation: false,
                opt_out_notification_methods: Vec::new(),
                channel_capacity: codex_app_server_client::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
            },
        )
        .await?,
    );
    Ok((client, mcp, HeadlessWorkflowController(supervisor)))
}
