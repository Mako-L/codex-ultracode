//! Detached owner of native workflow bridges and their daemon connection.
//!
//! Each run attempt reserves a durable completion record before daemon submission.
//! Reattachment never resubmits that completion. A lost daemon acknowledgement leaves
//! the record in `submitting` with an unresolved outcome; it does not establish
//! crash-safe exactly-once delivery because the native completion RPC is not idempotent.
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::dynamic_tools_mcp::DynamicToolMcpServer;
use crate::dynamic_tools_mcp::WorkflowMcpHandler;
use crate::ultracode_bridge::BridgeError;
use crate::ultracode_bridge::BridgeEvent;
use crate::ultracode_bridge::BridgeLaunch;
use crate::ultracode_bridge::UltracodeBridge;
use codex_app_server_client::AppServerClient;
use codex_app_server_client::AppServerEvent;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::*;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::collections::HashSet;
use std::future::Future;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::result::Result;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use uuid::Uuid;

#[path = "ultracode_host_binding.rs"]
mod binding;
#[path = "ultracode_host_headless.rs"]
mod headless;
#[path = "ultracode_host_socket.rs"]
mod socket;
#[cfg(test)]
#[path = "ultracode_host_socket_tests.rs"]
mod socket_tests;
#[path = "ultracode_host_workers.rs"]
mod workers;
use binding::HeadlessBinding;

pub(crate) use socket::attach;
pub(crate) use socket::connect;
pub use socket::run;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
type Reply = mpsc::UnboundedSender<Value>;
pub(super) enum Resolution {
    Accept(RequestId, Value),
    Reject(RequestId, JSONRPCErrorError),
}

pub(super) struct Parent {
    bridge: UltracodeBridge,
    attachment: Mutex<Option<(Uuid, Reply)>>,
    directory: PathBuf,
    plugin_root: PathBuf,
    completions: tokio::sync::Mutex<()>,
    frontend_access: Arc<tokio::sync::Mutex<()>>,
}

pub(super) struct Runtime {
    home: PathBuf,
    handle: AppServerRequestHandle,
    parents: tokio::sync::Mutex<HashMap<String, Arc<Parent>>>,
    workers: Mutex<workers::Workers>,
    pending: Mutex<HashMap<String, PendingConsent>>,
    approvals: Mutex<HashMap<String, Value>>,
    resolve: mpsc::UnboundedSender<Resolution>,
    status: broadcast::Sender<ThreadStatusChangedNotification>,
    mcp: tokio::sync::Mutex<HashMap<String, DynamicToolMcpServer>>,
    headless_parents: Mutex<HashSet<String>>,
    headless_runs: Mutex<HashSet<(String, String)>>,
    headless_owners: Mutex<HashMap<String, Uuid>>,
}

pub(super) struct PendingConsent {
    parent_id: String,
    connection_id: Uuid,
    response: oneshot::Sender<Value>,
}

struct WorkflowFrontend {
    runtime: Arc<Runtime>,
    plugin_root: PathBuf,
    headless: Option<Arc<HeadlessBinding>>,
}

fn request_id() -> RequestId {
    RequestId::String(format!("workflow-host:{}", Uuid::new_v4()))
}

impl Runtime {
    fn headless_run_marker(parent: &Parent, run_id: &str) -> Result<PathBuf, BridgeError> {
        let run_id =
            Uuid::parse_str(run_id).map_err(|error| BridgeError::host(error.to_string()))?;
        Ok(parent.directory.join(format!("headless-run-{run_id}.json")))
    }

