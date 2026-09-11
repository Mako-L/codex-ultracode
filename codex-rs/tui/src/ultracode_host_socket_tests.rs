use super::socket::authentication_result;

#[test]
fn authenticated_response_identifies_the_supervisor_process() {
    assert_eq!(
        authentication_result(),
        serde_json::json!({
            "authenticated": true,
            "protocolVersion": 4,
            "supervisorProcessId": std::process::id(),
        })
    );
}
