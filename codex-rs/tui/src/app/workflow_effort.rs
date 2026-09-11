use super::*;

impl App {
    /// Apply the same native setting whether selected by the menu or `/effort VALUE`.
    pub(crate) async fn apply_workflow_effort(
        &mut self,
        app_server: &mut AppServerSession,
        label: &str,
    ) -> Result<()> {
        let enabled = label == "ultracode";
        if enabled && self.config.disable_workflows {
            color_eyre::eyre::bail!("Workflows are disabled by configuration");
        }
        let effort = match label {
            "ultracode" | "xhigh" => ReasoningEffortConfig::XHigh,
            "low" => ReasoningEffortConfig::Low,
            "medium" => ReasoningEffortConfig::Medium,
            "high" => ReasoningEffortConfig::High,
            "max" => ReasoningEffortConfig::Max,
            _ => color_eyre::eyre::bail!(
                "Expected effort low, medium, high, xhigh, max, or ultracode"
            ),
        };
        let model = self.chat_widget.current_model().to_string();
        let supported = self.model_catalog.try_list_models()?.iter().any(|preset| {
            preset.model == model
                && preset
                    .supported_reasoning_efforts
                    .iter()
                    .any(|option| option.effort == effort)
        });
        if !supported {
            color_eyre::eyre::bail!("Model {model} does not support {effort} effort");
        }
        let mut params = self
            .active_thread_reasoning_setting_update_params(Some(effort.clone()))
            .ok_or_else(|| color_eyre::eyre::eyre!("Parent session is unavailable"))?;
        if let Some(mode) = &mut params.collaboration_mode {
            mode.settings.reasoning_effort = Some(effort.clone());
        }
        // The daemon must acknowledge the new parent authority before another workflow launches.
        if !app_server.thread_settings_update(params).await? {
            color_eyre::eyre::bail!("Native backend does not support updating parent effort");
        }
        self.on_update_reasoning_effort(Some(effort.clone()));
        self.config.ultracode = enabled;
        self.chat_widget.set_ultracode_mode(enabled);
        if !enabled {
            let mut edits =
                crate::config_update::build_model_selection_edits(&model, Some(&effort));
            edits.push(crate::config_update::replace_config_value(
                "ultracode",
                serde_json::json!(false),
            ));
            if let Err(error) = crate::config_update::write_config_batch_to_path(
                app_server.request_handle(),
                edits,
                self.config
                    .config_layer_stack
                    .get_user_config_file()
                    .map(|path| path.to_string_lossy().into_owned()),
            )
            .await
            {
                self.chat_widget.add_warning_message(format!(
                    "Effort set to {label} for this session, but the default could not be saved: {error}"
                ));
                return Ok(());
            }
        }
        self.chat_widget.add_info_message(
            format!("Effort set to {label}"),
            enabled.then(|| "Session only: xhigh with workflow orchestration".to_string()),
        );
        Ok(())
    }
}
