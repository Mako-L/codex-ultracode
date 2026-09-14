//! The reference worker card shows the latest three tool-call summaries.

use serde_json::Value;
use std::collections::HashMap;

use super::cell_cut;
use super::clean;

pub(super) fn activity_rows(worker: &Value, width: usize) -> Vec<String> {
    let mut calls: Vec<&Value> = Vec::new();
    let mut positions: HashMap<&str, usize> = HashMap::new();
    for item in worker["activity"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| {
            let kind = item["type"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            kind.contains("tool")
                || kind.contains("command")
                || kind.contains("file")
                || matches!(
                    kind.as_str(),
                    "websearch" | "imageview" | "sleep" | "imagegeneration"
                )
        })
    {
        if let Some(id) = item["id"].as_str().filter(|id| !id.is_empty()) {
            if let Some(index) = positions.get(id) {
                calls[*index] = item;
                continue;
            }
            positions.insert(id, calls.len());
        }
        calls.push(item);
    }
    let mut rows = vec![if calls.len() > 3 {
        format!("Activity · last 3 of {} tool calls", calls.len())
    } else {
        "Activity".into()
    }];
    for item in calls.iter().skip(calls.len().saturating_sub(3)) {
        let kind = item["type"].as_str().unwrap_or_default();
        let default_name = match kind {
            "commandExecution" | "command_execution" => "exec_command",
            "fileChange" | "file_change" => "apply_patch",
            "webSearch" => "web_search",
            "imageView" => "view_image",
            "imageGeneration" => "image_generation",
            _ => kind,
        };
        let name = item["toolName"]
            .as_str()
            .or_else(|| item["name"].as_str())
            .or_else(|| item["tool"].as_str())
            .or_else(|| item["title"].as_str())
            .unwrap_or(default_name);
        let command = item["command"].as_array().map(|parts| {
            parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        });
        let paths = item["changes"].as_array().map(|changes| {
            changes
                .iter()
                .filter_map(|change| change["path"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        });
        let summary = item["summary"]
            .as_str()
            .or(command.as_deref())
            .or_else(|| item["command"].as_str())
            .or(paths.as_deref())
            .or_else(|| item["action"]["query"].as_str())
            .unwrap_or_default();
        let text = if summary.is_empty() || summary == name {
            name.to_string()
        } else {
            format!("{name}({summary})")
        };
        rows.push(format!(
            "  {}",
            cell_cut(&clean(&text), width.saturating_sub(2))
        ));
    }
    if calls.is_empty() {
        let text = if let Some(name) = worker["lastToolName"].as_str() {
            match worker["lastToolSummary"]
                .as_str()
                .filter(|summary| !summary.is_empty())
            {
                Some(summary) => format!("{name}({summary})"),
                None => name.to_string(),
            }
        } else if worker["status"] == "running" {
            "No tool calls yet.".into()
        } else {
            "No tool calls.".into()
        };
        rows.push(format!(
            "  {}",
            cell_cut(&clean(&text), width.saturating_sub(2))
        ));
    }
    rows
}
