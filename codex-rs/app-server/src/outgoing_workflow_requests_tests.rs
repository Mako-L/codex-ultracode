use super::*;
use codex_app_server_protocol::ToolRequestUserInputParams;
use pretty_assertions::assert_eq;
use serde_json::json;

#[tokio::test]
async fn workflow_approval_survives_parent_turn_but_not_thread_cleanup() {
    let (tx, _rx) = mpsc::channel(8);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        AnalyticsEventsClient::disabled(),
    ));
    let thread_id = ThreadId::new();
    let request = || {
        ServerRequestPayload::ToolRequestUserInput(ToolRequestUserInputParams {
            thread_id: thread_id.to_string(),
            turn_id: "workflow-workspace-checkout".to_string(),
            item_id: "checkout".to_string(),
            questions: vec![],
            is_blocking: true,
            auto_resolution_ms: None,
        })
    };
    let (_, mut workflow) = outgoing
        .send_workflow_request_to_connections(&[ConnectionId(1)], request(), thread_id)
        .await;
    let scoped =
        ThreadScopedOutgoingMessageSender::new(outgoing.clone(), vec![ConnectionId(1)], thread_id);
    let (_, ordinary) = scoped.send_request(request()).await;
    scoped.abort_pending_server_requests().await;
    let mut transition =
        internal_error("client request resolved because the turn state was changed");
    transition.data = Some(json!({"reason": TURN_TRANSITION_PENDING_REQUEST_ERROR_REASON}));
    assert_eq!(ordinary.await.unwrap(), Err(transition));
    assert_eq!(
        workflow.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    );

    let cleanup = internal_error("thread unloaded");
    outgoing
        .cancel_requests_for_thread(thread_id, Some(cleanup.clone()))
        .await;
    assert_eq!(workflow.await.unwrap(), Err(cleanup));
    assert!(
        outgoing
            .pending_requests_for_thread(thread_id)
            .await
            .is_empty()
    );
}
