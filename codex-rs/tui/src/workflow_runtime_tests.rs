use super::*;
use codex_install_context::CodexPackageLayout;
use codex_install_context::InstallMethod;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::fs;
use tempfile::TempDir;

fn fixture() -> (TempDir, InstallContext, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().canonicalize().unwrap();
    let resources = package.join("resources");
    let root = resources.join("workflow-runtime");
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::create_dir_all(package.join("bin")).unwrap();
    fs::write(root.join("bin/workflow.mjs"), "// workflow engine").unwrap();
    fs::write(root.join(NODE), "fixture executable").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.join(NODE), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let install = InstallContext {
        method: InstallMethod::Other,
        package_layout: Some(CodexPackageLayout {
            package_dir: AbsolutePathBuf::from_absolute_path(&package).unwrap(),
            bin_dir: AbsolutePathBuf::from_absolute_path(package.join("bin")).unwrap(),
            resources_dir: Some(AbsolutePathBuf::from_absolute_path(resources).unwrap()),
            path_dir: None,
        }),
    };
    (directory, install, root)
}

#[test]
fn packaged_runtime_uses_its_own_engine_and_node() {
    let (_directory, install, root) = fixture();
    let runtime = WorkflowRuntime::for_install(&install).unwrap();
    assert_eq!(runtime.root, root);
    assert_eq!(runtime.script, root.join("bin/workflow.mjs"));
    assert_eq!(runtime.node, root.join(NODE));
}

#[test]
fn missing_package_or_node_fails_instead_of_using_an_external_runtime() {
    let install = InstallContext {
        method: InstallMethod::Other,
        package_layout: None,
    };
    assert_eq!(
        WorkflowRuntime::for_install(&install).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    let (_directory, install, root) = fixture();
    fs::remove_file(root.join(NODE)).unwrap();
    assert_eq!(
        WorkflowRuntime::for_install(&install).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        WorkflowRuntime::from_root(Path::new("relative"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
}

#[cfg(unix)]
#[test]
fn packaged_runtime_rejects_escaping_symlinks_and_non_executable_node() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;
    let (directory, install, root) = fixture();
    fs::set_permissions(root.join(NODE), fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        WorkflowRuntime::for_install(&install).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    fs::remove_file(root.join(NODE)).unwrap();
    let external = directory.path().join("external-node");
    fs::write(&external, "not the bundled runtime").unwrap();
    symlink(external, root.join(NODE)).unwrap();
    assert_eq!(
        WorkflowRuntime::for_install(&install).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
