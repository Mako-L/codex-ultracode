//! Shared launch preparation after the frontend's native route has authorized a workflow call.
use crate::ultracode_bridge::BridgeError;
use crate::ultracode_bridge::UltracodeBridge;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::DynamicToolCallResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::WorkflowAuthorityCaptureResponse;
use codex_app_server_protocol::WorkflowScriptReadParams;
use codex_app_server_protocol::WorkflowScriptReadResponse;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;
use uuid::Uuid;

pub(crate) struct WorkflowLaunch {
    pub result: Result<Value, BridgeError>,
    pub source_digest: Option<String>,
}

pub(crate) fn validate_arguments(arguments: &Value) -> Result<(), BridgeError> {
    let object = arguments
        .as_object()
        .ok_or_else(|| BridgeError::host("Workflow arguments must be an object"))?;
    for (key, value) in object {
        if !matches!(
            key.as_str(),
            "script" | "name" | "scriptPath" | "args" | "resumeFromRunId" | "title" | "description"
        ) {
            return Err(BridgeError::host(format!(
                "Unknown workflow argument {key}. This tool launches or resumes workflows; completion arrives automatically."
            )));
        }
        if key != "args" && !value.is_string() {
            return Err(BridgeError::host(format!(
                "Workflow argument {key} must be a string"
            )));
        }
        if matches!(
            key.as_str(),
            "script" | "name" | "scriptPath" | "resumeFromRunId"
        ) && value.as_str() == Some("")
        {
            return Err(BridgeError::host(format!(
                "Workflow argument {key} must not be empty"
            )));
        }
    }
    if !["script", "name", "scriptPath", "resumeFromRunId"]
        .iter()
        .any(|key| object.contains_key(*key))
    {
        return Err(BridgeError::host(
            "Workflow requires script, name, scriptPath, or resumeFromRunId",
        ));
    }
    Ok(())
}

pub(crate) async fn launch(
    handle: &AppServerRequestHandle,
    parent_id: &str,
    authority: &WorkflowAuthorityCaptureResponse,
    bridge: &UltracodeBridge,
    arguments: &Value,
) -> Result<WorkflowLaunch, BridgeError> {
    validate_arguments(arguments)?;
    let file_source = if let Some(script_path) = arguments.get("scriptPath").and_then(Value::as_str)
    {
        let response: WorkflowScriptReadResponse = handle
            .request_typed(ClientRequest::WorkflowScriptRead {
                request_id: RequestId::String(format!("workflow-script:{}", Uuid::new_v4())),
                params: WorkflowScriptReadParams {
                    parent_thread_id: parent_id.to_string(),
                    authority_ref: authority.authority_ref.clone(),
                    authority_digest: authority.authority_digest.clone(),
                    script_path: script_path.to_string(),
                },
            })
            .await
            .map_err(|error| BridgeError::host(error.to_string()))?;
        Some(response.source)
    } else {
        None
    };
    let method = if arguments.get("resumeFromRunId").is_some() {
        "resumeRun"
    } else if arguments.get("name").is_some() {
        "runSaved"
    } else {
        "runSource"
    };
    let mut params = json!({"authorityRef":authority.authority_ref,"authorityDigest":authority.authority_digest,"model":authority.parent_model,"effort":authority.parent_effort});
    if let Some(args) = arguments.get("args") {
        params["args"] = args.clone();
    }
    let mut source_digest = None;
    if method == "runSaved" {
        let catalog = bridge
            .request(
                "listSavedWorkflows",
                json!({"cwd":authority.cwd}),
                Duration::from_secs(30),
            )
            .await?;
        let name = arguments["name"].as_str().unwrap_or_default();
        let saved = catalog["workflows"]
            .as_array()
            .and_then(|items| items.iter().find(|item| item["name"] == name))
            .ok_or_else(|| BridgeError::host("Saved workflow not found"))?;
        params["workflowId"] = saved["workflowId"].clone();
        source_digest = saved
            .get("sourceDigest")
            .or_else(|| saved.get("digest"))
            .and_then(Value::as_str)
            .map(str::to_string);
    } else if method == "resumeRun" {
        params["runId"] = arguments["resumeFromRunId"].clone();
        if let Some(source) = file_source {
            params["source"] = json!(source);
        } else if let Some(script) = arguments.get("script") {
            params["source"] = script.clone();
        }
    } else {
        params["source"] = file_source
            .map(Value::String)
            .unwrap_or_else(|| arguments["script"].clone());
    }
    if let Some(source) = params.get("source").and_then(Value::as_str) {
        let validated = bridge
            .request(
                "validateSource",
                json!({"source":source}),
                Duration::from_secs(30),
            )
            .await?;
        source_digest = validated
            .get("digest")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    Ok(WorkflowLaunch {
        result: bridge
            .request(method, params, Duration::from_secs(30))
            .await,
        source_digest,
    })
}

pub(crate) fn response(result: Result<Value, BridgeError>) -> DynamicToolCallResponse {
    let response = result.and_then(|value| {
        let run_id = value.get("runId").and_then(Value::as_str).ok_or_else(|| BridgeError::host("workflow launch did not return runId"))?;
        let mut output = json!({"status":"async_launched","taskId":run_id,"taskType":"local_workflow","runId":run_id});
        output["completion"] = json!("A typed workflow.completion message will arrive automatically. Acknowledge launch and finish this turn. The run ID is not a worker thread ID; do not poll thread tools or call workflow again to wait.");
        for key in ["workflowName","transcriptDir","scriptPath"] {
            if let Some(field) = value.get(key).filter(|field| !field.is_null()) { output[key] = field.clone(); }
        }
        crate::dynamic_tools::success_response(output).map_err(BridgeError::host)
    });
    response.unwrap_or_else(|error| crate::dynamic_tools::failure_response(error.to_string()))
}
