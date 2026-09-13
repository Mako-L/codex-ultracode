use super::*;
use codex_code_mode_protocol::render_json_schema_to_typescript;
use pretty_assertions::assert_eq;

#[test]
fn workflow_schema_renderer_exposes_launch_arguments() {
    let DynamicToolSpec::Function(workflow) = workflow_tool_specs().remove(0) else {
        panic!("workflow must be a function tool");
    };
    let rendered = render_json_schema_to_typescript(&workflow.input_schema);
    assert_eq!(
        rendered,
        "{\n  args?: unknown;\n  // Maximum simultaneous workflow workers; omitted uses the runtime default or saved run limit.\n  concurrency?: number;\n  description?: string;\n  name?: string;\n  resumeFromRunId?: string;\n  script?: string;\n  scriptPath?: string;\n  title?: string;\n} & ({ script: string; } | { name: string; } | { scriptPath: string; } | { resumeFromRunId: string; })"
    );
}
