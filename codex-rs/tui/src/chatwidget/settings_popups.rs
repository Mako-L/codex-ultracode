//! Settings-adjacent popup surfaces for `ChatWidget`.
//!
//! This keeps theme and experimental-feature UI out of the main
//! orchestration module without changing their event wiring.

use super::*;

impl ChatWidget {
    pub(crate) fn open_workflow_config_popup(&mut self) {
        use codex_protocol::config_types::WorkflowSizeGuideline;

        let configured = self.config.workflow_size_guideline;
        let effective = configured.unwrap_or(WorkflowSizeGuideline::Medium);
        let items = [
            (
                WorkflowSizeGuideline::Unrestricted,
                "unrestricted",
                "No agent-count guideline",
            ),
            (
                WorkflowSizeGuideline::Small,
                "small (aim for <5 agents)",
                "Advisory only",
            ),
            (
                WorkflowSizeGuideline::Medium,
                workflow_size_label(WorkflowSizeGuideline::Medium, configured.is_none()),
                "Advisory only",
            ),
            (
                WorkflowSizeGuideline::Large,
                "large (aim for <50 agents)",
                "Advisory only",
            ),
        ]
        .into_iter()
        .map(|(guideline, name, description)| SelectionItem {
            name: name.to_string(),
            description: Some(description.to_string()),
            is_current: guideline == effective,
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::PersistWorkflowSizeGuideline { guideline });
            })],
            dismiss_on_select: true,
            ..Default::default()
        })
        .collect();
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Dynamic workflow size".to_string()),
            subtitle: Some("Choose the advisory size for newly authored workflows.".to_string()),
            items,
            footer_hint: Some(standard_popup_hint_line()),
            ..Default::default()
        });
    }

    pub(super) fn open_theme_picker(&mut self) {
        let codex_home = codex_utils_home_dir::find_codex_home().ok();
        let params = crate::theme_picker::build_theme_picker_params(
            self.local_settings.tui.theme.as_deref(),
            codex_home.as_deref(),
            self.last_rendered_width.get(),
        );
        self.bottom_pane.show_selection_view(params);
    }

    pub(crate) fn open_experimental_popup(&mut self) {
        let Some(thread_id) = self.thread_id() else {
            self.add_info_message(
                "Experimental features are unavailable until startup completes.".to_string(),
                /*hint*/ None,
            );
            return;
        };
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        self.app_event_tx.send(AppEvent::FetchExperimentalFeatures {
            thread_id,
            response_tx,
        });
        let view = ExperimentalFeaturesView::new(
            Vec::new(),
            thread_id,
            Some(response_rx),
            self.app_event_tx.clone(),
            self.bottom_pane.list_keymap(),
        );
        self.bottom_pane.show_view(Box::new(view));
    }
}

fn workflow_size_label(
    guideline: codex_protocol::config_types::WorkflowSizeGuideline,
    is_default: bool,
) -> &'static str {
    use codex_protocol::config_types::WorkflowSizeGuideline::*;
    match (guideline, is_default) {
        (Medium, true) => "medium (default)",
        (Unrestricted, _) => "unrestricted",
        (Small, _) => "small (aim for <5 agents)",
        (Medium, _) => "medium (aim for <15 agents)",
        (Large, _) => "large (aim for <50 agents)",
    }
}

#[cfg(test)]
mod workflow_size_tests {
    use super::workflow_size_label;
    use codex_protocol::config_types::WorkflowSizeGuideline::Medium;

    #[test]
    fn medium_default_and_explicit_labels_differ() {
        assert_eq!(workflow_size_label(Medium, true), "medium (default)");
        assert_eq!(
            workflow_size_label(Medium, false),
            "medium (aim for <15 agents)"
        );
    }
}
