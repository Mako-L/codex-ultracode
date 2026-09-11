//! Durable consent decisions for native workflow launches.

use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConsentData {
    auto_first_launch_approved: bool,
    named: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug)]
pub(crate) struct WorkflowConsentStore {
    path: PathBuf,
    data: ConsentData,
}

impl WorkflowConsentStore {
    pub(crate) fn load(codex_home: &Path) -> io::Result<Self> {
        let path = codex_home.join("workflow-consent.json");
        let data = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid workflow consent: {error}"),
                )
            })?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => ConsentData::default(),
            Err(error) => return Err(error),
        };
        Ok(Self { path, data })
    }

    pub(crate) fn allows_auto_first_launch(&self) -> bool {
        self.data.auto_first_launch_approved
    }

    pub(crate) fn allows_named(
        &self,
        project: &Path,
        name: &str,
        source_digest: &str,
    ) -> io::Result<bool> {
        let project = canonical_project(project)?;
        Ok(self
            .data
            .named
            .get(&project)
            .and_then(|workflows| workflows.get(name))
            .is_some_and(|digest| digest == source_digest))
    }

    pub(crate) fn remember_auto_first_launch(&mut self) -> io::Result<()> {
        let previous = self.data.clone();
        self.data.auto_first_launch_approved = true;
        if let Err(error) = self.persist() {
            self.data = previous;
            return Err(error);
        }
        Ok(())
    }

    /// Remember consent only for a named saved, bundled, or plugin workflow.
    /// Inline scripts have no canonical name and must not call this method.
    pub(crate) fn remember_named(
        &mut self,
        project: &Path,
        name: &str,
        source_digest: &str,
    ) -> io::Result<()> {
        if name.is_empty() || source_digest.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "named workflow consent requires a name and source digest",
            ));
        }
        let previous = self.data.clone();
        self.data
            .named
            .entry(canonical_project(project)?)
            .or_default()
            .insert(name.to_string(), source_digest.to_string());
        if let Err(error) = self.persist() {
            self.data = previous;
            return Err(error);
        }
        Ok(())
    }

    fn persist(&self) -> io::Result<()> {
        let json = serde_json::to_string_pretty(&self.data)
            .map_err(|error| io::Error::other(format!("serialize workflow consent: {error}")))?;
        codex_utils_path::write_atomically(&self.path, &json)
    }
}

fn canonical_project(project: &Path) -> io::Result<String> {
    Ok(std::fs::canonicalize(project)?
        .to_string_lossy()
        .into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn consent_is_project_scoped_digest_bound_and_persistent() -> io::Result<()> {
        let home = tempdir()?;
        let first = tempdir()?;
        let second = tempdir()?;
        let mut store = WorkflowConsentStore::load(home.path())?;
        assert!(!store.allows_auto_first_launch());
        assert!(!store.allows_named(first.path(), "plugin:review", "digest-1")?);
        store.remember_auto_first_launch()?;
        store.remember_named(first.path(), "plugin:review", "digest-1")?;

        let restarted = WorkflowConsentStore::load(home.path())?;
        assert!(restarted.allows_auto_first_launch());
        assert!(restarted.allows_named(first.path(), "plugin:review", "digest-1")?);
        assert!(!restarted.allows_named(first.path(), "plugin:review", "digest-2")?);
        assert!(!restarted.allows_named(second.path(), "plugin:review", "digest-1")?);
        Ok(())
    }

    #[test]
    fn inline_and_corrupt_consent_fail_closed() -> io::Result<()> {
        let home = tempdir()?;
        let project = tempdir()?;
        let mut store = WorkflowConsentStore::load(home.path())?;
        assert!(store.remember_named(project.path(), "", "digest").is_err());
        assert!(store.remember_named(project.path(), "inline", "").is_err());
        std::fs::write(home.path().join("workflow-consent.json"), b"not json")?;
        assert_eq!(
            WorkflowConsentStore::load(home.path()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        Ok(())
    }

    #[test]
    fn failed_persistence_does_not_approve_in_memory() -> io::Result<()> {
        let home = tempdir()?;
        let project = tempdir()?;
        let mut store = WorkflowConsentStore::load(home.path())?;
        store.path = home.path().to_path_buf();
        assert!(store.remember_auto_first_launch().is_err());
        assert!(!store.allows_auto_first_launch());
        assert!(
            store
                .remember_named(project.path(), "review", "digest")
                .is_err()
        );
        assert!(!store.allows_named(project.path(), "review", "digest")?);
        Ok(())
    }
}
