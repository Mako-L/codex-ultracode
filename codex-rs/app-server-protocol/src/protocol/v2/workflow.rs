use codex_protocol::openai_models::ReasoningEffort;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::JsonSchema;
use crate::TS;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowAuthorityCaptureParams {
    pub parent_thread_id: String,
    #[serde(default)]
    pub allow_isolated_workspaces: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowAuthorityCaptureResponse {
    pub authority_ref: String,
    pub generation: u64,
    pub authority_digest: String,
    pub cwd: String,
    pub parent_model: String,
    pub parent_effort: Option<ReasoningEffort>,
    pub models: Vec<WorkflowModelOption>,
    pub plugins: Vec<WorkflowPluginOption>,
    pub web_search_available: bool,
    /// Effective native workflow listener, used to bind a headless controller to its parent.
    pub workflow_host_url: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowPluginOption {
    pub name: String,
    pub root: String,
    pub workflows: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowScriptReadParams {
    pub parent_thread_id: String,
    pub authority_ref: String,
    pub authority_digest: String,
    pub script_path: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowScriptReadResponse {
    pub resolved_path: String,
    pub source: String,
    pub source_digest: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowSaveParams {
    pub parent_thread_id: String,
    pub authority_ref: String,
    pub authority_digest: String,
    pub run_id: String,
    pub name: String,
    pub scope: String,
    pub source: String,
    pub source_digest: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowSaveResponse {
    pub path: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowCompletionInjectParams {
    pub parent_thread_id: String,
    pub run_id: String,
    pub summary: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowCompletionInjectResponse {
    /// The turn that accepted the completion; acceptance can precede persistence.
    pub turn_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowModelOption {
    pub model: String,
    pub default_effort: ReasoningEffort,
    pub supported_efforts: Vec<ReasoningEffort>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowWorkerStartParams {
    pub run_id: String,
    pub worker_id: String,
    pub parent_thread_id: String,
    pub authority_ref: String,
    pub authority_generation: u64,
    pub authority_digest: String,
    pub prompt: String,
    pub model: String,
    pub effort: ReasoningEffort,
    #[serde(default)]
    pub model_explicit: bool,
    #[serde(default)]
    pub effort_explicit: bool,
    /// Native role name. The host resolves configured roles; this protocol does not freeze an enum.
    pub agent_type: String,
    pub role_digest: String,
    /// Restrict this worker to the parent's read-only intersection.
    pub read_only: bool,
    pub workspace: WorkflowWorkspace,
    pub schema: Option<Value>,
    pub resume_thread_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowWorkspace {
    pub workspace_id: Option<String>,
    pub cwd: String,
    pub isolated: bool,
    pub base_commit: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowWorkspacePrepareParams {
    pub run_id: String,
    pub worker_id: String,
    pub agent_type: String,
    pub authority_ref: String,
    pub authority_digest: String,
    pub isolation: Option<String>,
    pub requested_cwd: String,
    pub previous: Option<WorkflowWorkspace>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowWorkspacePrepareResponse {
    pub workspace_id: Option<String>,
    pub cwd: String,
    pub isolated: bool,
    pub base_commit: Option<String>,
    pub authority_generation: u64,
    pub role_digest: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowWorkspaceReleaseParams {
    pub run_id: String,
    pub worker_id: String,
    pub workspace_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowWorkspaceReleaseResponse {
    pub eligible: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowWorkerStartResponse {
    pub thread_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub model: String,
    pub effort: ReasoningEffort,
}
