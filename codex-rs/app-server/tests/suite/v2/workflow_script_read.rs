use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::WorkflowAuthorityCaptureResponse;
use codex_app_server_protocol::WorkflowScriptReadResponse;
use serde_json::json;
use tempfile::TempDir;

async fn request<T: serde::de::DeserializeOwned>(
    server: &mut TestAppServer,
    method: &str,
    params: serde_json::Value,
) -> Result<T> {
    let id = server.send_request(method, Some(params)).await?;
    server.read_response(id).await
}

#[tokio::test]
#[cfg(unix)]
async fn workflow_script_read_uses_idle_parent_sandbox_and_fresh_authority() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    let denied = TempDir::new()?;
    MockResponsesConfig::new("http://127.0.0.1:1").write(codex_home.path())?;
    use std::io::Write;
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(codex_home.path().join("config.toml"))?,
        "\n[permissions.boundary.filesystem]\n\"/\" = \"read\"\n{:?} = \"deny\"",
        denied.path().to_string_lossy()
    )?;
    std::fs::write(project.path().join("relative.js"), "relative")?;
    std::fs::write(denied.path().join("secret.js"), "secret")?;
    std::os::unix::fs::symlink(
        denied.path().join("secret.js"),
        project.path().join("escape.js"),
    )?;

    let mut server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(std::time::Duration::from_secs(60))
        .await?;
    let thread = server
        .start_thread(ThreadStartParams {
            cwd: Some(project.path().to_string_lossy().into_owned()),
            permissions: Some("boundary".into()),
            ..Default::default()
        })
        .await?
        .thread;
    let authority: WorkflowAuthorityCaptureResponse = request(
        &mut server,
        "workflow/authority/capture",
        json!({"parentThreadId":thread.id,"allowIsolatedWorkspaces":false}),
    )
    .await?;
    let read: WorkflowScriptReadResponse = request(
        &mut server,
        "workflow/script/read",
        json!({"parentThreadId":thread.id,"authorityRef":authority.authority_ref,"authorityDigest":authority.authority_digest,"scriptPath":"relative.js"}),
    )
    .await?;
    assert_eq!(read.source, "relative");
    assert_eq!(
        read.resolved_path,
        project
            .path()
            .join("relative.js")
            .canonicalize()?
            .to_string_lossy()
    );

    for script_path in [
        denied.path().join("secret.js"),
        project.path().join("escape.js"),
    ] {
        let id=server.send_request("workflow/script/read",Some(json!({"parentThreadId":thread.id,"authorityRef":authority.authority_ref,"authorityDigest":authority.authority_digest,"scriptPath":script_path}))).await?;
        let error = server
            .read_stream_until_error_message(RequestId::Integer(id))
            .await?;
        assert!(
            error.error.message.contains("denied")
                || error.error.message.contains("sandbox")
                || error.error.message.contains("Operation not permitted"),
            "{}",
            error.error.message
        );
    }
    let id=server.send_request("workflow/script/read",Some(json!({"parentThreadId":thread.id,"authorityRef":authority.authority_ref,"authorityDigest":"stale","scriptPath":"relative.js"}))).await?;
    let error = server
        .read_stream_until_error_message(RequestId::Integer(id))
        .await?;
    assert_eq!(
        error.error.message,
        "workflow authority does not match parent"
    );
    Ok(())
}
