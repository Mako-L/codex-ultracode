use super::*;
use crate::app_event::WorkflowEvent;
use crate::pager_overlay::Overlay;
use crate::tui::TuiEvent;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use futures::SinkExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn workflow_effort_updates_native_authority_and_persists_only_normal_selection() -> Result<()>
{
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    let id = ThreadId::new();
    app.active_thread_id = Some(id);
    app.chat_widget
        .handle_thread_session(test_thread_session(id, app.config.cwd.to_path_buf()));
    let model = app.chat_widget.current_model().to_string();
    let mut preset = app.model_catalog.try_list_models()?.remove(0);
    preset.model = model.clone();
    preset.supported_reasoning_efforts = [
        ReasoningEffortConfig::Low,
        ReasoningEffortConfig::XHigh,
        ReasoningEffortConfig::Max,
    ]
    .into_iter()
    .map(
        |effort| codex_protocol::openai_models::ReasoningEffortPreset {
            effort,
            description: String::new(),
        },
    )
    .collect();
    app.model_catalog = Arc::new(ModelCatalog::new(vec![preset]));
    let home = tempdir()?;
    let selected_file = codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(
        home.path().join("work.config.toml"),
    )?;
    app.config.config_layer_stack = app.config.config_layer_stack.with_user_config_profile(
        &selected_file,
        Some(&"work".parse()?),
        toml::Value::Table(Default::default()),
    )?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let websocket_url = format!("ws://{}", listener.local_addr()?);
    let (requests, mut received) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
    let file = selected_file.to_string_lossy().into_owned();
    let fake = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = tokio_tungstenite::accept_async(stream).await?;
        while let Some(frame) = socket.next().await {
            let Message::Text(text) = frame? else {
                continue;
            };
            let request: serde_json::Value = serde_json::from_str(&text)?;
            if request["id"].is_null() {
                continue;
            }
            let result = match request["method"].as_str() {
                Some("initialize") => json!({"userAgent":"test"}),
                Some("thread/settings/update") => {
                    requests.send(request.clone())?;
                    json!({})
                }
                Some("config/batchWrite") => {
                    requests.send(request.clone())?;
                    json!({"status":"ok","version":"1","filePath":file,"overriddenMetadata":null})
                }
                other => panic!("unexpected method {other:?}"),
            };
            socket
                .send(Message::Text(
                    json!({"id":request["id"],"result":result})
                        .to_string()
                        .into(),
                ))
                .await?;
        }
        Result::<()>::Ok(())
    });
    let client = crate::connect_remote_app_server(crate::RemoteAppServerEndpoint::WebSocket {
        websocket_url,
        auth_token: None,
    })
    .await?;
    let mut server = AppServerSession::new(
        client,
        crate::app_server_session::ThreadParamsMode::Embedded,
    );
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.handle_event(
        &mut tui,
        &mut server,
        AppEvent::Workflow(WorkflowEvent::Open {
            effort: Some("ultracode".into()),
        }),
    )
    .await?;
    let update = received.recv().await.unwrap();
    assert_eq!(update["method"], json!("thread/settings/update"));
    assert_eq!(update["params"]["threadId"], json!(id.to_string()));
    assert_eq!(update["params"]["effort"], json!("xhigh"));
    for field in [
        "permissions",
        "approvalPolicy",
        "approvalsReviewer",
        "sandboxPolicy",
        "cwd",
        "model",
    ] {
        assert!(
            update["params"]
                .get(field)
                .is_none_or(serde_json::Value::is_null),
            "unexpected {field} update"
        );
    }
    assert_eq!(
        update["params"]["collaborationMode"]["settings"]["reasoning_effort"],
        json!("xhigh")
    );
    assert!(app.config.ultracode && app.chat_widget.config_ref().ultracode);
    assert_eq!(
        app.chat_widget.current_reasoning_effort(),
        Some(ReasoningEffortConfig::XHigh)
    );
    assert!(received.try_recv().is_err());
    assert!(app.workflow_sessions.is_empty());
    // Menu actions call this same native implementation. Maximum remains distinct from Ultra.
    app.apply_workflow_effort(&mut server, "max").await?;
    assert_eq!(
        received.recv().await.unwrap()["params"]["effort"],
        json!("max")
    );
    let persist = received.recv().await.unwrap();
    assert_eq!(persist["method"], json!("config/batchWrite"));
    assert_eq!(
        persist["params"]["filePath"],
        json!(selected_file.to_string_lossy().into_owned())
    );
    assert!(
        persist["params"]["edits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|edit| edit["keyPath"] == "model_reasoning_effort" && edit["value"] == "max")
    );
    assert!(!app.config.ultracode && !app.chat_widget.config_ref().ultracode);
    app.on_update_reasoning_effort(Some(ReasoningEffortConfig::Low));
    assert!(!app.config.ultracode);
    app.overlay = Some(Overlay::Workflow(Box::new(
        crate::pager_overlay::WorkflowOverlay::new(json!({"runs": []}), Some("low".into())),
    )));
    app.handle_backtrack_overlay_event(
        &mut tui,
        &mut server,
        TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
    )
    .await?;
    assert!(app.overlay.is_none());
    fake.abort();
    Ok(())
}

