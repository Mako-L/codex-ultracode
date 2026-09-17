//! Permission-aware execution for native, host-owned workflow checkout operations.
use super::*;
use crate::exec::ExecCapturePolicy;
use crate::exec::ExecExpiration;
use crate::exec::ExecParams;
use crate::exec_policy::ExecApprovalRequest;
use crate::guardian::GuardianApprovalRequest;
use crate::guardian::new_guardian_review_id;
use crate::sandboxing::SandboxPermissions;
use crate::tools::sandboxing::ApprovalRequestReasons;
use crate::tools::sandboxing::ExecApprovalRequirement;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_protocol::protocol::ReviewDecision;
use codex_protocol::request_permissions::RequestPermissionProfile;

impl CodexThread {
    async fn workflow_operation_turn(
        &self,
    ) -> (Arc<crate::session::turn_context::TurnContext>, bool) {
        if let Some((turn, _, strict)) = self
            .session
            .active_turn_context_and_strict_auto_review()
            .await
        {
            (turn, strict)
        } else {
            (self.session.new_default_turn().await, false)
        }
    }

    /// Evaluate a checkout command using the parent's native rules. `Some` requires
    /// ordinary user command approval; `None` is already authorized by native policy.
    pub async fn workflow_command_approval(
        &self,
        command: &[String],
        additional_permissions: &AdditionalPermissionProfile,
    ) -> Result<Option<String>, String> {
        let (turn, strict_review) = self.workflow_operation_turn().await;
        let environment = turn
            .environments
            .single_local_environment()
            .ok_or("workflow checkout requires one local executor")?;
        let snapshot = self.config_snapshot().await;
        let policy = environment.permission_profile().clone();
        if matches!(policy, PermissionProfile::External { .. }) {
            return Err("workflow checkout cannot bypass an external executor".into());
        }
        let requested = additional_permissions
            .file_system
            .as_ref()
            .is_some_and(|permissions| {
                permissions.entries.iter().any(|entry| match &entry.path {
                    codex_protocol::permissions::FileSystemPath::Path { path } => {
                        path.to_abs_path().ok().is_none_or(|path| {
                            !policy
                                .file_system_sandbox_policy()
                                .can_write_local_path_with_cwd(
                                    path.as_path(),
                                    snapshot.cwd().as_path(),
                                )
                        })
                    }
                    _ => true,
                })
            });
        let sandbox_permissions = if requested {
            SandboxPermissions::RequireEscalated
        } else {
            SandboxPermissions::UseDefault
        };
        let session_shell = self.session.user_shell();
        let requirement = self
            .session
            .services
            .exec_policy
            .create_exec_approval_requirement_for_shell(
                ExecApprovalRequest {
                    command,
                    approval_policy: snapshot.approval_policy,
                    permission_profile: policy,
                    environment_policy: environment.config().exec_policy.as_ref(),
                    windows_sandbox_level: environment.config().windows_sandbox_level,
                    sandbox_permissions,
                    prefix_rule: None,
                    allow_prefix_rules: turn.allow_prefix_rules(),
                },
                environment.shell.as_ref().unwrap_or(session_shell.as_ref()),
                &codex_tools::UnifiedExecShellMode::Direct,
                codex_shell_command::is_dangerous_command::DangerousCommandPlatform::host(),
            )
            .await;
        let reason = match requirement {
            ExecApprovalRequirement::Forbidden { reason } => return Err(reason),
            ExecApprovalRequirement::Skip { .. } if !strict_review => return Ok(None),
            ExecApprovalRequirement::Skip { .. } => {
                "The parent requires strict review of every command".into()
            }
            ExecApprovalRequirement::NeedsApproval { reason, .. } => {
                reason.unwrap_or_else(|| "Create an isolated workflow checkout".into())
            }
        };
        if snapshot.approvals_reviewer == ApprovalsReviewer::User && !strict_review {
            return Ok(Some(reason));
        }
        let cwd = snapshot.cwd();
        let request = GuardianApprovalRequest::ExecCommand {
            id: new_guardian_review_id(),
            environment_id: environment.selection.environment_id.clone(),
            command: command.to_vec(),
            cwd: cwd.clone().into(),
            guardian_cwd: codex_utils_path_uri::LegacyAppPathString::from_abs_path(cwd),
            sandbox_permissions,
            additional_permissions: Some(additional_permissions.clone()),
            justification: Some(reason.clone()),
            tty: false,
        };
        let decision = crate::guardian::decide_approval(
            Arc::clone(&self.session),
            turn,
            new_guardian_review_id(),
            request,
            ApprovalRequestReasons {
                approval: Some(reason),
                retry: None,
            },
            crate::guardian::GuardianReviewOptions {
                require_guardian: true,
                plugin_attribution_override: None,
                approval_request_source: codex_analytics::GuardianApprovalRequestSource::MainTurn,
                external_cancel: None,
                require_synchronous_review: false,
            },
        )
        .await;
        match decision {
            Some(ReviewDecision::Approved) | Some(ReviewDecision::ApprovedForSession) => Ok(None),
            _ => Err("workflow checkout command was not approved".into()),
        }
    }

