use super::BridgeError;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::JSONRPCErrorError;
use pretty_assertions::assert_eq;

#[test]
fn rejected_worker_configuration_preserves_fatal_runtime_classification() {
    for code in [-32600, -32602, -32603] {
        let error = TypedRequestError::Server {
            method: "workflow/worker/start".to_string(),
            source: JSONRPCErrorError {
                code,
                message: "worker model is unavailable".to_string(),
                data: None,
            },
        };
        let expected = BridgeError {
            code: if code == -32603 {
                "HOST_ERROR"
            } else {
                "INVALID_WORKER_CONFIGURATION"
            }
            .to_string(),
            message: error.to_string(),
            outcome_unresolved: false,
        };
        assert_eq!(BridgeError::worker_start(error), expected);
    }
}
