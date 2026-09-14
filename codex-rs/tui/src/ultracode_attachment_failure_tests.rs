use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn failed_attachment_reports_conflict_and_keeps_connection_usable() {
    let fixture = Fixture::new(CompletionOrder::Manual).await;
    let (owner, owner_connection) = fixture.attach().await;
    let (contender, contender_connection) = fixture.connect(None).await;
    let params = json!({
        "parentThreadId": fixture.parent_id,
        "pluginRoot": fixture.parent.plugin_root,
    });
    let error = contender
        .request("attach", params.clone(), REQUEST_TIMEOUT)
        .await
        .unwrap_err();
    assert_eq!(error.code, "ATTACH_FAILED");
    assert_eq!(
        error.message,
        "workflow parent already has an attached frontend"
    );
    assert!(fixture.parent.attachment.lock().unwrap().is_some());

    // Failure must preserve the rightful owner, and the contender can retry after detach.
    owner
        .request("detach", json!({}), REQUEST_TIMEOUT)
        .await
        .unwrap();
    assert_eq!(
        contender
            .request("attach", params, REQUEST_TIMEOUT)
            .await
            .unwrap(),
        json!({"attached": true})
    );
    assert!(contender.list_runs().await.unwrap()["runs"].is_array());
    owner_connection.abort();
    contender_connection.abort();
    fixture.close().await;
}
