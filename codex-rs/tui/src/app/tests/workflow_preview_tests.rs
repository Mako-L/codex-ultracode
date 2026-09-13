use super::*;
use crate::app_event::WorkflowEvent;
use crate::app_event::WorkflowPreviewMode;
use crate::ultracode_source::WorkflowMetadata;
use crate::ultracode_source::WorkflowPhase;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::HashMap;
use std::collections::HashSet;

const FILE_SOURCE: &str = "export const meta = {name: 'file'};\nreturn 'file source';";
const SAVED_SOURCE: &str = "export const meta = {name: 'saved'};\nreturn 'saved source';";
const RESUMED_SOURCE: &str = "export const meta = {name: 'resumed'};\nreturn 'resumed source';";

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
    if(request.method==='validateSource')result={consent:{...(Object.hasOwn(request.params,'args')?{args:{text:String(request.params.args),needsGutter:false,withheld:false}}:{}),phases:[{title:'Inspect',detail:'Read selected bytes.',prompts:[]}],source:{text:request.params.source,withheld:false,originalLength:request.params.source.length}},digest:hash(request.params.source),meta:{name:'preview',title:'Preview workflow',description:'Preview the selected workflow source.',phases:[{title:'Inspect',detail:'Read the selected bytes.'}]}};
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

macro_rules! workflow_source_case {
    ($name:ident, $arguments:expr, $expected:expr) => {
        #[tokio::test]
        async fn $name() -> Result<()> {
            workflow_consent_preview_shows_selected_source_for_every_launch_form(
                $arguments, $expected,
            )
            .await
        }
    };
}

workflow_source_case!(
    workflow_consent_preview_inline,
    json!({"script": "return 'inline';"}),
    "return 'inline';"
);
workflow_source_case!(
    workflow_consent_preview_file,
    json!({"scriptPath":"file.js","script":"ignored"}),
    FILE_SOURCE
);
workflow_source_case!(
    workflow_consent_preview_saved,
    json!({"name":"saved","script":"ignored"}),
    SAVED_SOURCE
);
workflow_source_case!(
    workflow_consent_preview_saved_precedes_file,
    json!({"name":"saved","scriptPath":"missing.js","script":"ignored"}),
    SAVED_SOURCE
);
workflow_source_case!(
    workflow_consent_preview_resume_file,
    json!({"resumeFromRunId":"previous","scriptPath":"file.js","script":"ignored"}),
    FILE_SOURCE
);
workflow_source_case!(
    workflow_consent_preview_resume_journal,
    json!({"resumeFromRunId":"previous","name":"ignored"}),
    RESUMED_SOURCE
);
workflow_source_case!(
    workflow_consent_preview_resume_inline,
    json!({"resumeFromRunId":"previous","script":"return 'override';"}),
    "return 'override';"
);

