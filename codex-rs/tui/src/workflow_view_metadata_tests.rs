use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn worker_detail_shows_role_and_isolation_before_retry_metadata() {
    let worker = json!({
        "status": "completed",
        "model": "gpt-5.6-luna",
        "agentType": "general-purpose",
        "isolation": "worktree",
        "options": {"agentType": "general-purpose"},
        "attempt": 2,
        "lastAttemptReason": "user-retry",
    });

    let rows = detail_rows(&worker, 100, false);

    insta::assert_snapshot!(rows[0], @r"✔ Completed · gpt-5.6-luna · general-purpose · worktree · attempt 2 (user retry)");
}

#[test]
fn worker_detail_omits_synthesized_default_role() {
    let worker = json!({
        "status": "completed",
        "model": "gpt-5.6-luna",
        "agentType": "general-purpose",
        "options": {},
    });

    let rows = detail_rows(&worker, 100, false);

    assert_eq!(rows[0], "✔ Completed · gpt-5.6-luna");
}

#[test]
fn worker_detail_falls_back_to_workspace_isolation_and_cleans_metadata() {
    let worker = json!({
        "status": "running",
        "model": "gpt-5.6-luna",
        "agentType": "\u{1b}[35m Explore\n",
        "workspace": {"isolated": true},
    });

    let rows = detail_rows(&worker, 100, false);

    assert_eq!(rows[0], "✻ running · gpt-5.6-luna · Explore · worktree");
}

#[test]
fn worker_detail_preserves_explicit_general_purpose_role() {
    let worker = json!({
        "status": "completed",
        "model": "gpt-5.6-luna",
        "agentType": "general-purpose",
        "options": {"agentType": "general-purpose"},
    });

    let rows = detail_rows(&worker, 100, false);

    assert_eq!(rows[0], "✔ Completed · gpt-5.6-luna · general-purpose");
}
