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

    assert_eq!(rows[0], "✻ Running · gpt-5.6-luna · Explore · worktree");
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

#[test]
fn worker_detail_shows_journal_replay_before_retry_metadata() {
    let worker = json!({
        "status": "completed",
        "model": "gpt-5.6-luna",
        "cached": true,
        "attempt": 2,
        "lastAttemptReason": "user-retry",
    });
    let rows = detail_rows(&worker, 100, false);
    insta::assert_snapshot!(rows[0], @r"✔ Completed · gpt-5.6-luna · from resume journal · attempt 2 (user retry)");
}

#[test]
fn queued_worker_detail_reports_waiting_time() {
    let worker = json!({
        "status": "queued", "model": "gpt-5.6-luna",
        "queuedAt": chrono::Utc::now().timestamp_millis() - 62_000,
    });
    let rows = detail_rows(&worker, 100, false);
    assert_eq!(rows[0], "◌ Queued · gpt-5.6-luna");
    assert!(rows[1].starts_with("waiting 1m "));
}

#[test]
fn running_worker_detail_reports_idle_time_after_thirty_seconds() {
    let worker = json!({
        "status": "running", "model": "gpt-5.6-luna",
        "lastProgressAt": chrono::Utc::now().timestamp_millis() - 62_000,
    });
    let rows = detail_rows(&worker, 100, false);
    assert_eq!(rows[0], "✻ Running · gpt-5.6-luna");
    assert!(rows[1].starts_with("idle 1m "));
}
