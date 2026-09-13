use super::*;
use crate::app_event::WorkflowEvent;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::HashMap;
use std::collections::HashSet;

const FILE_SOURCE: &str = "export const meta = {name: 'file'};\nreturn 'file source';";
const SAVED_SOURCE: &str = "export const meta = {name: 'saved'};\nreturn 'saved source';";
const RESUMED_SOURCE: &str = "export const meta = {name: 'resumed'};\nreturn 'resumed source';";

fn preview_text(app: &mut App) -> String {
    let area = ratatui::layout::Rect::new(0, 0, 80, 12);
    let mut buffer = ratatui::buffer::Buffer::empty(area);
    let Some(Overlay::Static(overlay)) = app.overlay.as_mut() else {
        panic!("Expected read-only source pager");
    };
    overlay.render(area, &mut buffer);
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn preview_app_server(config: &Config, root: &Path) -> Result<AppServerSession> {
    let binary = codex_utils_cargo_bin::cargo_bin("codex-tui")?;
    #[cfg(unix)]
    let sandbox_alias = {
        let alias = root.join("codex-linux-sandbox");
        std::os::unix::fs::symlink(&binary, &alias)?;
        Some(alias)
    };
    #[cfg(not(unix))]
    let sandbox_alias = None;
    let runtime_paths = codex_exec_server::ExecServerRuntimePaths::new(binary, sandbox_alias)?;
    let environment_manager = Arc::new(
        EnvironmentManager::create_for_tests(/*exec_server_url*/ None, Some(runtime_paths)).await,
    );
    let state_db =
        crate::init_state_db_for_app_server_target(config, &crate::AppServerTarget::Embedded)
            .await?;
    crate::start_app_server_for_picker(
        config,
        &crate::AppServerTarget::Embedded,
        state_db,
        environment_manager,
    )
    .await
}

async fn preview_bridge(root: &Path) -> Result<crate::ultracode_bridge::UltracodeBridge> {
    let script = root.join("bridge.mjs");
    std::fs::write(root.join("saved.js"), SAVED_SOURCE)?;
    std::fs::write(root.join("resumed.js"), RESUMED_SOURCE)?;
    std::fs::write(root.join("file.js"), FILE_SOURCE)?;
    std::fs::write(
        &script,
        r#"import readline from 'node:readline';
import fs from 'node:fs';
import crypto from 'node:crypto';
const hash=source=>crypto.createHash('sha256').update(source).digest('hex');
let selected;
for await (const line of readline.createInterface({input:process.stdin})) {
  const request=JSON.parse(line);
  fs.appendFileSync('requests.jsonl',JSON.stringify(request)+'\n');
  try {
    let result={};
    if(request.method==='hello')result={protocolVersion:1};
    if(request.method==='listRuns')result={runs:[]};
    if(request.method==='listSavedWorkflows') {
      const digest=hash(fs.readFileSync('saved.js','utf8'));
      selected={name:'saved',workflowId:digest,digest};result={workflows:[selected]};
    }
    if(request.method==='readSavedSource'||request.method==='runSaved') {
      const source=fs.readFileSync('saved.js','utf8');
      if(!selected||selected.workflowId!==request.params.workflowId)throw Error('Saved workflow not found');
      if(hash(source)!==selected.digest)throw Error('Saved workflow changed');
      result={workflowId:selected.workflowId,source,digest:selected.digest};
    }
    if(request.method==='inspectRun') {
      const source=fs.readFileSync('resumed.js','utf8');
      result={id:request.params.runId,source,sourceDigest:hash(source)};
    }
    if(request.method==='validateSource')result={digest:hash(request.params.source)};
    if(['runSource','runSaved','resumeRun'].includes(request.method))result={runId:'launched'};
    process.stdout.write(JSON.stringify({id:request.id,ok:true,result})+'\n');
  }catch(error){process.stdout.write(JSON.stringify({id:request.id,ok:false,error:{code:'CONFLICT',message:error.message}})+'\n');}
}"#,
    )?;
    crate::ultracode_bridge::UltracodeBridge::spawn(crate::ultracode_bridge::BridgeLaunch {
        node: "node".into(),
        script,
        plugin_root: root.into(),
        cwd: root.into(),
        state_dir: root.join("state"),
        models: json!([]),
        plugins: json!([]),
        web_search_available: false,
    })
    .await
    .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))
}

