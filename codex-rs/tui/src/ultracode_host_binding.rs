use super::*;

/// A CLI-selected connection owns one listener, one canonical root, and one parent.
/// The daemon's effective parent configuration proves the listener association;
/// caller-supplied thread IDs and model arguments cannot establish ownership.
pub(super) struct HeadlessBinding {
    pub(super) connection_id: Uuid,
    plugin_root: PathBuf,
    pub(super) listener_url: Mutex<Option<String>>,
    parent_id: tokio::sync::Mutex<Option<String>>,
    closed: std::sync::atomic::AtomicBool,
}

impl HeadlessBinding {
    pub(super) fn new(connection_id: Uuid, plugin_root: &Path) -> Result<Self, BridgeError> {
        if !plugin_root.is_absolute() {
            return Err(BridgeError::host("plugin root must be absolute"));
        }
        Ok(Self {
            connection_id,
            plugin_root: plugin_root
                .canonicalize()
                .map_err(|error| BridgeError::host(error.to_string()))?,
            listener_url: Mutex::new(None),
            parent_id: tokio::sync::Mutex::new(None),
            closed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub(super) async fn authorize(
        &self,
        runtime: &Arc<Runtime>,
        parent_id: &str,
    ) -> Result<(Arc<Parent>, tokio::sync::OwnedMutexGuard<()>), BridgeError> {
        if Uuid::parse_str(parent_id)
            .ok()
            .map(|id| id.to_string())
            .as_deref()
            != Some(parent_id)
        {
            return Err(BridgeError::host(
                "parent thread ID must be a canonical UUID",
            ));
        }
        let mut bound = self.parent_id.lock().await;
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(BridgeError::host("headless connection is closed"));
        }
        if bound.as_deref().is_some_and(|owned| owned != parent_id) {
            return Err(BridgeError::host(
                "headless connection belongs to another parent",
            ));
        }
        if bound.is_none() {
            let expected = self
                .listener_url
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| BridgeError::host("headless listener is not configured"))?;
            let authority: WorkflowAuthorityCaptureResponse = runtime
                .handle
                .request_typed(ClientRequest::WorkflowAuthorityCapture {
                    request_id: request_id(),
                    params: WorkflowAuthorityCaptureParams {
                        parent_thread_id: parent_id.to_string(),
                        allow_isolated_workspaces: false,
                    },
                })
                .await
                .map_err(|error| BridgeError::host(error.to_string()))?;
            if authority.workflow_host_url.as_deref() != Some(expected.as_str()) {
                return Err(BridgeError::host(
                    "parent does not belong to this headless listener",
                ));
            }
        }
        let parent = runtime.parent(parent_id, &self.plugin_root).await?;
        let access = parent.frontend_access.clone().lock_owned().await;
        {
            let attachment = parent.attachment.lock().unwrap();
            if self.closed.load(std::sync::atomic::Ordering::Acquire) {
                return Err(BridgeError::host("headless connection is closed"));
            }
            if attachment.is_some() {
                return Err(BridgeError::host("parent has an interactive frontend"));
            }
            let mut owners = runtime.headless_owners.lock().unwrap();
            if owners
                .get(parent_id)
                .is_some_and(|owner| *owner != self.connection_id)
            {
                return Err(BridgeError::host(
                    "parent belongs to another headless connection",
                ));
            }
            if bound.is_some() && owners.get(parent_id) != Some(&self.connection_id) {
                return Err(BridgeError::host(
                    "headless connection no longer owns parent",
                ));
            }
            owners.insert(parent_id.to_string(), self.connection_id);
            runtime
                .headless_parents
                .lock()
                .unwrap()
                .insert(parent_id.to_string());
        }
        *bound = Some(parent_id.to_string());
        Ok((parent, access))
    }

    pub(super) async fn release(&self, runtime: &Runtime) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(parent_id) = self.parent_id.lock().await.as_ref() {
            let mut owners = runtime.headless_owners.lock().unwrap();
            if owners.get(parent_id) == Some(&self.connection_id) {
                owners.remove(parent_id);
            }
        }
    }
}