async fn workflow_consent_preview_shows_selected_source_for_every_launch_form(
    arguments: serde_json::Value,
    expected: &str,
) -> Result<()> {
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
    let AppEvent::Workflow(WorkflowEvent::ToggleWorkflowPreview {
        consent,
        feedback_state,
        preview,
        mode: WorkflowPreviewMode::Raw,
    }) = event
    else {
        panic!("Expected source preview");
    };
    let source = preview.source.clone();
    let origin = preview.thread_id.clone();
    let history_count = app.transcript_cells.len();
    app.handle_workflow_event(
        &mut tui,
        &mut app_server,
        WorkflowEvent::ToggleWorkflowPreview {
            consent,
            feedback_state,
            preview,
            mode: WorkflowPreviewMode::Raw,
        },
    )
    .await?;
    let rendered = render_bottom_popup(&app.chat_widget, 100);
    let ui_only = app.transcript_cells.len() == history_count && events.try_recv().is_err();
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
                response: codex_app_server_protocol::DynamicToolCallResponse { success: true, .. },
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
        let rendered =
            rendered.replace(root.path().file_name().unwrap().to_str().unwrap(), "[TEMP]");
        insta::assert_snapshot!("workflow_saved_source_preview", rendered);
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
        let AppEvent::Workflow(WorkflowEvent::Consent {
            request_id,
            params,
            choice,
            preview,
            ..
        }) = events.try_recv().expect("consent event")
        else {
            panic!("Expected workflow consent");
        };
        let result = app
            .handle_workflow_event(
                &mut tui,
                &mut app_server,
                WorkflowEvent::Consent {
                    request_id,
                    params,
                    choice,
                    feedback: Some("keep validation strict".into()),
                    preview,
                },
            )
            .await;
        let AppEvent::DynamicToolCallCompleted { response, .. } =
            events.try_recv().expect("failed launch completion")
        else {
            panic!("Expected failed launch completion");
        };
        let requests = std::fs::read_to_string(root.path().join("requests.jsonl"))?;
        bridge
            .shutdown()
            .await
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        app_server.shutdown().await?;
        assert!(
            result.is_ok(),
            "Consent must settle its request: {result:?}"
        );
        assert!(!response.success);
        assert!(response.content_items.iter().any(|item| matches!(
            item,
            codex_app_server_protocol::DynamicToolCallOutputContentItem::InputText { text }
                if text.contains("changed")
        )));
        assert_eq!(
            response
                .content_items
                .iter()
                .filter(|item| matches!(
                    item,
                    codex_app_server_protocol::DynamicToolCallOutputContentItem::InputText { text }
                        if text == "Workflow consent feedback: keep validation strict"
                ))
                .count(),
            1
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
    assert!(popup.contains("args: original args"));
    let preview_requests: Vec<serde_json::Value> =
        std::fs::read_to_string(root.path().join("requests.jsonl"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?;
    assert!(preview_requests.iter().any(|request| {
        request["method"] == "validateSource" && request["params"]["args"] == "original args"
    }));

    for code in [KeyCode::Up, KeyCode::Up, KeyCode::Enter] {
        app.chat_widget
            .handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
    }
    let AppEvent::Workflow(WorkflowEvent::ToggleWorkflowPreview {
        consent,
        feedback_state,
        preview,
        mode: WorkflowPreviewMode::Raw,
    }) = events.try_recv().expect("saved source preview")
    else {
        panic!("Expected saved source preview");
    };
    assert_eq!(preview.thread_id, origin.to_string());
    assert_eq!(preview.source, SAVED_SOURCE);
    app.handle_workflow_event(
        &mut tui,
        &mut app_server,
        WorkflowEvent::ToggleWorkflowPreview {
            consent,
            feedback_state,
            preview,
            mode: WorkflowPreviewMode::Raw,
        },
    )
    .await?;
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
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
    let popup = popup.replace(root.path().file_name().unwrap().to_str().unwrap(), "[TEMP]");
    insta::assert_snapshot!("workflow_saved_consent", popup);
    Ok(())
}

#[tokio::test]
async fn workflow_consent_toggles_summary_and_raw_without_settling_request() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    while events.try_recv().is_ok() {}
    let arguments = json!({"script": "export const meta = {name: 'proof', description: 'Proof'};\nreturn 'raw';"});
    let mut preview = crate::ultracode_source::WorkflowSourcePreview::inline("thread", &arguments)
        .expect("inline preview");
    preview.metadata = Some(WorkflowMetadata {
        name: "proof".into(),
        title: Some("Proof workflow".into()),
        description: "Checks the consent summary before launching.".into(),
        phases: vec![
            WorkflowPhase::Detailed {
                title: "Inspect".into(),
                detail: Some("Read the selected source bytes.".into()),
            },
            WorkflowPhase::Name("Report".into()),
        ],
    });
    attach_consent_presentation(&mut preview);
    let params = codex_app_server_protocol::DynamicToolCallParams {
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        call_id: "call".into(),
        namespace: None,
        tool: "workflow".into(),
        arguments,
    };
    app.show_workflow_consent(AppServerRequestId::Integer(91), params, preview);
    insta::assert_snapshot!(
        "workflow_consent_summary",
        render_bottom_popup(&app.chat_widget, 100)
    );

    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::Workflow(WorkflowEvent::ToggleWorkflowPreview {
        consent,
        feedback_state,
        preview,
        mode,
    }) = events.try_recv().expect("raw toggle event")
    else {
        panic!("Expected workflow preview toggle");
    };
    assert_eq!(mode, WorkflowPreviewMode::Raw);
    let crate::app_event::WorkflowConsentContext::Dynamic { request_id, params } = &consent else {
        panic!("Expected dynamic consent context");
    };
    assert_eq!(request_id, &AppServerRequestId::Integer(91));
    assert_eq!(params.thread_id, "thread");
    assert_eq!(preview.thread_id, "thread");
    app.show_workflow_consent_context(consent, feedback_state, preview, mode);
    insta::assert_snapshot!(
        "workflow_consent_raw",
        render_bottom_popup(&app.chat_widget, 100)
    );
    assert!(events.try_recv().is_err(), "Toggle must not decide consent");
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::Workflow(WorkflowEvent::Consent {
        request_id,
        choice: crate::app_event::WorkflowConsentChoice::Run,
        preview: Some(preview),
        ..
    }) = events.try_recv().expect("original consent decision")
    else {
        panic!("Expected original workflow consent decision");
    };
    assert_eq!(request_id, AppServerRequestId::Integer(91));
    assert_eq!(preview.thread_id, "thread");
    assert!(
        events.try_recv().is_err(),
        "Toggle must not emit stale cancellation"
    );
    Ok(())
}

#[tokio::test]
async fn saved_workflow_consent_toggle_retains_name_args_and_remember_identity() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    while events.try_recv().is_ok() {}
    let arguments = json!({"script": "return 'saved';"});
    let mut preview = crate::ultracode_source::WorkflowSourcePreview::inline("thread", &arguments)
        .expect("inline preview");
    preview.workflow_id = Some("saved-digest".into());
    preview.metadata = Some(WorkflowMetadata {
        name: "saved-proof".into(),
        title: None,
        description: "Run a saved proof workflow.".into(),
        phases: vec![WorkflowPhase::Name("Run".into())],
    });
    attach_consent_presentation(&mut preview);
    let digest = preview.digest.clone();
    app.show_saved_workflow_consent(
        "thread".into(),
        "saved-proof".into(),
        Some("original args".into()),
        preview,
    );
    insta::assert_snapshot!(
        "workflow_saved_consent_summary",
        render_bottom_popup(&app.chat_widget, 100)
    );

    for code in [KeyCode::Down, KeyCode::Down, KeyCode::Enter] {
        app.chat_widget
            .handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
    }
    let AppEvent::Workflow(WorkflowEvent::ToggleWorkflowPreview {
        consent,
        feedback_state: _,
        preview,
        mode: WorkflowPreviewMode::Raw,
    }) = events.try_recv().expect("saved raw toggle event")
    else {
        panic!("Expected saved workflow preview toggle");
    };
    let crate::app_event::WorkflowConsentContext::Saved {
        thread_id,
        name,
        args,
    } = consent
    else {
        panic!("Expected saved consent context");
    };
    assert_eq!(
        (thread_id, name, args, preview.digest),
        (
            "thread".to_string(),
            "saved-proof".to_string(),
            Some("original args".to_string()),
            digest,
        )
    );
    assert!(events.try_recv().is_err(), "Toggle must not decide consent");
    Ok(())
}

#[tokio::test]
async fn invalid_workflow_consent_keeps_raw_source_and_editor_available() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    while events.try_recv().is_ok() {}
    let arguments = json!({"script": "export const meta = ;"});
    let mut preview = crate::ultracode_source::WorkflowSourcePreview::inline("thread", &arguments)
        .expect("inline preview");
    preview.validation_error = Some("Unexpected token ';'".into());
    let digest = preview.digest.clone();
    let params = codex_app_server_protocol::DynamicToolCallParams {
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        call_id: "call".into(),
        namespace: None,
        tool: "workflow".into(),
        arguments,
    };
    app.show_workflow_consent(AppServerRequestId::Integer(92), params, preview);
    insta::assert_snapshot!(
        "workflow_invalid_consent_raw",
        render_bottom_popup(&app.chat_widget, 100)
    );

    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
    let AppEvent::Workflow(WorkflowEvent::EditWorkflowSource {
        consent,
        feedback_state: _,
        preview,
    }) = events.try_recv().expect("editor event")
    else {
        panic!("Expected workflow editor event");
    };
    let crate::app_event::WorkflowConsentContext::Dynamic { request_id, params } = consent else {
        panic!("Expected dynamic consent context");
    };
    assert_eq!(
        (request_id, params.thread_id, preview.digest),
        (AppServerRequestId::Integer(92), "thread".into(), digest)
    );
    assert!(events.try_recv().is_err(), "Editor must not decide consent");
    Ok(())
}

