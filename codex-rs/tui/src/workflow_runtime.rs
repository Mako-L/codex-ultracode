//! Resolve the workflow engine shipped with this Codex installation.

use codex_install_context::InstallContext;
use std::io;
use std::path::Path;
use std::path::PathBuf;

pub(crate) const INSTRUCTIONS: &str = include_str!("workflow_instructions.md");

#[cfg(windows)]
const NODE: &str = "node.exe";
#[cfg(not(windows))]
const NODE: &str = "node";

#[derive(Debug)]
pub(crate) struct WorkflowRuntime {
    pub(crate) root: PathBuf,
    pub(crate) script: PathBuf,
    pub(crate) node: PathBuf,
}

impl WorkflowRuntime {
    pub(crate) fn bundled() -> io::Result<Self> {
        Self::for_install(InstallContext::current())
    }

    fn for_install(install: &InstallContext) -> io::Result<Self> {
        // bundled_resource resolves files, not directories.
        let script = install
            .bundled_resource("workflow-runtime/bin/workflow.mjs")
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "Native workflow runtime is missing from this Codex installation",
                )
            })?;
        let root = script
            .as_path()
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid native workflow runtime location",
                )
            })?;
        Self::from_root(root)
    }

    pub(crate) fn from_root(root: &Path) -> io::Result<Self> {
        if !root.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Native workflow runtime root must be absolute",
            ));
        }
        let root = root.canonicalize()?;
        let script = runtime_file(&root, "bin/workflow.mjs")?;
        let node = runtime_file(&root, NODE)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if node.metadata()?.permissions().mode() & 0o111 == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Bundled workflow Node runtime is not executable",
                ));
            }
        }
        Ok(Self { root, script, node })
    }
}

fn runtime_file(root: &Path, relative: &str) -> io::Result<PathBuf> {
    let file = root.join(relative).canonicalize().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("Native workflow runtime is missing {relative}: {error}"),
        )
    })?;
    if !file.starts_with(root) || !file.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Native workflow runtime {relative} must be a file inside the runtime directory"
            ),
        ));
    }
    Ok(file)
}

#[cfg(test)]
#[path = "workflow_runtime_tests.rs"]
mod tests;