    /// Review an exact checkout grant without adding it to parent turn/session grants.
    pub async fn review_workflow_checkout_grant(
        &self,
        identity: String,
        permissions: RequestPermissionProfile,
    ) -> bool {
        let (turn, _) = self.workflow_operation_turn().await;
        let request = GuardianApprovalRequest::RequestPermissions {
            id: new_guardian_review_id(),
            turn_id: turn.sub_id.clone(),
            reason: Some(identity),
            permissions,
        };
        matches!(
            crate::guardian::decide_approval(
                Arc::clone(&self.session),
                turn,
                new_guardian_review_id(),
                request,
                ApprovalRequestReasons {
                    approval: Some("Grant only this isolated workflow checkout".into()),
                    retry: None
                },
                crate::guardian::GuardianReviewOptions {
                    require_guardian: true,
                    plugin_attribution_override: None,
                    approval_request_source:
                        codex_analytics::GuardianApprovalRequestSource::MainTurn,
                    external_cancel: None,
                    require_synchronous_review: false,
                },
            )
            .await,
            Some(ReviewDecision::Approved) | Some(ReviewDecision::ApprovedForSession)
        )
    }

    /// Execute an already-approved checkout command under its operation-only profile.
    /// Worker profiles never receive this command's repository metadata grant.
    pub async fn execute_workflow_checkout_command(
        &self,
        command: Vec<String>,
        permission_profile: PermissionProfile,
        timeout: std::time::Duration,
    ) -> Result<codex_protocol::exec_output::ExecToolCallOutput, String> {
        let (turn, _) = self.workflow_operation_turn().await;
        let environment = turn
            .environments
            .single_local_environment()
            .ok_or("workflow checkout requires one local executor")?;
        let cwd = environment
            .cwd()
            .to_abs_path()
            .map_err(|error| error.to_string())?;
        turn.config
            .permissions
            .can_set_permission_profile(&permission_profile)
            .map_err(|error| error.to_string())?;
        let mut env = crate::exec_env::create_env(environment.shell_environment_policy(), None);
        env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
        let mut command = command;
        if command.first().map(String::as_str) == Some("git") {
            command[0] = codex_git_utils::git_program();
        }
        let output = crate::exec::process_exec_tool_call(
            ExecParams {
                command,
                cwd: cwd.clone(),
                expiration: ExecExpiration::Timeout(timeout),
                capture_policy: ExecCapturePolicy::ShellTool,
                env,
                network: turn.network.clone(),
                network_environment_id: Some(environment.selection.environment_id.clone()),
                sandbox_permissions: SandboxPermissions::UseDefault,
                windows_sandbox_level: environment.config().windows_sandbox_level,
                windows_sandbox_private_desktop: environment
                    .config()
                    .windows_sandbox_private_desktop,
                justification: None,
                arg0: None,
            },
            &permission_profile,
            &cwd,
            &[cwd.clone()],
            &turn.config.codex_linux_sandbox_exe,
            &turn.config.codex_self_exe,
            turn.config.features.use_legacy_landlock(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
        Ok(output)
    }
}
