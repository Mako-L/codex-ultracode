use super::*;
use crate::app_event::WorkflowConsentChoice;
use crate::app_event::WorkflowEvent;
use crate::ultracode_source::WorkflowSourcePreview;

impl App {
    pub(super) async fn prepare_workflow_preview(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: &str,
        arguments: &serde_json::Value,
    ) -> Result<WorkflowSourcePreview> {
        if let Some(preview) = WorkflowSourcePreview::inline(thread_id, arguments) {
            return Ok(preview);
        }
        let authority = app_server
            .workflow_authority_capture(codex_app_server_protocol::WorkflowAuthorityCaptureParams {
                parent_thread_id: thread_id.to_owned(),
                allow_isolated_workspaces: false,
            })
            .await?;
        let bridge = self
            .ensure_workflow_session(&authority, thread_id, app_server)
            .await?;
        Ok(crate::ultracode_source::read_preview(
            &app_server.request_handle(),
            thread_id,
            &authority,
            &bridge,
            arguments,
            /*selected*/ None,
        )
        .await?)
    }

    pub(super) fn show_workflow_consent(
        &mut self,
        request_id: codex_app_server_protocol::RequestId,
        params: codex_app_server_protocol::DynamicToolCallParams,
        preview: WorkflowSourcePreview,
    ) {
        let can_remember = preview.workflow_id.is_some();
        let items = [
            ("Run once", WorkflowConsentChoice::Run),
            (
                "Run and remember for this project",
                WorkflowConsentChoice::Remember,
            ),
        ]
        .into_iter()
        .map(|(name, choice)| SelectionItem {
            name: name.into(),
            is_disabled: matches!(choice, WorkflowConsentChoice::Remember) && !can_remember,
            disabled_reason: (matches!(choice, WorkflowConsentChoice::Remember) && !can_remember)
                .then(|| "Inline workflows cannot be remembered".into()),
            actions: vec![Box::new({
                let request_id = request_id.clone();
                let params = params.clone();
                let preview = preview.clone();
                move |tx| {
                    tx.send(AppEvent::Workflow(WorkflowEvent::Consent {
                        request_id: request_id.clone(),
                        params: params.clone(),
                        choice,
                        preview: Some(preview.clone()),
                    }))
                }
            })],
            dismiss_on_select: true,
            ..Default::default()
        })
        .collect::<Vec<_>>();
        let cancel = {
            move |tx: &AppEventSender| {
                tx.send(AppEvent::Workflow(WorkflowEvent::Consent {
                    request_id: request_id.clone(),
                    params: params.clone(),
                    choice: WorkflowConsentChoice::Cancel,
                    preview: None,
                }))
            }
        };
        let mut items = items;
        items.push(SelectionItem {
            name: "View raw script".into(),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::Workflow(WorkflowEvent::ViewScript {
                    thread_id: preview.thread_id.clone(),
                    source: preview.source.clone(),
                }))
            })],
            dismiss_on_select: false,
            ..Default::default()
        });
        items.push(SelectionItem {
            name: "Cancel".into(),
            actions: vec![Box::new(cancel.clone())],
            dismiss_on_select: true,
            ..Default::default()
        });
        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some("Run workflow?".into()),
            subtitle: Some("This workflow may start multiple native Codex workers.".into()),
            on_cancel: Some(Box::new(cancel)),
            items,
            ..Default::default()
        });
    }

    pub(super) fn show_saved_workflow_consent(
        &mut self,
        thread_id: String,
        name: String,
        args: Option<String>,
        preview: WorkflowSourcePreview,
    ) {
        let mut items = [
            ("Run once", WorkflowConsentChoice::Run),
            (
                "Run and remember for this project",
                WorkflowConsentChoice::Remember,
            ),
        ]
        .into_iter()
        .map(|(label, choice)| SelectionItem {
            name: label.into(),
            actions: vec![Box::new({
                let thread_id = thread_id.clone();
                let name = name.clone();
                let args = args.clone();
                let preview = preview.clone();
                move |tx| {
                    tx.send(AppEvent::Workflow(WorkflowEvent::RunSavedConsent {
                        thread_id: thread_id.clone(),
                        name: name.clone(),
                        args: args.clone(),
                        choice,
                        preview: Some(preview.clone()),
                    }))
                }
            })],
            dismiss_on_select: true,
            ..Default::default()
        })
        .collect::<Vec<_>>();
        items.push(SelectionItem {
            name: "View raw script".into(),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::Workflow(WorkflowEvent::ViewScript {
                    thread_id: preview.thread_id.clone(),
                    source: preview.source.clone(),
                }))
            })],
            dismiss_on_select: false,
            ..Default::default()
        });
        let cancel = {
            let name = name.clone();
            move |tx: &AppEventSender| {
                tx.send(AppEvent::Workflow(WorkflowEvent::RunSavedConsent {
                    thread_id: thread_id.clone(),
                    name: name.clone(),
                    args: args.clone(),
                    choice: WorkflowConsentChoice::Cancel,
                    preview: None,
                }))
            }
        };
        items.push(SelectionItem {
            name: "Cancel".into(),
            actions: vec![Box::new(cancel.clone())],
            dismiss_on_select: true,
            ..Default::default()
        });
        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some(format!("Run workflow /{name}?")),
            on_cancel: Some(Box::new(cancel)),
            items,
            ..Default::default()
        });
    }
}
