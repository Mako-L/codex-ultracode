use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn restarted_worker_detail_shows_cumulative_metrics_and_retry_metadata() {
    let worker = json!({
        "label": "review",
        "status": "completed",
        "model": "gpt-5.6-luna",
        "attempt": 2,
        "lastAttemptReason": "user-retry",
        "usage": {"totalTokens": 1500},
        "rawUsage": {"totalTokens": 400},
        "toolCalls": 3,
        "durationMs": 72000,
        "durationUpdatedAt": "2026-09-13T10:10:00Z",
        "startedAt": "2026-09-13T10:00:00Z",
        "endedAt": "2026-09-13T10:10:00Z"
    });
    let rows = detail_rows(&worker, /*size*/ 100, /*expanded*/ false);
    insta::assert_snapshot!(rows[..2].join("\n"), @r"
    ✔ Completed · gpt-5.6-luna · attempt 2 (user retry)
    1.5k tok · 3 tool calls · 1m 12s
    ");
    assert!(worker_row(&worker, /*size*/ 80, /*label_width*/ 12).ends_with("1m 12s"));

    let mut without_start = worker.clone();
    without_start.as_object_mut().unwrap().remove("startedAt");
    assert_eq!(
        worker_row(&worker, /*size*/ 80, /*label_width*/ 12),
        worker_row(&without_start, /*size*/ 80, /*label_width*/ 12),
    );
}

#[test]
fn first_attempt_and_legacy_worker_detail_keep_existing_rendering() {
    let legacy = json!({
        "label": "review",
        "status": "completed",
        "model": "gpt-5.6-luna",
        "usage": {"totalTokens": 1500},
        "startedAt": "2026-09-13T10:00:00Z",
        "endedAt": "2026-09-13T10:00:07Z"
    });
    let mut first = legacy.clone();
    first["attempt"] = json!(1);
    first["toolCalls"] = json!(0);
    first["durationMs"] = json!(7000);
    first["durationUpdatedAt"] = json!("2026-09-13T10:00:07Z");
    assert_eq!(
        detail_rows(&first, /*size*/ 100, /*expanded*/ false),
        detail_rows(&legacy, /*size*/ 100, /*expanded*/ false),
    );
    assert_eq!(
        worker_row(&first, /*size*/ 80, /*label_width*/ 12),
        worker_row(&legacy, /*size*/ 80, /*label_width*/ 12),
    );
    first["toolCalls"] = json!(1);
    let rows = detail_rows(&first, /*size*/ 100, /*expanded*/ false);
    assert_eq!(
        &rows[..2],
        ["✔ Completed · gpt-5.6-luna", "1.5k tok · 1 tool call · 7s"],
    );
}

#[test]
fn cumulative_duration_adds_only_running_time_since_its_checkpoint() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-13T10:00:04.900Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let mut worker = json!({
        "durationMs": 70500,
        "durationUpdatedAt": "2026-09-13T10:00:03.200Z",
        "startedAt": "2026-09-13T09:00:00Z",
        "endedAt": "2026-09-13T10:00:00Z"
    });
    for (status, expected) in [
        ("running", "1m 12s"),
        ("paused", "1m 10s"),
        ("completed", "1m 10s"),
        ("failed", "1m 10s"),
        ("stopped", "1m 10s"),
        ("queued", "1m 10s"),
    ] {
        worker["status"] = json!(status);
        assert_eq!(elapsed_at(&worker, now), expected, "status {status}");
    }
    worker["status"] = json!("running");
    for checkpoint in [json!("2026-09-13T10:00:05Z"), json!("invalid"), Value::Null] {
        worker["durationUpdatedAt"] = checkpoint;
        assert_eq!(elapsed_at(&worker, now), "1m 10s");
    }
}
