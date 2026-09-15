use super::workflow_consent_render::workflow_preview_header;
use super::*;
use crate::app_event::WorkflowConsentChoice;
use crate::app_event::WorkflowConsentContext;
use crate::app_event::WorkflowConsentFeedbackState;
use crate::app_event::WorkflowEvent;
use crate::app_event::WorkflowPreviewMode;
use crate::render::renderable::ColumnRenderable;
use crate::workflow_source::WorkflowSourcePreview;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;

impl App {
    pub(super) async fn prepare_workflow_preview(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: &str,
        arguments: &serde_json::Value,
    ) -> Result<WorkflowSourcePreview> {
        let authority = app_server
            .workflow_authority_capture(codex_app_server_protocol::WorkflowAuthorityCaptureParams {
                parent_thread_id: thread_id.to_owned(),
                allow_isolated_workspaces: false,
            })
            .await?;
        let bridge = self
            .ensure_workflow_session(&authority, thread_id, app_server)
            .await?;
        let mut preview = crate::workflow_source::read_preview(
            &app_server.request_handle(),
            thread_id,
            &authority,
            &bridge,
            arguments,
            /*selected*/ None,
        )
        .await?;
        let validated = bridge
            .request(
                "validateSource",
                {
                    let mut params = serde_json::json!({"source": preview.source});
                    if let Some(args) = arguments.get("args") {
                        params["args"] = args.clone();
                    }
                    params
                },
                std::time::Duration::from_secs(30),
            )
            .await
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        let digest = validated["digest"]
            .as_str()
            .ok_or_else(|| color_eyre::eyre::eyre!("Validated workflow digest is unavailable"))?;
        if digest != preview.digest {
            return Err(color_eyre::eyre::eyre!(
                "Validated workflow digest does not match its preview"
            ));
        }
        preview.metadata = Some(
            serde_json::from_value(validated["meta"].clone())
                .map_err(|error| color_eyre::eyre::eyre!("Invalid workflow metadata: {error}"))?,
        );
        preview.consent = Some(
            serde_json::from_value(validated["consent"].clone()).map_err(|error| {
                color_eyre::eyre::eyre!("Invalid workflow presentation: {error}")
            })?,
        );
        Ok(preview)
    }

    pub(super) fn show_workflow_consent(
        &mut self,
        request_id: codex_app_server_protocol::RequestId,
        params: codex_app_server_protocol::DynamicToolCallParams,
        preview: WorkflowSourcePreview,
    ) {
        self.show_workflow_consent_context(
            WorkflowConsentContext::Dynamic { request_id, params },
            WorkflowConsentFeedbackState::default(),
            preview,
            WorkflowPreviewMode::Summary,
        );
    }

    pub(super) fn show_saved_workflow_consent(
        &mut self,
        thread_id: String,
        name: String,
        args: Option<String>,
        preview: WorkflowSourcePreview,
    ) {
        self.show_workflow_consent_context(
            WorkflowConsentContext::Saved {
                thread_id,
                name,
                args,
            },
            WorkflowConsentFeedbackState::default(),
            preview,
            WorkflowPreviewMode::Summary,
        );
    }

