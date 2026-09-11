use super::*;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use pretty_assertions::assert_eq;

#[test]
fn disjoint_checkout_requires_scope_and_preserves_denials_network_and_temp_exclusions() {
    let temp = tempfile::tempdir().unwrap();
    let root = AbsolutePathBuf::from_absolute_path(temp.path().canonicalize().unwrap()).unwrap();
    let source = root.join("source");
    let checkout = root.join("checkout");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&checkout).unwrap();
    let profile =
        PermissionProfile::workspace_write_with(&[], NetworkSandboxPolicy::Restricted, true, true);
    let mut policy = profile.file_system_sandbox_policy();
    policy.entries.push(FileSystemSandboxEntry::new(
        FileSystemPath::Special {
            value: FileSystemSpecialPath::ProjectRoots {
                subpath: Some("secret".into()),
            },
        },
        FileSystemAccessMode::Deny,
    ));
    let profile =
        PermissionProfile::from_runtime_permissions(&policy, NetworkSandboxPolicy::Restricted);
    let authority = profile
        .clone()
        .materialize_project_roots_with_workspace_roots(&[source.clone()]);
    let without_grant = bounded_checkout_profile(
        &authority,
        &profile,
        &[source.clone()],
        &checkout,
        Default::default(),
    )
    .unwrap();
    assert!(
        !without_grant
            .file_system_sandbox_policy()
            .can_write_path_with_cwd(checkout.join("result").as_path(), checkout.as_path())
    );
    let mut returned = write_scope(vec![checkout.clone(), root.join("unrelated")]);
    returned.network = Some(codex_protocol::models::NetworkPermissions {
        enabled: Some(true),
    });
    let bounded =
        bounded_checkout_profile(&authority, &profile, &[source], &checkout, returned).unwrap();
    let policy = bounded.file_system_sandbox_policy();
    assert!(policy.can_write_path_with_cwd(checkout.join("result").as_path(), checkout.as_path()));
    assert!(
        !policy
            .can_write_path_with_cwd(root.join("unrelated/result").as_path(), checkout.as_path())
    );
    assert!(!policy.can_write_path_with_cwd(checkout.join("secret").as_path(), checkout.as_path()));
    assert!(!policy.can_read_path_with_cwd(checkout.join("secret").as_path(), checkout.as_path()));
    assert!(
        !policy.can_write_path_with_cwd(checkout.join(".git/config").as_path(), checkout.as_path())
    );
    assert_eq!(
        bounded.network_sandbox_policy(),
        NetworkSandboxPolicy::Restricted
    );
    assert!(
        !policy
            .entries
            .iter()
            .any(|entry| entry.access == FileSystemAccessMode::Write
                && matches!(
                    entry.path,
                    FileSystemPath::Special {
                        value: FileSystemSpecialPath::Tmpdir | FileSystemSpecialPath::SlashTmp
                    }
                ))
    );
}

#[test]
fn grant_cannot_be_reused_by_another_workspace_or_worker() {
    let temp = tempfile::tempdir().unwrap();
    let root = AbsolutePathBuf::from_absolute_path(temp.path().canonicalize().unwrap()).unwrap();
    let parent = ThreadId::new();
    let grant = CheckoutGrant {
        parent_thread_id: parent,
        run_id: "run".into(),
        worker_id: "worker".into(),
        workspace_id: "workspace".into(),
        path: root.join("checkout"),
    };
    let mut workspace = WorkflowWorkspaceOwnership {
        parent_thread_id: parent,
        authority_ref: "authority".into(),
        authority_digest: "digest".into(),
        run_id: "run".into(),
        worker_id: "worker".into(),
        cwd: root.join("checkout"),
        base_commit: "base".into(),
        permission_profile: PermissionProfile::read_only(),
        released: false,
        common_git_dir: root.join(".git").to_path_buf(),
        repository_cwd: root,
        branch: "branch".into(),
        removed: false,
        checkout_grant: Some(grant.clone()),
    };
    assert!(grant.matches("workspace", &workspace));
    assert!(!grant.matches("other-workspace", &workspace));
    workspace.worker_id = "other-worker".into();
    assert!(!grant.matches("workspace", &workspace));
    workspace.worker_id = "worker".into();
    workspace.parent_thread_id = ThreadId::new();
    assert!(!grant.matches("workspace", &workspace));
    workspace.parent_thread_id = parent;
    workspace.run_id = "other-run".into();
    assert!(!grant.matches("workspace", &workspace));
}

#[cfg(unix)]
#[test]
fn checkout_grant_rejects_symlink_ancestor_before_creation() {
    let temp = tempfile::tempdir().unwrap();
    let root = AbsolutePathBuf::from_absolute_path(temp.path().canonicalize().unwrap()).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join(".ultracode")).unwrap();
    let checkout = root.join(".ultracode/worktrees/run-worker");
    assert!(validate_checkout_path(&root, &checkout).is_err());
    assert!(!outside.path().join("worktrees").exists());
}
