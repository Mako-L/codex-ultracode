use super::*;
use crate::app_event::WorkflowConsentChoice;
use crate::app_event::WorkflowEvent;
use pretty_assertions::assert_eq;

fn inline_call() -> codex_app_server_protocol::DynamicToolCallParams {
    codex_app_server_protocol::DynamicToolCallParams {
        thread_id: ThreadId::new().to_string(),
        turn_id: "turn-1".into(),
        call_id: "call-1".into(),
        namespace: None,
        tool: "workflow".into(),
        arguments: serde_json::json!({"script":"// workflow"}),
    }
}

#[tokio::test]
async fn invalid_workflow_wait_call_is_rejected_before_consent() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut params = inline_call();
    params.arguments = serde_json::json!({"runId":"workflow-run","action":"wait"});
    app.handle_workflow_event(
        &mut tui,
        &mut app_server,
        WorkflowEvent::ToolCall {
            request_id: AppServerRequestId::Integer(40),
            params,
        },
    )
    .await?;
    let AppEvent::DynamicToolCallCompleted { response, .. } =
        events.try_recv().expect("invalid call response")
    else {
        panic!("expected tool failure without consent");
    };
    assert!(!response.success);
    assert!(app.workflow_sessions.is_empty());
    assert!(!render_bottom_popup(&app.chat_widget, 100).contains("Run once"));
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cancelled_workflow_answers_tool_without_launch_or_persistence() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let home = tempdir()?;
    app.config.codex_home = home.path().to_path_buf().abs();
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.handle_workflow_event(
        &mut tui,
        &mut app_server,
        WorkflowEvent::Consent {
            request_id: AppServerRequestId::Integer(41),
            params: inline_call(),
            choice: WorkflowConsentChoice::Cancel,
            feedback: Some("use the saved workflow".into()),
            preview: None,
        },
    )
    .await?;

    let AppEvent::DynamicToolCallCompleted { response, .. } =
        events.try_recv().expect("cancel response")
    else {
        panic!("expected cancelled tool response")
    };
    assert!(!response.success);
    assert_eq!(
        response
            .content_items
            .iter()
            .filter(|item| matches!(
                item,
                codex_app_server_protocol::DynamicToolCallOutputContentItem::InputText { text }
                    if text == "Workflow consent feedback: use the saved workflow"
            ))
            .count(),
        1
    );
    assert!(app.workflow_sessions.is_empty());
    assert!(!home.path().join("workflow-consent.json").exists());
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn manual_inline_consent_disables_remember_and_starts_nothing() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    app.config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())?;
    app.config.approvals_reviewer = ApprovalsReviewer::User;
    let params = inline_call();
    let preview = crate::ultracode_source::WorkflowSourcePreview::inline(
        &params.thread_id,
        &params.arguments,
    )
    .expect("inline workflow preview");
    app.show_workflow_consent(AppServerRequestId::Integer(42), params, preview);
    assert!(events.try_recv().is_err());
    assert!(app.workflow_sessions.is_empty());
    let popup = render_bottom_popup(&app.chat_widget, 100);
    assert!(popup.contains("Yes, run it"));
    assert!(!popup.contains("don't ask again"));
    Ok(())
}