    fn remember_headless_run(
        parent: &Parent,
        parent_id: &str,
        run_id: &str,
    ) -> Result<(), BridgeError> {
        let marker = Self::headless_run_marker(parent, run_id)?;
        match socket::write_private_new(
            &marker,
            &json!({"parentThreadId":parent_id,"runId":run_id}),
        ) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let value = socket::read_private(&marker)
                    .map_err(|error| BridgeError::host(error.to_string()))?;
                if value == json!({"parentThreadId":parent_id,"runId":run_id}) {
                    Ok(())
                } else {
                    Err(BridgeError::host("headless run marker identity is stale"))
                }
            }
            Err(error) => Err(BridgeError::host(error.to_string())),
        }
    }

    fn owns_headless_run(
        parent: &Parent,
        parent_id: &str,
        run_id: &str,
    ) -> Result<bool, BridgeError> {
        let marker = Self::headless_run_marker(parent, run_id)?;
        match socket::read_private(&marker) {
            Ok(value) if value == json!({"parentThreadId":parent_id,"runId":run_id}) => Ok(true),
            Ok(_) => Err(BridgeError::host("headless run marker identity is stale")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(BridgeError::host(error.to_string())),
        }
    }

    async fn parent(
        self: &Arc<Self>,
        parent_id: &str,
        plugin_root: &Path,
    ) -> Result<Arc<Parent>, BridgeError> {
        let canonical = Uuid::parse_str(parent_id)
            .map_err(|error| BridgeError::host(error.to_string()))?
            .to_string();
        if canonical != parent_id {
            return Err(BridgeError::host(
                "parent thread ID must be a canonical UUID",
            ));
        }
        let runtime = crate::workflow_runtime::WorkflowRuntime::from_root(plugin_root)
            .map_err(|error| BridgeError::host(error.to_string()))?;
        let root = runtime.root;
        let mut parents = self.parents.lock().await;
        if let Some(parent) = parents.get(parent_id) {
            if parent.plugin_root != root {
                return Err(BridgeError::host(
                    "parent workflow runtime belongs to a different bundled workflow runtime",
                ));
            }
            return Ok(parent.clone());
        }
        let authority: WorkflowAuthorityCaptureResponse = self
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
        let directory = self.home.join("ultracode/sessions").join(parent_id);
        std::fs::create_dir_all(&directory)
            .map_err(|error| BridgeError::host(error.to_string()))?;
        let bridge = UltracodeBridge::spawn(BridgeLaunch {
            node: runtime.node,
            script: runtime.script,
            plugin_root: root.clone(),
            cwd: authority.cwd.into(),
            state_dir: directory.clone(),
            models: serde_json::to_value(authority.models)
                .map_err(|error| BridgeError::host(error.to_string()))?,
            plugins: serde_json::to_value(authority.plugins)
                .map_err(|error| BridgeError::host(error.to_string()))?,
            web_search_available: authority.web_search_available,
        })
        .await?;
        let parent = Arc::new(Parent {
            bridge,
            attachment: Mutex::new(None),
            directory,
            plugin_root: root,
            completions: tokio::sync::Mutex::new(()),
            frontend_access: Arc::new(tokio::sync::Mutex::new(())),
        });
        parents.insert(parent_id.to_string(), parent.clone());
        self.listen(parent_id, &parent)?;
        Ok(parent)
    }

    fn listen(self: &Arc<Self>, parent_id: &str, parent: &Arc<Parent>) -> Result<(), BridgeError> {
        let mut events = parent
            .bridge
            .take_events()
            .ok_or_else(|| BridgeError::host("bridge events unavailable"))?;
        let runtime = self.clone();
        let parent_id = parent_id.to_string();
        let owner = parent.clone();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                let runtime = runtime.clone();
                let parent_id = parent_id.clone();
                let parent = owner.clone();
                tokio::spawn(async move {
                    match event {
                        BridgeEvent::Request { id, method, params } => {
                            if method == "worker.start" {
                                runtime.start_worker(&parent_id, &parent, id, params).await;
                            } else {
                                let result = runtime.host_request(&method, params).await;
                                let _ = parent.bridge.respond(&id, result);
                            }
                        }
                        BridgeEvent::RunChanged { run_id, revision } => {
                            if let Some((_, sender)) = parent.attachment.lock().unwrap().as_ref() {
                                let _ = sender.send(json!({"event":"runChanged","runId":run_id,"revision":revision}));
                            }
                            if let Err(error) = runtime.complete(&parent_id, &parent, &run_id).await
                            {
                                tracing::warn!(%error, %run_id, "workflow completion remains unresolved");
                            }
                        }
                    }
                });
            }
        });
        Ok(())
    }

    async fn host_request(&self, method: &str, params: Value) -> Result<Value, BridgeError> {
        if method == "worker.interrupt" {
            return crate::workflow_worker_interrupt::interrupt(
                self.handle.clone(),
                serde_json::from_value(params)
                    .map_err(|error| BridgeError::host(error.to_string()))?,
            )
            .await;
        }
        let id = request_id();
        let request = match method {
            "workspace.prepare" => ClientRequest::WorkflowWorkspacePrepare {
                request_id: id,
                params: serde_json::from_value(params)
                    .map_err(|e| BridgeError::host(format!("{e}")))?,
            },
            "workspace.release" => ClientRequest::WorkflowWorkspaceRelease {
                request_id: id,
                params: serde_json::from_value(params)
                    .map_err(|e| BridgeError::host(format!("{e}")))?,
            },
            _ => {
                return Err(BridgeError::host(format!(
                    "unknown native host request: {method}"
                )));
            }
        };
        self.handle
            .request_typed(request)
            .await
            .map_err(|error| BridgeError::host(error.to_string()))
    }

    async fn complete(
        &self,
        parent_id: &str,
        parent: &Parent,
        run_id: &str,
    ) -> Result<(), BridgeError> {
        let _guard = parent.completions.lock().await;
        let state = parent.bridge.inspect_run(run_id).await?;
        let status = state["status"].as_str().unwrap_or_default();
        if !["completed", "failed", "stopped", "interrupted"].contains(&status) {
            return Ok(());
        }
        let attempt = state["attempt"].as_u64().unwrap_or(1);
        // Reserve before submission: a lost daemon acknowledgement must never replay a parent turn.
        let run_uuid =
            Uuid::parse_str(run_id).map_err(|error| BridgeError::host(error.to_string()))?;
        let ledger = parent
            .directory
            .join(format!("completion-{run_uuid}-{attempt}.json"));
        match socket::write_private_new(
            &ledger,
            &json!({"runId":run_id,"attempt":attempt,"status":"submitting"}),
        ) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
            Err(error) => return Err(BridgeError::host(error.to_string())),
        }
        let body = state
            .get("result")
            .or_else(|| state.get("error"))
            .cloned()
            .unwrap_or(Value::Null);
        let summary = format!(
            "Ultracode workflow {run_id} attempt {attempt} finished with status {status}. scriptPath={} transcriptDir={} Consolidated result: {body}",
            state["scriptPath"], state["transcriptDir"]
        );
        let completion = self
            .handle
            .request_typed::<WorkflowCompletionInjectResponse>(
                ClientRequest::WorkflowCompletionInject {
                    request_id: request_id(),
                    params: WorkflowCompletionInjectParams {
                        parent_thread_id: parent_id.to_string(),
                        run_id: run_id.to_string(),
                        summary: summary.chars().take(8_000).collect(),
                    },
                },
            )
            .await;
        let completion = match completion {
            Ok(completion) => completion,
            Err(error) => {
                socket::replace_private(&ledger,&json!({"runId":run_id,"attempt":attempt,"status":"unresolved","error":error.to_string()})).map_err(|error| BridgeError::host(error.to_string()))?;
                return Err(BridgeError::host(error.to_string()));
            }
        };
        socket::replace_private(&ledger, &json!({"runId":run_id,"attempt":attempt,"status":"delivered","turnId":completion.turn_id})).map_err(|error| BridgeError::host(error.to_string()))?;
        match std::fs::remove_file(Self::headless_run_marker(parent, run_id)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(BridgeError::host(error.to_string())),
        }
    }

    async fn configure(
        self: &Arc<Self>,
        mut params: Value,
        headless: Option<Arc<HeadlessBinding>>,
    ) -> Result<Value, BridgeError> {
        let root = params["pluginRoot"]
            .as_str()
            .ok_or_else(|| BridgeError::host("frontend bundled workflow runtime is unavailable"))?;
        if !Path::new(root).is_absolute() {
            return Err(BridgeError::host(
                "bundled workflow runtime root must be absolute",
            ));
        }
        let plugin_root = Path::new(root)
            .canonicalize()
            .map_err(|error| BridgeError::host(error.to_string()))?;
        params["pluginRoot"] = json!(plugin_root);
        // Each complete frontend template owns a listener so an existing parent's tools keep
        // their configuration while another project or frontend connects to this supervisor.
        if let Some(binding) = &headless {
            params["connectionId"] = json!(binding.connection_id);
        }
        let template =
            serde_json::to_string(&params).map_err(|error| BridgeError::host(error.to_string()))?;
        let mut mcp = self.mcp.lock().await;
        if let Some(server) = mcp.get(&template) {
            return Ok(server.configuration());
        }
        let (events, mut receiver) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                if let AppEvent::DynamicToolThreadStarted { registered, .. } = event {
                    let _ = registered.send(());
                }
            }
        });
        // Interactive workflow consent travels over the native dynamic-tool channel.
        // Only explicitly headless parents launch workflows through MCP.
        let workflows_enabled = headless.is_some()
            && !params["threadStartParams"]
                .get("config")
                .and_then(|config| config.get("disable_workflows"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
        match params.get("frontend").and_then(Value::as_str) {
            None | Some("interactive") if headless.is_none() => {}
            Some("headless") if headless.is_some() => {}
            _ => {
                return Err(BridgeError::host(
                    "invalid native workflow frontend binding",
                ));
            }
        }
        let server = DynamicToolMcpServer::start(
            self.handle.clone(),
            serde_json::from_value(params["threadStartParams"].clone())
                .map_err(|error| BridgeError::host(error.to_string()))?,
            AppEventSender::new(events),
            self.status.clone(),
            None,
            Some(Arc::new(WorkflowFrontend {
                runtime: self.clone(),
                plugin_root,
                headless: headless.clone(),
            })),
            workflows_enabled,
        )
        .await
        .map_err(|error| BridgeError::host(error.to_string()))?;
        let config = server.configuration();
        if let Some(binding) = &headless {
            *binding.listener_url.lock().unwrap() = Some(
                config["url"]
                    .as_str()
                    .ok_or_else(|| BridgeError::host("headless listener URL is unavailable"))?
                    .to_string(),
            );
        }
        mcp.insert(template, server);
        Ok(config)
    }
}

