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
        crate::workflow_bridge::WorkflowBridge::spawn(crate::workflow_bridge::BridgeLaunch {
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

#[tokio::test]
async fn inspecting_nonfirst_workflow_preserves_picker_and_control_targets() -> Result<()> {
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
  const result=request.method==='hello'?{protocolVersion:1}:request.method==='inspectRun'?{id:request.params.runId,name:'second',description:'inspected',status:'running',phases:[],workers:[]}:{runs:[]};
  process.stdout.write(JSON.stringify({id:request.id,ok:true,result})+'\n');
}"#,
    )?;
    let bridge =
        crate::workflow_bridge::WorkflowBridge::spawn(crate::workflow_bridge::BridgeLaunch {
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
    let mut overlay = crate::pager_overlay::WorkflowOverlay::new(
        serde_json::json!({"runs":[{"id":"run-1","name":"first","status":"running","phases":[],"workers":[]},{"id":"run-2","name":"second","status":"running","phases":[],"workers":[]}]}),
        None,
    );
    overlay.view.state.run = 1;
    app.overlay = Some(crate::pager_overlay::Overlay::Workflow(Box::new(overlay)));
    app.handle_backtrack_overlay_event(
        &mut crate::tui::test_support::make_test_tui()?,
        &mut app_server,
        TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
    )
    .await?;
    let Some(crate::pager_overlay::Overlay::Workflow(overlay)) = app.overlay.as_mut() else {
        panic!("Expected workflow overlay");
    };
    let area = ratatui::layout::Rect::new(0, 0, 120, 40);
    let mut buffer = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&overlay.view, area, &mut buffer);
    let inspected_text = buffer
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect::<String>();
    let back = overlay
        .view
        .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let returned_to_picker =
        overlay.view.state.screen == crate::workflow_view::WorkflowScreen::Picker;
    overlay.view.state.screen = crate::workflow_view::WorkflowScreen::Overview;
    let runs = serde_json::json!([
        {"id":"run-1","name":"first","status":"running","phases":[],"workers":[]},
        {"id":"run-2","name":"second","status":"running","phases":[],"workers":[]}
    ]);
    overlay.update(serde_json::json!({"runs":runs}));
    let stop = overlay
        .view
        .handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    let pause = overlay
        .view
        .handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
    overlay.update(serde_json::json!({"runs":[runs[1].clone(),runs[0].clone()]}));
    let reordered_stop = overlay
        .view
        .handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    overlay
        .view
        .handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
    let save = overlay
        .view
        .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    bridge
        .shutdown()
        .await
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
    app_server.shutdown().await?;
    assert_eq!(back, None);
    assert!(
        inspected_text.contains("inspected"),
        "Inspected content was not merged"
    );
    assert!(
        returned_to_picker,
        "Inspecting one run must retain the picker"
    );
    use crate::workflow_view::WorkflowAction;
    assert_eq!(
        stop,
        Some(WorkflowAction::StopRun {
            run_id: "run-2".into(),
            worker_id: None
        })
    );
    assert_eq!(
        pause,
        Some(WorkflowAction::PauseRun {
            run_id: "run-2".into()
        })
    );
    assert_eq!(
        reordered_stop,
        Some(WorkflowAction::StopRun {
            run_id: "run-2".into(),
            worker_id: None
        })
    );
    assert!(matches!(save, Some(WorkflowAction::Save {run_id,..}) if run_id == "run-2"));
    Ok(())
}
