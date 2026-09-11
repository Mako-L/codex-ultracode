use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::WorkflowAuthorityCaptureResponse;
use codex_app_server_protocol::WorkflowWorkerStartResponse;
use codex_app_server_protocol::WorkflowWorkspacePrepareResponse;
use codex_protocol::openai_models::ReasoningEffort;
use core_test_support::responses;
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
async fn worker_start_attaches_requester_before_submitting_first_turn() -> Result<()> {
    let model_server = responses::start_mock_server().await;
    // The built-in provider negotiates WebSocket first; this fixture serves HTTP SSE.
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path_regex(".*/responses$"))
        .respond_with(wiremock::ResponseTemplate::new(426))
        .mount(&model_server)
        .await;
    let worker_response = responses::mount_sse_once(
        &model_server,
        responses::sse(vec![
            responses::ev_response_created("worker-response"),
            responses::ev_assistant_message("worker-message", "worker complete"),
            responses::ev_completed("worker-response"),
        ]),
    )
    .await;
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            "model = \"gpt-5.6-luna\"\nmodel_provider = \"openai\"\nopenai_base_url = \"{}/v1\"\napproval_policy = \"never\"\nsandbox_mode = \"read-only\"\n",
            model_server.uri(),
        ),
    )?;
    let mut server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", Some("test-workflow-key"))])
        .build_initialized()
        .await?;
    let parent = server
        .start_thread(ThreadStartParams {
            cwd: Some(project.path().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?
        .thread;
    let authority: WorkflowAuthorityCaptureResponse = request(
        &mut server,
        "workflow/authority/capture",
        json!({"parentThreadId":parent.id,"allowIsolatedWorkspaces":false}),
    )
    .await?;
    let workspace: WorkflowWorkspacePrepareResponse = request(
        &mut server,
        "workflow/workspace/prepare",
        json!({
            "runId":"run-1","workerId":"worker-1","agentType":"general-purpose",
            "authorityRef":authority.authority_ref,"authorityDigest":authority.authority_digest,
            "isolation":null,"requestedCwd":project.path(),"previous":null
        }),
    )
    .await?;
    let started: WorkflowWorkerStartResponse = request(
        &mut server,
        "workflow/worker/start",
        json!({
            "runId":"run-1","workerId":"worker-1","parentThreadId":parent.id,
            "authorityRef":authority.authority_ref,"authorityGeneration":workspace.authority_generation,
            "authorityDigest":authority.authority_digest,"prompt":"finish the worker",
            "model":"gpt-5.6-luna","effort":ReasoningEffort::Low,
            "modelExplicit":false,"effortExplicit":false,"agentType":"general-purpose",
            "roleDigest":workspace.role_digest,"readOnly":false,
            "workspace":{"workspaceId":workspace.workspace_id,"cwd":workspace.cwd,"isolated":workspace.isolated,"baseCommit":workspace.base_commit},
            "schema":null,"resumeThreadId":null
        }),
    )
    .await?;

    assert!(!started.thread_id.is_empty());
    let completed = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        server.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let completed = completed.params.expect("completion parameters");
    assert_eq!(completed["threadId"], started.thread_id);
    assert_eq!(completed["turn"]["id"], started.turn_id);
    assert_eq!(completed["turn"]["status"], "completed");
    assert_eq!(worker_response.requests().len(), 1);
    Ok(())
}
