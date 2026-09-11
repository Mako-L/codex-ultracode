use crate::dynamic_tools::*;
use codex_app_server_protocol::DynamicToolNamespaceTool;
use codex_app_server_protocol::DynamicToolSpec;

#[test]
fn workflow_is_a_direct_dynamic_tool() {
    assert!(tool_specs().iter().any(
        |spec| matches!(spec, DynamicToolSpec::Function(function) if function.name == "workflow")
    ));
}

#[test]
fn workflow_specs_exclude_task_tools() {
    let specs = workflow_tool_specs();
    assert_eq!(specs.len(), 1);
    assert!(
        matches!(&specs[0], DynamicToolSpec::Function(function) if function.name == "workflow")
    );
}

#[test]
fn task_specs_keep_sentinel_and_exclude_workflow() {
    let specs = task_tool_specs();
    assert!(!specs.iter().any(
        |spec| matches!(spec, DynamicToolSpec::Function(function) if function.name == "workflow")
    ));
    assert!(specs.iter().any(|spec| matches!(spec, DynamicToolSpec::Namespace(namespace) if namespace.tools.iter().any(|tool| matches!(tool, DynamicToolNamespaceTool::Function(function) if function.name == "list_threads")))));
}
