use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SandboxPolicy;
use codex_app_server_protocol::ThreadSettingsUpdateParams;
use codex_app_server_protocol::ThreadSettingsUpdateResponse;
use codex_app_server_protocol::ThreadSettingsUpdatedNotification;
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

async fn rejected_start(server: &mut TestAppServer, params: serde_json::Value) -> Result<String> {
    let id = server
        .send_request("workflow/worker/start", Some(params))
        .await?;
    let response = server
        .read_stream_until_error_message(RequestId::Integer(id))
        .await?;
    assert_eq!(response.error.code, -32600);
    Ok(response.error.message)
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
    let start_params = json!({
        "runId":"run-1","workerId":"worker-1","parentThreadId":parent.id,
        "authorityRef":authority.authority_ref,"authorityGeneration":workspace.authority_generation,
        "authorityDigest":authority.authority_digest,"prompt":"finish the worker",
        "model":"gpt-5.6-luna","effort":ReasoningEffort::Low,
        "modelExplicit":false,"effortExplicit":false,"agentType":"general-purpose",
        "roleDigest":workspace.role_digest,"readOnly":false,
        "workspace":{"workspaceId":workspace.workspace_id,"cwd":workspace.cwd,"isolated":workspace.isolated,"baseCommit":workspace.base_commit},
        "schema":null,"resumeThreadId":null
    });
    let started: WorkflowWorkerStartResponse =
        request(&mut server, "workflow/worker/start", start_params.clone()).await?;

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

    let mut stale = start_params.clone();
    stale["authorityDigest"] = json!("stale-authority");
    assert!(
        rejected_start(&mut server, stale)
            .await?
            .contains("authority")
    );
    let mut foreign = start_params.clone();
    foreign["workspace"]["cwd"] = json!(codex_home.path());
    assert!(!rejected_start(&mut server, foreign).await?.is_empty());

    // Keep the replacement active long enough to verify concurrent duplicates are rejected.
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path_regex(".*/responses$"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(responses::sse(vec![
                    responses::ev_response_created("replacement-response"),
                    responses::ev_assistant_message("replacement-message", "replacement complete"),
                    responses::ev_completed("replacement-response"),
                ]))
                .set_delay(std::time::Duration::from_secs(2)),
        )
        .with_priority(1)
        .expect(1)
        .up_to_n_times(1)
        .mount(&model_server)
        .await;
    let replacement: WorkflowWorkerStartResponse =
        request(&mut server, "workflow/worker/start", start_params.clone()).await?;
    assert_ne!(replacement.thread_id, started.thread_id);
    assert_eq!(
        rejected_start(&mut server, start_params.clone()).await?,
        "workflow worker launch is already owned"
    );
    let completed = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        server.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let completed = completed.params.expect("replacement completion parameters");
    assert_eq!(completed["threadId"], replacement.thread_id);
    assert_eq!(completed["turn"]["status"], "completed");

    responses::mount_sse_once(
        &model_server,
        responses::sse(vec![
            responses::ev_response_created("resumed-response"),
            responses::ev_assistant_message("resumed-message", "resumed complete"),
            responses::ev_completed("resumed-response"),
        ]),
    )
    .await;
    let mut resume_params = start_params;
    resume_params["resumeThreadId"] = json!(replacement.thread_id);
    let foreign_workspace = TempDir::new()?;
    let original_policy = SandboxPolicy::ReadOnly {
        network_access: false,
    };
    let requests_before_drift = model_server
        .received_requests()
        .await
        .expect("model requests")
        .into_iter()
        .filter(|request| request.method == "POST" && request.url.path().ends_with("/responses"))
        .count();
    for (cwd, reject_resume) in [(foreign_workspace.path(), true), (project.path(), false)] {
        let id = server
            .send_thread_settings_update_request(ThreadSettingsUpdateParams {
                thread_id: replacement.thread_id.clone(),
                cwd: Some(cwd.to_path_buf()),
                ..Default::default()
            })
            .await?;
        let _: ThreadSettingsUpdateResponse = server.read_response(id).await?;
        let updated: ThreadSettingsUpdatedNotification = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            server.read_notification("thread/settings/updated"),
        )
        .await??;
        assert_eq!(updated.thread_id, replacement.thread_id);
        assert_eq!(updated.thread_settings.cwd.as_path(), cwd);
        assert_eq!(updated.thread_settings.sandbox_policy, original_policy);
        if reject_resume {
            assert_eq!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    rejected_start(&mut server, resume_params.clone()),
                )
                .await??,
                "workflow worker resume lineage is invalid"
            );
        }
    }
    // Workflow children also retain the native permission constraint on settings updates.
    let id = server
        .send_thread_settings_update_request(ThreadSettingsUpdateParams {
            thread_id: replacement.thread_id.clone(),
            sandbox_policy: Some(SandboxPolicy::DangerFullAccess),
            ..Default::default()
        })
        .await?;
    let rejected = server
        .read_stream_until_error_message(RequestId::Integer(id))
        .await?;
    assert_eq!(rejected.error.code, -32600);
    assert_eq!(
        model_server
            .received_requests()
            .await
            .expect("model requests")
            .into_iter()
            .filter(|request| {
                request.method == "POST" && request.url.path().ends_with("/responses")
            })
            .count(),
        requests_before_drift
    );
    let resumed: WorkflowWorkerStartResponse =
        request(&mut server, "workflow/worker/start", resume_params).await?;
    assert_eq!(resumed.thread_id, replacement.thread_id);
    assert_ne!(resumed.turn_id, replacement.turn_id);
    let completed = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        server.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let completed = completed.params.expect("resumed completion parameters");
    assert_eq!(completed["threadId"], resumed.thread_id);
    assert_eq!(completed["turn"]["id"], resumed.turn_id);
    assert_eq!(completed["turn"]["status"], "completed");
    Ok(())
}
