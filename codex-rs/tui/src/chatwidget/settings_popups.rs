//! Settings-adjacent popup surfaces for `ChatWidget`.
//!
//! This keeps theme, personality, and experimental-feature UI out of the main
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
            self.config.tui_theme.as_deref(),
            codex_home.as_deref(),
            self.last_rendered_width.get(),
        );
        self.bottom_pane.show_selection_view(params);
    }

    pub(crate) fn open_personality_popup(&mut self) {
        if !self.is_session_configured() {
            self.add_info_message(
                "Personality selection is disabled until startup completes.".to_string(),
                /*hint*/ None,
            );
            return;
        }
        if !self.current_model_supports_personality() {
            let current_model = self.current_model();
            self.add_error_message(format!(
                "Current model ({current_model}) doesn't support personalities. Try /model to pick a different model."
            ));
            return;
        }
        self.open_personality_popup_for_current_model();
    }

    fn open_personality_popup_for_current_model(&mut self) {
        let current_personality = self.config.personality.unwrap_or(Personality::Friendly);
        let personalities = [Personality::Friendly, Personality::Pragmatic];
        let supports_personality = self.current_model_supports_personality();

        let items: Vec<SelectionItem> = personalities
            .into_iter()
            .map(|personality| {
                let name = Self::personality_label(personality).to_string();
                let description = Some(Self::personality_description(personality).to_string());
                let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                    tx.send(AppEvent::CodexOp(AppCommand::override_turn_context(
                        /*cwd*/ None,
                        /*approval_policy*/ None,
                        /*approvals_reviewer*/ None,
                        /*permission_profile*/ None,
                        /*active_permission_profile*/ None,
                        /*windows_sandbox_level*/ None,
                        /*model*/ None,
                        /*effort*/ None,
                        /*summary*/ None,
                        /*service_tier*/ None,
                        /*collaboration_mode*/ None,
                        Some(personality),
                    )));
                    tx.send(AppEvent::UpdatePersonality(personality));
                    tx.send(AppEvent::PersistPersonalitySelection { personality });
                })];
                SelectionItem {
                    name,
                    description,
                    is_current: current_personality == personality,
                    is_disabled: !supports_personality,
                    actions,
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();

        let mut header = ColumnRenderable::new();
        header.push(Line::from("Select Personality".bold()));
        header.push(Line::from("Choose a communication style for Codex.".dim()));

        self.bottom_pane.show_selection_view(SelectionViewParams {
            header: Box::new(header),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn open_experimental_popup(&mut self) {
        let features: Vec<ExperimentalFeatureItem> = FEATURES
            .iter()
            .filter_map(|spec| {
                let name = spec.stage.experimental_menu_name()?;
                let description = spec.stage.experimental_menu_description()?;
                Some(ExperimentalFeatureItem {
                    feature: spec.id,
                    name: name.to_string(),
                    description: description.to_string(),
                    enabled: self.config.features.enabled(spec.id),
                })
            })
            .collect();

        let view = ExperimentalFeaturesView::new(
            features,
            self.app_event_tx.clone(),
            self.bottom_pane.list_keymap(),
        );
        self.bottom_pane.show_view(Box::new(view));
    }

    fn personality_label(personality: Personality) -> &'static str {
        match personality {
            Personality::None => "None",
            Personality::Friendly => "Friendly",
            Personality::Pragmatic => "Pragmatic",
        }
    }

    fn personality_description(personality: Personality) -> &'static str {
        match personality {
            Personality::None => "No personality instructions.",
            Personality::Friendly => "Warm, collaborative, and helpful.",
            Personality::Pragmatic => "Concise, task-focused, and direct.",
        }
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
