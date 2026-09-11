//! A workflow backend uses the launching fork without modifying the managed stock install.
use super::*;

/// Resolve the workflow backend's control socket separately from the stock daemon.
pub fn workflow_backend_socket_path(codex_home: &Path) -> Result<PathBuf> {
    Ok(codex_home
        .canonicalize()?
        .join("ultracode-daemon/control.sock"))
}

/// Start or reuse the fork-owned daemon using the existing process locks and PID checks.
/// This route never starts the stock installation's updater or uses its process records.
pub async fn ensure_workflow_backend(codex_home: &Path, codex_bin: &Path) -> Result<PathBuf> {
    ensure_supported_platform()?;
    let codex_home = codex_home.canonicalize()?;
    let codex_bin = codex_bin.canonicalize()?;
    let socket_path = workflow_backend_socket_path(&codex_home)?;
    let state_dir = codex_home.join("ultracode-daemon");
    let daemon = Daemon {
        socket_path: socket_path.clone(),
        backend_socket_path: Some(socket_path.clone()),
        backend_codex_home: Some(codex_home),
        pid_file: state_dir.join(PID_FILE_NAME),
        update_pid_file: state_dir.join(UPDATE_PID_FILE_NAME),
        operation_lock_file: state_dir.join(OPERATION_LOCK_FILE_NAME),
        settings_file: state_dir.join(SETTINGS_FILE_NAME),
        managed_codex_bin: codex_bin,
    };
    daemon.run(LifecycleCommand::Start).await?;
    Ok(socket_path)
}

#[cfg(test)]
#[path = "workflow_backend_tests.rs"]
mod tests;
