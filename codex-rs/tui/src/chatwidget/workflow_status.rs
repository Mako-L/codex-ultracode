use ratatui::style::Stylize;
use ratatui::text::Line;
use serde_json::Value;

use super::ChatWidget;
use crate::workflow_advisory::WorkflowWarningSettings;
use crate::workflow_advisory::large_workflow_warning;

fn workflow_status_line(
    snapshot: &Value,
    settings: &WorkflowWarningSettings<'_>,
) -> Option<Line<'static>> {
    let mut runs = 0;
    let mut agents = 0;
    let mut warning = false;
    let mut current = None;
    let mut indicator = " ●".green();
    for run in snapshot["runs"].as_array()? {
        let status = run["status"].as_str();
        if !matches!(status, Some("running" | "paused" | "completed" | "stopped")) {
            continue;
        }
        runs += 1;
        let workers = run["workers"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        agents += workers.len();
        if runs == 1 {
            indicator = if status == Some("paused") {
                " ●".yellow()
            } else if status == Some("completed") {
                " ◯".into()
            } else if status == Some("stopped")
                || workers
                    .iter()
                    .any(|worker| worker["status"].as_str() == Some("failed"))
            {
                " ●".red()
            } else {
                " ●".green()
            };
        }
        if current.is_none()
            && let Some(name) = run["name"]
                .as_str()
                .filter(|name| name.chars().any(|c| !c.is_control() && !c.is_whitespace()))
        {
            let completed = if status == Some("completed") {
                workers.len()
            } else {
                workers
                    .iter()
                    .filter(|worker| worker["status"].as_str() == Some("completed"))
                    .count()
            };
            let name: String = name.chars().filter(|c| !c.is_control()).take(60).collect();
            current = Some(if status == Some("completed") {
                let description = run["description"]
                    .as_str()
                    .map(|value| {
                        value
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(80)
                            .collect::<String>()
                    })
                    .filter(|value| value.chars().any(|c| !c.is_whitespace()))
                    .map(|value| format!("  {value}"))
                    .unwrap_or_default();
                let elapsed = match (
                    run["startedAt"]
                        .as_str()
                        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok()),
                    run["endedAt"]
                        .as_str()
                        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok()),
                ) {
                    (Some(started), Some(ended)) => {
                        let seconds = (ended - started).num_seconds().max(0) as u64;
                        if seconds >= 60 {
                            format!(" · {}m {}s", seconds / 60, seconds % 60)
                        } else {
                            format!(" · {seconds}s")
                        }
                    }
                    _ => String::new(),
                };
                format!(
                    " {name}{description}  {completed}/{} agents done{elapsed}",
                    workers.len()
                )
            } else {
                format!(
                    " {name} · {completed}/{} agents · {}",
                    workers.len(),
                    status.unwrap_or_default()
                )
            });
        } else if current.is_none() && status == Some("completed") {
            current = Some(format!(" {}/{} agents done", workers.len(), workers.len()));
        }
        let started = workers
            .iter()
            .filter(|worker| worker["startedAt"].as_str().is_some())
            .count();
        let tokens = workers.iter().fold(0_u64, |total, worker| {
            total.saturating_add(
                worker
                    .pointer("/usage/totalTokens")
                    .or_else(|| worker.pointer("/usage/total/totalTokens"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            )
        });
        warning |= large_workflow_warning(workers.len(), started, tokens, settings).is_some();
    }
    if runs == 0 {
        return None;
    }
    let summary = format!(
        "  {runs} workflow{} · {agents} agent{}",
        if runs == 1 { "" } else { "s" },
        if agents == 1 { "" } else { "s" }
    );
    let summary = current.map_or(summary, |summary| {
        if runs > 1 {
            format!("{summary} · +{} workflows", runs - 1)
        } else {
            summary
        }
    });
    Some(if warning {
        Line::from(vec![
            indicator,
            summary.into(),
            " · ⚠ Large workflow · /workflows to stop".yellow(),
        ])
    } else {
        Line::from(vec![indicator, format!("{summary} · /workflows").into()])
    })
}

impl ChatWidget {
    pub(crate) fn update_workflow_status(&mut self, snapshot: &Value) {
        let size = self
            .config
            .workflow_size_guideline
            .map(|size| size.to_string());
        let threshold = |name| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.parse().ok())
        };
        let settings = WorkflowWarningSettings {
            size_guideline: size.as_deref(),
            ultracode_active: self.config.ultracode,
            agent_threshold: threshold("ULTRACODE_WORKFLOW_SIZE_WARNING_AGENTS"),
            token_threshold: threshold("ULTRACODE_WORKFLOW_SIZE_WARNING_TOKENS"),
        };
        self.bottom_pane
            .set_workflow_status(if self.config.disable_workflows {
                None
            } else {
                workflow_status_line(snapshot, &settings)
            });
    }
}

#[cfg(test)]
#[path = "workflow_status_tests.rs"]
mod tests;
