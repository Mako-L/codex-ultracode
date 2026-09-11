//! Checkout authority belongs to one parent/run/worker/workspace, never the parent session.
use super::*;
use codex_app_server_protocol::CommandExecutionApprovalDecision;
use codex_app_server_protocol::CommandExecutionRequestApprovalParams;
use codex_app_server_protocol::CommandExecutionRequestApprovalResponse;
use codex_app_server_protocol::PermissionsRequestApprovalParams;
use codex_app_server_protocol::PermissionsRequestApprovalResponse;
use codex_protocol::models::AdditionalPermissionProfile as CoreAdditionalPermissions;
use codex_protocol::models::FileSystemPermissions;
use codex_sandboxing::policy_transforms::effective_file_system_sandbox_policy;
use codex_sandboxing::policy_transforms::intersect_permission_profiles;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct CheckoutGrant {
    parent_thread_id: ThreadId,
    run_id: String,
    worker_id: String,
    workspace_id: String,
    path: AbsolutePathBuf,
}

impl CheckoutGrant {
    pub(super) fn matches(&self, id: &str, workspace: &WorkflowWorkspaceOwnership) -> bool {
        self.parent_thread_id == workspace.parent_thread_id
            && self.run_id == workspace.run_id
            && self.worker_id == workspace.worker_id
            && self.workspace_id == id
            && self.path == workspace.cwd
    }
}

pub(super) fn write_scope(paths: Vec<AbsolutePathBuf>) -> CoreAdditionalPermissions {
    CoreAdditionalPermissions {
        file_system: Some(FileSystemPermissions::from_read_write_roots(
            Some(vec![]),
            Some(paths),
        )),
        ..Default::default()
    }
}

pub(super) fn checkout_command_scope(
    checkout: &AbsolutePathBuf,
    common_git_dir: &std::path::Path,
) -> Result<CoreAdditionalPermissions, JSONRPCErrorError> {
    let root = checkout
        .parent()
        .ok_or_else(|| invalid_request("checkout has no parent"))?;
    let namespace = root
        .parent()
        .ok_or_else(|| invalid_request("checkout namespace is missing"))?;
    // Git needs the parent directory to create/remove a checkout. An explicit
    // checkout root would be protected against removal by the native sandbox.
    let mut paths = vec![root];
    match std::fs::symlink_metadata(namespace.as_path()) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => paths.push(namespace),
        Err(error) => return Err(invalid_request(error.to_string())),
        Ok(_) => {}
    }
    paths.push(
        AbsolutePathBuf::from_absolute_path(common_git_dir)
            .map_err(|error| invalid_request(error.to_string()))?,
    );
    Ok(write_scope(paths))
}

pub(super) fn bounded_checkout_profile(
    parent_profile: &PermissionProfile,
    checkout_template: &PermissionProfile,
    workspace_roots: &[AbsolutePathBuf],
    checkout: &AbsolutePathBuf,
    approved: CoreAdditionalPermissions,
) -> Result<PermissionProfile, String> {
    let requested_grant = write_scope(vec![checkout.clone()]);
    let grant = intersect_permission_profiles(requested_grant, approved, checkout.as_path());
    let authority = parent_profile
        .clone()
        .materialize_project_roots_with_workspace_roots(workspace_roots);
    let requested = checkout_template
        .clone()
        .materialize_project_roots_with_workspace_roots(std::slice::from_ref(checkout));
    let granted_policy =
        effective_file_system_sandbox_policy(&authority.file_system_sandbox_policy(), Some(&grant));
    let granted = PermissionProfile::from_runtime_permissions_with_enforcement(
        authority.enforcement(),
        &granted_policy,
        authority.network_sandbox_policy(),
    );
    codex_protocol::intersect_effective_permission_profiles(
        &granted,
        &requested,
        checkout.as_path(),
    )
    .map_err(|error| error.to_string())
}

pub(super) fn validate_checkout_path(
    cwd: &AbsolutePathBuf,
    checkout: &AbsolutePathBuf,
) -> Result<(), String> {
    let cwd = std::fs::canonicalize(cwd).map_err(|error| error.to_string())?;
    if !checkout.starts_with(&cwd) {
        return Err("workflow checkout is outside its recorded repository".into());
    }
    let mut path = checkout.to_path_buf();
    while path.starts_with(cwd.as_path()) {
        if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err("workflow checkout path contains a symlink".into());
        }
        if !path.pop() {
            break;
        }
    }
    Ok(())
}

