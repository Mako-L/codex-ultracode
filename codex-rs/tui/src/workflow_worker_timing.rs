use chrono::DateTime;
use chrono::Utc;
use serde_json::Value;

use super::elapsed_at;
use super::token_text;
use super::token_total;

pub(super) fn detail_metrics(worker: &Value, now: DateTime<Utc>) -> Option<String> {
    let mut parts = Vec::new();
    if worker["usage"].is_object() {
        parts.push(format!("{} tok", token_text(token_total(worker))));
    }
    if let Some(count) = worker["toolCalls"].as_u64().filter(|count| *count > 0) {
        parts.push(format!(
            "{count} tool call{}",
            if count == 1 { "" } else { "s" }
        ));
    }
    if worker["durationMs"].is_u64() || worker["startedAt"].is_string() {
        parts.push(elapsed_at(worker, now));
    }
    let timing = match worker["status"].as_str() {
        Some("queued" | "preparing") => worker["queuedAt"].as_i64().map(|at| ("waiting", at, 0)),
        Some("running") => worker["lastProgressAt"].as_i64().map(|at| ("idle", at, 30)),
        _ => None,
    };
    if let Some((label, timestamp, threshold)) = timing {
        let seconds = now.timestamp_millis().saturating_sub(timestamp).max(0) as u64 / 1000;
        if seconds >= threshold {
            let duration = if seconds >= 60 {
                format!("{}m {}s", seconds / 60, seconds % 60)
            } else {
                format!("{seconds}s")
            };
            parts.push(format!("{label} {duration}"));
        }
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

#[cfg(test)]
#[path = "workflow_worker_timing_tests.rs"]
mod tests;
