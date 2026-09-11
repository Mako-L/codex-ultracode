use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use codex_protocol::config_types::WorktreeBaseRef;
use codex_protocol::protocol::NetworkSandboxPolicy;
use tokio::process::Command;

use crate::git_process::run_git_command_with_timeout_output;

const GIT_TIMEOUT: Duration = Duration::from_secs(5);
const FETCH_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

async fn git(cwd: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    if timeout.is_zero() {
        return None;
    }
    let output = run_git_command_with_timeout_output(
        Command::new("git")
            .current_dir(cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(args),
        timeout,
    )
    .await?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

async fn origin_default_ref<F, Fut, E>(run: &mut F) -> Result<Option<String>, E>
where
    F: FnMut(Vec<String>, Duration) -> Fut,
    Fut: std::future::Future<Output = Result<Option<String>, E>>,
{
    Ok(run(
        strings(&["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"]),
        GIT_TIMEOUT,
    )
    .await?
    .filter(|name| name.starts_with("refs/remotes/origin/") && name != "refs/remotes/origin/HEAD"))
}

/// Resolve the reference used for a new workflow worktree. Network refresh is
/// advisory and bounded; failure preserves the cached remote tip or local HEAD.
pub async fn workflow_worktree_base(
    cwd: &Path,
    base_ref: WorktreeBaseRef,
    network: NetworkSandboxPolicy,
) -> Option<String> {
    workflow_worktree_base_with(cwd, base_ref, network, |args, timeout| async move {
        let args: Vec<_> = args.iter().map(String::as_str).collect();
        Ok::<_, std::convert::Infallible>(git(cwd, &args, timeout).await)
    })
    .await
    .unwrap_or_else(|never| match never {})
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

/// Resolve the same base using a caller-owned permission-aware command executor.
/// Command failures remain advisory; the executor must enforce the supplied timeout.
pub async fn workflow_worktree_base_with<F, Fut, E>(
    cwd: &Path,
    base_ref: WorktreeBaseRef,
    network: NetworkSandboxPolicy,
    mut run: F,
) -> Result<Option<String>, E>
where
    F: FnMut(Vec<String>, Duration) -> Fut,
    Fut: std::future::Future<Output = Result<Option<String>, E>>,
{
    let Some(head) = run(
        strings(&["rev-parse", "--verify", "HEAD^{commit}"]),
        GIT_TIMEOUT,
    )
    .await?
    else {
        return Ok(None);
    };
    if base_ref == WorktreeBaseRef::Head
        || run(strings(&["remote", "get-url", "origin"]), GIT_TIMEOUT)
            .await?
            .is_none()
    {
        return Ok(Some(head));
    }
    let mut remote_ref = origin_default_ref(&mut run).await?;
    let common_dir = run(strings(&["rev-parse", "--git-common-dir"]), GIT_TIMEOUT).await?;
    let stale = common_dir
        .and_then(|directory| std::fs::metadata(cwd.join(directory).join("FETCH_HEAD")).ok())
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.elapsed().ok())
        .is_none_or(|age| age > FETCH_MAX_AGE);
    if network == NetworkSandboxPolicy::Enabled && (stale || remote_ref.is_none()) {
        let deadline = Instant::now() + GIT_TIMEOUT;
        if remote_ref.is_none() {
            let _ = run(
                strings(&["remote", "set-head", "origin", "--auto"]),
                deadline.saturating_duration_since(Instant::now()),
            )
            .await?;
            remote_ref = origin_default_ref(&mut run).await?;
        }
        if let Some(branch) = remote_ref
            .as_deref()
            .and_then(|name| name.strip_prefix("refs/remotes/origin/"))
        {
            let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
            let _ = run(
                strings(&["fetch", "--quiet", "--no-tags", "origin", &refspec]),
                deadline.saturating_duration_since(Instant::now()),
            )
            .await?;
        }
    }
    if let Some(remote_ref) = remote_ref {
        let commit_ref = format!("{remote_ref}^{{commit}}");
        if let Some(commit) = run(
            strings(&["rev-parse", "--verify", &commit_ref]),
            GIT_TIMEOUT,
        )
        .await?
        {
            return Ok(Some(commit));
        }
    }
    Ok(Some(head))
}

#[cfg(test)]
#[path = "workflow_worktree_tests.rs"]
mod tests;