#[tokio::test]
async fn edited_saved_and_resumed_previews_launch_exact_bound_source() -> Result<()> {
    for (arguments, edited_source, expected_request) in [
        (
            json!({"name":"saved","args":{"ticket":4}}),
            "export const meta = {name: 'edited-saved', description: 'Edited saved'};\nreturn 'edited saved';",
            json!({
                "method": "runSource",
                "source": "export const meta = {name: 'edited-saved', description: 'Edited saved'};\nreturn 'edited saved';",
                "args": {"ticket":4},
                "runId": null,
                "workflowId": null,
            }),
        ),
        (
            json!({"resumeFromRunId":"previous","args":["resume",2]}),
            "export const meta = {name: 'edited-resume', description: 'Edited resume'};\nreturn 'edited resume';",
            json!({
                "method": "resumeRun",
                "source": "export const meta = {name: 'edited-resume', description: 'Edited resume'};\nreturn 'edited resume';",
                "args": ["resume",2],
                "runId": "previous",
                "workflowId": null,
            }),
        ),
    ] {
        let (mut app, _events, _ops) = make_test_app_with_channels().await;
        let root = tempdir()?;
        app.config.cwd = root.path().canonicalize()?.abs();
        app.config.codex_home = root.path().join("home").abs();
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

        let original = app
            .prepare_workflow_preview(&mut app_server, &thread_id.to_string(), &arguments)
            .await?;
        let (edited_arguments, edited_preview) = original
            .with_edited_source(&arguments, edited_source.to_string())
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        assert_eq!(edited_preview.workflow_id, None);
        let authority = app_server
            .workflow_authority_capture(codex_app_server_protocol::WorkflowAuthorityCaptureParams {
                parent_thread_id: thread_id.to_string(),
                allow_isolated_workspaces: true,
            })
            .await?;
        let launch = crate::ultracode_launch::launch_with_preview(
            &app_server.request_handle(),
            &thread_id.to_string(),
            &authority,
            &bridge,
            &edited_arguments,
            Some(&edited_preview),
        )
        .await?;
        assert_eq!(launch.result?["runId"], "launched");

        let requests: Vec<serde_json::Value> =
            std::fs::read_to_string(root.path().join("requests.jsonl"))?
                .lines()
                .map(serde_json::from_str)
                .collect::<std::result::Result<_, _>>()?;
        let request = requests
            .iter()
            .rev()
            .find(|request| matches!(request["method"].as_str(), Some("runSource" | "resumeRun")))
            .expect("edited workflow launch request");
        assert_eq!(
            json!({
                "method": request["method"],
                "source": request["params"]["source"],
                "args": request["params"]["args"],
                "runId": request["params"]["runId"],
                "workflowId": request["params"]["workflowId"],
            }),
            expected_request,
        );
        assert_eq!(request["params"]["source"], edited_preview.source);

        bridge
            .shutdown()
            .await
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        app_server.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn unchanged_edited_saved_source_retries_validation_without_launching() -> Result<()> {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    let root = tempdir()?;
    app.config.cwd = root.path().canonicalize()?.abs();
    app.config.codex_home = root.path().join("home").abs();
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

    let source = "export const meta = {name: 'edited-saved', description: 'Edited saved'};\nreturn 'edited saved';";
    let arguments = json!({"script": source, "args": "original args"});
    let mut preview =
        crate::ultracode_source::WorkflowSourcePreview::inline(&thread_id.to_string(), &arguments)
            .expect("previously edited saved preview");
    preview.validation_error = Some("temporary validation failure".into());
    let digest = preview.digest.clone();
    let consent = crate::app_event::WorkflowConsentContext::Saved {
        thread_id: thread_id.to_string(),
        name: "saved".into(),
        args: Some("original args".into()),
    };

    let (consent, preview, mode) = app
        .apply_workflow_editor_source(&mut app_server, consent, preview, source.to_string())
        .await?;
    let crate::app_event::WorkflowConsentContext::Saved {
        thread_id: retained_thread,
        name,
        args,
    } = consent
    else {
        panic!("Expected saved consent context");
    };
    assert_eq!(
        (
            retained_thread,
            name,
            args,
            preview.source.clone(),
            preview.digest.clone(),
            preview.workflow_id.clone(),
            preview.validation_error.clone(),
            mode,
        ),
        (
            thread_id.to_string(),
            "saved".to_string(),
            Some("original args".to_string()),
            source.to_string(),
            digest,
            None,
            None,
            WorkflowPreviewMode::Summary,
        )
    );
    assert!(
        preview.metadata.is_some(),
        "Healthy validation restores summary metadata"
    );

    let requests: Vec<serde_json::Value> =
        std::fs::read_to_string(root.path().join("requests.jsonl"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?;
    assert!(requests.iter().any(|request| {
        request["method"] == "validateSource" && request["params"]["source"] == source
    }));
    assert!(requests.iter().all(|request| {
        !matches!(
            request["method"].as_str(),
            Some("readSavedSource" | "runSource" | "runSaved" | "resumeRun")
        )
    }));

    bridge
        .shutdown()
        .await
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn workflow_consent_collects_inline_accept_and_reject_feedback() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    while events.try_recv().is_ok() {}
    let arguments = json!({"script": "return 'feedback';"});
    let mut preview = crate::ultracode_source::WorkflowSourcePreview::inline("thread", &arguments)
        .expect("inline preview");
    preview.metadata = Some(WorkflowMetadata {
        name: "feedback".into(),
        title: Some("Feedback workflow".into()),
        description: "Collect inline consent feedback.".into(),
        phases: vec![WorkflowPhase::Name("Run".into())],
    });
    attach_consent_presentation(&mut preview);
    let params = codex_app_server_protocol::DynamicToolCallParams {
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        call_id: "call".into(),
        namespace: None,
        tool: "workflow".into(),
        arguments: arguments.clone(),
    };

    app.show_workflow_consent(AppServerRequestId::Integer(101), params, preview);
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    insta::assert_snapshot!(
        "workflow_consent_accept_feedback",
        render_bottom_popup(&app.chat_widget, 100)
    );
    for c in "  continue with report  ".chars() {
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::Workflow(WorkflowEvent::ToggleWorkflowPreview {
        consent,
        feedback_state,
        preview,
        mode,
    }) = events.try_recv().expect("feedback toggle event")
    else {
        panic!("Expected workflow feedback toggle");
    };
    assert_eq!(mode, WorkflowPreviewMode::Raw);
    app.show_workflow_consent_context(consent, feedback_state, preview.clone(), mode);
    insta::assert_snapshot!(
        "workflow_consent_accept_feedback_after_toggle",
        render_bottom_popup(&app.chat_widget, 100)
    );
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::Workflow(WorkflowEvent::Consent {
        choice: crate::app_event::WorkflowConsentChoice::Run,
        feedback: Some(feedback),
        params,
        ..
    }) = events.try_recv().expect("accept feedback event")
    else {
        panic!("Expected workflow accept feedback");
    };
    assert_eq!(feedback, "continue with report");
    assert_eq!(params.arguments, arguments);

    let params = codex_app_server_protocol::DynamicToolCallParams {
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        call_id: "call-2".into(),
        namespace: None,
        tool: "workflow".into(),
        arguments: arguments.clone(),
    };
    app.show_workflow_consent(AppServerRequestId::Integer(102), params, preview);
    for code in [KeyCode::Down, KeyCode::Down, KeyCode::Tab] {
        app.chat_widget
            .handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
    }
    insta::assert_snapshot!(
        "workflow_consent_reject_feedback",
        render_bottom_popup(&app.chat_widget, 100)
    );
    for c in "  use fewer agents  ".chars() {
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::Workflow(WorkflowEvent::Consent {
        choice: crate::app_event::WorkflowConsentChoice::Cancel,
        feedback: Some(feedback),
        params,
        ..
    }) = events.try_recv().expect("reject feedback event")
    else {
        panic!("Expected workflow reject feedback");
    };
    assert_eq!(feedback, "use fewer agents");
    assert_eq!(params.arguments, arguments);
    Ok(())
}

#[tokio::test]
async fn workflow_consent_carries_feedback_into_editor_replacement() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    while events.try_recv().is_ok() {}
    let arguments = json!({"script": "return 'edit feedback';"});
    let preview = crate::ultracode_source::WorkflowSourcePreview::inline("thread", &arguments)
        .expect("inline preview");
    let params = codex_app_server_protocol::DynamicToolCallParams {
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        call_id: "call".into(),
        namespace: None,
        tool: "workflow".into(),
        arguments,
    };
    app.show_workflow_consent(AppServerRequestId::Integer(103), params, preview);
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    for c in "keep this".chars() {
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
    let AppEvent::Workflow(WorkflowEvent::EditWorkflowSource { feedback_state, .. }) =
        events.try_recv().expect("editor replacement")
    else {
        panic!("Expected workflow editor replacement");
    };
    let feedback = feedback_state.snapshot();
    assert_eq!(feedback.accept, "keep this");
    assert!(feedback.accept_expanded);
    Ok(())
}

fn attach_consent_presentation(preview: &mut crate::ultracode_source::WorkflowSourcePreview) {
    use crate::ultracode_source::WorkflowConsentPhase;
    use crate::ultracode_source::WorkflowConsentPresentation;
    use crate::ultracode_source::WorkflowConsentSource;
    let phases = preview.metadata.as_ref().map(|meta| {
        meta.phases
            .iter()
            .map(|phase| match phase {
                WorkflowPhase::Name(title) => WorkflowConsentPhase {
                    title: title.clone(),
                    detail: None,
                    prompts: vec![],
                },
                WorkflowPhase::Detailed { title, detail } => WorkflowConsentPhase {
                    title: title.clone(),
                    detail: detail.clone(),
                    prompts: vec![],
                },
            })
            .collect()
    });
    preview.consent = Some(WorkflowConsentPresentation {
        phases,
        args: None,
        source: WorkflowConsentSource {
            text: preview.source.clone(),
            withheld: false,
            original_length: preview.source.encode_utf16().count(),
        },
    });
}

#[tokio::test]
async fn workflow_consent_presentation_controls_toggle_and_remember_eligibility() -> Result<()> {
    for (has_phases, source_withheld, args_withheld, toggle, remember) in [
        (false, false, false, false, true),
        (true, false, false, true, true),
        (true, false, true, true, false),
        (true, true, false, false, false),
    ] {
        let (mut app, _events, _ops) = make_test_app_with_channels().await;
        let mut preview = crate::ultracode_source::WorkflowSourcePreview::inline(
            "thread",
            &json!({"script": "return 1;"}),
        )
        .expect("inline preview");
        preview.workflow_id = Some("saved-id".into());
        preview.consent = Some(serde_json::from_value(json!({
            "phases": if has_phases { json!([{"title":"Inspect","prompts":[]}]) } else { serde_json::Value::Null },
            "args": {"text":"arguments", "needsGutter":false, "withheld":args_withheld},
            "source": {"text":"return 1;", "withheld":source_withheld, "originalLength":9}
        }))?);
        app.show_saved_workflow_consent("thread".into(), "proof".into(), None, preview);
        let screen = render_bottom_popup(&app.chat_widget, 100);
        assert_eq!(screen.contains("View raw script"), toggle);
        assert_eq!(screen.contains("don't ask again"), remember);
        assert_eq!(
            screen.contains("following phases"),
            has_phases && !source_withheld
        );
    }
    Ok(())
}
