use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn interrupt_terminates_all_worker_terminals_before_acknowledging() {
    for rejected in [false, true] {
        let order = if rejected {
            CompletionOrder::InterruptRejected
        } else {
            CompletionOrder::WithBackgroundTerminals
        };
        let mut fixture = Fixture::new(order).await;
        let result = fixture
            .runtime
            .host_request(
                "worker.interrupt",
                json!({"threadId":fixture.thread_id,"turnId":"turn-1"}),
            )
            .await;
        if rejected {
            assert!(result.unwrap_err().message.contains("turn already ended"));
        } else {
            assert_eq!(result.unwrap(), json!({"status":"interrupted"}));
        }
        let mut requests = Vec::new();
        while let Ok(request) = fixture.daemon_messages.try_recv() {
            if request["method"] != "initialize" {
                requests.push(json!({"method":request["method"],"params":request["params"]}));
            }
        }
        assert_eq!(
            requests,
            vec![
                json!({"method":"turn/interrupt","params":{"threadId":fixture.thread_id,"turnId":"turn-1"}}),
                json!({"method":"thread/backgroundTerminals/list","params":{"threadId":fixture.thread_id,"cursor":null,"limit":null}}),
                json!({"method":"thread/backgroundTerminals/list","params":{"threadId":fixture.thread_id,"cursor":"page-2","limit":null}}),
                json!({"method":"thread/backgroundTerminals/terminate","params":{"threadId":fixture.thread_id,"processId":"17"}}),
                json!({"method":"thread/backgroundTerminals/terminate","params":{"threadId":fixture.thread_id,"processId":"18"}}),
            ]
        );
        fixture.close().await;
    }
}
