use super::*;
use crate::app_event::WorkflowEvent;
use codex_app_server_protocol::WorkflowSaveResponse;

#[cfg(unix)]
#[tokio::test]
async fn workflow_save_returns_before_pending_approval_and_closes_overlay() -> Result<()> {
    use std::collections::HashMap;
    use std::collections::HashSet;
    use std::time::Duration;
    use tempfile::tempdir;

    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    app.config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())?;
    app.config
        .permissions
        .set_permission_profile(PermissionProfile::read_only())?;
    app.chat_widget
        .set_approval_policy(AskForApproval::OnRequest);
    app.chat_widget
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::legacy(
            PermissionProfile::read_only(),
        ));

    let temp = tempdir()?;
    // Match a resolved workspace root; macOS temporary paths can traverse /var.
    app.config.cwd = codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(
        temp.path().canonicalize()?,
    )?;
    let bridge_script = temp.path().join("bridge.mjs");
    std::fs::write(
        &bridge_script,
        r#"import readline from 'node:readline';import {createHash} from 'node:crypto';
const source='export const meta={name:"saved"};';
const digest=createHash('sha256').update(source).digest('hex');
for await(const line of readline.createInterface({input:process.stdin})){const request=JSON.parse(line);const result=request.method==='hello'?{protocolVersion:1}:request.method==='prepareSave'?{source,digest}:{};process.stdout.write(JSON.stringify({id:request.id,ok:true,result})+'\n');}"#,
    )?;
    let bridge =
        crate::workflow_bridge::WorkflowBridge::spawn(crate::workflow_bridge::BridgeLaunch {
            node: "node".into(),
            script: bridge_script,
            plugin_root: temp.path().to_path_buf(),
            cwd: temp.path().to_path_buf(),
            state_dir: temp.path().join("state"),
            models: serde_json::json!([]),
            plugins: serde_json::json!([]),
            web_search_available: false,
        })
        .await
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
    let binary = codex_utils_cargo_bin::cargo_bin("codex-tui")?;
    let sandbox_alias = temp.path().join("codex-linux-sandbox");
    std::os::unix::fs::symlink(&binary, &sandbox_alias)?;
    let runtime_paths =
        codex_exec_server::ExecServerRuntimePaths::new(binary, Some(sandbox_alias))?;
    let environment_manager = Arc::new(
        EnvironmentManager::create_for_tests(/*exec_server_url*/ None, Some(runtime_paths)).await,
    );
    let state_db =
        crate::init_state_db_for_app_server_target(&app.config, &crate::AppServerTarget::Embedded)
            .await?;
    let mut app_server = crate::start_app_server_for_picker(
        &app.config,
        &crate::AppServerTarget::Embedded,
        state_db,
        environment_manager,
    )
    .await?;
    let started = app_server.start_thread(&app.config).await?;
    let thread_id = started.session.thread_id;
    app.primary_thread_id = Some(thread_id);
    app.active_thread_id = Some(thread_id);
    app.chat_widget.handle_thread_session(started.session);
    app.workflow_sessions.insert(
        thread_id.to_string(),
        crate::app::workflow::WorkflowSession {
            bridge: bridge.clone(),
            pending_workers: HashMap::new(),
            consent: crate::workflow_consent::WorkflowConsentStore::load(&app.config.codex_home)?,
            reported_runs: HashSet::new(),
            active_runs: true,
        },
    );
    let mut overlay = crate::pager_overlay::WorkflowOverlay::new(
        serde_json::json!({
            "runs": [{"id":"run-1","status":"completed","phases":[],"workers":[]}]
        }),
        None,
    );
    overlay.view.state.screen = crate::workflow_view::WorkflowScreen::Save;
    overlay.view.state.save_name = "saved".to_string();
    overlay.view.state.save_scope = "project".to_string();
    app.overlay = Some(crate::pager_overlay::Overlay::Workflow(Box::new(overlay)));
    let mut tui = crate::tui::test_support::make_test_tui()?;

    tokio::time::timeout(
        Duration::from_secs(1),
        app.handle_backtrack_overlay_event(
            &mut tui,
            &mut app_server,
            TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ),
    )
    .await??;
    assert!(app.overlay.is_none());

    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                event = app_server.next_event() => {
                    if let Some(codex_app_server_client::AppServerEvent::ServerRequest(request)) = event
                        && matches!(request.as_ref(), codex_app_server_protocol::ServerRequest::FileChangeRequestApproval { .. })
                    {
                        break request;
                    }
                }
                event = events.recv() => {
                    if let Some(AppEvent::Workflow(WorkflowEvent::SaveCompleted { result, .. })) = event {
                        panic!("Save completed before approval was observed: {result:?}");
                    }
                }
            }
        }
    })
    .await?;
    app_server
        .resolve_server_request(
            approval.id().clone(),
            serde_json::json!({"decision":"accept"}),
        )
        .await?;
    let completion = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(event) = events.recv().await
                && matches!(
                    event,
                    AppEvent::Workflow(WorkflowEvent::SaveCompleted { .. })
                )
            {
                break event;
            }
        }
    })
    .await?;
    let AppEvent::Workflow(WorkflowEvent::SaveCompleted {
        result: Ok(saved), ..
    }) = completion
    else {
        panic!("Expected successful workflow save completion: {completion:?}");
    };
    pretty_assertions::assert_eq!(
        std::fs::read_to_string(saved.path)?,
        "export const meta={name:\"saved\"};"
    );
    bridge
        .shutdown()
        .await
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn accepted_workflow_save_completion_refreshes_catalog() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let thread_id = ThreadId::new();

    app.handle_workflow_event(
        &mut crate::tui::test_support::make_test_tui()?,
        &mut app_server,
        WorkflowEvent::SaveCompleted {
            thread_id: thread_id.to_string(),
            result: Ok(WorkflowSaveResponse {
                path: "/repo/.codex/workflows/saved.js".to_string(),
            }),
        },
    )
    .await?;

    assert_matches!(
        events.try_recv(),
        Ok(AppEvent::Workflow(WorkflowEvent::LoadCatalog { thread_id: id }))
            if id == thread_id.to_string()
    );
    assert_matches!(
        events.try_recv(),
        Ok(AppEvent::Workflow(WorkflowEvent::Refresh))
    );
    app_server.shutdown().await?;
    Ok(())
}