#[tokio::test]
async fn workflow_consent_preview_shows_selected_source_for_every_launch_form() -> Result<()> {
    for (arguments, expected) in [
        (json!({"script": "return 'inline';"}), "return 'inline';"),
        (
            json!({"scriptPath":"file.js","script":"ignored"}),
            FILE_SOURCE,
        ),
        (json!({"name":"saved","script":"ignored"}), SAVED_SOURCE),
        (
            json!({"name":"saved","scriptPath":"missing.js","script":"ignored"}),
            SAVED_SOURCE,
        ),
        (
            json!({"resumeFromRunId":"previous","scriptPath":"file.js","script":"ignored"}),
            FILE_SOURCE,
        ),
        (
            json!({"resumeFromRunId":"previous","name":"ignored"}),
            RESUMED_SOURCE,
        ),
        (
            json!({"resumeFromRunId":"previous","script":"return 'override';"}),
            "return 'override';",
        ),
    ] {
        let (mut app, mut events, _ops) = make_test_app_with_channels().await;
        let root = tempdir()?;
        app.config.cwd = root.path().canonicalize()?.abs();
        app.config.codex_home = root.path().join("home").abs();
        app.config
            .permissions
            .approval_policy
            .set(AskForApproval::OnRequest.to_core())?;
        app.config.approvals_reviewer = ApprovalsReviewer::User;
        let bridge = preview_bridge(root.path()).await?;
        let mut app_server = preview_app_server(&app.config, root.path()).await?;
        let started = app_server.start_thread(&app.config).await?;
        let thread_id = started.session.thread_id;
        app.chat_widget.handle_thread_session(started.session);
        app.workflow_sessions.insert(
            thread_id.to_string(),
            crate::app::workflow::WorkflowSession {
                bridge: bridge.clone(),
                pending_workers: HashMap::new(),
                consent: crate::workflow_consent::WorkflowConsentStore::load(root.path())?,
                reported_runs: HashSet::new(),
                active_runs: false,
            },
        );
        let mut tui = crate::tui::test_support::make_test_tui()?;
        while events.try_recv().is_ok() {}
        app.handle_workflow_event(
            &mut tui,
            &mut app_server,
            WorkflowEvent::ToolCall {
                request_id: AppServerRequestId::Integer(80),
                params: codex_app_server_protocol::DynamicToolCallParams {
                    thread_id: thread_id.to_string(),
                    turn_id: "turn".into(),
                    call_id: "call".into(),
                    namespace: None,
                    tool: "workflow".into(),
                    arguments,
                },
            },
        )
        .await?;
        // Wrap through Cancel so the disabled Remember item cannot change navigation.
        for code in [KeyCode::Up, KeyCode::Up, KeyCode::Enter] {
            app.chat_widget
                .handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
        }
        let event = events.try_recv().expect("source preview event");
        let AppEvent::Workflow(WorkflowEvent::ViewScript {
            source,
            thread_id: origin,
        }) = event
        else {
            panic!("Expected source preview");
        };
        let pending_consent = render_bottom_popup(&app.chat_widget, 100);
        let history_count = app.transcript_cells.len();
        app.handle_workflow_event(
            &mut tui,
            &mut app_server,
            WorkflowEvent::ViewScript {
                thread_id: ThreadId::new().to_string(),
                source: source.clone(),
            },
        )
        .await?;
        let wrong_thread_hidden = app.overlay.is_none();
        app.handle_workflow_event(
            &mut tui,
            &mut app_server,
            WorkflowEvent::ViewScript {
                thread_id: origin.clone(),
                source: source.clone(),
            },
        )
        .await?;
        let rendered = preview_text(&mut app);
        app.handle_backtrack_overlay_event(
            &mut tui,
            &mut app_server,
            TuiEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        )
        .await?;
        let returned_to_consent =
            app.overlay.is_none() && render_bottom_popup(&app.chat_widget, 100) == pending_consent;
        let ui_only = app.transcript_cells.len() == history_count
            && events.try_recv().is_err()
            && !app.backtrack.overlay_preview_active;
        for _ in 0..if expected == SAVED_SOURCE { 2 } else { 1 } {
            app.chat_widget
                .handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        }
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let AppEvent::Workflow(consent) = events.try_recv().expect("pending consent choice") else {
            panic!("Expected preserved consent");
        };
        let launch_result = app
            .handle_workflow_event(&mut tui, &mut app_server, consent)
            .await;
        let completion = events.try_recv().ok();
        let requests: Vec<serde_json::Value> =
            std::fs::read_to_string(root.path().join("requests.jsonl"))?
                .lines()
                .map(serde_json::from_str)
                .collect::<std::result::Result<_, _>>()?;
        let launched = requests
            .iter()
            .rev()
            .find(|request| {
                matches!(
                    request["method"].as_str(),
                    Some("runSource" | "runSaved" | "resumeRun")
                )
            })
            .cloned();
        bridge
            .shutdown()
            .await
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        app_server.shutdown().await?;
        assert_eq!(source, expected);
        assert_eq!(origin, thread_id.to_string());
        assert!(wrong_thread_hidden);
        assert!(returned_to_consent);
        assert!(
            ui_only,
            "Source inspection must not enter model history or decide consent"
        );
        launch_result?;
        assert!(
            matches!(
                completion,
                Some(AppEvent::DynamicToolCallCompleted {
                    request_id: AppServerRequestId::Integer(80),
                    response: codex_app_server_protocol::DynamicToolCallResponse {
                        success: true,
                        ..
                    },
                })
            ),
            "Unchanged preview must complete the original tool call successfully"
        );
        let launched = launched.expect("unchanged preview launches");
        if expected != SAVED_SOURCE {
            assert_eq!(launched["params"]["source"], source);
        } else {
            let preview_id = requests
                .iter()
                .find(|request| request["method"] == "readSavedSource")
                .unwrap()["params"]["workflowId"]
                .clone();
            assert_eq!(launched["params"]["workflowId"], preview_id);
        }
        if expected == SAVED_SOURCE {
            insta::assert_snapshot!("workflow_saved_source_preview", rendered);
        }
    }
    Ok(())
}