#[tokio::test]
async fn unavailable_workflow_effort_keeps_native_selection_unchanged() -> Result<()> {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    app.model_catalog = Arc::new(ModelCatalog::new(Vec::new()));
    let original = app.chat_widget.current_reasoning_effort();
    let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    assert!(
        app.apply_workflow_effort(&mut server, "ultracode")
            .await
            .is_err()
    );
    assert_eq!(app.chat_widget.current_reasoning_effort(), original);
    assert!(!app.config.ultracode);
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn effort_rejections_preserve_session_or_report_applied_session_truthfully() -> Result<()> {
    for failed_method in ["thread/settings/update", "config/batchWrite"] {
        let (mut app, mut events, _ops) = make_test_app_with_channels().await;
        let id = ThreadId::new();
        app.active_thread_id = Some(id);
        app.chat_widget
            .handle_thread_session(test_thread_session(id, app.config.cwd.to_path_buf()));
        app.config.ultracode = true;
        app.chat_widget.set_ultracode_mode(true);
        app.config.model_reasoning_effort = Some(ReasoningEffortConfig::XHigh);
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::XHigh));
        let original_context = app.chat_widget.current_collaboration_mode().clone();
        let mut preset = app.model_catalog.try_list_models()?.remove(0);
        preset.model = app.chat_widget.current_model().to_string();
        preset.supported_reasoning_efforts =
            vec![codex_protocol::openai_models::ReasoningEffortPreset {
                effort: ReasoningEffortConfig::Max,
                description: String::new(),
            }];
        app.model_catalog = Arc::new(ModelCatalog::new(vec![preset]));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let websocket_url = format!("ws://{}", listener.local_addr()?);
        let (requests, mut received) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
        let fake = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut socket = tokio_tungstenite::accept_async(stream).await?;
            while let Some(frame) = socket.next().await {
                let Message::Text(text) = frame? else {
                    continue;
                };
                let request: serde_json::Value = serde_json::from_str(&text)?;
                if request["id"].is_null() {
                    continue;
                }
                let response = if request["method"] == "initialize" {
                    json!({"id":request["id"],"result":{"userAgent":"test"}})
                } else {
                    requests.send(request.clone())?;
                    if request["method"] == failed_method {
                        json!({"id":request["id"],"error":{"code":-32603,"message":"write denied"}})
                    } else {
                        json!({"id":request["id"],"result":{}})
                    }
                };
                socket
                    .send(Message::Text(response.to_string().into()))
                    .await?;
            }
            Result::<()>::Ok(())
        });
        let client = crate::connect_remote_app_server(crate::RemoteAppServerEndpoint::WebSocket {
            websocket_url,
            auth_token: None,
        })
        .await?;
        let mut server = AppServerSession::new(
            client,
            crate::app_server_session::ThreadParamsMode::Embedded,
        );
        while events.try_recv().is_ok() {}
        let result = app.apply_workflow_effort(&mut server, "max").await;
        let update = received.recv().await.unwrap();
        for field in [
            "permissions",
            "approvalPolicy",
            "approvalsReviewer",
            "sandboxPolicy",
            "cwd",
            "model",
        ] {
            assert!(
                update["params"]
                    .get(field)
                    .is_none_or(serde_json::Value::is_null),
                "unexpected {field} update"
            );
        }
        if failed_method == "thread/settings/update" {
            assert!(result.is_err());
            assert_eq!(
                app.config.model_reasoning_effort,
                Some(ReasoningEffortConfig::XHigh)
            );
            assert_eq!(
                app.chat_widget.current_collaboration_mode(),
                &original_context
            );
            assert!(app.config.ultracode && app.chat_widget.config_ref().ultracode);
            assert!(received.try_recv().is_err());
        } else {
            assert!(result.is_ok());
            assert_eq!(
                app.chat_widget.current_reasoning_effort(),
                Some(ReasoningEffortConfig::Max)
            );
            assert!(!app.config.ultracode && !app.chat_widget.config_ref().ultracode);
            assert_eq!(
                received.recv().await.unwrap()["method"],
                json!("config/batchWrite")
            );
            let messages = std::iter::from_fn(|| events.try_recv().ok())
                .filter_map(|event| match event {
                    AppEvent::InsertHistoryCell(cell) => {
                        Some(lines_to_single_string(&cell.display_lines(200)))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(messages.contains(
                "Effort set to max for this session, but the default could not be saved"
            ));
        }
        fake.abort();
    }
    Ok(())
}
