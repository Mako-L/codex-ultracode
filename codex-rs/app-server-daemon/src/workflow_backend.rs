//! A workflow backend uses the launching fork without modifying the managed stock install.
use super::*;
use sha2::Digest;
use sha2::Sha256;
use std::io::Read;

/// Keep each executable's backend and supervisor separate, including across upgrades.
pub fn workflow_backend_state_dir(codex_home: &Path, codex_bin: &Path) -> Result<PathBuf> {
    let codex_bin = codex_bin.canonicalize()?;
    let mut digest = Sha256::new();
    digest.update(codex_bin.as_os_str().as_encoded_bytes());
    digest.update([0]);
    let mut binary = std::fs::File::open(&codex_bin)?;
    let mut buffer = [0_u8; 65536];
    loop {
        let count = binary.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let identity = format!("{:x}", digest.finalize());
    let home = codex_home.canonicalize()?;
    let directory = home.join("ultracode-daemon").join(&identity[..16]);
    #[cfg(unix)]
    if directory
        .join("control.sock")
        .as_os_str()
        .as_encoded_bytes()
        .len()
        > 100
    {
        use std::os::unix::fs::DirBuilderExt;
        use std::os::unix::fs::MetadataExt;

        // macOS Unix sockets have only 104 bytes for the entire pathname.
        // Keep durable runs in CODEX_HOME; only daemon transport state lives here.
        let uid = unsafe { libc::geteuid() };
        let mut digest = Sha256::new();
        digest.update(home.as_os_str().as_encoded_bytes());
        digest.update([0]);
        digest.update(identity.as_bytes());
        let key = format!("{:x}", digest.finalize());
        let short = PathBuf::from(format!("/tmp/codex-workflow-{uid}-{}", &key[..32]));
        match std::fs::DirBuilder::new().mode(0o700).create(&short) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let metadata = std::fs::symlink_metadata(&short)?;
        anyhow::ensure!(
            metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o077 == 0,
            "workflow socket directory must be a private directory owned by the current user"
        );
        return Ok(short);
    }
    Ok(directory)
}

/// Resolve the workflow backend's control socket separately from the stock daemon.
pub fn workflow_backend_socket_path(codex_home: &Path) -> Result<PathBuf> {
    Ok(workflow_backend_state_dir(codex_home, &std::env::current_exe()?)?.join("control.sock"))
}

/// Start or reuse the fork-owned daemon using the existing process locks and PID checks.
/// This route never starts the stock installation's updater or uses its process records.
pub async fn ensure_workflow_backend(codex_home: &Path, codex_bin: &Path) -> Result<PathBuf> {
    ensure_supported_platform()?;
    let codex_home = codex_home.canonicalize()?;
    let codex_bin = codex_bin.canonicalize()?;
    let state_dir = workflow_backend_state_dir(&codex_home, &codex_bin)?;
    let socket_path = state_dir.join("control.sock");
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
