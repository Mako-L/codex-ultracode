use super::*;
use pretty_assertions::assert_eq;

#[test]
fn workflow_backend_isolates_upgrades_and_preserves_legacy_sockets() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let binary = home.join("codex");
    std::fs::write(&binary, b"old executable").unwrap();
    let old = workflow_backend_state_dir(home, &binary).unwrap();
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("control.sock"), b"old daemon").unwrap();
    std::fs::write(old.join("host.sock"), b"old supervisor").unwrap();
    let legacy = home.join("ultracode-daemon/control.sock");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, b"legacy daemon").unwrap();
    assert_eq!(workflow_backend_state_dir(home, &binary).unwrap(), old);

    std::fs::write(&binary, b"new executable").unwrap();
    let new = workflow_backend_state_dir(home, &binary).unwrap();
    assert_ne!(old, new);
    assert!(!new.join("control.sock").exists());
    assert!(!new.join("host.sock").exists());
    assert_eq!(
        std::fs::read(old.join("control.sock")).unwrap(),
        b"old daemon"
    );
    assert_eq!(
        std::fs::read(old.join("host.sock")).unwrap(),
        b"old supervisor"
    );
    assert_eq!(std::fs::read(legacy).unwrap(), b"legacy daemon");

    let other_install = home.join("other-codex");
    std::fs::copy(&binary, &other_install).unwrap();
    assert_ne!(
        new,
        workflow_backend_state_dir(home, &other_install).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn long_account_home_uses_private_short_bindable_sockets() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("account-switcher".repeat(12));
    std::fs::create_dir_all(&home).unwrap();
    let binary = home.join("codex");
    std::fs::write(&binary, b"test executable").unwrap();
    let state = workflow_backend_state_dir(&home, &binary).unwrap();
    assert_eq!(workflow_backend_state_dir(&home, &binary).unwrap(), state);
    assert!(
        state
            .join("control.sock")
            .as_os_str()
            .as_encoded_bytes()
            .len()
            <= 100
    );
    assert_eq!(
        std::fs::metadata(&state).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let control = UnixListener::bind(state.join("control.sock")).unwrap();
    let host = UnixListener::bind(state.join("host.sock")).unwrap();
    let other_home = directory.path().join("other-account".repeat(12));
    std::fs::create_dir_all(&other_home).unwrap();
    let other = workflow_backend_state_dir(&other_home, &binary).unwrap();
    assert_ne!(state, other);
    drop((control, host));
    std::fs::remove_dir_all(&state).unwrap();
    std::os::unix::fs::symlink(&home, &state).unwrap();
    assert!(workflow_backend_state_dir(&home, &binary).is_err());
    std::fs::remove_file(&state).unwrap();
    std::fs::remove_dir(&other).unwrap();
}

#[tokio::test]
async fn workflow_backend_uses_current_fork_and_preserves_stock_install() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let stock_pid = home.join("app-server-daemon/app-server.pid");
    let stock_install = home.join("packages/standalone/current/codex");
    std::fs::create_dir_all(stock_pid.parent().unwrap()).unwrap();
    std::fs::create_dir_all(stock_install.parent().unwrap()).unwrap();
    std::fs::write(&stock_pid, "stock process record").unwrap();
    std::fs::write(&stock_install, "stock executable").unwrap();
    let error = ensure_workflow_backend(home, &home.join("missing-fork"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("No such file"));
    assert_eq!(
        std::fs::read_to_string(stock_pid).unwrap(),
        "stock process record"
    );
    assert_eq!(
        std::fs::read_to_string(stock_install).unwrap(),
        "stock executable"
    );
    assert_ne!(
        workflow_backend_socket_path(home).unwrap(),
        app_server_control_socket_path(home).unwrap().as_path()
    );
}
