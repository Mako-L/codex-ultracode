use super::*;
use pretty_assertions::assert_eq;

fn saved_preview() -> WorkflowSourcePreview {
    WorkflowSourcePreview {
        thread_id: "origin-thread".into(),
        source: "original script".into(),
        digest: format!("{:x}", Sha256::digest(b"original script")),
        workflow_id: Some("saved-identity".into()),
        metadata: None,
        validation_error: None,
        resolved_path: None,
        resume_run_id: None,
        saved_name: Some("review".into()),
    }
}

#[test]
fn editor_changes_pin_inline_bytes_and_drop_saved_permission_identity() {
    let original = saved_preview();
    let arguments = json!({"name":"review","args":{"ticket":4},"concurrency":2});
    let (edited, preview) = original
        .with_edited_source(&arguments, "edited script".into())
        .unwrap();
    assert_eq!(
        edited,
        json!({"script":"edited script","args":{"ticket":4},"concurrency":2})
    );
    assert_eq!(preview.thread_id, original.thread_id);
    assert_eq!(preview.source, "edited script");
    assert_eq!(
        preview.digest,
        format!("{:x}", Sha256::digest(b"edited script"))
    );
    assert_eq!(preview.workflow_id, None);
    assert_eq!(preview.saved_name, None);
    assert_eq!(preview.resolved_path, None);
    assert_eq!(arguments["name"], "review");
    assert_eq!(original.source, "original script");
}

#[test]
fn editor_without_changes_preserves_saved_identity() {
    let original = saved_preview();
    let arguments = json!({"name":"review"});
    let (unchanged, preview) = original
        .with_edited_source(&arguments, original.source.clone())
        .unwrap();
    assert_eq!(unchanged, arguments);
    assert_eq!(preview.digest, original.digest);
    assert_eq!(preview.workflow_id, original.workflow_id);
    assert_eq!(preview.saved_name, original.saved_name);
}

#[test]
fn editor_changes_preserve_resume_journal_but_remove_mutable_file_lookup() {
    let mut original = saved_preview();
    original.workflow_id = None;
    original.saved_name = None;
    original.resolved_path = Some("/tmp/workflow.js".into());
    original.resume_run_id = Some("prior-run".into());
    let arguments = json!({"scriptPath":"workflow.js","resumeFromRunId":"prior-run","args":false});
    let (edited, preview) = original
        .with_edited_source(&arguments, "edited resume script".into())
        .unwrap();
    assert_eq!(
        edited,
        json!({"script":"edited resume script","resumeFromRunId":"prior-run","args":false})
    );
    assert_eq!(preview.resume_run_id, original.resume_run_id);
    assert_eq!(preview.resolved_path, None);
    assert!(matches!(
        source_location(&edited).unwrap(),
        SourceLocation::Inline("edited resume script")
    ));
}
