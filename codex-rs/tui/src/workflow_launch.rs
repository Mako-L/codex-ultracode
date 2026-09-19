//! Shared launch preparation after the frontend's native route has authorized a workflow call.
use crate::workflow_bridge::BridgeError;
use crate::workflow_bridge::WorkflowBridge;
use crate::workflow_source::SourceLocation;
use crate::workflow_source::WorkflowSourcePreview;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::DynamicToolCallResponse;
use codex_app_server_protocol::WorkflowAuthorityCaptureResponse;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;

#[cfg(test)]
#[path = "workflow_launch_tests.rs"]
mod tests;

pub(crate) struct WorkflowLaunch {
    pub result: Result<Value, BridgeError>,
    pub source_digest: Option<String>,
}

pub(crate) fn with_isolate_writes(arguments: &Value, default: bool) -> Value {
    let mut value = arguments.clone();
    if let Some(object) = value.as_object_mut() {
        object
            .entry("isolateWrites")
            .or_insert(serde_json::json!(default));
    }
    value
}

pub(crate) fn validate_arguments(arguments: &Value) -> Result<(), BridgeError> {
    let object = arguments
        .as_object()
        .ok_or_else(|| BridgeError::host("Workflow arguments must be an object"))?;
    for (key, value) in object {
        if !matches!(
            key.as_str(),
            "script"
                | "name"
                | "scriptPath"
                | "args"
                | "resumeFromRunId"
                | "title"
                | "description"
                | "concurrency"
                | "isolateWrites"
        ) {
            return Err(BridgeError::host(format!(
                "Unknown workflow argument {key}. This tool launches or resumes workflows; completion arrives automatically."
            )));
        }
        if key == "concurrency" {
            if !value
                .as_u64()
                .is_some_and(|limit| (1..=16).contains(&limit))
            {
                return Err(BridgeError::host(
                    "Workflow concurrency must be an integer from 1 to 16",
                ));
            }
            continue;
        }
        if key == "isolateWrites" {
            if !value.is_boolean() {
                return Err(BridgeError::host("Workflow isolateWrites must be a boolean"));
            }
            continue;
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
    bridge: &WorkflowBridge,
    arguments: &Value,
) -> Result<WorkflowLaunch, BridgeError> {
    launch_with_preview(
        handle, parent_id, authority, bridge, arguments, /*preview*/ None,
    )
    .await
}

pub(crate) async fn launch_with_preview(
    handle: &AppServerRequestHandle,
    parent_id: &str,
    authority: &WorkflowAuthorityCaptureResponse,
    bridge: &WorkflowBridge,
    arguments: &Value,
    preview: Option<&WorkflowSourcePreview>,
) -> Result<WorkflowLaunch, BridgeError> {
    validate_arguments(arguments)?;
    let selected_source = crate::workflow_source::source_location(arguments)?;
    let verified = if let Some(preview) = preview {
        Some(
            crate::workflow_source::read_preview(
                handle,
                parent_id,
                authority,
                bridge,
                arguments,
                Some(preview),
            )
            .await?,
        )
    } else {
        None
    };
    let file_source = if let Some(verified) = &verified {
        // Pin verified bytes even when resume would otherwise reread mutable stored source.
        Some(verified.source.clone())
    } else if let SourceLocation::File(path) = selected_source {
        Some(
            crate::workflow_source::read_file(handle, parent_id, authority, path)
                .await?
                .source,
        )
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
    if let Some(concurrency) = arguments.get("concurrency") {
        params["concurrency"] = concurrency.clone();
    }
    if let Some(isolate_writes) = arguments.get("isolateWrites") {
        params["isolateWrites"] = isolate_writes.clone();
    }
    let mut source_digest = None;
    if method == "runSaved" {
        if let Some(verified) = &verified {
            params["workflowId"] =
                json!(
                    verified
                        .workflow_id
                        .as_ref()
                        .ok_or_else(|| BridgeError::host(
                            "Saved workflow preview identity is unavailable"
                        ))?
                );
            source_digest = Some(verified.digest.clone());
        } else {
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
        }
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
