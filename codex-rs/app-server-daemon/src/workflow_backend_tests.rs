use super::*;
use pretty_assertions::assert_eq;

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
