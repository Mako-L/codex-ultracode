use super::thread_input::ensure_direct_input_allowed;
use super::*;
use codex_agent_extension::AgentInvocation;
use codex_agent_extension::AgentRun;
use codex_agent_extension::AgentRunner;
use codex_app_server_protocol::ImageReference as V2ImageReference;
use codex_app_server_protocol::WorkflowPluginOption;
use codex_core::ThreadConfigSnapshot;
use codex_core::config::PermissionProfileSnapshot;
use codex_exec_server::CreateDirectoryOptions;
use codex_exec_server::WriteFileOptions;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ImageReference as CoreImageReference;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AdditionalContextEntry as CoreAdditionalContextEntry;
use codex_protocol::protocol::AdditionalContextKind as CoreAdditionalContextKind;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::ThreadSource;
use codex_protocol::protocol::TurnSettingsUpdate;
use codex_protocol::protocol::TurnSettingsUpdateOutcome;
use codex_skills::system_cache_root_dir;
use sha2::Digest;
use sha2::Sha256;

use crate::image_url::REMOTE_IMAGE_URL_ERROR;
use crate::image_url::is_remote_image_url;

pub(super) fn validate_user_input_image_urls(
    input: &[V2UserInput],
) -> Result<(), JSONRPCErrorError> {
    if input.iter().any(|item| {
        matches!(
            item,
            V2UserInput::Image {
                image: V2ImageReference::Inline { url },
                ..
            } if is_remote_image_url(url)
        )
    }) {
        return Err(invalid_request(REMOTE_IMAGE_URL_ERROR));
    }
    Ok(())
}

