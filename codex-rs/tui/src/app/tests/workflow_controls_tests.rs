use super::*;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::collections::HashSet;

#[tokio::test]
async fn resuming_workflow_returns_to_parent_chat() -> Result<()> {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    let temp = tempfile::tempdir()?;
    let requests = temp.path().join("requests.jsonl");
    let script = temp.path().join("bridge.mjs");
    std::fs::write(
        &script,
        format!("const log = {};\n", serde_json::to_string(&requests)?)
            + r#"import readline from 'node:readline';
import fs from 'node:fs';
for await (const line of readline.createInterface({input:process.stdin})) {
  const request=JSON.parse(line);
  fs.appendFileSync(log,JSON.stringify(request)+'\n');
  const result=request.method==='hello'?{protocolVersion:1}:request.method==='listRuns'?{runs:[]}:{status:'running'};
  process.stdout.write(JSON.stringify({id:request.id,ok:true,result})+'\n');
}"#,
    )?;
    let bridge =
        crate::ultracode_bridge::UltracodeBridge::spawn(crate::ultracode_bridge::BridgeLaunch {
            node: "node".into(),
            script,
            plugin_root: temp.path().to_path_buf(),
            cwd: temp.path().to_path_buf(),
            state_dir: temp.path().join("state"),
            models: serde_json::json!([]),
            plugins: serde_json::json!([]),
            web_search_available: false,
        })
        .await
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
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
    let overlay = crate::pager_overlay::WorkflowOverlay::new(
        serde_json::json!({"runs":[{"id":"run-1","status":"paused","phases":[],"workers":[]}]}),
        None,
    );
    app.overlay = Some(crate::pager_overlay::Overlay::Workflow(Box::new(overlay)));
    app.handle_backtrack_overlay_event(
        &mut crate::tui::test_support::make_test_tui()?,
        &mut app_server,
        TuiEvent::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)),
    )
    .await?;
    let overlay_closed = app.overlay.is_none();
    let calls = std::fs::read_to_string(requests)?
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    bridge
        .shutdown()
        .await
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
    app_server.shutdown().await?;
    assert!(
        overlay_closed,
        "Successful resume must dismiss the workflow dialog"
    );
    let resume_calls: Vec<_> = calls
        .iter()
        .filter(|call| call["method"] == "resumeRun")
        .collect();
    assert_eq!(resume_calls.len(), 1);
    assert_eq!(
        resume_calls[0]["params"],
        serde_json::json!({"runId":"run-1"})
    );
    Ok(())
}