#[tokio::test]
async fn workflow_consent_rejects_source_changes_before_launch() -> Result<()> {
    for (arguments, file) in [
        (json!({"scriptPath":"file.js"}), "file.js"),
        (json!({"name":"saved"}), "saved.js"),
        (json!({"resumeFromRunId":"previous"}), "resumed.js"),
    ] {
        let (mut app, mut events, _ops) = make_test_app_with_channels().await;
        let root = tempdir()?;
        app.config.cwd = root.path().canonicalize()?.abs();
        app.config.codex_home = root.path().join("home").abs();
        app.config
            .permissions
            .approval_policy
            .set(AskForApproval::OnRequest.to_core())?;
        app.config.approvals_reviewer = ApprovalsReviewer::User;
        let bridge = preview_bridge(root.path()).await?;
        let mut app_server = preview_app_server(&app.config, root.path()).await?;
        let started = app_server.start_thread(&app.config).await?;
        let thread_id = started.session.thread_id;
        app.chat_widget.handle_thread_session(started.session);
        app.workflow_sessions.insert(
            thread_id.to_string(),
            crate::app::workflow::WorkflowSession {
                bridge: bridge.clone(),
                pending_workers: HashMap::new(),
                consent: crate::workflow_consent::WorkflowConsentStore::load(root.path())?,
                reported_runs: HashSet::new(),
                active_runs: false,
            },
        );
        let mut tui = crate::tui::test_support::make_test_tui()?;
        while events.try_recv().is_ok() {}
        app.handle_workflow_event(
            &mut tui,
            &mut app_server,
            WorkflowEvent::ToolCall {
                request_id: AppServerRequestId::Integer(81),
                params: codex_app_server_protocol::DynamicToolCallParams {
                    thread_id: thread_id.to_string(),
                    turn_id: "turn".into(),
                    call_id: "call".into(),
                    namespace: None,
                    tool: "workflow".into(),
                    arguments,
                },
            },
        )
        .await?;
        std::fs::write(root.path().join(file), "return 'changed';")?;
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let AppEvent::Workflow(consent) = events.try_recv().expect("consent event") else {
            panic!("Expected workflow consent");
        };
        let result = app
            .handle_workflow_event(&mut tui, &mut app_server, consent)
            .await;
        let requests = std::fs::read_to_string(root.path().join("requests.jsonl"))?;
        bridge
            .shutdown()
            .await
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        app_server.shutdown().await?;
        assert!(
            result
                .as_ref()
                .err()
                .is_some_and(|error| error.to_string().contains("changed")),
            "Expected changed-source rejection for {file}, got {result:?}"
        );
        let launches: Vec<serde_json::Value> = requests
            .lines()
            .map(serde_json::from_str::<serde_json::Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|request| {
                matches!(
                    request["method"].as_str(),
                    Some("runSource" | "runSaved" | "resumeRun")
                )
            })
            .collect();
        assert_eq!(launches, Vec::<serde_json::Value>::new());
    }
    Ok(())
}