#[tokio::test]
async fn keyboard_cancellation_answers_pending_workflow_consent() -> Result<()> {
    for key in [
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    ] {
        let (mut app, mut events, _ops) = make_test_app_with_channels().await;
        let home = tempdir()?;
        app.config.codex_home = home.path().to_path_buf().abs();
        app.config
            .permissions
            .approval_policy
            .set(AskForApproval::OnRequest.to_core())?;
        app.config.approvals_reviewer = ApprovalsReviewer::User;
        let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
        let mut tui = crate::tui::test_support::make_test_tui()?;
        let params = inline_call();
        let preview = crate::ultracode_source::WorkflowSourcePreview::inline(
            &params.thread_id,
            &params.arguments,
        )
        .expect("inline workflow preview");
        app.show_workflow_consent(AppServerRequestId::Integer(45), params.clone(), preview);
        assert!(render_bottom_popup(&app.chat_widget, 100).contains("Yes, run it"));
        app.chat_widget.handle_key_event(key);
        let cancellation = events.try_recv().ok();
        if cancellation.is_none() {
            app_server.shutdown().await?;
            panic!(
                "Keyboard cancellation dismissed consent without answering its request: {key:?}"
            );
        }
        let Some(AppEvent::Workflow(WorkflowEvent::Consent {
            request_id,
            params: cancelled_params,
            choice: WorkflowConsentChoice::Cancel,
            feedback: None,
            preview: None,
        })) = cancellation
        else {
            panic!("Expected workflow cancellation");
        };
        assert_eq!(request_id, AppServerRequestId::Integer(45));
        assert_eq!(
            serde_json::to_value(&cancelled_params)?,
            serde_json::to_value(&params)?
        );
        app.handle_workflow_event(
            &mut tui,
            &mut app_server,
            WorkflowEvent::Consent {
                request_id,
                params: cancelled_params,
                choice: WorkflowConsentChoice::Cancel,
                feedback: None,
                preview: None,
            },
        )
        .await?;
        let response = events.try_recv().expect("cancelled tool response");
        let no_extra_response = events.try_recv().is_err();
        app_server.shutdown().await?;
        let AppEvent::DynamicToolCallCompleted {
            request_id,
            response,
        } = response
        else {
            panic!("Expected cancelled tool response");
        };
        assert_eq!(request_id, AppServerRequestId::Integer(45));
        assert!(!response.success);
        assert!(no_extra_response);
        assert!(app.workflow_sessions.is_empty());
        assert!(!home.path().join("workflow-consent.json").exists());
        assert!(!render_bottom_popup(&app.chat_widget, 100).contains("Yes, run it"));
    }
    Ok(())
}

#[tokio::test]
async fn never_and_auto_ultracode_route_directly_to_real_consent_event() -> Result<()> {
    for (policy, reviewer, ultracode) in [
        (AskForApproval::Never, ApprovalsReviewer::User, false),
        (
            AskForApproval::OnRequest,
            ApprovalsReviewer::AutoReview,
            true,
        ),
    ] {
        let (mut app, mut events, _ops) = make_test_app_with_channels().await;
        app.config
            .permissions
            .approval_policy
            .set(policy.to_core())?;
        app.config.approvals_reviewer = reviewer;
        app.config.ultracode = ultracode;
        let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
        let mut tui = crate::tui::test_support::make_test_tui()?;
        app.handle_workflow_event(
            &mut tui,
            &mut app_server,
            WorkflowEvent::ToolCall {
                request_id: AppServerRequestId::Integer(43),
                params: inline_call(),
            },
        )
        .await?;
        assert_matches!(
            events.try_recv(),
            Ok(AppEvent::Workflow(WorkflowEvent::Consent {
                choice: WorkflowConsentChoice::Run,
                ..
            }))
        );
        assert!(app.workflow_sessions.is_empty());
        app_server.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn persisted_auto_consent_skips_the_next_auto_prompt() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let home = tempdir()?;
    app.config.codex_home = home.path().to_path_buf().abs();
    app.config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())?;
    app.config.approvals_reviewer = ApprovalsReviewer::AutoReview;
    crate::workflow_consent::WorkflowConsentStore::load(home.path())?
        .remember_auto_first_launch()?;
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.handle_workflow_event(
        &mut tui,
        &mut app_server,
        WorkflowEvent::ToolCall {
            request_id: AppServerRequestId::Integer(44),
            params: inline_call(),
        },
    )
    .await?;
    assert_matches!(
        events.try_recv(),
        Ok(AppEvent::Workflow(WorkflowEvent::Consent {
            choice: WorkflowConsentChoice::Run,
            ..
        }))
    );
    assert!(home.path().join("workflow-consent.json").exists());
    app_server.shutdown().await?;
    Ok(())
}
