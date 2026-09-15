use crate::workflow_bridge::BridgeError;
use crate::workflow_bridge::WorkflowBridge;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::WorkflowAuthorityCaptureResponse;
use codex_app_server_protocol::WorkflowScriptReadParams;
use codex_app_server_protocol::WorkflowScriptReadResponse;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use std::time::Duration;
use uuid::Uuid;

#[cfg(test)]
#[path = "workflow_source_tests.rs"]
mod tests;

/// Source bytes and identity selected for one originating thread's consent.
/// This is local UI state, never a dynamic-tool argument or model-history item.
#[derive(Clone, Debug)]
pub(crate) struct WorkflowSourcePreview {
    pub(crate) thread_id: String,
    pub(crate) source: String,
    pub(crate) digest: String,
    pub(crate) workflow_id: Option<String>,
    pub(crate) metadata: Option<WorkflowMetadata>,
    pub(crate) consent: Option<WorkflowConsentPresentation>,
    pub(crate) validation_error: Option<String>,
    resolved_path: Option<String>,
    resume_run_id: Option<String>,
    saved_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct WorkflowConsentPresentation {
    pub(crate) phases: Option<Vec<WorkflowConsentPhase>>,
    pub(crate) args: Option<WorkflowConsentArgs>,
    pub(crate) source: WorkflowConsentSource,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct WorkflowConsentPhase {
    pub(crate) title: String,
    pub(crate) detail: Option<String>,
    pub(crate) prompts: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkflowConsentArgs {
    pub(crate) text: String,
    pub(crate) needs_gutter: bool,
    pub(crate) withheld: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkflowConsentSource {
    pub(crate) text: String,
    pub(crate) withheld: bool,
    pub(crate) original_length: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct WorkflowMetadata {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) title: Option<String>,
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) phases: Vec<WorkflowPhase>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub(crate) enum WorkflowPhase {
    Name(String),
    Detailed {
        title: String,
        detail: Option<String>,
    },
}

#[derive(Clone, Copy)]
pub(crate) enum SourceLocation<'a> {
    Saved(&'a str),
    File(&'a str),
    Inline(&'a str),
    Resumed(&'a str),
}

pub(crate) fn source_location(arguments: &Value) -> Result<SourceLocation<'_>, BridgeError> {
    if arguments.get("resumeFromRunId").is_none()
        && let Some(name) = arguments.get("name").and_then(Value::as_str)
    {
        return Ok(SourceLocation::Saved(name));
    }
    if let Some(path) = arguments.get("scriptPath").and_then(Value::as_str) {
        return Ok(SourceLocation::File(path));
    }
    if let Some(source) = arguments.get("script").and_then(Value::as_str) {
        return Ok(SourceLocation::Inline(source));
    }
    if let Some(run) = arguments.get("resumeFromRunId").and_then(Value::as_str) {
        return Ok(SourceLocation::Resumed(run));
    }
    Err(BridgeError::host("Workflow source is unavailable"))
}

impl WorkflowSourcePreview {
    /// Bind an editor result to the bytes that the next consent decision approves.
    /// Edited saved/file workflows become inline launches; they must never resolve
    /// the old location again or inherit its remembered permission identity.
    pub(crate) fn with_edited_source(
        &self,
        arguments: &Value,
        source: String,
    ) -> Result<(Value, Self), BridgeError> {
        if source == self.source {
            return Ok((arguments.clone(), self.clone()));
        }
        let mut arguments = arguments.clone();
        let object = arguments
            .as_object_mut()
            .ok_or_else(|| BridgeError::host("Workflow arguments must be an object"))?;
        object.remove("name");
        object.remove("scriptPath");
        object.insert("script".into(), Value::String(source));
        let preview = Self::inline(&self.thread_id, &arguments)
            .ok_or_else(|| BridgeError::host("Edited workflow source is unavailable"))?;
        Ok((arguments, preview))
    }

    pub(crate) fn inline(thread_id: &str, arguments: &Value) -> Option<Self> {
        let SourceLocation::Inline(source) = source_location(arguments).ok()? else {
            return None;
        };
        Some(Self {
            thread_id: thread_id.to_owned(),
            source: source.to_owned(),
            digest: format!("{:x}", Sha256::digest(source.as_bytes())),
            workflow_id: None,
            metadata: None,
            consent: None,
            validation_error: None,
            resolved_path: None,
            resume_run_id: arguments
                .get("resumeFromRunId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            saved_name: None,
        })
    }
}

pub(crate) async fn read_file(
    handle: &AppServerRequestHandle,
    thread_id: &str,
    authority: &WorkflowAuthorityCaptureResponse,
    path: &str,
) -> Result<WorkflowScriptReadResponse, BridgeError> {
    handle
        .request_typed(ClientRequest::WorkflowScriptRead {
            request_id: RequestId::String(format!("workflow-script:{}", Uuid::new_v4())),
            params: WorkflowScriptReadParams {
                parent_thread_id: thread_id.to_owned(),
                authority_ref: authority.authority_ref.clone(),
                authority_digest: authority.authority_digest.clone(),
                script_path: path.to_owned(),
            },
        })
        .await
        .map_err(|error| BridgeError::host(error.to_string()))
}

pub(crate) async fn read_preview(
    handle: &AppServerRequestHandle,
    thread_id: &str,
    authority: &WorkflowAuthorityCaptureResponse,
    bridge: &WorkflowBridge,
    arguments: &Value,
    selected: Option<&WorkflowSourcePreview>,
) -> Result<WorkflowSourcePreview, BridgeError> {
    if selected.is_some_and(|preview| preview.thread_id != thread_id) {
        return Err(BridgeError::host(
            "Workflow preview belongs to another parent",
        ));
    }
    let preview = if let Some(inline) = WorkflowSourcePreview::inline(thread_id, arguments) {
        inline
    } else {
        let (source, expected_digest, workflow_id, resolved_path) =
            match source_location(arguments)? {
                SourceLocation::File(path) => {
                    let file = read_file(handle, thread_id, authority, path).await?;
                    (
                        file.source,
                        file.source_digest,
                        None,
                        Some(file.resolved_path),
                    )
                }
                SourceLocation::Saved(name) => {
                    let workflow_id = if let Some(selected) = selected {
                        selected
                            .workflow_id
                            .clone()
                            .ok_or_else(|| BridgeError::host("Workflow preview identity changed"))?
                    } else {
                        let catalog = bridge
                            .request(
                                "listSavedWorkflows",
                                json!({"cwd":authority.cwd}),
                                Duration::from_secs(30),
                            )
                            .await?;
                        catalog["workflows"]
                            .as_array()
                            .and_then(|items| items.iter().find(|item| item["name"] == name))
                            .and_then(|item| item["workflowId"].as_str())
                            .map(str::to_owned)
                            .ok_or_else(|| BridgeError::host("Saved workflow not found"))?
                    };
                    let saved = bridge
                        .request(
                            "readSavedSource",
                            json!({"workflowId":workflow_id}),
                            Duration::from_secs(30),
                        )
                        .await?;
                    let source = saved["source"]
                        .as_str()
                        .ok_or_else(|| BridgeError::host("Saved workflow source is unavailable"))?
                        .to_owned();
                    let digest = saved["digest"]
                        .as_str()
                        .ok_or_else(|| BridgeError::host("Saved workflow digest is unavailable"))?
                        .to_owned();
                    if saved["workflowId"] != workflow_id {
                        return Err(BridgeError::host("Saved workflow identity changed"));
                    }
                    (source, digest, Some(workflow_id), None)
                }
                SourceLocation::Resumed(run_id) => {
                    let run = bridge.inspect_run(run_id).await?;
                    if run["id"] != run_id {
                        return Err(BridgeError::host("Workflow resume identity changed"));
                    }
                    let source = run["source"]
                        .as_str()
                        .ok_or_else(|| BridgeError::host("Resumed workflow source is unavailable"))?
                        .to_owned();
                    let digest = run["sourceDigest"]
                        .as_str()
                        .ok_or_else(|| BridgeError::host("Resumed workflow digest is unavailable"))?
                        .to_owned();
                    (source, digest, None, None)
                }
                SourceLocation::Inline(_) => unreachable!("inline source handled above"),
            };
        let digest = format!("{:x}", Sha256::digest(source.as_bytes()));
        if digest != expected_digest {
            return Err(BridgeError::host(
                "Workflow source digest does not match its identity",
            ));
        }
        WorkflowSourcePreview {
            thread_id: thread_id.to_owned(),
            source,
            digest,
            workflow_id,
            metadata: None,
            consent: None,
            validation_error: None,
            resolved_path,
            resume_run_id: arguments
                .get("resumeFromRunId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            saved_name: match source_location(arguments)? {
                SourceLocation::Saved(name) => Some(name.to_owned()),
                _ => None,
            },
        }
    };
    if let Some(selected) = selected
        && (preview.digest != selected.digest
            || preview.workflow_id != selected.workflow_id
            || preview.resolved_path != selected.resolved_path
            || preview.resume_run_id != selected.resume_run_id
            || preview.saved_name != selected.saved_name)
    {
        return Err(BridgeError::host(
            "Workflow source changed after preview; request consent again",
        ));
    }
    Ok(preview)
}
