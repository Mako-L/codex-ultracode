use super::*;
use codex_config::McpServerConfig;
use codex_config::RawMcpServerConfig;
use codex_core::config::ConfigBuilder;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn workflow_child_disables_only_workflow_in_native_task_tools() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = ConfigBuilder::default()
        .codex_home(directory.path().into())
        .build()
        .await
        .unwrap();
    let native: McpServerConfig = serde_json::from_value::<RawMcpServerConfig>(serde_json::json!({
        "url":"http://127.0.0.1:9000/mcp",
        "disabled_tools":["already-denied"],
        "http_headers":{"Authorization":"Bearer native-test"}
    }))
    .unwrap()
    .try_into()
    .unwrap();
    let mut servers = config.mcp_servers.get().clone();
    servers.insert("codex_tui".into(), native.clone());
    servers.insert("project_docs".into(), native.clone());
    config.mcp_servers.set(servers).unwrap();
    workflow_apply_role(&mut config, "default").await.unwrap();
    let mut expected = native.clone();
    expected
        .disabled_tools
        .as_mut()
        .unwrap()
        .push("workflow".into());
    assert_eq!(config.mcp_servers.get().get("codex_tui"), Some(&expected));
    assert_eq!(config.mcp_servers.get().get("project_docs"), Some(&native));
    workflow_apply_role(&mut config, "default").await.unwrap();
    assert_eq!(config.mcp_servers.get().get("codex_tui"), Some(&expected));
}
