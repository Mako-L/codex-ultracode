use crate::ultracode_bridge::BridgeError;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadBackgroundTerminalsListParams;
use codex_app_server_protocol::ThreadBackgroundTerminalsListResponse;
use codex_app_server_protocol::ThreadBackgroundTerminalsTerminateParams;
use codex_app_server_protocol::ThreadBackgroundTerminalsTerminateResponse;
use codex_app_server_protocol::TurnInterruptParams;
use codex_app_server_protocol::TurnInterruptResponse;
use serde_json::Value;
use serde_json::json;

pub(crate) async fn interrupt(
    handle: AppServerRequestHandle,
    params: TurnInterruptParams,
) -> Result<Value, BridgeError> {
    let thread_id = params.thread_id.clone();
    let request_id = || RequestId::String(format!("workflow-interrupt:{}", uuid::Uuid::new_v4()));
    let interruption = handle
        .request_typed::<TurnInterruptResponse>(ClientRequest::TurnInterrupt {
            request_id: request_id(),
            params,
        })
        .await
        .map_err(|error| BridgeError::host(error.to_string()));
    // A yielded unified-exec process outlives turn interruption. Terminate the worker's
    // terminals before acknowledging stop/restart, so its old commands cannot continue.
    let mut cursor = None;
    let mut process_ids = Vec::new();
    loop {
        let page: ThreadBackgroundTerminalsListResponse = handle
            .request_typed(ClientRequest::ThreadBackgroundTerminalsList {
                request_id: request_id(),
                params: ThreadBackgroundTerminalsListParams {
                    thread_id: thread_id.clone(),
                    cursor,
                    limit: None,
                },
            })
            .await
            .map_err(|error| BridgeError::host(error.to_string()))?;
        process_ids.extend(page.data.into_iter().map(|terminal| terminal.process_id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    for process_id in process_ids {
        let _: ThreadBackgroundTerminalsTerminateResponse = handle
            .request_typed(ClientRequest::ThreadBackgroundTerminalsTerminate {
                request_id: request_id(),
                params: ThreadBackgroundTerminalsTerminateParams {
                    thread_id: thread_id.clone(),
                    process_id,
                },
            })
            .await
            .map_err(|error| BridgeError::host(error.to_string()))?;
    }
    interruption?;
    Ok(json!({"status":"interrupted"}))
}
