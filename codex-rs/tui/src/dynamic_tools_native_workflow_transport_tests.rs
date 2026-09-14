use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn local_daemon_workflow_uses_native_requests_alongside_mcp_tasks() {
    let mcp_config = json!({"url":"http://127.0.0.1:1234/mcp"});
    let server = DynamicToolMcpServer {
        native_workflow_tool: true,
        connection: Arc::new(RwLock::new(None)),
        config: mcp_config.clone(),
        task: tokio::spawn(async {}),
        supervisor: None,
    };
    let mut params = ThreadStartParams::default();
    ThreadToolTransport::Mcp(Arc::new(server)).configure(&mut params);
    assert_eq!(
        params.dynamic_tools,
        Some(dynamic_tools::workflow_tool_specs())
    );
    assert_eq!(
        params.config.unwrap().get("mcp_servers.codex_tui"),
        Some(&mcp_config)
    );
}

#[tokio::test]
async fn generic_remote_mcp_does_not_gain_a_native_workflow_tool() {
    let server = DynamicToolMcpServer {
        native_workflow_tool: false,
        connection: Arc::new(RwLock::new(None)),
        config: json!({"url":"http://127.0.0.1:1234/mcp"}),
        task: tokio::spawn(async {}),
        supervisor: None,
    };
    let mut params = ThreadStartParams::default();
    ThreadToolTransport::Mcp(Arc::new(server)).configure(&mut params);
    assert_eq!(params.dynamic_tools, None);
    assert!(params.config.unwrap().contains_key("mcp_servers.codex_tui"));
}