fn validate_response_item_image_urls(items: &[ResponseItem]) -> Result<(), JSONRPCErrorError> {
    if items.iter().any(|item| match item {
        ResponseItem::Message { content, .. } => content.iter().any(|item| {
            matches!(
                item,
                ContentItem::InputImage { image: CoreImageReference::Inline { image_url }, .. } if is_remote_image_url(image_url)
            )
        }),
        ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. } => {
            output.content_items().is_some_and(|content| {
                content.iter().any(|item| {
                    matches!(
                        item,
                        FunctionCallOutputContentItem::InputImage { image: CoreImageReference::Inline { image_url }, .. }
                            if is_remote_image_url(image_url)
                    )
                })
            })
        }
        ResponseItem::Reasoning { .. }
        | ResponseItem::AgentMessage { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::FunctionCall { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::ConfigurationUpdate { .. }
        | ResponseItem::CompactionTrigger { .. }
        | ResponseItem::ContextCompaction { .. }
        | ResponseItem::AdditionalTools { .. }
        | ResponseItem::Other => false,
    }) {
        return Err(invalid_request(REMOTE_IMAGE_URL_ERROR));
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct TurnRequestProcessor {
    agent_runner: AgentRunner,
    auth_manager: Arc<AuthManager>,
    thread_manager: Arc<ThreadManager>,
    outgoing: Arc<OutgoingMessageSender>,
    analytics_events_client: AnalyticsEventsClient,
    arg0_paths: Arg0DispatchPaths,
    config: Arc<Config>,
    config_manager: ConfigManager,
    pending_thread_unloads: Arc<Mutex<HashSet<ThreadId>>>,
    thread_state_manager: ThreadStateManager,
    thread_watch_manager: ThreadWatchManager,
    skills_watcher: Arc<SkillsWatcher>,
    turn_cost_worker: Option<crate::turn_cost_worker::TurnCostWorkerHandle>,
    workflow_authorities: Arc<Mutex<HashMap<String, WorkflowAuthority>>>,
    workflow_workers: Arc<Mutex<HashMap<ThreadId, WorkflowWorkerOwnership>>>,
    workflow_workspaces: Arc<Mutex<HashMap<String, WorkflowWorkspaceOwnership>>>,
}

#[derive(Clone)]
struct WorkflowAuthority {
    parent_thread_id: ThreadId,
    generation: u64,
    snapshot: ThreadConfigSnapshot,
    digest: String,
    allow_isolated_workspaces: bool,
    codex_home: AbsolutePathBuf,
}

#[derive(Clone)]
struct WorkflowWorkerOwnership {
    parent_thread_id: ThreadId,
    authority_digest: String,
    run_id: String,
    worker_id: String,
    launch_intent: bool,
    admitted_authority: Option<ThreadConfigSnapshot>,
    read_only: bool,
    agent_type: String,
    schema_digest: Option<String>,
    workspace_id: Option<String>,
    cwd: AbsolutePathBuf,
    role_digest: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct WorkflowWorkspaceOwnership {
    parent_thread_id: ThreadId,
    authority_ref: String,
    authority_digest: String,
    run_id: String,
    worker_id: String,
    cwd: AbsolutePathBuf,
    base_commit: String,
    permission_profile: PermissionProfile,
    released: bool,
    common_git_dir: std::path::PathBuf,
    repository_cwd: AbsolutePathBuf,
    branch: String,
    #[serde(default)]
    removed: bool,
    #[serde(default)]
    checkout_grant: Option<workflow_workspace_permissions::CheckoutGrant>,
}

#[path = "workflow_workspace_permissions.rs"]
mod workflow_workspace_permissions;

fn workflow_lock_reason(id: &str) -> String {
    format!("ultracode-workflow:{id}")
}

fn workflow_owned_lock(cwd: &std::path::Path, id: &str) -> bool {
    let Ok(expected) = std::fs::canonicalize(cwd) else {
        return false;
    };
    let Ok(text) = workflow_git(
        cwd,
        &[
            "worktree".as_ref(),
            "list".as_ref(),
            "--porcelain".as_ref(),
            "-z".as_ref(),
        ],
    ) else {
        return false;
    };
    let mut matching_record = false;
    for field in text.split('\0') {
        if field.is_empty() {
            matching_record = false;
        } else if let Some(path) = field.strip_prefix("worktree ") {
            matching_record = std::fs::canonicalize(path).is_ok_and(|path| path == expected);
        } else if matching_record && field == format!("locked {}", workflow_lock_reason(id)) {
            return true;
        }
    }
    false
}

fn workflow_workspace_unchanged(workspace: &WorkflowWorkspaceOwnership) -> bool {
    workflow_git(
        workspace.cwd.as_path(),
        &["rev-parse".as_ref(), "HEAD".as_ref()],
    )
    .is_ok_and(|head| head == workspace.base_commit)
        && workflow_git(
            workspace.cwd.as_path(),
            &["status".as_ref(), "--porcelain".as_ref()],
        )
        .is_ok_and(|status| status.is_empty())
}

#[cfg(test)]
fn materialize_fixture_worktree(
    id: &str,
    workspace: &mut WorkflowWorkspaceOwnership,
) -> Result<(), String> {
    workflow_git(
        workspace.repository_cwd.as_path(),
        &[
            "worktree".as_ref(),
            "add".as_ref(),
            "-b".as_ref(),
            workspace.branch.as_ref(),
            workspace.cwd.as_os_str(),
            workspace.base_commit.as_ref(),
        ],
    )?;
    workflow_git(
        workspace.repository_cwd.as_path(),
        &[
            "worktree".as_ref(),
            "lock".as_ref(),
            "--reason".as_ref(),
            workflow_lock_reason(id).as_ref(),
            workspace.cwd.as_os_str(),
        ],
    )?;
    workspace.removed = false;
    Ok(())
}

fn workflow_workspace_record_path(
    common_git_dir: &std::path::Path,
    id: &str,
) -> std::path::PathBuf {
    common_git_dir
        .join("ultracode-workspaces")
        .join(format!("{id}.json"))
}

fn persist_workflow_workspace(
    id: &str,
    workspace: &WorkflowWorkspaceOwnership,
) -> Result<(), String> {
    let path = workflow_workspace_record_path(&workspace.common_git_dir, id);
    let directory = path.parent().ok_or("invalid workspace record path")?;
    for component in [directory, path.as_path()] {
        if std::fs::symlink_metadata(component)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err("workflow ownership metadata contains a symlink".into());
        }
    }
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    use std::io::Write;
    file.write_all(&serde_json::to_vec(workspace).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    std::fs::rename(temporary, &path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    std::fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn load_workflow_workspace(
    authority_cwd: &std::path::Path,
    id: &str,
) -> Result<WorkflowWorkspaceOwnership, String> {
    let common = workflow_common_git_dir(authority_cwd)?;
    let bytes =
        std::fs::read(workflow_workspace_record_path(&common, id)).map_err(|e| e.to_string())?;
    let workspace: WorkflowWorkspaceOwnership =
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if workspace.common_git_dir != common {
        return Err("workspace record belongs to another repository".into());
    }
    Ok(workspace)
}

fn workflow_id_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn validate_workflow_workspace_id(value: &str) -> Result<(), JSONRPCErrorError> {
    let id =
        Uuid::parse_str(value).map_err(|_| invalid_request("invalid workflow workspace ID"))?;
    if id.to_string() != value {
        return Err(invalid_request("invalid workflow workspace ID"));
    }
    Ok(())
}

fn workflow_git(cwd: &std::path::Path, args: &[&std::ffi::OsStr]) -> Result<String, String> {
    let mut command = std::process::Command::new("git");
    command.current_dir(cwd).args(args);
    codex_protocol::shell_environment::scrub_non_inheritable_env_vars(&mut command);
    let output = command.output().map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn workflow_common_git_dir(cwd: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let value = workflow_git(cwd, &["rev-parse".as_ref(), "--git-common-dir".as_ref()])?;
    std::fs::canonicalize(cwd.join(value)).map_err(|err| err.to_string())
}

fn workflow_project_save_directory(cwd: &std::path::Path) -> Result<AbsolutePathBuf, String> {
    let boundary = workflow_git(cwd, &["rev-parse".as_ref(), "--show-toplevel".as_ref()])
        .ok()
        .and_then(|path| AbsolutePathBuf::from_absolute_path(path).ok())
        .or_else(|| AbsolutePathBuf::from_absolute_path(cwd).ok())
        .ok_or_else(|| "workflow cwd is not absolute".to_string())?;
    let cwd =
        AbsolutePathBuf::from_absolute_path(std::fs::canonicalize(cwd).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let mut current = Some(cwd.as_path());
    while let Some(root) = current {
        if !root.starts_with(boundary.as_path()) {
            break;
        }
        let candidate = root.join(".codex/workflows");
        if candidate.is_dir() {
            return AbsolutePathBuf::from_absolute_path(candidate).map_err(|e| e.to_string());
        }
        if root == boundary.as_path() {
            break;
        }
        current = root.parent();
    }
    Ok(boundary.join(".codex/workflows"))
}

fn workflow_save_directory_creation_policy(
    mut policy: codex_protocol::permissions::FileSystemSandboxPolicy,
    parent_directory: &AbsolutePathBuf,
) -> codex_protocol::permissions::FileSystemSandboxPolicy {
    policy
        .entries
        .push(codex_protocol::permissions::FileSystemSandboxEntry::new(
            parent_directory.clone().into(),
            codex_protocol::permissions::FileSystemAccessMode::Write,
        ));
    policy
}

fn workflow_worktree_matches(
    cwd: &std::path::Path,
    common_git_dir: &std::path::Path,
    base_commit: &str,
    branch: Option<&str>,
) -> bool {
    let Ok(canonical_cwd) = std::fs::canonicalize(cwd) else {
        return false;
    };
    let Ok(top_level) = workflow_git(cwd, &["rev-parse".as_ref(), "--show-toplevel".as_ref()])
    else {
        return false;
    };
    let Ok(canonical_top_level) = std::fs::canonicalize(top_level) else {
        return false;
    };
    canonical_cwd == canonical_top_level
        && workflow_common_git_dir(cwd).ok().as_deref() == Some(common_git_dir)
        && match branch {
            Some(branch) => workflow_git(
                cwd,
                &["symbolic-ref".as_ref(), "-q".as_ref(), "HEAD".as_ref()],
            )
            .is_ok_and(|value| value == format!("refs/heads/{branch}")),
            None => workflow_git(
                cwd,
                &["symbolic-ref".as_ref(), "-q".as_ref(), "HEAD".as_ref()],
            )
            .is_err(),
        }
        && workflow_git(
            cwd,
            &[
                "merge-base".as_ref(),
                "--is-ancestor".as_ref(),
                base_commit.as_ref(),
                "HEAD".as_ref(),
            ],
        )
        .is_ok()
}

fn workflow_release_eligible(status: &AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::Completed(_)
            | AgentStatus::Errored(_)
            | AgentStatus::Interrupted
            | AgentStatus::Shutdown
    )
}

fn workflow_release_lease(workspace: &mut WorkflowWorkspaceOwnership) {
    workspace.released = true;
}

fn workflow_schema_digest(schema: Option<&serde_json::Value>) -> Option<String> {
    schema.map(|value| format!("{:x}", Sha256::digest(value.to_string().as_bytes())))
}

fn workflow_effective_selection<T>(
    parent: Option<T>,
    role: Option<T>,
    requested: T,
    explicit: bool,
) -> Option<T> {
    if explicit {
        Some(requested)
    } else {
        role.or(parent)
    }
}

fn workflow_role_digest(config: &Config) -> String {
    let material = format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
        config.developer_instructions,
        config.model_reasoning_summary,
        config.model_verbosity,
        config.personality,
        config.features,
        config.include_skill_instructions,
        config.config_layer_stack,
    );
    format!("{:x}", Sha256::digest(material.as_bytes()))
}

async fn workflow_apply_role(config: &mut Config, agent_type: &str) -> Result<String, String> {
    let native_role = match agent_type {
        "general-purpose" | "Plan" => "default",
        "Explore" => "explorer",
        custom => custom,
    };
    codex_core::apply_role_to_config(config, Some(native_role)).await?;
    config.exclude_ultracode_plugin_mcp = true;
    let _ = config.features.disable(Feature::Collab);
    let _ = config.features.disable(Feature::MultiAgentV2);
    let mut mcp_servers = config.mcp_servers.get().clone();
    mcp_servers.remove("ultracode");
    if let Some(task_tools) = mcp_servers.get_mut("codex_tui") {
        let disabled = task_tools.disabled_tools.get_or_insert_default();
        if !disabled.iter().any(|tool| tool == "workflow") {
            disabled.push("workflow".to_string());
        }
    }
    config
        .mcp_servers
        .set(mcp_servers)
        .map_err(|err| format!("workflow MCP policy is invalid: {err}"))?;
    Ok(workflow_role_digest(config))
}

fn workflow_authority_digest(
    snapshot: &ThreadConfigSnapshot,
    plugins: &[WorkflowPluginOption],
    web_search_available: bool,
) -> String {
    let material = format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{web_search_available}",
        snapshot.approval_policy,
        snapshot.approvals_reviewer,
        snapshot.permission_profile,
        snapshot.active_permission_profile,
        snapshot.environments,
        snapshot.workspace_roots,
        snapshot.profile_workspace_roots,
        snapshot.cwd(),
        plugins,
    );
    format!("{:x}", Sha256::digest(material.as_bytes()))
}

fn workflow_authority_matches(left: &ThreadConfigSnapshot, right: &ThreadConfigSnapshot) -> bool {
    left.approval_policy == right.approval_policy
        && left.approvals_reviewer == right.approvals_reviewer
        && left.permission_profile == right.permission_profile
        && left.active_permission_profile == right.active_permission_profile
        && left.environments == right.environments
        && left.workspace_roots == right.workspace_roots
        && left.profile_workspace_roots == right.profile_workspace_roots
        && left.cwd() == right.cwd()
}

fn workflow_authority_ref_matches(
    authority_parent_thread_id: ThreadId,
    authority_generation: u64,
    parent_thread_id: ThreadId,
    generation: u64,
) -> bool {
    authority_parent_thread_id == parent_thread_id && authority_generation == generation
}

fn map_additional_context(
    additional_context: Option<HashMap<String, AdditionalContextEntry>>,
) -> BTreeMap<String, CoreAdditionalContextEntry> {
    additional_context
        .unwrap_or_default()
        .into_iter()
        .map(|(key, entry)| {
            (
                key,
                CoreAdditionalContextEntry {
                    value: entry.value,
                    kind: match entry.kind {
                        AdditionalContextKind::Untrusted => CoreAdditionalContextKind::Untrusted,
                        AdditionalContextKind::Application => {
                            CoreAdditionalContextKind::Application
                        }
                    },
                },
            )
        })
        .collect()
}

#[cfg(test)]
#[path = "workflow_child_inventory_tests.rs"]
mod workflow_child_inventory_tests;

#[cfg(test)]
mod workflow_authority_tests {
    use super::*;
    use codex_protocol::config_types::ApprovalsReviewer;
    use codex_protocol::config_types::ModeKind;
    use codex_protocol::config_types::Settings;
    use codex_protocol::models::PermissionProfile;
    use codex_protocol::permissions::FileSystemAccessMode;
    use codex_protocol::permissions::FileSystemSandboxEntry;
    use codex_protocol::protocol::AskForApproval;
    use codex_protocol::protocol::SessionSource;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use std::path::PathBuf;

    fn path(value: &str) -> AbsolutePathBuf {
        AbsolutePathBuf::try_from(PathBuf::from(value)).expect("absolute test path")
    }

    fn snapshot() -> ThreadConfigSnapshot {
        let cwd = path("/repo");
        ThreadConfigSnapshot {
            model: "gpt-test".into(),
            model_provider_id: "openai".into(),
            service_tier: None,
            approval_policy: AskForApproval::OnRequest,
            approvals_reviewer: ApprovalsReviewer::User,
            permission_profile: PermissionProfile::Disabled,
            full_access: true,
            active_permission_profile: None,
            environments: TurnEnvironmentSelections::new(cwd.clone(), Vec::new()),
            workspace_roots: vec![cwd.clone()],
            profile_workspace_roots: vec![cwd],
            ephemeral: false,
            reasoning_effort: None,
            reasoning_summary: None,
            personality: None,
            collaboration_mode: CollaborationMode {
                mode: ModeKind::Default,
                settings: Settings {
                    model: "gpt-test".into(),
                    reasoning_effort: None,
                    developer_instructions: None,
                },
            },
            session_source: SessionSource::Cli,
            history_mode: Default::default(),
            forked_from_thread_id: None,
            parent_thread_id: None,
            thread_source: None,
            originator: "test".into(),
            disabled_plugin_ids: Vec::new(),
        }
    }

    #[test]
    fn rejects_every_authority_change() {
        let captured = snapshot();

        let mut changed = captured.clone();
        changed.permission_profile = PermissionProfile::default();
        assert!(!workflow_authority_matches(&captured, &changed));

        let mut changed = captured.clone();
        changed.workspace_roots = vec![path("/other")];
        assert!(!workflow_authority_matches(&captured, &changed));

        let mut changed = captured.clone();
        changed.profile_workspace_roots = vec![path("/other")];
        assert!(!workflow_authority_matches(&captured, &changed));

        let mut changed = captured.clone();
        changed.approval_policy = AskForApproval::Never;
        assert!(!workflow_authority_matches(&captured, &changed));

        let mut changed = captured.clone();
        changed.approvals_reviewer = ApprovalsReviewer::AutoReview;
        assert!(!workflow_authority_matches(&captured, &changed));

        let mut changed = captured.clone();
        changed.environments = TurnEnvironmentSelections::new(path("/other"), Vec::new());
        assert!(!workflow_authority_matches(&captured, &changed));
    }

    #[test]
    fn rejects_parent_or_generation_mismatch() {
        let parent = ThreadId::new();
        assert!(workflow_authority_ref_matches(parent, 3, parent, 3));
        assert!(!workflow_authority_ref_matches(
            parent,
            3,
            ThreadId::new(),
            3
        ));
        assert!(!workflow_authority_ref_matches(parent, 3, parent, 4));
    }

    #[test]
    fn nested_worktree_intersection_preserves_parent_denial() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = AbsolutePathBuf::from_absolute_path(temp.path()).expect("root");
        let checkout = root.join(".ultracode/worktrees/run-worker");
        std::fs::create_dir_all(&checkout).expect("checkout");
        let denied = checkout.join("secret");
        std::fs::write(&denied, "secret").expect("secret");
        let authority = PermissionProfile::workspace_write_with(
            &[],
            codex_protocol::permissions::NetworkSandboxPolicy::Restricted,
            false,
            true,
        )
        .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&root));
        let mut policy = authority.file_system_sandbox_policy();
        policy.entries.push(FileSystemSandboxEntry::new(
            denied.clone().into(),
            FileSystemAccessMode::Deny,
        ));
        let authority = PermissionProfile::from_runtime_permissions(
            &policy,
            codex_protocol::permissions::NetworkSandboxPolicy::Restricted,
        );
        let requested = PermissionProfile::workspace_write_with(
            &[],
            codex_protocol::permissions::NetworkSandboxPolicy::Restricted,
            false,
            true,
        )
        .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&checkout));
        let result = codex_protocol::intersect_effective_permission_profiles(
            &authority,
            &requested,
            checkout.as_path(),
        )
        .expect("safe intersection");
        let denied = denied.canonicalize().expect("canonical denied path");
        assert!(result.file_system_sandbox_policy().entries.contains(
            &FileSystemSandboxEntry::new(denied.into(), FileSystemAccessMode::Deny)
        ));
    }

    #[test]
    fn workflow_save_directory_bootstrap_is_scoped_before_final_write() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = AbsolutePathBuf::from_absolute_path(
            temp.path().canonicalize().expect("canonical root"),
        )
        .expect("absolute root");
        let parent = root.join(".codex");
        let config = parent.join("config.toml");
        let workflows = parent.join("workflows");
        let final_policy = PermissionProfile::workspace_write_with(
            &[],
            codex_protocol::permissions::NetworkSandboxPolicy::Restricted,
            false,
            true,
        )
        .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&root))
        .file_system_sandbox_policy();
        let additional = codex_protocol::models::AdditionalPermissionProfile {
            file_system: Some(
                codex_protocol::models::FileSystemPermissions::from_read_write_roots(
                    Some(vec![]),
                    Some(vec![workflows]),
                ),
            ),
            ..Default::default()
        };
        let final_policy =
            codex_sandboxing::policy_transforms::effective_file_system_sandbox_policy(
                &final_policy,
                Some(&additional),
            );
        assert!(!final_policy.can_write_local_path_with_cwd(config.as_path(), root.as_path()));
        assert!(!final_policy.can_write_local_path_with_cwd(parent.as_path(), root.as_path()));

        let bootstrap = workflow_save_directory_creation_policy(final_policy.clone(), &parent);
        assert!(bootstrap.can_write_local_path_with_cwd(parent.as_path(), root.as_path()));
        assert!(bootstrap.can_write_local_path_with_cwd(config.as_path(), root.as_path()));
        let mut explicitly_denied = final_policy;
        explicitly_denied.entries.push(FileSystemSandboxEntry::new(
            parent.clone().into(),
            FileSystemAccessMode::Deny,
        ));
        let denied_bootstrap = workflow_save_directory_creation_policy(explicitly_denied, &parent);
        assert!(!denied_bootstrap.can_write_local_path_with_cwd(parent.as_path(), root.as_path()));
    }

    #[test]
    fn creates_detached_worktree_at_parent_head() {
        let temp = tempfile::tempdir().expect("tempdir");
        workflow_git(temp.path(), &["init".as_ref()]).expect("init");
        workflow_git(
            temp.path(),
            &[
                "config".as_ref(),
                "user.email".as_ref(),
                "test@example.com".as_ref(),
            ],
        )
        .expect("email");
        workflow_git(
            temp.path(),
            &["config".as_ref(), "user.name".as_ref(), "Test".as_ref()],
        )
        .expect("name");
        std::fs::write(temp.path().join("file"), "base").expect("file");
        workflow_git(temp.path(), &["add".as_ref(), "file".as_ref()]).expect("add");
        workflow_git(
            temp.path(),
            &["commit".as_ref(), "-m".as_ref(), "base".as_ref()],
        )
        .expect("commit");
        let base =
            workflow_git(temp.path(), &["rev-parse".as_ref(), "HEAD".as_ref()]).expect("head");
        let checkout = temp.path().join(".ultracode/worktrees/run-worker");
        std::fs::create_dir_all(checkout.parent().expect("parent")).expect("root");
        workflow_git(
            temp.path(),
            &[
                "worktree".as_ref(),
                "add".as_ref(),
                "--detach".as_ref(),
                checkout.as_os_str(),
                base.as_ref(),
            ],
        )
        .expect("worktree");
        assert_eq!(
            workflow_git(&checkout, &["rev-parse".as_ref(), "HEAD".as_ref()])
                .expect("worktree head"),
            base
        );
        assert!(checkout.join(".git").is_file());
        let common_git_dir = workflow_common_git_dir(temp.path()).expect("common git dir");
        assert!(workflow_worktree_matches(
            &checkout,
            &common_git_dir,
            &base,
            None,
        ));
        workflow_git(
            &checkout,
            &["switch".as_ref(), "-c".as_ref(), "retargeted".as_ref()],
        )
        .expect("attach worktree to another branch");
        assert!(!workflow_worktree_matches(
            &checkout,
            &common_git_dir,
            &base,
            None,
        ));
        assert!(!workflow_worktree_matches(
            &checkout,
            &common_git_dir,
            &base,
            Some("owned-branch"),
        ));
        let mut workspace = WorkflowWorkspaceOwnership {
            parent_thread_id: ThreadId::new(),
            authority_ref: "authority".into(),
            authority_digest: "digest".into(),
            run_id: "run".into(),
            worker_id: "worker".into(),
            cwd: AbsolutePathBuf::from_absolute_path(&checkout).expect("checkout path"),
            base_commit: base,
            permission_profile: PermissionProfile::Disabled,
            released: false,
            common_git_dir,
            repository_cwd: AbsolutePathBuf::from_absolute_path(temp.path()).expect("repo cwd"),
            branch: "retargeted".into(),
            removed: false,
            checkout_grant: None,
        };
        persist_workflow_workspace("workspace-id", &workspace).expect("persist ownership");
        let restored = load_workflow_workspace(temp.path(), "workspace-id")
            .expect("restore ownership after registry loss");
        assert_eq!(restored.parent_thread_id, workspace.parent_thread_id);
        assert_eq!(restored.authority_digest, workspace.authority_digest);
        assert_eq!(restored.cwd, workspace.cwd);
        assert_eq!(restored.base_commit, workspace.base_commit);
        workflow_git(
            temp.path(),
            &[
                "worktree".as_ref(),
                "lock".as_ref(),
                "--reason".as_ref(),
                workflow_lock_reason("workspace-id").as_ref(),
                checkout.as_os_str(),
            ],
        )
        .expect("owned lock");
        assert!(workflow_owned_lock(&checkout, "workspace-id"));
        assert!(workflow_workspace_unchanged(&workspace));
        workflow_git(
            temp.path(),
            &["worktree".as_ref(), "unlock".as_ref(), checkout.as_os_str()],
        )
        .expect("unlock");
        workflow_git(
            temp.path(),
            &[
                "worktree".as_ref(),
                "lock".as_ref(),
                "--reason".as_ref(),
                "foreign".as_ref(),
                checkout.as_os_str(),
            ],
        )
        .expect("foreign lock");
        assert!(!workflow_owned_lock(&checkout, "workspace-id"));
        workflow_git(
            temp.path(),
            &["worktree".as_ref(), "unlock".as_ref(), checkout.as_os_str()],
        )
        .expect("unlock foreign");
        workflow_git(
            temp.path(),
            &["worktree".as_ref(), "remove".as_ref(), checkout.as_os_str()],
        )
        .expect("remove clean checkout");
        workflow_git(
            temp.path(),
            &["branch".as_ref(), "-D".as_ref(), "retargeted".as_ref()],
        )
        .expect("remove temporary branch");
        workspace.removed = true;
        materialize_fixture_worktree("workspace-id", &mut workspace)
            .expect("restore cache-miss workspace");
        assert!(checkout.exists());
        assert!(workflow_owned_lock(&checkout, "workspace-id"));
        std::fs::write(checkout.join("edit"), "retained").expect("edit");
        assert!(!workflow_workspace_unchanged(&workspace));
        workflow_git(
            temp.path(),
            &["worktree".as_ref(), "unlock".as_ref(), checkout.as_os_str()],
        )
        .expect("release changed owned lock");
        assert!(!workflow_owned_lock(&checkout, "workspace-id"));
        workflow_git(&checkout, &["add".as_ref(), "edit".as_ref()]).expect("add edit");
        workflow_git(
            &checkout,
            &["commit".as_ref(), "-m".as_ref(), "changed".as_ref()],
        )
        .expect("commit edit");
        assert!(!workflow_workspace_unchanged(&workspace));
        workflow_release_lease(&mut workspace);
        assert!(workspace.released);
        assert_eq!(
            std::fs::read_to_string(checkout.join("edit")).expect("retained edit"),
            "retained"
        );
    }

    #[test]
    fn release_rejects_active_workers() {
        assert!(!workflow_release_eligible(&AgentStatus::PendingInit));
        assert!(!workflow_release_eligible(&AgentStatus::Running));
        assert!(workflow_release_eligible(&AgentStatus::Completed(None)));
        assert!(workflow_release_eligible(&AgentStatus::Interrupted));
    }

    #[test]
    fn project_save_prefers_nearest_existing_ancestor_workflows() {
        let temp = tempfile::tempdir().expect("tempdir");
        workflow_git(temp.path(), &["init".as_ref()]).expect("init");
        let nested = temp.path().join("packages/tool/src");
        std::fs::create_dir_all(&nested).expect("nested cwd");
        let nearest = temp.path().join("packages/tool/.codex/workflows");
        std::fs::create_dir_all(&nearest).expect("nearest workflows");
        assert_eq!(
            workflow_project_save_directory(&nested).expect("save directory"),
            AbsolutePathBuf::from_absolute_path(
                std::fs::canonicalize(nearest).expect("canonical nearest"),
            )
            .expect("absolute nearest")
        );
    }

    #[test]
    fn workflow_workspace_ids_are_canonical_uuids_before_record_lookup() {
        let valid = Uuid::new_v4().to_string();
        assert!(validate_workflow_workspace_id(&valid).is_ok());
        let uppercase = valid.to_uppercase();
        for invalid in ["../owner", "/tmp/owner", "workspace-id", &uppercase] {
            assert!(
                validate_workflow_workspace_id(invalid).is_err(),
                "{invalid}"
            );
        }
    }
}