impl TurnRequestProcessor {
    pub(super) async fn workflow_checkout_grant(
        &self,
        parent: &codex_core::CodexThread,
        authority: &WorkflowAuthority,
        params: &WorkflowWorkspacePrepareParams,
        workspace_id: &str,
        checkout: &AbsolutePathBuf,
    ) -> Result<(CheckoutGrant, CoreAdditionalPermissions), JSONRPCErrorError> {
        let current = parent.config_snapshot().await;
        validate_checkout_path(current.cwd(), checkout).map_err(invalid_request)?;
        let requested = write_scope(vec![checkout.clone()]);
        let parent_policy = current
            .permission_profile
            .clone()
            .materialize_project_roots_with_workspace_roots(&current.workspace_roots)
            .file_system_sandbox_policy();
        let granted = if parent_policy
            .can_write_path_with_cwd(checkout.as_path(), current.cwd().as_path())
        {
            requested.clone()
        } else {
            if matches!(
                current.approval_policy,
                codex_protocol::protocol::AskForApproval::Never
            ) || matches!(current.approval_policy,codex_protocol::protocol::AskForApproval::Granular(ref policy) if !policy.request_permissions)
            {
                return Err(invalid_request(
                    "workflow checkout permission request is prohibited by native policy",
                ));
            }
            let reason = format!(
                "Grant isolated checkout only: parent={} run={} worker={} workspace={} path={}",
                authority.parent_thread_id,
                params.run_id,
                params.worker_id,
                workspace_id,
                checkout.display()
            );
            let permissions = codex_protocol::request_permissions::RequestPermissionProfile::from(
                requested.clone(),
            );
            if current.approvals_reviewer
                == codex_protocol::config_types::ApprovalsReviewer::AutoReview
            {
                if !parent
                    .review_workflow_checkout_grant(reason, permissions)
                    .await
                {
                    return Err(invalid_request(
                        "workflow checkout permission was not approved",
                    ));
                }
                requested.clone()
            } else {
                let connections = self
                    .thread_state_manager
                    .subscribed_connection_ids(authority.parent_thread_id)
                    .await;
                if connections.is_empty() {
                    return Err(invalid_request(
                        "workflow checkout permission requires an attached native frontend",
                    ));
                }
                let (_, response) = self
                    .outgoing
                    .send_request_to_connections(
                        Some(&connections),
                        ServerRequestPayload::PermissionsRequestApproval(
                            PermissionsRequestApprovalParams {
                                thread_id: authority.parent_thread_id.to_string(),
                                turn_id: format!("workflow-workspace-{workspace_id}"),
                                item_id: workspace_id.to_string(),
                                environment_id: None,
                                started_at_ms: chrono::Utc::now().timestamp_millis(),
                                cwd: current.cwd().clone(),
                                reason: Some(reason),
                                permissions: permissions.into(),
                            },
                        ),
                        Some(authority.parent_thread_id),
                    )
                    .await;
                let response = response
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .and_then(|value| {
                        serde_json::from_value::<PermissionsRequestApprovalResponse>(value).ok()
                    })
                    .ok_or_else(|| {
                        invalid_request("workflow checkout permission was not approved")
                    })?;
                if response.strict_auto_review.unwrap_or(false) {
                    return Err(invalid_request(
                        "workflow checkout cannot retain a strict-review grant outside its originating turn",
                    ));
                }
                CoreAdditionalPermissions::try_from(response.permissions)
                    .map_err(|error| invalid_request(error.to_string()))?
            }
        };
        let grant = intersect_permission_profiles(requested, granted, current.cwd().as_path());
        if grant.file_system.as_ref().is_none_or(|permissions| {
            !permissions.entries.iter().any(|entry| {
                entry.access == codex_protocol::permissions::FileSystemAccessMode::Write
            })
        }) {
            return Err(invalid_request("workflow checkout permission was denied"));
        }
        if !workflow_authority_matches(&authority.snapshot, &parent.config_snapshot().await) {
            return Err(invalid_request(
                "workflow parent authority changed during checkout approval",
            ));
        }
        Ok((
            CheckoutGrant {
                parent_thread_id: authority.parent_thread_id,
                run_id: params.run_id.clone(),
                worker_id: params.worker_id.clone(),
                workspace_id: workspace_id.to_string(),
                path: checkout.clone(),
            },
            grant,
        ))
    }

