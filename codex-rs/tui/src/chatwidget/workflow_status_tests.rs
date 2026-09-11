use super::*;
use serde_json::json;

#[test]
fn workflow_footer_warns_from_real_progress_and_clears_terminal_runs() {
    let mut snapshot = json!({"runs":[{"status":"running","workers":vec![json!({"startedAt":"now","usage":{"totalTokens":1}});26]}]});
    let line = workflow_status_line(&snapshot, &WorkflowWarningSettings::default())
        .unwrap()
        .to_string();
    assert!(line.contains("1 workflow · 26 agents"));
    assert!(line.contains("Large workflow · /workflows to stop"));
    snapshot["runs"][0]["status"] = json!("completed");
    assert!(workflow_status_line(&snapshot, &WorkflowWarningSettings::default()).is_none());
}

#[test]
fn workflow_footer_keeps_counts_when_ultracode_suppresses_warning() {
    let snapshot = json!({"runs":[{"status":"running","workers":vec![json!({});26]},{"status":"paused","workers":[]},{"status":"failed","workers":[{}]}]});
    let line = workflow_status_line(
        &snapshot,
        &WorkflowWarningSettings {
            ultracode_active: true,
            ..Default::default()
        },
    )
    .unwrap()
    .to_string();
    assert!(line.contains("2 workflows · 26 agents · /workflows"));
    assert!(!line.contains("Large workflow"));
}

#[test]
fn workflow_footer_uses_projection_and_explicit_guideline() {
    let snapshot = json!({"runs":[{"status":"running","workers":vec![json!({"startedAt":"now","usage":{"total":{"totalTokens":1}}});6]}]});
    assert!(
        !workflow_status_line(&snapshot, &WorkflowWarningSettings::default())
            .unwrap()
            .to_string()
            .contains("Large workflow")
    );
    assert!(
        workflow_status_line(
            &snapshot,
            &WorkflowWarningSettings {
                size_guideline: Some("small"),
                ..Default::default()
            }
        )
        .unwrap()
        .to_string()
        .contains("Large workflow")
    );
    let pending = json!({"runs":[{"status":"running","workers":vec![json!({});22]}]});
    assert!(
        workflow_status_line(&pending, &WorkflowWarningSettings::default())
            .unwrap()
            .to_string()
            .contains("Large workflow")
    );
}
