use crate::dynamic_tools_mcp::ThreadToolTransport;
use codex_app_server_protocol::DynamicToolNamespaceTool;
use codex_app_server_protocol::DynamicToolSpec;
use codex_app_server_protocol::ThreadStartParams;

#[test]
fn task_transport_keeps_sentinel_but_removes_workflow() {
    let mut params = ThreadStartParams::default();
    ThreadToolTransport::Tasks.configure(&mut params);
    let specs = params.dynamic_tools.expect("task inventory");
    assert!(!specs.iter().any(
        |spec| matches!(spec, DynamicToolSpec::Function(function) if function.name == "workflow")
    ));
    assert!(specs.iter().any(|spec| matches!(spec, DynamicToolSpec::Namespace(namespace) if namespace.tools.iter().any(|tool| matches!(tool, DynamicToolNamespaceTool::Function(function) if function.name == "list_threads")))));
}
