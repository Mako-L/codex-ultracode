use super::*;

#[test]
fn bounds_completion_summary_on_utf8_boundary() {
    let completion = WorkflowCompletion::new("run-1", "é".repeat(5_000));
    assert!(completion.body().len() <= 8_000);
    assert!(completion.body().ends_with('é'));
    assert_eq!(completion.content_kind().0, "workflow.completion");
}