impl WorkflowMcpHandler for WorkflowFrontend {
    fn call(
        &self,
        params: DynamicToolCallParams,
    ) -> Pin<Box<dyn Future<Output = DynamicToolCallResponse> + Send>> {
        let runtime = self.runtime.clone();
        let plugin_root = self.plugin_root.clone();
        let headless = self.headless.clone();
        Box::pin(async move {
            if let Some(binding) = headless {
                let result = async {
                    let (parent, _access) = binding.authorize(&runtime, &params.thread_id).await?;
                    let authority: WorkflowAuthorityCaptureResponse = runtime
                        .handle
                        .request_typed(ClientRequest::WorkflowAuthorityCapture {
                            request_id: request_id(),
                            params: WorkflowAuthorityCaptureParams {
                                parent_thread_id: params.thread_id.clone(),
                                allow_isolated_workspaces: true,
                            },
                        })
                        .await
                        .map_err(|error| BridgeError::host(error.to_string()))?;
                    let result = crate::ultracode_launch::launch(
                        &runtime.handle,
                        &params.thread_id,
                        &authority,
                        &parent.bridge,
                        &params.arguments,
                    )
                    .await?
                    .result?;
                    if let Some(run_id) = result["runId"].as_str() {
                        Runtime::remember_headless_run(&parent, &params.thread_id, run_id)?;
                        runtime
                            .headless_runs
                            .lock()
                            .unwrap()
                            .insert((params.thread_id.clone(), run_id.to_string()));
                    }
                    Ok(result)
                }
                .await;
                return crate::ultracode_launch::response(result);
            }

            // Contention while another parent starts is not evidence that this frontend detached.
            let parent = runtime.parents.lock().await.get(&params.thread_id).cloned();
            let Some(parent) = parent.filter(|parent| parent.plugin_root == plugin_root) else {
                return crate::dynamic_tools::failure_response(
                    "Workflow launch requires an attached native frontend using the same bundled workflow runtime for consent.",
                );
            };
            let receiver = {
                let attachment = parent.attachment.lock().unwrap();
                let Some((connection_id, delivery)) = attachment.as_ref() else {
                    return crate::dynamic_tools::failure_response(
                        "Workflow launch requires an attached native frontend for consent.",
                    );
                };
                let id = format!("workflow-supervisor:{}", Uuid::new_v4());
                let (response, receiver) = oneshot::channel();
                runtime.pending.lock().unwrap().insert(
                    id.clone(),
                    PendingConsent {
                        parent_id: params.thread_id.clone(),
                        connection_id: *connection_id,
                        response,
                    },
                );
                if delivery
                    .send(json!({"id":id,"method":"supervisor.workflow","params":params}))
                    .is_err()
                {
                    runtime.pending.lock().unwrap().remove(&id);
                }
                receiver
            };
            match receiver.await {
                Ok(value) => serde_json::from_value(value).unwrap_or_else(|error| {
                    crate::dynamic_tools::failure_response(error.to_string())
                }),
                Err(_) => crate::dynamic_tools::failure_response(
                    "Workflow consent frontend disconnected; launch was not authorized.",
                ),
            }
        })
    }
}