    pub(super) async fn materialized_checkout_profile(
        &self,
        parent: &codex_core::CodexThread,
        authority: &WorkflowAuthority,
        checkout: &AbsolutePathBuf,
        grant: CoreAdditionalPermissions,
    ) -> Result<PermissionProfile, JSONRPCErrorError> {
        let current = parent.config_snapshot().await;
        if !workflow_authority_matches(&authority.snapshot, &current) {
            return Err(invalid_request(
                "workflow parent authority changed during checkout creation",
            ));
        }
        let parent_config = parent.effective_config().await;
        // The effective parent profile already contains absolute workspace paths.
        // Rebind the canonical environment template, retaining effective authority
        // as the other side of the intersection and keeping the grant exact.
        let template = match current
            .environments
            .environments
            .first()
            .map(|selection| &selection.config)
        {
            Some(codex_protocol::protocol::EnvironmentConfigState::Ready(environment)) => {
                environment.permission_profile.permission_profile()
            }
            None | Some(codex_protocol::protocol::EnvironmentConfigState::FromThread) => {
                parent_config.permissions.permission_profile()
            }
            Some(_) => {
                return Err(invalid_request(
                    "workflow checkout environment is not ready",
                ));
            }
        };
        let profile = bounded_checkout_profile(
            &current.permission_profile,
            template,
            &current.workspace_roots,
            checkout,
            grant,
        )
        .map_err(invalid_request)?;
        parent_config
            .permissions
            .can_set_permission_profile(&profile)
            .map_err(|error| {
                invalid_request(format!(
                    "workflow checkout exceeds managed permission constraints: {error}"
                ))
            })?;
        Ok(profile)
    }

    pub(super) async fn workflow_checkout_command(
        &self,
        parent: &codex_core::CodexThread,
        parent_id: ThreadId,
        workspace_id: &str,
        command: Vec<String>,
        additional: CoreAdditionalPermissions,
        timeout: std::time::Duration,
    ) -> Result<String, JSONRPCErrorError> {
        let current = parent.config_snapshot().await;
        if let Some(reason) = parent
            .workflow_command_approval(&command, &additional)
            .await
            .map_err(invalid_request)?
        {
            let connections = self
                .thread_state_manager
                .subscribed_connection_ids(parent_id)
                .await;
            if connections.is_empty() {
                return Err(invalid_request(
                    "workflow checkout command requires an attached native frontend",
                ));
            }
            let params = serde_json::from_value::<CommandExecutionRequestApprovalParams>(serde_json::json!({
                "threadId":parent_id.to_string(),"turnId":format!("workflow-workspace-{workspace_id}"),
                "itemId":workspace_id,"startedAtMs":chrono::Utc::now().timestamp_millis(),"reason":reason,
                "command":codex_shell_command::parse_command::shlex_join(&command),"cwd":current.cwd(),
                "additionalPermissions":codex_app_server_protocol::AdditionalPermissionProfile::from(additional.clone()),
            })).map_err(|error|internal_error(error.to_string()))?;
            let (_, response) = self
                .outgoing
                .send_request_to_connections(
                    Some(&connections),
                    ServerRequestPayload::CommandExecutionRequestApproval(params),
                    Some(parent_id),
                )
                .await;
            let approved = response
                .await
                .ok()
                .and_then(Result::ok)
                .and_then(|value| {
                    serde_json::from_value::<CommandExecutionRequestApprovalResponse>(value).ok()
                })
                .is_some_and(|response| {
                    matches!(
                        response.decision,
                        CommandExecutionApprovalDecision::Accept
                            | CommandExecutionApprovalDecision::AcceptForSession
                    )
                });
            if !approved {
                return Err(invalid_request(
                    "workflow checkout command was not approved",
                ));
            }
        }
        if !workflow_authority_matches(&current, &parent.config_snapshot().await) {
            return Err(invalid_request(
                "workflow parent authority changed during command approval",
            ));
        }
        let authority = current
            .permission_profile
            .clone()
            .materialize_project_roots_with_workspace_roots(&current.workspace_roots);
        let policy = effective_file_system_sandbox_policy(
            &authority.file_system_sandbox_policy(),
            Some(&additional),
        );
        let profile = PermissionProfile::from_runtime_permissions_with_enforcement(
            authority.enforcement(),
            &policy,
            authority.network_sandbox_policy(),
        );
        let output = parent
            .execute_workflow_checkout_command(command, profile, timeout)
            .await
            .map_err(|message| JSONRPCErrorError {
                code: -32011,
                message,
                data: None,
            })?;
        if output.exit_code != 0 {
            return Err(JSONRPCErrorError {
                code: -32010,
                message: output.stderr.text,
                data: Some(serde_json::json!({"exitCode":output.exit_code})),
            });
        }
        Ok(output.stdout.text.trim().to_string())
    }

