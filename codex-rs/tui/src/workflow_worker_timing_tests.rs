use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn waiting_uses_queue_time_and_clamps_future_timestamps() {
    let now = DateTime::from_timestamp_millis(100_000).unwrap();
    let mut worker = json!({"status": "queued", "queuedAt": 38_000});
    insta::assert_snapshot!(detail_metrics(&worker, now).unwrap(), @r"waiting 1m 2s");
    worker["queuedAt"] = json!(110_000);
    assert_eq!(detail_metrics(&worker, now), Some("waiting 0s".into()));
}

#[test]
fn idle_starts_at_thirty_seconds_and_resets_on_progress() {
    let now = DateTime::from_timestamp_millis(100_000).unwrap();
    let mut worker = json!({"status": "running", "lastProgressAt": 70_001});
    assert_eq!(detail_metrics(&worker, now), None);
    worker["lastProgressAt"] = json!(70_000);
    insta::assert_snapshot!(detail_metrics(&worker, now).unwrap(), @r"idle 30s");
    worker["lastProgressAt"] = json!(100_000);
    assert_eq!(detail_metrics(&worker, now), None);
}

#[test]
fn completed_metrics_omit_idle_and_missing_values() {
    let now = DateTime::from_timestamp_millis(100_000).unwrap();
    let worker = json!({
        "status": "completed", "usage": {"totalTokens": 1200},
        "toolCalls": 1, "durationMs": 5500, "lastProgressAt": 0,
    });
    insta::assert_snapshot!(detail_metrics(&worker, now).unwrap(), @r"1.2k tok · 1 tool call · 5s");
    assert_eq!(detail_metrics(&json!({"status": "completed"}), now), None);
}