    pub(super) fn show_workflow_consent_context(
        &mut self,
        consent: WorkflowConsentContext,
        feedback_state: WorkflowConsentFeedbackState,
        preview: WorkflowSourcePreview,
        mut mode: WorkflowPreviewMode,
    ) {
        let has_summary = preview.consent.as_ref().is_some_and(|consent| {
            consent
                .phases
                .as_ref()
                .is_some_and(|phases| !phases.is_empty())
                && !consent.source.withheld
        });
        if preview.validation_error.is_some() || !has_summary {
            mode = WorkflowPreviewMode::Raw;
        }
        let title = "Run a dynamic workflow?".to_string();
        let mut remember_name = match &consent {
            WorkflowConsentContext::Dynamic { params, .. } if preview.workflow_id.is_some() => {
                params
                    .arguments
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            }
            WorkflowConsentContext::Dynamic { .. } => None,
            WorkflowConsentContext::Saved { name, .. } if preview.workflow_id.is_some() => {
                Some(name.clone())
            }
            WorkflowConsentContext::Saved { .. } => None,
        };

        if preview.consent.as_ref().is_none_or(|consent| {
            consent.source.withheld || consent.args.as_ref().is_some_and(|args| args.withheld)
        }) {
            remember_name = None;
        }

        let send_choice = |choice| {
            let consent = consent.clone();
            let preview = preview.clone();
            Box::new(
                move |feedback: Option<String>, tx: &AppEventSender| match &consent {
                    WorkflowConsentContext::Dynamic { request_id, params } => {
                        tx.send(AppEvent::Workflow(WorkflowEvent::Consent {
                            request_id: request_id.clone(),
                            params: params.clone(),
                            choice,
                            feedback,
                            preview: (!matches!(choice, WorkflowConsentChoice::Cancel))
                                .then(|| preview.clone()),
                        }));
                    }
                    WorkflowConsentContext::Saved {
                        thread_id,
                        name,
                        args,
                    } => {
                        tx.send(AppEvent::Workflow(WorkflowEvent::RunSavedConsent {
                            thread_id: thread_id.clone(),
                            name: name.clone(),
                            args: args.clone(),
                            choice,
                            feedback,
                            preview: (!matches!(choice, WorkflowConsentChoice::Cancel))
                                .then(|| preview.clone()),
                        }));
                    }
                },
            ) as crate::bottom_pane::SelectionFeedbackAction
        };

        let send_plain_choice = |choice| {
            let action = send_choice(choice);
            Box::new(move |tx: &AppEventSender| action(None, tx))
                as crate::bottom_pane::SelectionAction
        };

        let feedback = feedback_state.snapshot();

        let validation_error = preview.validation_error.clone().or_else(|| {
            preview
                .consent
                .as_ref()
                .and_then(|consent| consent.source.withheld.then(|| consent.source.text.clone()))
        });
        let mut items = vec![SelectionItem {
            name: "Yes, run it".into(),
            is_disabled: validation_error.is_some(),
            disabled_reason: validation_error.clone(),
            feedback: Some({
                let mut input = crate::bottom_pane::SelectionFeedback::new(
                    "tell Codex what to do next",
                    feedback.accept,
                    feedback.accept_expanded,
                    send_choice(WorkflowConsentChoice::Run),
                );
                let state = feedback_state.clone();
                input.on_change = Some(Box::new(move |text| {
                    state.update(|feedback| feedback.accept = text.to_string());
                }));
                let state = feedback_state.clone();
                input.on_expanded_change = Some(Box::new(move |expanded| {
                    state.update(|feedback| feedback.accept_expanded = expanded);
                }));
                input
            }),
            dismiss_on_select: true,
            ..Default::default()
        }];
        if let Some(name) = remember_name {
            items.push(SelectionItem {
                name: sanitize_display(&format!(
                    "Yes, and don't ask again for /{name} in {}",
                    self.config.cwd.display()
                )),
                is_disabled: validation_error.is_some(),
                disabled_reason: validation_error.clone(),
                actions: vec![send_plain_choice(WorkflowConsentChoice::Remember)],
                dismiss_on_select: true,
                ..Default::default()
            });
        }
        if has_summary && validation_error.is_none() {
            let next_mode = match mode {
                WorkflowPreviewMode::Summary => WorkflowPreviewMode::Raw,
                WorkflowPreviewMode::Raw => WorkflowPreviewMode::Summary,
            };
            let toggle_label = match next_mode {
                WorkflowPreviewMode::Summary => "View workflow summary",
                WorkflowPreviewMode::Raw => "View raw script",
            };
            items.push(SelectionItem {
                name: toggle_label.into(),
                actions: vec![Box::new({
                    let consent = consent.clone();
                    let feedback_state = feedback_state.clone();
                    let preview = preview.clone();
                    move |tx| {
                        tx.send(AppEvent::Workflow(WorkflowEvent::ToggleWorkflowPreview {
                            consent: consent.clone(),
                            feedback_state: feedback_state.clone(),
                            preview: preview.clone(),
                            mode: next_mode,
                        }));
                    }
                })],
                dismiss_on_select: true,
                ..Default::default()
            });
        }
        items.push(SelectionItem {
            name: "No".into(),
            feedback: Some({
                let feedback = feedback_state.snapshot();
                let mut input = crate::bottom_pane::SelectionFeedback::new(
                    "tell Codex what to do differently",
                    feedback.reject,
                    feedback.reject_expanded,
                    send_choice(WorkflowConsentChoice::Cancel),
                );
                let state = feedback_state.clone();
                input.on_change = Some(Box::new(move |text| {
                    state.update(|feedback| feedback.reject = text.to_string());
                }));
                let state = feedback_state.clone();
                input.on_expanded_change = Some(Box::new(move |expanded| {
                    state.update(|feedback| feedback.reject_expanded = expanded);
                }));
                input
            }),
            dismiss_on_select: true,
            ..Default::default()
        });
        let cancel = send_plain_choice(WorkflowConsentChoice::Cancel);
        let edit_shortcut: crate::key_hint::ShortcutHint =
            crate::key_hint::ctrl(crossterm::event::KeyCode::Char('g')).into();
        let edit_action = Box::new({
            let consent = consent.clone();
            let feedback_state = feedback_state;
            let preview = preview.clone();
            move |tx: &AppEventSender| {
                tx.send(AppEvent::Workflow(WorkflowEvent::EditWorkflowSource {
                    consent: consent.clone(),
                    feedback_state: feedback_state.clone(),
                    preview: preview.clone(),
                }));
            }
        });

        self.chat_widget.show_selection_view(SelectionViewParams {
            header: Box::new(ColumnRenderable::with([
                Box::new(Paragraph::new(vec![title.bold().into(), Line::default()])),
                workflow_preview_header(&preview, mode),
                Box::new(
                    Paragraph::new(vec![
                        Line::default(),
                        "You can stop a running workflow at any time with /workflows, or disable dynamic workflows in /config.".dim().into(),
                    ])
                    .wrap(Wrap { trim: false }),
                ),
            ])),
            footer_hint: Some(
                vec![
                    edit_shortcut.display_label().cyan(),
                    " edit in $EDITOR".dim(),
                ]
                .into(),
            ),
            additional_shortcuts: vec![(edit_shortcut, edit_action)],
            on_cancel: Some(cancel),
            items,
            ..Default::default()
        });
    }
}

fn sanitize_display(text: &str) -> String {
    crate::history_cell::sanitize_user_text(text.to_owned().into()).into_owned()
}
