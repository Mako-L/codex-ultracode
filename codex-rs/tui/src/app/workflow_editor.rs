use super::*;
use crate::app_event::WorkflowConsentContext;
use crate::app_event::WorkflowConsentFeedbackState;
use crate::app_event::WorkflowPreviewMode;
use crate::workflow_source::WorkflowSourcePreview;

impl App {
    pub(super) async fn edit_workflow_source(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        consent: WorkflowConsentContext,
        feedback_state: WorkflowConsentFeedbackState,
        preview: WorkflowSourcePreview,
    ) {
        let result: Result<String> = async {
            let editor_cmd = external_editor::resolve_editor_command()?;
            let config = self.chat_widget.config_ref();
            let file_system_policy = config.permissions.file_system_sandbox_policy();
            tui.with_restored(|| async {
                external_editor::run_editor(
                    &preview.source,
                    &editor_cmd,
                    config.codex_home.as_path(),
                    &file_system_policy,
                    config.cwd.as_path(),
                )
                .await
            })
            .await
        }
        .await;

        let source = match result {
            Ok(source) => source,
            Err(error) => {
                self.chat_widget
                    .add_to_history(history_cell::new_error_event(format!(
                        "Failed to edit workflow: {error}"
                    )));
                self.show_workflow_consent_context(
                    consent,
                    feedback_state,
                    preview,
                    WorkflowPreviewMode::Summary,
                );
                tui.frame_requester().schedule_frame();
                return;
            }
        };
        let original_consent = consent.clone();
        let original_preview = preview.clone();
        let (consent, preview, mode) = match self
            .apply_workflow_editor_source(app_server, consent, preview, source)
            .await
        {
            Ok(updated) => updated,
            Err(error) => {
                self.chat_widget
                    .add_to_history(history_cell::new_error_event(format!(
                        "Failed to bind edited workflow: {error}"
                    )));
                (
                    original_consent,
                    original_preview,
                    WorkflowPreviewMode::Summary,
                )
            }
        };
        self.show_workflow_consent_context(consent, feedback_state, preview, mode);
        tui.frame_requester().schedule_frame();
    }

    pub(super) async fn apply_workflow_editor_source(
        &mut self,
        app_server: &mut AppServerSession,
        mut consent: WorkflowConsentContext,
        preview: WorkflowSourcePreview,
        source: String,
    ) -> Result<(
        WorkflowConsentContext,
        WorkflowSourcePreview,
        WorkflowPreviewMode,
    )> {
        if source == preview.source && preview.validation_error.is_none() {
            return Ok((consent, preview, WorkflowPreviewMode::Summary));
        }
        let arguments = match &consent {
            WorkflowConsentContext::Dynamic { params, .. } => params.arguments.clone(),
            WorkflowConsentContext::Saved { name, args, .. } => {
                let mut arguments = if preview.workflow_id.is_some() {
                    serde_json::json!({"name": name})
                } else {
                    serde_json::json!({"script": preview.source})
                };
                if let Some(args) = args {
                    arguments["args"] = serde_json::Value::String(args.clone());
                }
                arguments
            }
        };
        let (arguments, mut edited) = preview
            .with_edited_source(&arguments, source)
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        match self
            .prepare_workflow_preview(app_server, &edited.thread_id, &arguments)
            .await
        {
            Ok(validated) => edited = validated,
            Err(error) => edited.validation_error = Some(error.to_string()),
        }
        if let WorkflowConsentContext::Dynamic { params, .. } = &mut consent {
            params.arguments = arguments;
        }
        // Keep invalid edits available for correction without settling the request.
        let mode = if edited.validation_error.is_some() {
            WorkflowPreviewMode::Raw
        } else {
            WorkflowPreviewMode::Summary
        };
        Ok((consent, edited, mode))
    }
}
