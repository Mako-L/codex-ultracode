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