#[derive(Default)]
struct ThreadEnvironmentOverride {
    environments: Option<TurnEnvironmentSelections>,
    // Only default-environment updates replace the task's separately persisted root selection.
    runtime_workspace_roots: Option<Vec<AbsolutePathBuf>>,
}

struct ThreadSettingsBuildParams {
    method: &'static str,
    disabled_plugin_ids: Option<Vec<String>>,
    environment_override: ThreadEnvironmentOverride,
    approval_policy: Option<codex_app_server_protocol::AskForApproval>,
    approvals_reviewer: Option<codex_app_server_protocol::ApprovalsReviewer>,
    sandbox_policy: Option<codex_app_server_protocol::SandboxPolicy>,
    permissions: Option<String>,
    model: Option<String>,
    service_tier: Option<Option<String>>,
    effort: Option<ReasoningEffort>,
    summary: Option<ReasoningSummary>,
    collaboration_mode: Option<CollaborationMode>,
    personality: Option<Personality>,
}

impl TurnRequestProcessor {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        auth_manager: Arc<AuthManager>,
        thread_manager: Arc<ThreadManager>,
        outgoing: Arc<OutgoingMessageSender>,
        analytics_events_client: AnalyticsEventsClient,
        arg0_paths: Arg0DispatchPaths,
        config: Arc<Config>,
        config_manager: ConfigManager,
        pending_thread_unloads: Arc<Mutex<HashSet<ThreadId>>>,
        thread_state_manager: ThreadStateManager,
        thread_watch_manager: ThreadWatchManager,
        skills_watcher: Arc<SkillsWatcher>,
        turn_cost_worker: Option<crate::turn_cost_worker::TurnCostWorkerHandle>,
    ) -> Self {
        let agent_runner = AgentRunner::new(Arc::downgrade(&thread_manager));
        Self {
            agent_runner,
            auth_manager,
            thread_manager,
            outgoing,
            analytics_events_client,
            arg0_paths,
            config,
            config_manager,
            pending_thread_unloads,
            thread_state_manager,
            thread_watch_manager,
            skills_watcher,
            turn_cost_worker,
            workflow_authorities: Arc::new(Mutex::new(HashMap::new())),
            workflow_workers: Arc::new(Mutex::new(HashMap::new())),
            workflow_workspaces: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn workflow_authority_capture(
        &self,
        params: WorkflowAuthorityCaptureParams,
    ) -> Result<WorkflowAuthorityCaptureResponse, JSONRPCErrorError> {
        let (parent_thread_id, parent) = self.load_thread(&params.parent_thread_id).await?;
        let snapshot = parent.config_snapshot().await;
        let codex_home = parent.effective_config().await.codex_home.clone();
        let authority_ref = Uuid::new_v4().to_string();
        let generation = 1;
        let plugins = parent
            .workflow_plugin_roots()
            .await
            .into_iter()
            .filter(|(name, _)| {
                !name.is_empty()
                    && name.len() <= 64
                    && name.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || matches!(byte, b'-' | b'_')
                    })
            })
            .map(|(name, root)| WorkflowPluginOption {
                name,
                root: root.to_string_lossy().into_owned(),
                workflows: vec!["workflows".to_string()],
            })
            .collect::<Vec<_>>();
        let effective_config = parent.effective_config().await;
        let web_search_available = effective_config.web_search_mode.value()
            != codex_protocol::config_types::WebSearchMode::Disabled;
        let authority_digest = workflow_authority_digest(&snapshot, &plugins, web_search_available);
        let models = self
            .thread_manager
            .get_models_manager()
            .list_models(
                codex_models_manager::manager::RefreshStrategy::Offline,
                self.config.http_client_factory(),
            )
            .await
            .into_iter()
            .map(|preset| WorkflowModelOption {
                model: preset.model,
                default_effort: preset.default_reasoning_effort,
                supported_efforts: preset
                    .supported_reasoning_efforts
                    .into_iter()
                    .map(|effort| effort.effort)
                    .collect(),
            })
            .collect();
        self.workflow_authorities.lock().await.insert(
            authority_ref.clone(),
            WorkflowAuthority {
                parent_thread_id,
                generation,
                snapshot: snapshot.clone(),
                digest: authority_digest.clone(),
                allow_isolated_workspaces: params.allow_isolated_workspaces,
                codex_home,
            },
        );
        Ok(WorkflowAuthorityCaptureResponse {
            authority_ref,
            generation,
            authority_digest,
            cwd: snapshot.cwd().to_string_lossy().into_owned(),
            parent_model: snapshot.model,
            parent_effort: snapshot.reasoning_effort,
            models,
            plugins,
            web_search_available,
            workflow_host_url: effective_config
                .mcp_servers
                .get()
                .get("codex_tui")
                .filter(|server| server.enabled)
                .and_then(|server| match &server.transport {
                    codex_config::McpServerTransportConfig::StreamableHttp { url, .. } => {
                        Some(url.clone())
                    }
                    _ => None,
                }),
        })
    }

    pub(crate) async fn workflow_completion_inject(
        &self,
        params: WorkflowCompletionInjectParams,
    ) -> Result<WorkflowCompletionInjectResponse, JSONRPCErrorError> {
        if !workflow_id_component(&params.run_id) {
            return Err(invalid_request("invalid workflow completion run ID"));
        }
        let (_, parent) = self.load_thread(&params.parent_thread_id).await?;
        let submission = parent
            .inject_workflow_completion(params.run_id, params.summary)
            .await
            .map_err(|e| internal_error(format!("failed to deliver workflow completion: {e}")))?;
        workflow_completion_response(submission)
    }

    async fn workflow_file_authority(
        &self,
        parent_thread_id: &str,
        authority_ref: &str,
        authority_digest: &str,
    ) -> Result<(WorkflowAuthority, Arc<CodexThread>), JSONRPCErrorError> {
        let (parent_id, parent) = self.load_thread(parent_thread_id).await?;
        let authority = self
            .workflow_authorities
            .lock()
            .await
            .get(authority_ref)
            .cloned()
            .ok_or_else(|| invalid_request("unknown workflow authority"))?;
        if authority.parent_thread_id != parent_id || authority.digest != authority_digest {
            return Err(invalid_request("workflow authority does not match parent"));
        }
        let current = parent.config_snapshot().await;
        if !workflow_authority_matches(&authority.snapshot, &current) {
            return Err(invalid_request("workflow authority is stale"));
        }
        Ok((authority, parent))
    }

    pub(crate) async fn workflow_script_read(
        &self,
        params: WorkflowScriptReadParams,
    ) -> Result<WorkflowScriptReadResponse, JSONRPCErrorError> {
        let (authority, parent) = self
            .workflow_file_authority(
                &params.parent_thread_id,
                &params.authority_ref,
                &params.authority_digest,
            )
            .await?;
        let requested = std::path::Path::new(&params.script_path);
        let absolute = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            authority.snapshot.cwd().join(requested).to_path_buf()
        };
        let absolute = AbsolutePathBuf::from_absolute_path(absolute)
            .map_err(|err| invalid_request(format!("invalid workflow script path: {err}")))?;
        let (fs, sandbox, _) = parent
            .workflow_file_access()
            .await
            .ok_or_else(|| invalid_request("parent workflow filesystem is unavailable"))?;
        let path = codex_utils_path_uri::PathUri::from_abs_path(&absolute);
        let canonical = fs
            .canonicalize(&path, Some(&sandbox))
            .await
            .map_err(|err| invalid_request(format!("workflow script is not readable: {err}")))?;
        let source = fs
            .read_file_text(&canonical, Default::default(), Some(&sandbox))
            .await
            .map_err(|err| invalid_request(format!("workflow script is not readable: {err}")))?;
        Ok(WorkflowScriptReadResponse {
            resolved_path: canonical
                .to_abs_path()
                .map_err(|err| invalid_request(err.to_string()))?
                .to_string_lossy()
                .into_owned(),
            source_digest: format!("{:x}", Sha256::digest(source.as_bytes())),
            source,
        })
    }

    pub(crate) async fn workflow_save(
        &self,
        params: WorkflowSaveParams,
    ) -> Result<WorkflowSaveResponse, JSONRPCErrorError> {
        let valid_name = !params.name.is_empty()
            && params.name.len() <= 64
            && !params.name.starts_with('-')
            && params
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !valid_name || !workflow_id_component(&params.run_id) {
            return Err(invalid_request("invalid workflow save identity"));
        }
        if format!("{:x}", Sha256::digest(params.source.as_bytes())) != params.source_digest {
            return Err(invalid_request("workflow save source digest mismatch"));
        }
        let (authority, parent) = self
            .workflow_file_authority(
                &params.parent_thread_id,
                &params.authority_ref,
                &params.authority_digest,
            )
            .await?;
        let (fs, mut sandbox, cwd) = parent
            .workflow_file_access()
            .await
            .ok_or_else(|| invalid_request("parent workflow filesystem is unavailable"))?;
        let directory = match params.scope.as_str() {
            "user" => {
                let root = codex_utils_path_uri::PathUri::from_abs_path(&authority.codex_home);
                match fs.canonicalize(&root, Some(&sandbox)).await {
                    Ok(root) => root
                        .to_abs_path()
                        .map_err(|err| invalid_request(err.to_string()))?
                        .join("workflows"),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        authority.codex_home.join("workflows")
                    }
                    Err(error) => {
                        return Err(invalid_request(format!(
                            "personal workflow root is not accessible: {error}"
                        )));
                    }
                }
            }
            "project" => workflow_project_save_directory(authority.snapshot.cwd().as_path())
                .map_err(invalid_request)?,
            _ => return Err(invalid_request("invalid workflow save scope")),
        };
        let target = directory.join(format!("{}.js", params.name));
        let config_directory = (params.scope == "project")
            .then(|| directory.parent())
            .flatten();
        for path in [
            config_directory,
            Some(directory.clone()),
            Some(target.clone()),
        ]
        .into_iter()
        .flatten()
        {
            if std::fs::symlink_metadata(path.as_path()).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(invalid_request("unsafe symlink workflow save destination"));
            }
        }
        let permission = authority.snapshot.permission_profile.clone();
        let mut policy = permission.file_system_sandbox_policy();
        if !policy.can_write_local_path_with_cwd(target.as_path(), cwd.as_path()) {
            let approved = match authority.snapshot.approvals_reviewer {
                codex_protocol::config_types::ApprovalsReviewer::AutoReview => {
                    parent
                        .review_workflow_save(
                            target.clone(),
                            format!("create {}", target.display()),
                        )
                        .await
                }
                codex_protocol::config_types::ApprovalsReviewer::User => true,
            };
            if !approved {
                return Err(invalid_request("workflow save was not approved"));
            }
            let additional = codex_protocol::models::AdditionalPermissionProfile {
                file_system: Some(
                    codex_protocol::models::FileSystemPermissions::from_read_write_roots(
                        Some(vec![]),
                        Some(vec![directory.clone()]),
                    ),
                ),
                ..Default::default()
            };
            policy = codex_sandboxing::policy_transforms::effective_file_system_sandbox_policy(
                &policy,
                Some(&additional),
            );
            sandbox.permissions = PermissionProfile::from_runtime_permissions_with_enforcement(
                permission.enforcement(),
                &policy,
                permission.network_sandbox_policy(),
            )
            .into();
        }
        // Creating `.codex/workflows` may need write access to its protected
        // `.codex` parent. Keep that parent grant scoped to this typed mkdir
        // operation; restore protected metadata denials before writing source.
        let parent_directory = directory.parent();
        let parent_missing = match parent_directory.as_ref() {
            Some(path) => match std::fs::symlink_metadata(path.as_path()) {
                Ok(_) => false,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(error) => {
                    return Err(invalid_request(format!(
                        "workflow save parent is not accessible: {error}"
                    )));
                }
            },
            None => false,
        };
        let mut create_sandbox = sandbox.clone();
        if parent_missing {
            let parent_directory = parent_directory
                .as_ref()
                .expect("missing workflow save parent");
            let create_policy =
                workflow_save_directory_creation_policy(policy.clone(), parent_directory);
            create_sandbox.permissions =
                PermissionProfile::from_runtime_permissions_with_enforcement(
                    permission.enforcement(),
                    &create_policy,
                    permission.network_sandbox_policy(),
                )
                .into();
        }
        let dir_uri = codex_utils_path_uri::PathUri::from_abs_path(&directory);
        match fs
            .create_directory(
                &dir_uri,
                CreateDirectoryOptions {
                    recursive: true,
                    follow_symlinks: false,
                },
                Some(&create_sandbox),
            )
            .await
        {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(invalid_request(format!(
                    "workflow save directory failed: {e}"
                )));
            }
        }
        let target_uri = codex_utils_path_uri::PathUri::from_abs_path(&target);
        fs.write_file(
            &target_uri,
            params.source.into_bytes(),
            WriteFileOptions {
                follow_symlinks: false,
                create_new: true,
            },
            Some(&sandbox),
        )
        .await
        .map_err(|e| invalid_request(format!("workflow save failed: {e}")))?;
        Ok(WorkflowSaveResponse {
            path: target.to_string_lossy().into_owned(),
        })
    }

    pub(crate) async fn workflow_workspace_prepare(
        &self,
        params: WorkflowWorkspacePrepareParams,
    ) -> Result<WorkflowWorkspacePrepareResponse, JSONRPCErrorError> {
        if !workflow_id_component(&params.run_id) || !workflow_id_component(&params.worker_id) {
            return Err(invalid_request("workflow workspace identity is invalid"));
        }
        let authority = self
            .workflow_authorities
            .lock()
            .await
            .get(&params.authority_ref)
            .cloned()
            .ok_or_else(|| invalid_request("workflow authority reference is unknown"))?;
        if authority.digest != params.authority_digest {
            return Err(invalid_request("workflow authority reference is stale"));
        }
        let parent = self
            .thread_manager
            .get_thread(authority.parent_thread_id)
            .await
            .map_err(|_| invalid_request("workflow parent thread is unavailable"))?;
        let current = parent.config_snapshot().await;
        if !workflow_authority_matches(&authority.snapshot, &current)
            || params.requested_cwd != current.cwd().to_string_lossy()
        {
            return Err(invalid_request("workflow parent authority changed"));
        }
        let mut role_config = parent.effective_config().await.as_ref().clone();
        role_config.model = Some(current.model.clone());
        role_config
            .model_reasoning_effort
            .clone_from(&current.reasoning_effort);
        let role_digest = workflow_apply_role(&mut role_config, &params.agent_type)
            .await
            .map_err(invalid_request)?;
        if params.isolation.is_none() {
            return Ok(WorkflowWorkspacePrepareResponse {
                workspace_id: None,
                cwd: params.requested_cwd,
                isolated: false,
                base_commit: None,
                authority_generation: authority.generation,
                role_digest,
            });
        }
        if params.isolation.as_deref() != Some("worktree") {
            return Err(invalid_request(
                "workflow workspace isolation is unsupported",
            ));
        }
        if !authority.allow_isolated_workspaces {
            return Err(invalid_request(
                "workflow workspace isolation was not approved by the parent",
            ));
        }

        if let Some(previous_id) = params
            .previous
            .as_ref()
            .and_then(|previous| previous.workspace_id.as_ref())
        {
            validate_workflow_workspace_id(previous_id)?;
            let mut workspaces = self.workflow_workspaces.lock().await;
            if !workspaces.contains_key(previous_id) {
                let persisted = load_workflow_workspace(current.cwd().as_path(), previous_id)
                    .map_err(|_| {
                        invalid_request("previous workflow workspace is not owned by this host")
                    })?;
                workspaces.insert(previous_id.clone(), persisted);
            }
            let previous = workspaces
                .get_mut(previous_id)
                .expect("inserted or present");
            if previous
                .checkout_grant
                .as_ref()
                .is_none_or(|grant| !grant.matches(previous_id, previous))
                || previous.parent_thread_id != authority.parent_thread_id
                || previous.authority_digest != params.authority_digest
                || previous.run_id != params.run_id
                || previous.worker_id != params.worker_id
                || params.previous.as_ref().is_none_or(|p| {
                    p.cwd != previous.cwd.to_string_lossy()
                        || p.base_commit.as_ref() != Some(&previous.base_commit)
                })
                || (!previous.removed
                    && !workflow_worktree_matches(
                        previous.cwd.as_path(),
                        &previous.common_git_dir,
                        &previous.base_commit,
                        Some(&previous.branch),
                    ))
            {
                return Err(invalid_request(
                    "previous workflow workspace ownership is stale",
                ));
            }
            previous.authority_ref.clone_from(&params.authority_ref);
            previous.released = false;
            persist_workflow_workspace(previous_id, previous).map_err(|e| {
                internal_error(format!("failed to persist workflow ownership: {e}"))
            })?;
            return Ok(WorkflowWorkspacePrepareResponse {
                workspace_id: Some(previous_id.clone()),
                cwd: previous.cwd.to_string_lossy().into_owned(),
                isolated: true,
                base_commit: Some(previous.base_commit.clone()),
                authority_generation: authority.generation,
                role_digest,
            });
        }

        let canonical_cwd = AbsolutePathBuf::from_absolute_path(
            std::fs::canonicalize(current.cwd())
                .map_err(|error| invalid_request(error.to_string()))?,
        )
        .map_err(|error| invalid_request(error.to_string()))?;
        let workspace_root = canonical_cwd.join(".ultracode/worktrees");
        let checkout = workspace_root.join(format!("{}-{}", params.run_id, params.worker_id));
        if checkout.exists() {
            return Err(invalid_request("workflow workspace already exists"));
        }
        let workspace_id = Uuid::new_v4().to_string();
        let (checkout_grant, checkout_permissions) = self
            .workflow_checkout_grant(&parent, &authority, &params, &workspace_id, &checkout)
            .await?;
        let common_git_dir = workflow_common_git_dir(current.cwd().as_path()).map_err(|err| {
            internal_error(format!(
                "failed to resolve workflow common Git directory: {err}"
            ))
        })?;
        let git_scope = workflow_workspace_permissions::write_scope(vec![
            AbsolutePathBuf::from_absolute_path(&common_git_dir)
                .map_err(|error| invalid_request(error.to_string()))?,
        ]);
        let base_commit = codex_git_utils::workflow_worktree_base_with(
            current.cwd().as_path(),
            role_config.worktree_base_ref,
            authority
                .snapshot
                .permission_profile
                .network_sandbox_policy(),
            |args, timeout| {
                let mut command = vec!["git".to_string()];
                command.extend(args);
                let additional = if matches!(command.get(1).map(String::as_str), Some("fetch"))
                    || command.get(2).map(String::as_str) == Some("set-head")
                {
                    git_scope.clone()
                } else {
                    Default::default()
                };
                let parent = &parent;
                let workspace_id = &workspace_id;
                async move {
                    if timeout.is_zero() {
                        return Ok(None);
                    }
                    match self
                        .workflow_checkout_command(
                            parent,
                            authority.parent_thread_id,
                            workspace_id,
                            command,
                            additional,
                            timeout,
                        )
                        .await
                    {
                        Ok(output) => Ok(Some(output)),
                        Err(error) if error.code == -32010 => Ok(None),
                        Err(error) => Err(error),
                    }
                }
            },
        )
        .await?
        .ok_or_else(|| {
            internal_error("failed to resolve workflow base commit under native permission policy")
        })?;
        let branch = format!(
            "ultracode/{}-{}-{}",
            params.run_id,
            params.worker_id,
            &workspace_id[..8]
        );
        let operation_scope =
            workflow_workspace_permissions::checkout_command_scope(&checkout, &common_git_dir)?;
        let mut ownership = WorkflowWorkspaceOwnership {
            parent_thread_id: authority.parent_thread_id,
            authority_ref: params.authority_ref.clone(),
            authority_digest: params.authority_digest.clone(),
            run_id: params.run_id.clone(),
            worker_id: params.worker_id.clone(),
            cwd: checkout.clone(),
            base_commit: base_commit.clone(),
            // This provisional record is used only to clean up a failed creation.
            // The exact checkout profile is resolved after Git materializes it.
            permission_profile: current.permission_profile.clone(),
            released: false,
            common_git_dir: common_git_dir.clone(),
            repository_cwd: current.cwd().clone(),
            branch: branch.clone(),
            removed: false,
            checkout_grant: Some(checkout_grant),
        };
        let create_result = self
            .workflow_checkout_command(
                &parent,
                authority.parent_thread_id,
                &workspace_id,
                vec![
                    "git".into(),
                    "worktree".into(),
                    "add".into(),
                    "-b".into(),
                    branch.clone(),
                    checkout.to_string_lossy().into_owned(),
                    base_commit.clone(),
                ],
                operation_scope.clone(),
                std::time::Duration::from_secs(30),
            )
            .await;
        if let Err(mut error) = create_result {
            if matches!(error.code, -32010 | -32011)
                && let Err(cleanup) = self
                    .cleanup_workflow_checkout(
                        &parent,
                        &workspace_id,
                        &ownership,
                        operation_scope.clone(),
                    )
                    .await
            {
                error.message.push_str(&format!(
                    "; cleanup unresolved at {}: {}",
                    checkout.display(),
                    cleanup.message
                ));
            }
            return Err(error);
        }
        if !workflow_worktree_matches(
            checkout.as_path(),
            &common_git_dir,
            &base_commit,
            Some(&branch),
        ) {
            let mut error = internal_error("created workflow worktree failed identity validation");
            if let Err(cleanup) = self
                .cleanup_workflow_checkout(&parent, &workspace_id, &ownership, operation_scope)
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
                &parent,
                authority.parent_thread_id,
                &workspace_id,
                vec![
                    "git".into(),
                    "worktree".into(),
                    "lock".into(),
                    "--reason".into(),
                    workflow_lock_reason(&workspace_id),
                    checkout.to_string_lossy().into_owned(),
                ],
                operation_scope.clone(),
                std::time::Duration::from_secs(30),
            )
            .await
        {
            if let Err(cleanup) = self
                .cleanup_workflow_checkout(&parent, &workspace_id, &ownership, operation_scope)
                .await
            {
                error.message.push_str(&format!(
                    "; cleanup unresolved at {}: {}",
                    checkout.display(),
                    cleanup.message
                ));
            }
            return Err(error);
        }
        match self
            .materialized_checkout_profile(&parent, &authority, &checkout, checkout_permissions)
            .await
        {
            Ok(profile) => ownership.permission_profile = profile,
            Err(mut error) => {
                if let Err(cleanup) = self
                    .cleanup_workflow_checkout(
                        &parent,
                        &workspace_id,
                        &ownership,
                        operation_scope.clone(),
                    )
                    .await
                {
                    error.message.push_str(&format!(
                        "; cleanup unresolved at {}: {}",
                        checkout.display(),
                        cleanup.message
                    ));
                }
                return Err(error);
            }
        }
        if let Err(error) = persist_workflow_workspace(&workspace_id, &ownership) {
            let mut error =
                internal_error(format!("failed to persist workflow ownership: {error}"));
            if let Err(cleanup) = self
                .cleanup_workflow_checkout(&parent, &workspace_id, &ownership, operation_scope)
                .await
            {
                error.message.push_str(&format!(
                    "; cleanup unresolved at {}: {}",
                    checkout.display(),
                    cleanup.message
                ));
            }
            return Err(error);
        }
        self.workflow_workspaces
            .lock()
            .await
            .insert(workspace_id.clone(), ownership);
        Ok(WorkflowWorkspacePrepareResponse {
            workspace_id: Some(workspace_id),
            cwd: checkout.to_string_lossy().into_owned(),
            isolated: true,
            base_commit: Some(base_commit),
            authority_generation: authority.generation,
            role_digest,
        })
    }

    pub(crate) async fn workflow_workspace_release(
        &self,
        params: WorkflowWorkspaceReleaseParams,
    ) -> Result<WorkflowWorkspaceReleaseResponse, JSONRPCErrorError> {
        let Some(workspace_id) = params.workspace_id.as_ref() else {
            return Ok(WorkflowWorkspaceReleaseResponse { eligible: true });
        };
        validate_workflow_workspace_id(workspace_id)?;
        let workspace = self
            .workflow_workspaces
            .lock()
            .await
            .get(workspace_id)
            .cloned()
            .ok_or_else(|| invalid_request("workflow workspace is not owned by this host"))?;
        if workspace
            .checkout_grant
            .as_ref()
            .is_none_or(|grant| !grant.matches(workspace_id, &workspace))
            || workspace.run_id != params.run_id
            || workspace.worker_id != params.worker_id
        {
            return Err(invalid_request("workflow workspace ownership is stale"));
        }
        let worker_thread_id =
            self.workflow_workers
                .lock()
                .await
                .iter()
                .find_map(|(thread_id, worker)| {
                    (worker.parent_thread_id == workspace.parent_thread_id
                        && worker.run_id == params.run_id
                        && worker.worker_id == params.worker_id
                        && worker.workspace_id.as_ref() == Some(workspace_id))
                    .then_some(*thread_id)
                });
        if let Some(thread_id) = worker_thread_id {
            let thread = self
                .thread_manager
                .get_thread(thread_id)
                .await
                .map_err(|_| invalid_request("workflow worker state is unavailable"))?;
            if !workflow_release_eligible(&thread.agent_status().await) {
                return Ok(WorkflowWorkspaceReleaseResponse { eligible: false });
            }
        } else {
            return Ok(WorkflowWorkspaceReleaseResponse { eligible: false });
        }
        let parent = self
            .thread_manager
            .get_thread(workspace.parent_thread_id)
            .await
            .map_err(|_| invalid_request("workflow parent thread is unavailable"))?;
        let authority = self
            .workflow_authorities
            .lock()
            .await
            .get(&workspace.authority_ref)
            .cloned()
            .ok_or_else(|| invalid_request("workflow workspace authority is unavailable"))?;
        if authority.digest != workspace.authority_digest
            || !workflow_authority_matches(&authority.snapshot, &parent.config_snapshot().await)
        {
            return Err(invalid_request(
                "workflow parent authority changed before checkout release",
            ));
        }
        let scope = workflow_workspace_permissions::checkout_command_scope(
            &workspace.cwd,
            &workspace.common_git_dir,
        )?;
        if let Some(workspace) = self.workflow_workspaces.lock().await.get_mut(workspace_id) {
            let identity_matches = workflow_worktree_matches(
                workspace.cwd.as_path(),
                &workspace.common_git_dir,
                &workspace.base_commit,
                Some(&workspace.branch),
            );
            let owned_lock =
                identity_matches && workflow_owned_lock(workspace.cwd.as_path(), workspace_id);
            if owned_lock {
                self.workflow_checkout_command(
                    &parent,
                    workspace.parent_thread_id,
                    workspace_id,
                    vec![
                        "git".into(),
                        "worktree".into(),
                        "unlock".into(),
                        workspace.cwd.to_string_lossy().into_owned(),
                    ],
                    scope.clone(),
                    std::time::Duration::from_secs(30),
                )
                .await?;
            }
            if owned_lock && workflow_workspace_unchanged(workspace) {
                self.workflow_checkout_command(
                    &parent,
                    workspace.parent_thread_id,
                    workspace_id,
                    vec![
                        "git".into(),
                        "worktree".into(),
                        "remove".into(),
                        workspace.cwd.to_string_lossy().into_owned(),
                    ],
                    scope.clone(),
                    std::time::Duration::from_secs(30),
                )
                .await?;
                workspace.removed = true;
                persist_workflow_workspace(workspace_id, workspace).map_err(internal_error)?;
                self.workflow_checkout_command(
                    &parent,
                    workspace.parent_thread_id,
                    workspace_id,
                    vec![
                        "git".into(),
                        "update-ref".into(),
                        "-d".into(),
                        format!("refs/heads/{}", workspace.branch),
                        workspace.base_commit.clone(),
                    ],
                    scope,
                    std::time::Duration::from_secs(30),
                )
                .await?;
            }
            workflow_release_lease(workspace);
            persist_workflow_workspace(workspace_id, workspace).map_err(internal_error)?;
        }
        Ok(WorkflowWorkspaceReleaseResponse { eligible: true })
    }

    pub(crate) async fn workflow_worker_start(
        &self,
        request_id: &ConnectionRequestId,
        params: WorkflowWorkerStartParams,
    ) -> Result<WorkflowWorkerStartResponse, JSONRPCErrorError> {
        if params.prompt.trim().is_empty() {
            return Err(invalid_request("workflow worker prompt must not be empty"));
        }
        if params
            .schema
            .as_ref()
            .is_some_and(|schema| !schema.is_object())
        {
            return Err(invalid_request("workflow worker schema must be an object"));
        }
        let role_instructions = match params.agent_type.as_str() {
            "general-purpose" => "",
            "Explore" => {
                "Role: Explore. Inspect and report only. Do not modify files or external state."
            }
            "Plan" => {
                "Role: Plan. Produce an implementation plan only. Do not modify files or external state."
            }
            _ => "",
        };
        let (parent_thread_id, parent) = self.load_thread(&params.parent_thread_id).await?;
        let authority = self
            .workflow_authorities
            .lock()
            .await
            .get(&params.authority_ref)
            .cloned()
            .ok_or_else(|| invalid_request("workflow authority reference is unknown"))?;
        if !workflow_authority_ref_matches(
            authority.parent_thread_id,
            authority.generation,
            parent_thread_id,
            params.authority_generation,
        ) || authority.digest != params.authority_digest
        {
            return Err(invalid_request("workflow authority reference is stale"));
        }
        let current = parent.config_snapshot().await;
        if !workflow_authority_matches(&authority.snapshot, &current) {
            return Err(invalid_request("workflow parent authority changed"));
        }
        let isolated_workspace = if let Some(workspace_id) = params.workspace.workspace_id.as_ref()
        {
            validate_workflow_workspace_id(workspace_id)?;
            let mut workspaces = self.workflow_workspaces.lock().await;
            let workspace = workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| invalid_request("workflow workspace is not owned by this host"))?;
            if workspace
                .checkout_grant
                .as_ref()
                .is_none_or(|grant| !grant.matches(workspace_id, workspace))
                || workspace.parent_thread_id != parent_thread_id
                || workspace.authority_ref != params.authority_ref
                || workspace.authority_digest != params.authority_digest
                || workspace.run_id != params.run_id
                || workspace.worker_id != params.worker_id
                || params.workspace.cwd != workspace.cwd.to_string_lossy()
                || params.workspace.base_commit.as_ref() != Some(&workspace.base_commit)
                || !params.workspace.isolated
                || workspace.released
                || (!workspace.removed
                    && !workflow_worktree_matches(
                        workspace.cwd.as_path(),
                        &workspace.common_git_dir,
                        &workspace.base_commit,
                        Some(&workspace.branch),
                    ))
            {
                return Err(invalid_request("workflow workspace ownership is stale"));
            }
            if workspace.removed {
                self.restore_workflow_checkout(&parent, workspace_id, workspace)
                    .await?;
                persist_workflow_workspace(workspace_id, workspace).map_err(|e| {
                    internal_error(format!("failed to persist restored workflow worktree: {e}"))
                })?;
            }
            Some(workspace.clone())
        } else {
            if params.workspace.isolated
                || params.workspace.cwd != current.cwd().to_string_lossy()
                || params.workspace.base_commit.is_some()
            {
                return Err(invalid_request("workflow shared workspace is invalid"));
            }
            None
        };

        let force_read_only =
            params.read_only || matches!(params.agent_type.as_str(), "Explore" | "Plan");
        let schema_digest = workflow_schema_digest(params.schema.as_ref());
        let workspace_permission_profile = isolated_workspace.as_ref().map_or_else(
            || current.permission_profile.clone(),
            |workspace| workspace.permission_profile.clone(),
        );
        let permission_profile = if force_read_only {
            workspace_permission_profile
                .intersect_with_read_only()
                .ok_or_else(|| {
                    invalid_request("external sandbox authority cannot be reduced safely")
                })?
        } else {
            workspace_permission_profile
        };
        let permission_snapshot = match current.active_permission_profile.clone() {
            Some(active)
                if isolated_workspace.is_none()
                    && permission_profile == current.permission_profile =>
            {
                PermissionProfileSnapshot::active_with_profile_workspace_roots(
                    permission_profile,
                    active,
                    current.profile_workspace_roots.clone(),
                )
            }
            _ => PermissionProfileSnapshot::legacy(permission_profile),
        };

        let mut config = parent.effective_config().await.as_ref().clone();
        // OpenAI-authenticated proxies can have custom provider IDs. Preserve
        // the parent's endpoint and authentication; the catalog validates models.
        if current.model_provider_id != "openai" && !config.model_provider.requires_openai_auth {
            return Err(invalid_request(
                "workflow workers require an authenticated OpenAI Codex model provider",
            ));
        }
        config.service_tier.clone_from(&current.service_tier);
        config.model = Some(current.model.clone());
        config
            .model_reasoning_effort
            .clone_from(&current.reasoning_effort);
        let role_digest = workflow_apply_role(&mut config, &params.agent_type)
            .await
            .map_err(invalid_request)?;
        if role_digest != params.role_digest {
            return Err(invalid_request(
                "workflow worker role changed after preparation",
            ));
        }
        config.model = workflow_effective_selection(
            Some(current.model.clone()),
            config.model.take(),
            params.model.clone(),
            params.model_explicit,
        );
        config.model_reasoning_effort = workflow_effective_selection(
            current.reasoning_effort.clone(),
            config.model_reasoning_effort.take(),
            params.effort.clone(),
            params.effort_explicit,
        );
        let effective_model = config
            .model
            .clone()
            .ok_or_else(|| invalid_request("workflow worker model is unavailable"))?;
        let available_models = self
            .thread_manager
            .get_models_manager()
            .list_models(
                codex_models_manager::manager::RefreshStrategy::Offline,
                self.config.http_client_factory(),
            )
            .await;
        let model = available_models
            .iter()
            .find(|preset| preset.model == effective_model)
            .ok_or_else(|| invalid_request("workflow worker model is unavailable"))?;
        let effective_effort = config
            .model_reasoning_effort
            .clone()
            .unwrap_or_else(|| model.default_reasoning_effort.clone());
        if !model
            .supported_reasoning_efforts
            .iter()
            .any(|preset| preset.effort == effective_effort)
        {
            return Err(invalid_request(
                "workflow worker effort is unsupported by the selected model",
            ));
        }
        config.model = Some(effective_model.clone());
        config.model_reasoning_effort = Some(effective_effort.clone());
        config.cwd = isolated_workspace
            .as_ref()
            .map_or_else(|| current.cwd().clone(), |workspace| workspace.cwd.clone());
        config
            .permissions
            .set_workspace_roots(if isolated_workspace.is_some() {
                vec![config.cwd.clone()]
            } else {
                current.workspace_roots.clone()
            });
        config
            .permissions
            .replace_permission_profile_from_session_snapshot(permission_snapshot)
            .map_err(|err| {
                invalid_request(format!("workflow permission profile is invalid: {err}"))
            })?;
        config
            .permissions
            .approval_policy
            .set(current.approval_policy)
            .map_err(|err| {
                invalid_request(format!("workflow approval policy is invalid: {err}"))
            })?;
        config.approvals_reviewer = current.approvals_reviewer;

        if config.model_provider_id != current.model_provider_id {
            config.model_provider = config
                .model_providers
                .get(&current.model_provider_id)
                .cloned()
                .ok_or_else(|| invalid_request("workflow parent model provider is unavailable"))?;
            config
                .model_provider_id
                .clone_from(&current.model_provider_id);
        }

        let prompt = [
            "Do not launch sub-agents or delegate work. Complete only this workflow worker turn.",
            role_instructions,
            params.prompt.as_str(),
        ]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
        let trace = self.request_trace_context(request_id).await;
        let AgentRun {
            thread_id,
            turn_id,
            thread,
        } = if let Some(resume_thread_id) = params.resume_thread_id.as_ref() {
            let (thread_id, thread) = self.load_thread(resume_thread_id).await?;
            let child_snapshot = thread.config_snapshot().await;
            let mut workers = self.workflow_workers.lock().await;
            let ownership = workers.get_mut(&thread_id).ok_or_else(|| {
                invalid_request("workflow worker thread is not owned by this host")
            })?;
            if ownership.parent_thread_id != parent_thread_id
                || ownership.authority_digest != params.authority_digest
                || ownership.run_id != params.run_id
                || ownership.worker_id != params.worker_id
                || ownership.launch_intent
                || ownership
                    .admitted_authority
                    .as_ref()
                    .is_none_or(|admitted| !workflow_authority_matches(admitted, &child_snapshot))
                || ownership.read_only != force_read_only
                || ownership.agent_type != params.agent_type
                || ownership.schema_digest != schema_digest
                || ownership.workspace_id != params.workspace.workspace_id
                || ownership.cwd.to_string_lossy() != params.workspace.cwd
                || ownership.role_digest != role_digest
                || child_snapshot.forked_from_thread_id != Some(parent_thread_id)
                || child_snapshot.model != effective_model
                || child_snapshot.reasoning_effort.as_ref() != Some(&effective_effort)
            {
                return Err(invalid_request("workflow worker resume lineage is invalid"));
            }
            ownership.launch_intent = true;
            drop(workers);
            let result = self
                .agent_runner
                .resume(thread, prompt, params.schema.clone(), trace)
                .await;
            if let Some(worker) = self.workflow_workers.lock().await.get_mut(&thread_id) {
                worker.launch_intent = false;
            }
            result
        } else {
            let reserved_thread_id = ThreadId::new();
            let mut workers = self.workflow_workers.lock().await;
            if let Some((previous_id, previous)) = workers.iter().find(|(_, worker)| {
                worker.run_id == params.run_id && worker.worker_id == params.worker_id
            }) {
                let previous_id = *previous_id;
                if previous.parent_thread_id != parent_thread_id
                    || previous.authority_digest != params.authority_digest
                    || previous.launch_intent
                    || previous.workspace_id != params.workspace.workspace_id
                    || previous.cwd != config.cwd
                    || previous.role_digest != role_digest
                    || previous.agent_type != params.agent_type
                    || previous.read_only != force_read_only
                    || previous.schema_digest != schema_digest
                {
                    return Err(invalid_request(
                        "workflow worker replacement ownership is invalid",
                    ));
                }
                let previous_thread = self
                    .thread_manager
                    .get_thread(previous_id)
                    .await
                    .map_err(|_| invalid_request("workflow worker state unavailable"))?;
                let previous_config = previous_thread.config_snapshot().await;
                if previous_config.forked_from_thread_id != Some(parent_thread_id)
                    || previous_config.model != effective_model
                    || previous_config.reasoning_effort.as_ref() != Some(&effective_effort)
                {
                    return Err(invalid_request(
                        "workflow worker replacement lineage is invalid",
                    ));
                }
                if !workflow_release_eligible(&previous_thread.agent_status().await) {
                    return Err(invalid_request("workflow worker launch is already owned"));
                }
                // Replace a resolved terminal attempt atomically with its fresh reservation.
                // Retain uncertain launches and active workers so retries cannot duplicate work.
                workers.remove(&previous_id);
            }
            workers.insert(
                reserved_thread_id,
                WorkflowWorkerOwnership {
                    parent_thread_id,
                    authority_digest: params.authority_digest.clone(),
                    run_id: params.run_id.clone(),
                    worker_id: params.worker_id.clone(),
                    launch_intent: true,
                    admitted_authority: None,
                    read_only: force_read_only,
                    agent_type: params.agent_type.clone(),
                    schema_digest,
                    workspace_id: params.workspace.workspace_id.clone(),
                    cwd: config.cwd.clone(),
                    role_digest,
                },
            );
            drop(workers);
            let result = self
                .agent_runner
                .start(
                    parent_thread_id,
                    AgentInvocation {
                        config,
                        prompt,
                        parent_trace: trace,
                        output_schema: params.schema.clone(),
                        reserved_thread_id: Some(reserved_thread_id),
                        thread_source: Some(ThreadSource::Feature("ultracode-worker".to_string())),
                        start_gate: Some({
                            let (attached_tx, attached_rx) = tokio::sync::oneshot::channel();
                            let listener_task_context = self.listener_task_context();
                            let workflow_workers = Arc::clone(&self.workflow_workers);
                            let connection_id = request_id.connection_id;
                            tokio::spawn(async move {
                                for _ in 0..5_000 {
                                    if let Ok(thread) = listener_task_context
                                        .thread_manager
                                        .get_thread(reserved_thread_id)
                                        .await
                                    {
                                        if matches!(
                                            super::thread_lifecycle::ensure_conversation_listener(
                                                listener_task_context,
                                                reserved_thread_id,
                                                connection_id,
                                                false,
                                            )
                                            .await,
                                            Ok(EnsureConversationListenerResult::Attached)
                                        ) {
                                            let admitted_authority = thread.config_snapshot().await;
                                            if let Some(worker) = workflow_workers
                                                .lock()
                                                .await
                                                .get_mut(&reserved_thread_id)
                                            {
                                                worker.admitted_authority =
                                                    Some(admitted_authority);
                                            }
                                            let _ = attached_tx.send(());
                                        }
                                        return;
                                    }
                                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                                }
                            });
                            attached_rx
                        }),
                    },
                )
                .await;
            match result {
                Ok(run) => {
                    if let Some(worker) = self
                        .workflow_workers
                        .lock()
                        .await
                        .get_mut(&reserved_thread_id)
                    {
                        worker.launch_intent = false;
                    }
                    Ok(run)
                }
                Err(err) => {
                    // The reserved child may already exist if initial turn submission failed.
                    // Retain launch intent so retries cannot duplicate an uncertain launch.
                    Err(err)
                }
            }
        }
        .map_err(|err| internal_error(format!("failed to start workflow worker: {err}")))?;
        let session_id = thread.session_configured().session_id.to_string();
        Ok(WorkflowWorkerStartResponse {
            thread_id: thread_id.to_string(),
            session_id,
            turn_id,
            model: effective_model,
            effort: effective_effort,
        })
    }

    pub(crate) async fn turn_start(
        &self,
        request_id: ConnectionRequestId,
        params: TurnStartParams,
        app_server_client_name: Option<String>,
        app_server_client_version: Option<String>,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        validate_user_input_image_urls(&params.input)?;
        self.turn_start_inner(
            request_id,
            params,
            app_server_client_name,
            app_server_client_version,
        )
        .await
        .map(|response| Some(response.into()))
    }

    pub(crate) async fn thread_inject_items(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadInjectItemsParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_inject_items_response_inner(request_id, params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn thread_settings_update(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadSettingsUpdateParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_settings_update_inner(request_id, params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn turn_settings_update(
        &self,
        request_id: &ConnectionRequestId,
        params: TurnSettingsUpdateParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let (_, thread) = self.load_thread(&params.thread_id).await?;
        self.ensure_direct_input_allowed(request_id, thread.as_ref())
            .await?;
        let (reply, outcome) = oneshot::channel();
        self.submit_core_op(
            request_id,
            &thread,
            Op::TurnSettings {
                turn_id: params.turn_id,
                update: TurnSettingsUpdate {
                    approvals_reviewer: params
                        .approvals_reviewer
                        .map(codex_app_server_protocol::ApprovalsReviewer::to_core),
                    model: params.model,
                    // Match thread/settings/update: public null does not clear effort.
                    effort: params.effort.map(Some),
                    summary: params.summary,
                    service_tier: params.service_tier,
                },
                reply,
            },
        )
        .await
        .map_err(|err| internal_error(format!("failed to submit turn settings: {err}")))?;
        let outcome = outcome
            .await
            .map_err(|_| internal_error("turn settings operation ended before replying"))?;
        let status = match outcome {
            TurnSettingsUpdateOutcome::Applied => TurnSettingsUpdateStatus::Applied,
            TurnSettingsUpdateOutcome::TargetUnavailable => {
                TurnSettingsUpdateStatus::TargetUnavailable
            }
            TurnSettingsUpdateOutcome::Rejected { reason } => return Err(invalid_request(reason)),
        };
        Ok(Some(TurnSettingsUpdateResponse { status }.into()))
    }

    pub(crate) async fn turn_steer(
        &self,
        request_id: &ConnectionRequestId,
        params: TurnSteerParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        validate_user_input_image_urls(&params.input)?;
        self.turn_steer_inner(request_id, params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn turn_interrupt(
        &self,
        request_id: &ConnectionRequestId,
        params: TurnInterruptParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let result = self.turn_interrupt_inner(request_id, params).await;
        if let Err(error) = &result {
            self.track_error_response(request_id, error, /*error_type*/ None);
        }
        result.map(|response| response.map(Into::into))
    }

    pub(crate) async fn thread_realtime_start(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadRealtimeStartParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_realtime_start_inner(request_id, params)
            .await
            .map(|response| response.map(Into::into))
    }

    pub(crate) async fn thread_realtime_append_audio(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadRealtimeAppendAudioParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_realtime_append_audio_inner(request_id, params)
            .await
            .map(|response| response.map(Into::into))
    }

    pub(crate) async fn thread_realtime_append_text(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadRealtimeAppendTextParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_realtime_append_text_inner(request_id, params)
            .await
            .map(|response| response.map(Into::into))
    }

    pub(crate) async fn thread_realtime_append_speech(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadRealtimeAppendSpeechParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_realtime_append_speech_inner(request_id, params)
            .await
            .map(|response| response.map(Into::into))
    }

    pub(crate) async fn thread_realtime_stop(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadRealtimeStopParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_realtime_stop_inner(request_id, params)
            .await
            .map(|response| response.map(Into::into))
    }

    pub(crate) async fn thread_realtime_list_voices(
        &self,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        Ok(Some(
            ThreadRealtimeListVoicesResponse {
                voices: RealtimeVoicesList::builtin(),
            }
            .into(),
        ))
    }

    pub(crate) async fn review_start(
        &self,
        request_id: &ConnectionRequestId,
        params: ReviewStartParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        if matches!(params.delivery, Some(ApiReviewDelivery::Detached)) {
            self.outgoing
                .send_server_notification_to_connections(
                    &[request_id.connection_id],
                    ServerNotification::DeprecationNotice(DeprecationNoticeNotification {
                        summary: "review/start with delivery \"detached\" is deprecated and will be removed in a future release.".to_string(),
                        details: Some("Use thread/start followed by review/start with delivery \"inline\" for a separate review thread, or thread/fork followed by turn/start with your own review instructions.".to_string()),
                    }),
                )
                .await;
        }
        self.review_start_inner(request_id, params)
            .await
            .map(|()| None)
    }

    fn track_error_response(
        &self,
        request_id: &ConnectionRequestId,
        error: &JSONRPCErrorError,
        error_type: Option<AnalyticsJsonRpcError>,
    ) {
        self.analytics_events_client.track_error_response(
            request_id.connection_id.0,
            request_id.request_id.clone(),
            error.clone(),
            error_type,
        );
    }

    async fn load_thread(
        &self,
        thread_id: &str,
    ) -> Result<(ThreadId, Arc<CodexThread>), JSONRPCErrorError> {
        // Resolve the core conversation handle from a v2 thread id string.
        let thread_id = ThreadId::from_string(thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;

        let thread = self
            .thread_manager
            .get_thread(thread_id)
            .await
            .map_err(|_| invalid_request(format!("thread not found: {thread_id}")))?;

        Ok((thread_id, thread))
    }

    async fn ensure_direct_input_allowed(
        &self,
        request_id: &ConnectionRequestId,
        thread: &CodexThread,
    ) -> Result<(), JSONRPCErrorError> {
        ensure_direct_input_allowed(thread)
            .await
            .inspect_err(|error| {
                self.track_error_response(request_id, error, /*error_type*/ None);
            })
    }

    fn normalize_collaboration_mode(
        &self,
        mut collaboration_mode: CollaborationMode,
    ) -> CollaborationMode {
        if collaboration_mode.settings.developer_instructions.is_none()
            && let Some(instructions) = builtin_collaboration_mode_presets()
                .into_iter()
                .find(|preset| preset.mode == Some(collaboration_mode.mode))
                .and_then(|preset| preset.developer_instructions.flatten())
                .filter(|instructions| !instructions.is_empty())
        {
            collaboration_mode.settings.developer_instructions = Some(instructions);
        }

        collaboration_mode
    }

    fn review_request_from_target(
        target: ApiReviewTarget,
    ) -> Result<(ReviewRequest, String, String), JSONRPCErrorError> {
        let cleaned_target = match target {
            ApiReviewTarget::UncommittedChanges => ApiReviewTarget::UncommittedChanges,
            ApiReviewTarget::BaseBranch { branch } => {
                let branch = branch.trim().to_string();
                if branch.is_empty() {
                    return Err(invalid_request("branch must not be empty".to_string()));
                }
                ApiReviewTarget::BaseBranch { branch }
            }
            ApiReviewTarget::Commit { sha, title } => {
                let sha = sha.trim().to_string();
                if sha.is_empty() {
                    return Err(invalid_request("sha must not be empty".to_string()));
                }
                let title = title
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty());
                ApiReviewTarget::Commit { sha, title }
            }
            ApiReviewTarget::Custom { instructions } => {
                let trimmed = instructions.trim().to_string();
                if trimmed.is_empty() {
                    return Err(invalid_request(
                        "instructions must not be empty".to_string(),
                    ));
                }
                ApiReviewTarget::Custom {
                    instructions: trimmed,
                }
            }
        };

        let core_target = match cleaned_target {
            ApiReviewTarget::UncommittedChanges => CoreReviewTarget::UncommittedChanges,
            ApiReviewTarget::BaseBranch { branch } => CoreReviewTarget::BaseBranch { branch },
            ApiReviewTarget::Commit { sha, title } => CoreReviewTarget::Commit { sha, title },
            ApiReviewTarget::Custom { instructions } => CoreReviewTarget::Custom { instructions },
        };
        let target_prompt = match &core_target {
            CoreReviewTarget::UncommittedChanges => {
                "Review the current code changes (staged, unstaged, and untracked files)."
                    .to_string()
            }
            CoreReviewTarget::BaseBranch { branch } => {
                format!("Review the code changes against the base branch {branch:?}.")
            }
            CoreReviewTarget::Commit { sha, .. } => {
                format!("Review the changes introduced by commit {sha:?}.")
            }
            CoreReviewTarget::Custom { instructions } => instructions.clone(),
        };

        let hint = codex_core::review_prompts::user_facing_hint(&core_target);
        let review_request = ReviewRequest {
            target: core_target,
            user_facing_hint: Some(hint.clone()),
        };

        Ok((review_request, hint, target_prompt))
    }

    async fn request_trace_context(
        &self,
        request_id: &ConnectionRequestId,
    ) -> Option<codex_protocol::protocol::W3cTraceContext> {
        self.outgoing.request_trace_context(request_id).await
    }

    async fn submit_core_op(
        &self,
        request_id: &ConnectionRequestId,
        thread: &CodexThread,
        op: Op,
    ) -> CodexResult<String> {
        thread
            .submit_with_trace(op, self.request_trace_context(request_id).await)
            .await
    }

    pub(super) fn input_too_large_error(actual_chars: usize) -> JSONRPCErrorError {
        let mut error = invalid_params(format!(
            "Input exceeds the maximum length of {MAX_USER_INPUT_TEXT_CHARS} characters."
        ));
        error.data = Some(serde_json::json!({
            "input_error_code": INPUT_TOO_LARGE_ERROR_CODE,
            "max_chars": MAX_USER_INPUT_TEXT_CHARS,
            "actual_chars": actual_chars,
        }));
        error
    }

    pub(super) fn validate_v2_input_limit(items: &[V2UserInput]) -> Result<(), JSONRPCErrorError> {
        let actual_chars: usize = items.iter().map(V2UserInput::text_char_count).sum();
        if actual_chars > MAX_USER_INPUT_TEXT_CHARS {
            return Err(Self::input_too_large_error(actual_chars));
        }
        Ok(())
    }

    async fn turn_start_inner(
        &self,
        request_id: ConnectionRequestId,
        params: TurnStartParams,
        app_server_client_name: Option<String>,
        app_server_client_version: Option<String>,
    ) -> Result<TurnStartResponse, JSONRPCErrorError> {
        let (thread_id, thread) =
            self.load_thread(&params.thread_id)
                .await
                .inspect_err(|error| {
                    self.track_error_response(&request_id, error, /*error_type*/ None);
                })?;
        self.ensure_direct_input_allowed(&request_id, thread.as_ref())
            .await?;
        self.config_manager
            .check_thread_model_provider(thread.config().await.as_ref())
            .await
            .map_err(|error| config_load_error(&error))?;
        if let Some(tool_output) = &params.tool_output {
            if !params.input.is_empty() {
                return Err(invalid_request(
                    "`toolOutput` cannot be combined with nonempty `input`",
                ));
            }
            if tool_output.name.is_empty() {
                return Err(invalid_request("`toolOutput.name` must not be empty"));
            }
        }
        let actual_chars = params
            .input
            .iter()
            .map(V2UserInput::text_char_count)
            .sum::<usize>()
            + params
                .tool_output
                .as_ref()
                .map_or(0, |output| match &output.output {
                    FunctionCallOutputBody::Text(text) => text.chars().count(),
                    FunctionCallOutputBody::ContentItems(items) => items
                        .iter()
                        .map(|item| match item {
                            FunctionCallOutputContentItem::InputText { text } => {
                                text.chars().count()
                            }
                            _ => 0,
                        })
                        .sum(),
                });
        if actual_chars > MAX_USER_INPUT_TEXT_CHARS {
            let error = Self::input_too_large_error(actual_chars);
            self.track_error_response(
                &request_id,
                &error,
                Some(AnalyticsJsonRpcError::Input(InputError::TooLarge)),
            );
            return Err(error);
        }
        Self::set_app_server_client_info(
            thread.as_ref(),
            app_server_client_name,
            app_server_client_version,
        )
        .await
        .inspect_err(|error| {
            self.track_error_response(&request_id, error, /*error_type*/ None);
        })?;
        let runtime_workspace_roots = params
            .runtime_workspace_roots
            .map(resolve_runtime_workspace_roots);
        let environment_selections =
            resolve_turn_environment_selections(self.thread_manager.as_ref(), params.environments)?;

        let additional_context = map_additional_context(params.additional_context);
        let turn_has_input = !params.input.is_empty();
        let input = if let Some(tool_output) = params.tool_output {
            let item = ResponseItem::FunctionCallOutput {
                id: None,
                call_id: None,
                name: Some(tool_output.name),
                namespace: tool_output.namespace,
                output: FunctionCallOutputPayload {
                    body: tool_output.output,
                    success: None,
                },
                internal_chat_message_metadata_passthrough: None,
            };
            validate_response_item_image_urls(std::slice::from_ref(&item))?;
            TurnInput::ResponseItem(item)
        } else {
            TurnInput::UserInput {
                content: params
                    .input
                    .into_iter()
                    .map(V2UserInput::into_core)
                    .collect(),
                client_id: params.client_user_message_id,
            }
        };
        let cwd = resolve_request_cwd(params.cwd)?;
        let environment_override = self
            .build_environment_override(
                thread.as_ref(),
                cwd,
                runtime_workspace_roots,
                environment_selections,
            )
            .await;
        let thread_settings = self
            .build_thread_settings_overrides(
                thread.as_ref(),
                ThreadSettingsBuildParams {
                    method: "turn/start",
                    disabled_plugin_ids: params.disabled_plugin_ids,
                    environment_override,
                    approval_policy: params.approval_policy,
                    approvals_reviewer: params.approvals_reviewer,
                    sandbox_policy: params.sandbox_policy,
                    permissions: params.permissions,
                    model: params.model,
                    service_tier: params.service_tier,
                    effort: params.effort,
                    summary: params.summary,
                    collaboration_mode: params.collaboration_mode,
                    personality: params.personality,
                },
            )
            .await?;

        let submission = thread
            .start_or_steer_turn(
                TurnInputRequest::new(input)
                    .with_thread_settings(thread_settings)
                    .on_start(TurnStartOptions {
                        turn_trigger: params.turn_trigger,
                        final_output_json_schema: params.output_schema,
                        service_tier: params.service_tier_for_turn,
                        cyber_access_program: params.cyber_access_program.map(Into::into),
                        ..Default::default()
                    })
                    .with_additional_context(additional_context)
                    .with_responses_metadata(params.responsesapi_client_metadata)
                    .with_trace(self.request_trace_context(&request_id).await),
            )
            .await
            .map_err(|err| {
                let error = internal_error(format!("failed to submit turn input: {err}"));
                self.track_error_response(&request_id, &error, /*error_type*/ None);
                error
            })?;
        let (turn_id, started) = match submission {
            TurnInputSubmission::Started { turn_id } => (turn_id, true),
            TurnInputSubmission::Steered { turn_id } => (turn_id, false),
            TurnInputSubmission::NotSubmitted { reason } => {
                let error = if reason == NotSubmittedReason::ServerDraining {
                    crate::error_code::server_draining_error()
                } else {
                    internal_error(format!("failed to submit turn input: {reason:?}"))
                };
                self.track_error_response(&request_id, &error, /*error_type*/ None);
                return Err(error);
            }
        };

        if turn_has_input && started {
            let config_snapshot = thread.config_snapshot().await;
            if config_snapshot.is_primary_environment_configured() {
                codex_memories_write::start_memories_startup_task(
                    Arc::clone(&self.thread_manager),
                    Arc::clone(&self.auth_manager),
                    thread_id,
                    Arc::clone(&thread),
                    thread.config().await,
                    config_snapshot.permission_profile,
                    &config_snapshot.session_source,
                );
            }
        }

        self.outgoing
            .record_request_turn_id(&request_id, &turn_id)
            .await;
        let turn = Turn {
            id: turn_id,
            items: vec![],
            items_view: TurnItemsView::NotLoaded,
            error: None,
            status: TurnStatus::InProgress,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        };

        Ok(TurnStartResponse { turn })
    }

    async fn build_environment_override(
        &self,
        thread: &CodexThread,
        cwd: Option<AbsolutePathBuf>,
        workspace_roots: Option<Vec<AbsolutePathBuf>>,
        environment_selections: Option<Vec<TurnEnvironmentSelection>>,
    ) -> ThreadEnvironmentOverride {
        if cwd.is_none() && workspace_roots.is_none() && environment_selections.is_none() {
            return ThreadEnvironmentOverride::default();
        }

        // Explicit environment selections own their roots and pass through unchanged. Top-level
        // `runtimeWorkspaceRoots` is only a compatibility input for default environments.
        if let Some(environment_selections) = environment_selections {
            let legacy_fallback_cwd = match cwd {
                Some(cwd) => cwd,
                None => match environment_selections
                    .iter()
                    .find(|selection| selection.environment_id == LOCAL_ENVIRONMENT_ID)
                    .and_then(|selection| selection.cwd.to_abs_path().ok())
                {
                    Some(cwd) => cwd,
                    None => thread.config_snapshot().await.cwd().clone(),
                },
            };
            return ThreadEnvironmentOverride {
                environments: Some(TurnEnvironmentSelections::new(
                    legacy_fallback_cwd,
                    environment_selections,
                )),
                ..Default::default()
            };
        }

        // Default-environment updates retain the task's fallback roots, not its active roots.
        let snapshot = thread.thread_settings_snapshot().await;
        let current_cwd = snapshot.cwd;
        let legacy_fallback_cwd = cwd.unwrap_or_else(|| current_cwd.clone());
        let workspace_roots = match workspace_roots {
            Some(workspace_roots) => workspace_roots,
            None => path_utils::replace_path_and_deduplicate(
                snapshot.runtime_workspace_roots.unwrap_or_default(),
                current_cwd.as_path(),
                legacy_fallback_cwd.clone(),
            ),
        };
        let environment_selections = self
            .thread_manager
            .default_environment_selections(&legacy_fallback_cwd, &workspace_roots);
        ThreadEnvironmentOverride {
            environments: Some(TurnEnvironmentSelections::new(
                legacy_fallback_cwd,
                environment_selections,
            )),
            runtime_workspace_roots: Some(workspace_roots),
        }
    }

    async fn build_thread_settings_overrides(
        &self,
        thread: &CodexThread,
        params: ThreadSettingsBuildParams,
    ) -> Result<codex_protocol::protocol::ThreadSettingsOverrides, JSONRPCErrorError> {
        let ThreadSettingsBuildParams {
            method,
            disabled_plugin_ids,
            environment_override:
                ThreadEnvironmentOverride {
                    environments,
                    runtime_workspace_roots,
                },
            approval_policy,
            approvals_reviewer,
            sandbox_policy,
            permissions,
            model,
            service_tier,
            effort,
            summary,
            collaboration_mode,
            personality,
        } = params;

        if sandbox_policy.is_some() && permissions.is_some() {
            return Err(invalid_request(
                "`permissions` cannot be combined with `sandboxPolicy`",
            ));
        }

        let collaboration_mode =
            collaboration_mode.map(|mode| self.normalize_collaboration_mode(mode));
        let has_environment_override = environments.is_some();
        // `thread/settings/update` only acknowledges that the update was queued.
        // Clients that send dependent partial updates should wait for
        // `thread/settings/updated` or combine the fields in one request.
        let snapshot = if permissions.is_some() {
            Some(thread.config_snapshot().await)
        } else {
            None
        };

        let has_any_overrides = has_environment_override
            || disabled_plugin_ids.is_some()
            || approval_policy.is_some()
            || approvals_reviewer.is_some()
            || sandbox_policy.is_some()
            || permissions.is_some()
            || model.is_some()
            || service_tier.is_some()
            || effort.is_some()
            || summary.is_some()
            || collaboration_mode.is_some()
            || personality.is_some();

        let approval_policy =
            approval_policy.map(codex_app_server_protocol::AskForApproval::to_core);
        let approvals_reviewer =
            approvals_reviewer.map(codex_app_server_protocol::ApprovalsReviewer::to_core);
        let sandbox_policy = sandbox_policy.map(|policy| policy.to_core());
        let (permission_profile, active_permission_profile, profile_workspace_roots) =
            if let Some(permissions) = permissions {
                let Some(snapshot) = snapshot.as_ref() else {
                    return Err(internal_error(format!(
                        "{method} permission selection missing thread snapshot"
                    )));
                };
                let overrides = ConfigOverrides {
                    cwd: environments
                        .as_ref()
                        .map(|environments| environments.legacy_fallback_cwd.to_path_buf()),
                    default_permissions: Some(permissions),
                    codex_linux_sandbox_exe: self.arg0_paths.codex_linux_sandbox_exe.clone(),
                    main_execve_wrapper_exe: self.arg0_paths.main_execve_wrapper_exe.clone(),
                    ..Default::default()
                };
                let config = self
                    .config_manager
                    .load_for_cwd(
                        /*request_overrides*/ None,
                        overrides,
                        Some(snapshot.cwd().to_path_buf()),
                    )
                    .await
                    .map_err(|err| config_load_error(&err))?;
                // Startup config is allowed to fall back when requirements
                // disallow a configured profile. An explicit settings update
                // is different: reject it before accepting the request.
                if let Some(warning) = config.startup_warnings.iter().find(|warning| {
                    warning.contains("Configured value for `permission_profile` is disallowed")
                }) {
                    return Err(invalid_request(format!(
                        "invalid thread settings override: {warning}"
                    )));
                }
                (
                    Some(config.permissions.permission_profile().clone()),
                    config.permissions.active_permission_profile(),
                    Some(config.permissions.profile_workspace_roots().to_vec()),
                )
            } else {
                (None, None, None)
            };
        let effort = effort.map(Some);

        if has_any_overrides {
            thread
                .preview_thread_settings_overrides(CodexThreadSettingsOverrides {
                    disabled_plugin_ids: disabled_plugin_ids.clone(),
                    environments: environments.clone(),
                    runtime_workspace_roots: runtime_workspace_roots.clone(),
                    approval_policy,
                    approvals_reviewer,
                    sandbox_policy: sandbox_policy.clone(),
                    permission_profile: permission_profile.clone(),
                    active_permission_profile: active_permission_profile.clone(),
                    profile_workspace_roots: profile_workspace_roots.clone(),
                    windows_sandbox_level: None,
                    model: model.clone(),
                    effort: effort.clone(),
                    summary,
                    service_tier: service_tier.clone(),
                    collaboration_mode: collaboration_mode.clone(),
                    personality,
                })
                .await
                .map_err(|err| {
                    invalid_request(format!("invalid thread settings override: {err}"))
                })?;
        }

        Ok(codex_protocol::protocol::ThreadSettingsOverrides {
            disabled_plugin_ids,
            environments,
            runtime_workspace_roots,
            profile_workspace_roots,
            approval_policy,
            approvals_reviewer,
            sandbox_policy,
            permission_profile,
            active_permission_profile,
            windows_sandbox_level: None,
            model,
            effort,
            summary,
            service_tier,
            collaboration_mode,
            personality,
        })
    }

    async fn thread_settings_update_inner(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadSettingsUpdateParams,
    ) -> Result<ThreadSettingsUpdateResponse, JSONRPCErrorError> {
        let (_, thread) = self.load_thread(&params.thread_id).await?;
        self.ensure_direct_input_allowed(request_id, thread.as_ref())
            .await?;
        let cwd = resolve_request_cwd(params.cwd)?;
        let environment_override = self
            .build_environment_override(
                thread.as_ref(),
                cwd,
                /*workspace_roots*/ None,
                /*environment_selections*/ None,
            )
            .await;
        let thread_settings = self
            .build_thread_settings_overrides(
                thread.as_ref(),
                ThreadSettingsBuildParams {
                    method: "thread/settings/update",
                    disabled_plugin_ids: params.disabled_plugin_ids,
                    environment_override,
                    approval_policy: params.approval_policy,
                    approvals_reviewer: params.approvals_reviewer,
                    sandbox_policy: params.sandbox_policy,
                    permissions: params.permissions,
                    model: params.model,
                    service_tier: params.service_tier,
                    effort: params.effort,
                    summary: params.summary,
                    collaboration_mode: params.collaboration_mode,
                    personality: params.personality,
                },
            )
            .await?;

        if thread_settings != codex_protocol::protocol::ThreadSettingsOverrides::default() {
            self.submit_core_op(
                request_id,
                thread.as_ref(),
                Op::ThreadSettings { thread_settings },
            )
            .await
            .map_err(|err| internal_error(format!("failed to update thread settings: {err}")))?;
        }

        Ok(ThreadSettingsUpdateResponse {})
    }

    async fn thread_inject_items_response_inner(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadInjectItemsParams,
    ) -> Result<ThreadInjectItemsResponse, JSONRPCErrorError> {
        let (_, thread) = self.load_thread(&params.thread_id).await?;
        self.ensure_direct_input_allowed(request_id, thread.as_ref())
            .await?;

        let items = params
            .items
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                serde_json::from_value::<ResponseItem>(value)
                    .map_err(|err| format!("items[{index}] is not a valid response item: {err}"))
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(invalid_request)?;
        validate_response_item_image_urls(&items)?;

        thread
            .inject_response_items(items)
            .await
            .map_err(|err| match err.details() {
                CodexErrorDetails::InvalidRequest(message) => invalid_request(message.clone()),
                _ => internal_error(format!("failed to inject response items: {err}")),
            })?;
        Ok(ThreadInjectItemsResponse {})
    }

    async fn set_app_server_client_info(
        thread: &CodexThread,
        app_server_client_name: Option<String>,
        app_server_client_version: Option<String>,
    ) -> Result<(), JSONRPCErrorError> {
        let mcp_elicitations_auto_deny = xcode_26_4_mcp_elicitations_auto_deny(
            app_server_client_name.as_deref(),
            app_server_client_version.as_deref(),
        );
        thread
            .set_app_server_client_info(
                app_server_client_name,
                app_server_client_version,
                mcp_elicitations_auto_deny,
            )
            .await
            .map_err(|err| internal_error(format!("failed to set app server client info: {err}")))
    }

    async fn turn_steer_inner(
        &self,
        request_id: &ConnectionRequestId,
        params: TurnSteerParams,
    ) -> Result<TurnSteerResponse, JSONRPCErrorError> {
        let (_, thread) = self
            .load_thread(&params.thread_id)
            .await
            .inspect_err(|error| {
                self.track_error_response(request_id, error, /*error_type*/ None);
            })?;
        self.ensure_direct_input_allowed(request_id, thread.as_ref())
            .await?;
        self.config_manager
            .check_thread_model_provider(thread.config().await.as_ref())
            .await
            .map_err(|error| config_load_error(&error))?;

        if params.expected_turn_id.is_empty() {
            return Err(invalid_request("expectedTurnId must not be empty"));
        }
        self.outgoing
            .record_request_turn_id(request_id, &params.expected_turn_id)
            .await;
        if let Err(error) = Self::validate_v2_input_limit(&params.input) {
            self.track_error_response(
                request_id,
                &error,
                Some(AnalyticsJsonRpcError::Input(InputError::TooLarge)),
            );
            return Err(error);
        }

        let mapped_items: Vec<CoreInputItem> = params
            .input
            .into_iter()
            .map(V2UserInput::into_core)
            .collect();
        let additional_context = map_additional_context(params.additional_context);

        let submission = thread
            .steer_turn(
                TurnInputRequest::new(TurnInput::UserInput {
                    content: mapped_items,
                    client_id: params.client_user_message_id,
                })
                .with_additional_context(additional_context)
                .with_responses_metadata(params.responsesapi_client_metadata),
                params.expected_turn_id,
            )
            .await
            .map_err(|err| {
                let error = internal_error(format!("failed to steer turn: {err}"));
                self.track_error_response(request_id, &error, /*error_type*/ None);
                error
            })?;
        let turn_id = match submission {
            SteerSubmission::Steered { turn_id } => turn_id,
            SteerSubmission::NotSubmitted { reason } => {
                let (message, data, error_type) = match reason {
                    NotSubmittedReason::ServerDraining => {
                        return Err(crate::error_code::server_draining_error());
                    }
                    NotSubmittedReason::NoActiveTurn | NotSubmittedReason::NotIdle => (
                        "no active turn to steer".to_string(),
                        None,
                        Some(AnalyticsJsonRpcError::TurnSteer(
                            TurnSteerRequestError::NoActiveTurn,
                        )),
                    ),
                    NotSubmittedReason::ExpectedTurnMismatch { expected, actual } => (
                        format!("expected active turn id `{expected}` but found `{actual}`"),
                        None,
                        Some(AnalyticsJsonRpcError::TurnSteer(
                            TurnSteerRequestError::ExpectedTurnMismatch,
                        )),
                    ),
                    NotSubmittedReason::ActiveTurnNotSteerable { turn_kind } => {
                        let (message, turn_steer_error) = match turn_kind {
                            codex_protocol::protocol::NonSteerableTurnKind::Review => (
                                "cannot steer a review turn".to_string(),
                                TurnSteerRequestError::NonSteerableReview,
                            ),
                            codex_protocol::protocol::NonSteerableTurnKind::Compact => (
                                "cannot steer a compact turn".to_string(),
                                TurnSteerRequestError::NonSteerableCompact,
                            ),
                        };
                        let error = TurnError {
                            misalignment: None,
                            message: message.clone(),
                            codex_error_info: Some(CodexErrorInfo::ActiveTurnNotSteerable {
                                turn_kind: turn_kind.into(),
                            }),
                            additional_details: None,
                        };
                        let data = match serde_json::to_value(error) {
                            Ok(data) => Some(data),
                            Err(error) => {
                                tracing::error!(
                                    ?error,
                                    "failed to serialize active-turn-not-steerable turn error"
                                );
                                None
                            }
                        };
                        (
                            message,
                            data,
                            Some(AnalyticsJsonRpcError::TurnSteer(turn_steer_error)),
                        )
                    }
                    NotSubmittedReason::EmptyInput => (
                        "input must not be empty".to_string(),
                        None,
                        Some(AnalyticsJsonRpcError::Input(InputError::Empty)),
                    ),
                    NotSubmittedReason::ActiveTurnOutputSchemaMismatch => (
                        "active turn uses a different output schema".to_string(),
                        None,
                        None,
                    ),
                    NotSubmittedReason::PendingTriggerTurn | NotSubmittedReason::PlanMode => (
                        "no active turn to steer".to_string(),
                        None,
                        Some(AnalyticsJsonRpcError::TurnSteer(
                            TurnSteerRequestError::NoActiveTurn,
                        )),
                    ),
                };
                let mut error = invalid_request(message);
                error.data = data;
                self.track_error_response(request_id, &error, error_type);
                return Err(error);
            }
        };
        Ok(TurnSteerResponse { turn_id })
    }

    async fn prepare_realtime_conversation_thread(
        &self,
        request_id: &ConnectionRequestId,
        thread_id: &str,
    ) -> Result<Option<(ThreadId, Arc<CodexThread>)>, JSONRPCErrorError> {
        let (thread_id, thread) = self.load_thread(thread_id).await?;
        self.ensure_direct_input_allowed(request_id, thread.as_ref())
            .await?;

        match self
            .ensure_conversation_listener(
                thread_id,
                request_id.connection_id,
                /*raw_events_enabled*/ false,
            )
            .await
        {
            Ok(EnsureConversationListenerResult::Attached) => {}
            Ok(EnsureConversationListenerResult::ConnectionClosed) => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        }

        Ok(Some((thread_id, thread)))
    }

    async fn thread_realtime_start_inner(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadRealtimeStartParams,
    ) -> Result<Option<ThreadRealtimeStartResponse>, JSONRPCErrorError> {
        let attaches_existing_call = matches!(
            &params.transport,
            Some(ThreadRealtimeStartTransport::ExistingCall { .. })
        );
        if attaches_existing_call {
            let unsupported_option = if params.include_startup_context == Some(true) {
                Some("includeStartupContext")
            } else if params.prompt.is_some() {
                Some("prompt")
            } else if params
                .initial_items
                .as_ref()
                .is_some_and(|items| !items.is_empty())
            {
                Some("initialItems")
            } else if params.model.is_some() {
                Some("model")
            } else if params.voice.is_some() {
                Some("voice")
            } else if params.delegation_ack_filler.is_some() {
                Some("delegationAckFiller")
            } else {
                None
            };
            if let Some(option) = unsupported_option {
                return Err(invalid_request(format!(
                    "existingCall transport does not support {option}"
                )));
            }
        }
        let Some((_, thread)) = self
            .prepare_realtime_conversation_thread(request_id, &params.thread_id)
            .await?
        else {
            return Ok(None);
        };
        self.submit_core_op(
            request_id,
            thread.as_ref(),
            Op::RealtimeConversationStart(ConversationStartParams {
                client_managed_handoffs: params.client_managed_handoffs.unwrap_or(false),
                delegation_ack_filler: params.delegation_ack_filler,
                flush_transcript_tail_on_session_end: params
                    .flush_transcript_tail_on_session_end
                    .unwrap_or(false),
                codex_responses_as_items: params.codex_responses_as_items.unwrap_or(false),
                codex_response_item_prefix: params.codex_response_item_prefix,
                codex_response_handoff_mode: params.codex_response_handoff_mode.unwrap_or_default(),
                codex_response_handoff_channel_prefixes: params
                    .codex_response_handoff_channel_prefixes,
                model: params.model,
                output_modality: params.output_modality,
                include_startup_context: params
                    .include_startup_context
                    .unwrap_or(!attaches_existing_call),
                initial_items: params
                    .initial_items
                    .unwrap_or_default()
                    .into_iter()
                    .map(|item| ConversationTextParams {
                        text: item.text,
                        role: item.role,
                    })
                    .collect(),
                realtime_start_instructions: params.realtime_start_instructions,
                realtime_end_instructions: params.realtime_end_instructions,
                prompt: params.prompt,
                realtime_session_id: params.realtime_session_id,
                transport: params.transport.map(|transport| match transport {
                    ThreadRealtimeStartTransport::Websocket => {
                        ConversationStartTransport::Websocket
                    }
                    ThreadRealtimeStartTransport::Webrtc { sdp } => {
                        ConversationStartTransport::Webrtc { sdp }
                    }
                    ThreadRealtimeStartTransport::ExistingCall { call_id } => {
                        ConversationStartTransport::ExistingCall {
                            call_id,
                            sideband_base_url: None,
                        }
                    }
                }),
                version: params.version,
                voice: params.voice,
            }),
        )
        .await
        .map_err(|err| internal_error(format!("failed to start realtime conversation: {err}")))?;
        Ok(Some(ThreadRealtimeStartResponse::default()))
    }

    async fn thread_realtime_append_audio_inner(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadRealtimeAppendAudioParams,
    ) -> Result<Option<ThreadRealtimeAppendAudioResponse>, JSONRPCErrorError> {
        let Some((_, thread)) = self
            .prepare_realtime_conversation_thread(request_id, &params.thread_id)
            .await?
        else {
            return Ok(None);
        };
        self.submit_core_op(
            request_id,
            thread.as_ref(),
            Op::RealtimeConversationAudio(ConversationAudioParams {
                frame: params.audio.into(),
            }),
        )
        .await
        .map_err(|err| {
            internal_error(format!(
                "failed to append realtime conversation audio: {err}"
            ))
        })?;
        Ok(Some(ThreadRealtimeAppendAudioResponse::default()))
    }

    async fn thread_realtime_append_text_inner(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadRealtimeAppendTextParams,
    ) -> Result<Option<ThreadRealtimeAppendTextResponse>, JSONRPCErrorError> {
        let Some((_, thread)) = self
            .prepare_realtime_conversation_thread(request_id, &params.thread_id)
            .await?
        else {
            return Ok(None);
        };
        self.submit_core_op(
            request_id,
            thread.as_ref(),
            Op::RealtimeConversationText(ConversationTextParams {
                text: params.text,
                role: params.role,
            }),
        )
        .await
        .map_err(|err| {
            internal_error(format!(
                "failed to append realtime conversation text: {err}"
            ))
        })?;
        Ok(Some(ThreadRealtimeAppendTextResponse::default()))
    }

    async fn thread_realtime_append_speech_inner(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadRealtimeAppendSpeechParams,
    ) -> Result<Option<ThreadRealtimeAppendSpeechResponse>, JSONRPCErrorError> {
        let Some((_, thread)) = self
            .prepare_realtime_conversation_thread(request_id, &params.thread_id)
            .await?
        else {
            return Ok(None);
        };
        self.submit_core_op(
            request_id,
            thread.as_ref(),
            Op::RealtimeConversationSpeech(ConversationSpeechParams { text: params.text }),
        )
        .await
        .map_err(|err| {
            internal_error(format!(
                "failed to append realtime conversation speech: {err}"
            ))
        })?;
        Ok(Some(ThreadRealtimeAppendSpeechResponse::default()))
    }

    async fn thread_realtime_stop_inner(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadRealtimeStopParams,
    ) -> Result<Option<ThreadRealtimeStopResponse>, JSONRPCErrorError> {
        let Some((_, thread)) = self
            .prepare_realtime_conversation_thread(request_id, &params.thread_id)
            .await?
        else {
            return Ok(None);
        };
        self.submit_core_op(request_id, thread.as_ref(), Op::RealtimeConversationClose)
            .await
            .map_err(|err| {
                internal_error(format!("failed to stop realtime conversation: {err}"))
            })?;
        Ok(Some(ThreadRealtimeStopResponse::default()))
    }

    fn build_review_turn(turn_id: String, display_text: &str) -> Turn {
        let items = if display_text.is_empty() {
            Vec::new()
        } else {
            vec![ThreadItem::UserMessage {
                id: turn_id.clone(),
                client_id: None,
                content: vec![V2UserInput::Text {
                    text: display_text.to_string(),
                    // Review prompt display text is synthesized; no UI element ranges to preserve.
                    text_elements: Vec::new(),
                }],
            }]
        };

        Turn {
            id: turn_id,
            items,
            items_view: TurnItemsView::NotLoaded,
            error: None,
            status: TurnStatus::InProgress,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        }
    }

    async fn emit_review_started(
        &self,
        request_id: &ConnectionRequestId,
        turn: Turn,
        review_thread_id: String,
    ) {
        let response = ReviewStartResponse {
            turn,
            review_thread_id,
        };
        self.outgoing
            .send_response(request_id.clone(), response)
            .await;
    }

    async fn start_inline_review(
        &self,
        request_id: &ConnectionRequestId,
        parent_thread: Arc<CodexThread>,
        review_request: ReviewRequest,
        display_text: &str,
        parent_thread_id: String,
    ) -> std::result::Result<(), JSONRPCErrorError> {
        let turn_id = self
            .submit_core_op(
                request_id,
                parent_thread.as_ref(),
                Op::Review { review_request },
            )
            .await
            .map_err(|err| internal_error(format!("failed to start review: {err}")))?;
        let turn = Self::build_review_turn(turn_id, display_text);
        self.emit_review_started(request_id, turn, parent_thread_id)
            .await;
        Ok(())
    }

    async fn start_detached_review(
        &self,
        request_id: &ConnectionRequestId,
        parent_thread: Arc<CodexThread>,
        prompt: &str,
    ) -> std::result::Result<(), JSONRPCErrorError> {
        // AgentRunner::start still delegates to spawn_subagent, which forks from the parent's
        // full history. Paginated threads only allow bounded model-context reads, so keep this
        // closed until detached review has a bounded fork path.
        if matches!(
            parent_thread.config_snapshot().await.history_mode,
            codex_protocol::protocol::ThreadHistoryMode::Paginated
        ) {
            return Err(invalid_request(
                "paginated threads do not support detached review",
            ));
        }
        let mut config = parent_thread.config().await.as_ref().clone();
        if let Some(review_model) = &config.review_model {
            config.model = Some(review_model.clone());
        }

        let AgentRun {
            thread_id,
            thread: review_thread,
            turn_id,
        } = self
            .agent_runner
            .start(
                parent_thread.session_configured().thread_id,
                AgentInvocation {
                    config,
                    prompt: prompt.to_string(),
                    parent_trace: self.request_trace_context(request_id).await,
                    output_schema: None,
                    reserved_thread_id: None,
                    thread_source: None,
                    start_gate: None,
                },
            )
            .await
            .map_err(|err| internal_error(format!("failed to start detached review: {err}")))?;

        let fallback_provider = self.config.model_provider_id.as_str();
        let stored_thread = match review_thread
            .read_thread(
                /*include_archived*/ true, /*include_history*/ false,
            )
            .await
        {
            Ok(stored_thread) => {
                let (thread, _) =
                    thread_from_stored_thread(stored_thread, fallback_provider, &self.config.cwd);
                Some(thread)
            }
            Err(err) => {
                tracing::warn!("failed to load summary for review thread {thread_id}: {err}");
                None
            }
        };

        if let Some(mut thread) = stored_thread {
            let config_snapshot = review_thread.config_snapshot().await;
            apply_live_thread_settings(&mut thread, &config_snapshot);
            thread.session_id = review_thread.session_configured().session_id.to_string();
            self.thread_watch_manager
                .upsert_thread_silently(&thread.id)
                .await;
            thread.status = resolve_thread_status(
                self.thread_watch_manager
                    .loaded_status_for_thread(&thread.id)
                    .await,
                /*has_in_progress_turn*/ false,
            );
            let notif = thread_started_notification(thread);
            self.outgoing
                .send_server_notification(ServerNotification::ThreadStarted(notif))
                .await;
        }

        log_listener_attach_result(
            self.ensure_conversation_listener(
                thread_id,
                request_id.connection_id,
                /*raw_events_enabled*/ false,
            )
            .await,
            thread_id,
            request_id.connection_id,
            "review thread",
        );

        let turn = Self::build_review_turn(turn_id, prompt);
        let review_thread_id = thread_id.to_string();
        self.emit_review_started(request_id, turn, review_thread_id)
            .await;

        Ok(())
    }

    async fn review_start_inner(
        &self,
        request_id: &ConnectionRequestId,
        params: ReviewStartParams,
    ) -> Result<(), JSONRPCErrorError> {
        let ReviewStartParams {
            thread_id,
            target,
            delivery,
        } = params;

        let (_, parent_thread) = self.load_thread(&thread_id).await?;
        self.ensure_direct_input_allowed(request_id, parent_thread.as_ref())
            .await?;
        self.config_manager
            .check_thread_model_provider(parent_thread.config().await.as_ref())
            .await
            .map_err(|error| config_load_error(&error))?;
        let (review_request, display_text, target_prompt) =
            Self::review_request_from_target(target)?;
        match delivery.unwrap_or(ApiReviewDelivery::Inline).to_core() {
            CoreReviewDelivery::Inline => {
                self.start_inline_review(
                    request_id,
                    parent_thread,
                    review_request,
                    &display_text,
                    thread_id,
                )
                .await?;
            }
            CoreReviewDelivery::Detached => {
                let review_skill_path = system_cache_root_dir(&self.config.codex_home)
                    .join("review-agent")
                    .join("SKILL.md");
                let prompt = format!(
                    "Use [$review-agent]({}) for this review.\n\n{target_prompt}",
                    review_skill_path.display()
                );
                let actual_chars = prompt.chars().count();
                if actual_chars > MAX_USER_INPUT_TEXT_CHARS {
                    return Err(Self::input_too_large_error(actual_chars));
                }
                self.start_detached_review(request_id, parent_thread, &prompt)
                    .await?;
            }
        }
        Ok(())
    }

    async fn turn_interrupt_inner(
        &self,
        request_id: &ConnectionRequestId,
        params: TurnInterruptParams,
    ) -> Result<Option<TurnInterruptResponse>, JSONRPCErrorError> {
        let TurnInterruptParams { thread_id, turn_id } = params;
        let is_startup_interrupt = turn_id.is_empty();

        let (thread_uuid, thread) = self.load_thread(&thread_id).await?;

        // Record turn interrupts so we can reply when TurnAborted arrives. Startup
        // interrupts do not have a turn and are acknowledged after submission.
        if !is_startup_interrupt {
            let thread_state = self.thread_state_manager.thread_state(thread_uuid).await;
            let is_running = matches!(thread.agent_status().await, AgentStatus::Running);
            {
                let mut thread_state = thread_state.lock().await;
                if let Some(active_turn) = thread_state.active_turn_snapshot() {
                    if active_turn.id != turn_id {
                        return Err(invalid_request(format!(
                            "expected active turn id {turn_id} but found {}",
                            active_turn.id
                        )));
                    }
                } else if thread_state.last_terminal_turn_id.as_deref() == Some(turn_id.as_str())
                    || !is_running
                {
                    return Err(invalid_request("no active turn to interrupt"));
                }
                thread_state.pending_interrupts.push(request_id.clone());
            }

            self.outgoing
                .record_request_turn_id(request_id, &turn_id)
                .await;
        }

        // Submit the interrupt. Turn interrupts respond upon TurnAborted; startup
        // interrupts respond here because startup cancellation has no turn event.
        match self
            .submit_core_op(request_id, thread.as_ref(), Op::Interrupt)
            .await
        {
            Ok(_) if is_startup_interrupt => Ok(Some(TurnInterruptResponse {})),
            Ok(_) => Ok(None),
            Err(err) => {
                if !is_startup_interrupt {
                    let thread_state = self.thread_state_manager.thread_state(thread_uuid).await;
                    let mut thread_state = thread_state.lock().await;
                    thread_state
                        .pending_interrupts
                        .retain(|pending_request_id| pending_request_id != request_id);
                }
                let interrupt_target = if is_startup_interrupt {
                    "startup"
                } else {
                    "turn"
                };
                Err(internal_error(format!(
                    "failed to interrupt {interrupt_target}: {err}"
                )))
            }
        }
    }

    fn listener_task_context(&self) -> ListenerTaskContext {
        ListenerTaskContext {
            thread_manager: Arc::clone(&self.thread_manager),
            thread_state_manager: self.thread_state_manager.clone(),
            outgoing: Arc::clone(&self.outgoing),
            pending_thread_unloads: Arc::clone(&self.pending_thread_unloads),
            thread_watch_manager: self.thread_watch_manager.clone(),
            codex_home: self.config.codex_home.to_path_buf(),
            thread_unload_delay: self.config.thread_unload_delay,
            skills_watcher: Arc::clone(&self.skills_watcher),
            turn_cost_worker: self.turn_cost_worker.clone(),
        }
    }

    async fn ensure_conversation_listener(
        &self,
        conversation_id: ThreadId,
        connection_id: ConnectionId,
        raw_events_enabled: bool,
    ) -> Result<EnsureConversationListenerResult, JSONRPCErrorError> {
        super::thread_lifecycle::ensure_conversation_listener(
            self.listener_task_context(),
            conversation_id,
            connection_id,
            raw_events_enabled,
        )
        .await
    }
}

fn xcode_26_4_mcp_elicitations_auto_deny(
    client_name: Option<&str>,
    client_version: Option<&str>,
) -> bool {
    // Xcode 26.4 shipped before app-server MCP elicitation requests were
    // client-visible. Keep elicitations auto-denied for that client line.
    // TODO: Remove this compatibility hack once Xcode 26.4 ages out.
    client_name == Some("Xcode")
        && client_version.is_some_and(|version| version.starts_with("26.4"))
}

#[cfg(test)]
mod workflow_selection_tests {
    use super::workflow_effective_selection;

    #[test]
    fn explicit_worker_selection_overrides_role_while_omitted_preserves_it() {
        assert_eq!(
            workflow_effective_selection(
                Some("gpt-5.6-sol"),
                Some("gpt-5.6-luna"),
                "ignored",
                false,
            ),
            Some("gpt-5.6-luna")
        );
        assert_eq!(
            workflow_effective_selection(
                Some("gpt-5.6-sol"),
                Some("gpt-5.6-luna"),
                "gpt-5.6-sol",
                true,
            ),
            Some("gpt-5.6-sol")
        );
        assert_eq!(
            workflow_effective_selection(Some("medium"), Some("low"), "high", false),
            Some("low")
        );
        assert_eq!(
            workflow_effective_selection(Some("medium"), Some("low"), "high", true),
            Some("high")
        );
    }
}

fn workflow_completion_response(
    submission: TurnInputSubmission,
) -> Result<WorkflowCompletionInjectResponse, JSONRPCErrorError> {
    match submission {
        TurnInputSubmission::Started { turn_id } | TurnInputSubmission::Steered { turn_id } => {
            Ok(WorkflowCompletionInjectResponse { turn_id })
        }
        TurnInputSubmission::NotSubmitted { reason } => Err(internal_error(format!(
            "workflow completion was not submitted: {reason:?}"
        ))),
    }
}

#[cfg(test)]
#[path = "workflow_completion_tests.rs"]
mod workflow_completion_tests;