#[tokio::test]
async fn saved_slash_preview_keeps_original_thread_and_catalog_identity() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let root = tempdir()?;
    app.config.cwd = root.path().canonicalize()?.abs();
    app.config.codex_home = root.path().join("home").abs();
    app.config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())?;
    let bridge = preview_bridge(root.path()).await?;
    let mut app_server = preview_app_server(&app.config, root.path()).await?;
    let started = app_server.start_thread(&app.config).await?;
    let origin = started.session.thread_id;
    app.chat_widget.handle_thread_session(started.session);
    app.workflow_sessions.insert(
        origin.to_string(),
        crate::app::workflow::WorkflowSession {
            bridge: bridge.clone(),
            pending_workers: HashMap::new(),
            consent: crate::workflow_consent::WorkflowConsentStore::load(root.path())?,
            reported_runs: HashSet::new(),
            active_runs: false,
        },
    );
    let mut tui = crate::tui::test_support::make_test_tui()?;
    while events.try_recv().is_ok() {}
    app.handle_workflow_event(
        &mut tui,
        &mut app_server,
        WorkflowEvent::RunSaved {
            name: "saved".into(),
            args: Some("original args".into()),
        },
    )
    .await?;
    let popup = render_bottom_popup(&app.chat_widget, 100);
    for code in [KeyCode::Up, KeyCode::Up, KeyCode::Enter] {
        app.chat_widget
            .handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
    }
    let AppEvent::Workflow(WorkflowEvent::ViewScript { thread_id, source }) =
        events.try_recv().expect("saved source preview")
    else {
        panic!("Expected saved source preview");
    };
    assert_eq!(thread_id, origin.to_string());
    assert_eq!(source, SAVED_SOURCE);
    app.handle_workflow_event(
        &mut tui,
        &mut app_server,
        WorkflowEvent::ViewScript { thread_id, source },
    )
    .await?;
    app.handle_backtrack_overlay_event(
        &mut tui,
        &mut app_server,
        TuiEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
    )
    .await?;
    for code in [KeyCode::Up, KeyCode::Up, KeyCode::Enter] {
        app.chat_widget
            .handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
    }
    let AppEvent::Workflow(consent @ WorkflowEvent::RunSavedConsent { .. }) =
        events.try_recv().expect("saved consent")
    else {
        panic!("Expected saved consent");
    };
    let other = app_server.start_thread(&app.config).await?;
    app.chat_widget.handle_thread_session(other.session);
    while events.try_recv().is_ok() {}
    let before = std::fs::read_to_string(root.path().join("requests.jsonl"))?
        .lines()
        .count();
    let result = app
        .handle_workflow_event(&mut tui, &mut app_server, consent)
        .await;
    let after: Vec<serde_json::Value> =
        std::fs::read_to_string(root.path().join("requests.jsonl"))?
            .lines()
            .skip(before)
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?;
    bridge
        .shutdown()
        .await
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
    app_server.shutdown().await?;
    result?;
    assert!(
        after
            .iter()
            .all(|request| request["method"] != "listSavedWorkflows")
    );
    let launched = after
        .iter()
        .find(|request| request["method"] == "runSaved")
        .expect("saved workflow launch");
    assert_eq!(launched["params"]["args"], "original args");
    assert_eq!(
        app.workflow_sessions.len(),
        1,
        "Approval must not attach a workflow to the newly displayed thread"
    );
    insta::assert_snapshot!("workflow_saved_consent", popup);
    Ok(())
}