    pub(super) async fn restore_workflow_checkout(
        &self,
        parent: &codex_core::CodexThread,
        id: &str,
        workspace: &mut WorkflowWorkspaceOwnership,
    ) -> Result<(), JSONRPCErrorError> {
        if workspace
            .checkout_grant
            .as_ref()
            .is_none_or(|grant| !grant.matches(id, workspace))
        {
            return Err(invalid_request(
                "workflow checkout grant ownership is stale",
            ));
        }
        validate_checkout_path(&workspace.repository_cwd, &workspace.cwd)
            .map_err(invalid_request)?;
        if workspace.cwd.exists() {
            return Err(invalid_request(
                "removed workflow path was recreated externally",
            ));
        }
        let scope = checkout_command_scope(&workspace.cwd, &workspace.common_git_dir)?;
        if let Err(mut error) = self
            .workflow_checkout_command(
                parent,
                workspace.parent_thread_id,
                id,
                vec![
                    "git".into(),
                    "worktree".into(),
                    "add".into(),
                    "-b".into(),
                    workspace.branch.clone(),
                    workspace.cwd.to_string_lossy().into_owned(),
                    workspace.base_commit.clone(),
                ],
                scope.clone(),
                std::time::Duration::from_secs(30),
            )
            .await
        {
            if matches!(error.code, -32010 | -32011)
                && let Err(cleanup) = self
                    .cleanup_workflow_checkout(parent, id, workspace, scope)
                    .await
            {
                error.message.push_str(&format!(
                    "; cleanup unresolved at {}: {}",
                    workspace.cwd.display(),
                    cleanup.message
                ));
            }
            return Err(error);
        }
        if !workflow_worktree_matches(
            workspace.cwd.as_path(),
            &workspace.common_git_dir,
            &workspace.base_commit,
            Some(&workspace.branch),
        ) {
            let mut error = internal_error("restored workflow checkout failed identity validation");
            if let Err(cleanup) = self
                .cleanup_workflow_checkout(parent, id, workspace, scope.clone())
                .await
            {
                error
                    .message
                    .push_str(&format!("; cleanup unresolved: {}", cleanup.message));
            }
            return Err(error);
        }
        if let Err(mut error) = self
            .workflow_checkout_command(
                parent,
                workspace.parent_thread_id,
                id,
                vec![
                    "git".into(),
                    "worktree".into(),
                    "lock".into(),
                    "--reason".into(),
                    workflow_lock_reason(id),
                    workspace.cwd.to_string_lossy().into_owned(),
                ],
                scope.clone(),
                std::time::Duration::from_secs(30),
            )
            .await
        {
            if let Err(cleanup) = self
                .cleanup_workflow_checkout(parent, id, workspace, scope)
                .await
            {
                error.message.push_str(&format!(
                    "; cleanup unresolved at {}: {}",
                    workspace.cwd.display(),
                    cleanup.message
                ));
            }
            return Err(error);
        }
        workspace.removed = false;
        Ok(())
    }

    pub(super) async fn cleanup_workflow_checkout(
        &self,
        parent: &codex_core::CodexThread,
        id: &str,
        workspace: &WorkflowWorkspaceOwnership,
        scope: CoreAdditionalPermissions,
    ) -> Result<(), JSONRPCErrorError> {
        let timeout = std::time::Duration::from_secs(30);
        if workflow_worktree_matches(
            workspace.cwd.as_path(),
            &workspace.common_git_dir,
            &workspace.base_commit,
            Some(&workspace.branch),
        ) {
            if workflow_owned_lock(workspace.cwd.as_path(), id) {
                self.workflow_checkout_command(
                    parent,
                    workspace.parent_thread_id,
                    id,
                    vec![
                        "git".into(),
                        "worktree".into(),
                        "unlock".into(),
                        workspace.cwd.to_string_lossy().into_owned(),
                    ],
                    scope.clone(),
                    timeout,
                )
                .await?;
            }
            self.workflow_checkout_command(
                parent,
                workspace.parent_thread_id,
                id,
                vec![
                    "git".into(),
                    "worktree".into(),
                    "remove".into(),
                    "--force".into(),
                    workspace.cwd.to_string_lossy().into_owned(),
                ],
                scope.clone(),
                timeout,
            )
            .await?;
        }
        let reference = format!("refs/heads/{}", workspace.branch);
        let command = vec![
            "git".into(),
            "rev-parse".into(),
            "--verify".into(),
            reference.clone(),
        ];
        match self
            .workflow_checkout_command(
                parent,
                workspace.parent_thread_id,
                id,
                command,
                Default::default(),
                timeout,
            )
            .await
        {
            Ok(commit) if commit == workspace.base_commit => {
                self.workflow_checkout_command(
                    parent,
                    workspace.parent_thread_id,
                    id,
                    vec![
                        "git".into(),
                        "update-ref".into(),
                        "-d".into(),
                        reference,
                        workspace.base_commit.clone(),
                    ],
                    scope,
                    timeout,
                )
                .await?;
            }
            Err(error) if error.code == -32010 => {}
            Err(error) => return Err(error),
            Ok(_) => {
                return Err(internal_error(
                    "workflow branch changed during cleanup; evidence retained",
                ));
            }
        }
        match std::fs::symlink_metadata(workspace.cwd.as_path()) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => {
                return Err(internal_error(format!(
                    "checkout at {} could not be safely removed; evidence retained",
                    workspace.cwd.display()
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "workflow_workspace_permissions_tests.rs"]
mod tests;
