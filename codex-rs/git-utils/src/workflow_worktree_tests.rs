use super::*;

#[tokio::test]
async fn permission_denial_aborts_base_selection_but_missing_origin_keeps_head() {
    let temp = tempfile::tempdir().unwrap();
    for denial in [
        Some("never policy"),
        Some("granular policy"),
        Some("user declined"),
        None,
    ] {
        let mut commands = Vec::new();
        let result = workflow_worktree_base_with(
            temp.path(),
            WorktreeBaseRef::default(),
            NetworkSandboxPolicy::Enabled,
            |args, _timeout| {
                commands.push(args.clone());
                let response = match args.first().map(String::as_str) {
                    Some("rev-parse") => Ok(Some("head-commit".to_string())),
                    Some("remote") => denial.map_or(Ok(None), Err),
                    _ => panic!("permission denial must not reach another command"),
                };
                std::future::ready(response)
            },
        )
        .await;
        assert_eq!(commands.len(), 2);
        match denial {
            Some(reason) => assert_eq!(result, Err(reason)),
            None => assert_eq!(result, Ok(Some("head-commit".to_string()))),
        }
    }
}

async fn command(cwd: &Path, args: &[&str]) -> String {
    git(cwd, args, GIT_TIMEOUT)
        .await
        .expect("fixture git command")
}

async fn commit(cwd: &Path, message: &str) -> String {
    command(
        cwd,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            message,
        ],
    )
    .await;
    command(cwd, &["rev-parse", "HEAD"]).await
}

#[tokio::test]
async fn fresh_uses_origin_default_and_head_preserves_unpushed_commits() {
    let temp = tempfile::tempdir().unwrap();
    let upstream = temp.path().join("upstream");
    std::fs::create_dir(&upstream).unwrap();
    command(&upstream, &["init", "-b", "release/main"]).await;
    let base = commit(&upstream, "base").await;
    command(
        temp.path(),
        &["clone", upstream.to_str().unwrap(), "checkout"],
    )
    .await;
    let checkout = temp.path().join("checkout");
    let local = commit(&checkout, "unpushed").await;
    assert_eq!(
        workflow_worktree_base(
            &checkout,
            WorktreeBaseRef::Fresh,
            NetworkSandboxPolicy::Restricted
        )
        .await,
        Some(base)
    );
    assert_eq!(
        workflow_worktree_base(
            &checkout,
            WorktreeBaseRef::Head,
            NetworkSandboxPolicy::Enabled
        )
        .await,
        Some(local)
    );
    let remote = commit(&upstream, "remote update").await;
    assert_eq!(
        workflow_worktree_base(
            &checkout,
            WorktreeBaseRef::Fresh,
            NetworkSandboxPolicy::Enabled
        )
        .await,
        Some(remote.clone())
    );
    commit(&upstream, "newer remote update").await;
    assert_eq!(
        workflow_worktree_base(
            &checkout,
            WorktreeBaseRef::Fresh,
            NetworkSandboxPolicy::Enabled
        )
        .await,
        Some(remote)
    );
}

#[tokio::test]
async fn absent_origin_or_uncached_default_falls_back_to_current_head() {
    let temp = tempfile::tempdir().unwrap();
    command(temp.path(), &["init", "-b", "main"]).await;
    commit(temp.path(), "main").await;
    command(temp.path(), &["switch", "-c", "feature"]).await;
    let head = commit(temp.path(), "feature").await;
    assert_eq!(
        workflow_worktree_base(
            temp.path(),
            WorktreeBaseRef::Fresh,
            NetworkSandboxPolicy::Enabled
        )
        .await,
        Some(head.clone())
    );
    command(
        temp.path(),
        &[
            "remote",
            "add",
            "origin",
            temp.path().join("missing").to_str().unwrap(),
        ],
    )
    .await;
    assert_eq!(
        workflow_worktree_base(
            temp.path(),
            WorktreeBaseRef::Fresh,
            NetworkSandboxPolicy::Enabled
        )
        .await,
        Some(head)
    );
}

#[tokio::test]
async fn fresh_reuses_common_fetch_head_from_a_linked_worktree() {
    let temp = tempfile::tempdir().unwrap();
    let upstream = temp.path().join("upstream");
    std::fs::create_dir(&upstream).unwrap();
    command(&upstream, &["init", "-b", "main"]).await;
    let cached = commit(&upstream, "base").await;
    command(
        temp.path(),
        &["clone", upstream.to_str().unwrap(), "checkout"],
    )
    .await;
    let checkout = temp.path().join("checkout");
    command(&checkout, &["fetch", "origin"]).await;
    let linked = temp.path().join("linked");
    command(
        &checkout,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    )
    .await;
    commit(&upstream, "new remote commit after cached fetch").await;

    assert_eq!(
        workflow_worktree_base(
            &linked,
            WorktreeBaseRef::Fresh,
            NetworkSandboxPolicy::Enabled
        )
        .await,
        Some(cached.clone()),
    );
    assert_eq!(
        command(&checkout, &["rev-parse", "refs/remotes/origin/main"]).await,
        cached
    );
}

#[tokio::test]
async fn failed_refresh_keeps_cached_remote_tip() {
    let temp = tempfile::tempdir().unwrap();
    command(temp.path(), &["init", "-b", "main"]).await;
    let base = commit(temp.path(), "base").await;
    command(
        temp.path(),
        &[
            "remote",
            "add",
            "origin",
            temp.path().join("missing").to_str().unwrap(),
        ],
    )
    .await;
    command(
        temp.path(),
        &["update-ref", "refs/remotes/origin/main", &base],
    )
    .await;
    command(
        temp.path(),
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    )
    .await;
    commit(temp.path(), "unpushed").await;
    assert_eq!(
        workflow_worktree_base(
            temp.path(),
            WorktreeBaseRef::Fresh,
            NetworkSandboxPolicy::Enabled
        )
        .await,
        Some(base)
    );
}

#[tokio::test]
async fn head_uses_the_current_linked_worktree() {
    let temp = tempfile::tempdir().unwrap();
    command(temp.path(), &["init", "-b", "main"]).await;
    let base = commit(temp.path(), "base").await;
    let linked = temp.path().join("linked");
    command(
        temp.path(),
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    )
    .await;
    commit(temp.path(), "main update").await;
    assert_eq!(
        workflow_worktree_base(
            &linked,
            WorktreeBaseRef::Head,
            NetworkSandboxPolicy::Enabled
        )
        .await,
        Some(base)
    );
}
