use super::WorkflowFocus;
use super::WorkflowScreen;
use super::WorkflowView;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(crate) fn cell_width(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}
pub(crate) fn cell_cut(value: &str, size: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    let value = clean(value);
    for grapheme in value.graphemes(true) {
        let w = UnicodeWidthStr::width(grapheme);
        if used + w > size {
            break;
        }
        out.push_str(grapheme);
        used += w;
    }
    out
}
pub(crate) fn cell_fit(value: &str, size: usize) -> String {
    let mut out = cell_cut(value, size);
    out.push_str(&" ".repeat(size.saturating_sub(cell_width(&out))));
    out
}
pub(crate) fn dash_fit(value: &str, size: usize) -> String {
    let mut out = cell_cut(value, size);
    out.push_str(&"─".repeat(size.saturating_sub(cell_width(&out))));
    out
}
pub(crate) fn viewport(selected: usize, count: usize, rows: usize) -> usize {
    selected
        .saturating_sub(rows.saturating_sub(1))
        .min(count.saturating_sub(rows))
}
pub(crate) fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}
pub(crate) fn wrap(value: &str, size: usize) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in value.split('\n') {
        let mut rest = clean(paragraph);
        if rest.is_empty() {
            out.push(String::new());
            continue;
        }
        while cell_width(&rest) > size {
            let prefix = cell_cut(&rest, size);
            if prefix.is_empty() {
                // A terminal can be narrower than a single wide character. Consume it
                // so resizing to a one-cell detail column cannot stall the UI.
                let end = rest
                    .graphemes(true)
                    .next()
                    .expect("nonempty over-width text")
                    .len();
                out.push(rest[..end].to_string());
                rest = rest[end..].trim_start().to_string();
                continue;
            }
            let at = prefix.rfind(' ').filter(|v| *v > 0).unwrap_or(prefix.len());
            out.push(rest[..at].to_string());
            rest = rest[at..].trim_start().to_string();
        }
        if !rest.is_empty() {
            out.push(rest);
        }
    }
    out
}
pub(crate) fn dialog_at(width: usize, title: &str, body: Vec<String>, footer: &str) -> Vec<String> {
    let mut out = vec!["▔".repeat(width), format!("   {title}")];
    out.extend(body.into_iter().map(|line| {
        if line.is_empty() {
            line
        } else {
            format!("   {line}")
        }
    }));
    out.push(String::new());
    out.push(format!("   {footer}"));
    out
}
pub(crate) fn token_total(w: &Value) -> u64 {
    w.pointer("/usage/totalTokens")
        .or_else(|| w.pointer("/usage/total/totalTokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}
pub(crate) fn token_text(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.)
    } else {
        n.to_string()
    }
}

pub(crate) fn render_picker(view: &WorkflowView, width: usize) -> Vec<String> {
    let runs = view.runs();
    if runs.is_empty() {
        return dialog_at(
            width,
            "Dynamic workflows",
            vec![
                String::new(),
                "No dynamic workflows in this session.".into(),
            ],
            "Esc to close",
        );
    };
    let done = runs
        .iter()
        .filter(|r| {
            matches!(
                r["status"].as_str(),
                Some("completed" | "failed" | "stopped")
            )
        })
        .count();
    let mut body = vec![format!("{done} completed"), String::new()];
    for (i, r) in runs.iter().enumerate() {
        let workers = r["workers"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let total: u64 = workers.iter().map(token_total).sum();
        body.push(format!(
            "{} {} {}  {}{} · {}",
            if i == view.state.run { "❯" } else { " " },
            mark(r["status"].as_str()),
            clean(r["name"].as_str().unwrap_or_default()),
            plural(workers.len(), "agent"),
            if total > 0 {
                format!(" · {} tok", token_text(total))
            } else {
                String::new()
            },
            elapsed(r)
        ));
    }
    dialog_at(
        width,
        "Dynamic workflows",
        body,
        "↑/↓ to select · Enter to view · s to save · Esc to close",
    )
}
pub(crate) fn render_save(view: &WorkflowView, width: usize) -> Vec<String> {
    let scope = if view.state.save_scope == "user" {
        "User"
    } else {
        "Project"
    };
    let fallback = if view.state.save_scope == "user" {
        format!("~/.codex/workflows/{}.js", view.state.save_name)
    } else {
        format!(".codex/workflows/{}.js", view.state.save_name)
    };
    let resolved = view
        .snapshot
        .pointer(&format!("/savePaths/{}", view.state.save_scope))
        .and_then(Value::as_str)
        .map(clean)
        .unwrap_or(fallback);
    dialog_at(
        width,
        "Save dynamic workflow",
        vec![
            format!("{scope} scope · {resolved}"),
            String::new(),
            "Save as:".into(),
            String::new(),
            format!("> {}", view.state.save_name),
        ],
        "Enter to save · Tab to toggle scope · Esc to cancel",
    )
}
pub(crate) fn render_effort(view: &WorkflowView, width: usize) -> Vec<String> {
    let choices = ["low", "medium", "high", "xhigh", "max", "ultracode"];
    let index = choices
        .iter()
        .position(|v| *v == view.state.effort)
        .unwrap_or(0);
    let start = width.saturating_sub(68) / 2;
    let start = start.max(3);
    let ruler = "───────────────────────────────────────────┆──────────────────";
    let caret = [4, 10, 20, 30, 40, 53][index];
    let mut scale = String::new();
    for (i, c) in ruler.chars().enumerate() {
        scale.push(if i == caret { '▲' } else { c })
    }
    dialog_at(
        width,
        "Effort",
        vec![
            String::new(),
            format!("{}Faster{}Smarter", " ".repeat(start), " ".repeat(49)),
            format!("{}{}", " ".repeat(start), scale),
            format!(
                "{}low     medium     high     xhigh      max       ultracode",
                " ".repeat(start)
            ),
            format!("{}xhigh + workflows", " ".repeat(start + 45)),
        ],
        "←/→ to adjust · Enter to confirm · s for this session only · Esc to cancel",
    )
}

pub(crate) fn phase_state(workers: &[&Value]) -> &'static str {
    if workers.iter().any(|w| w["status"] == "failed") {
        "failed"
    } else if !workers.is_empty() && workers.iter().all(|w| w["status"] == "completed") {
        "completed"
    } else if workers.iter().any(|w| w["status"] == "running") {
        "running"
    } else if workers.iter().any(|w| w["status"] == "stopped") {
        "stopped"
    } else {
        "queued"
    }
}
pub(crate) fn worker_row(w: &Value, size: usize, label_width: usize) -> String {
    let status = match w["status"].as_str() {
        Some("failed") => " · failed",
        Some("stopped") => " · stopped",
        _ => "",
    };
    let duration = if w["durationMs"].as_u64().is_some() || w["startedAt"].is_string() {
        elapsed(w)
    } else {
        String::new()
    };
    let prefix = format!(
        "{} {} {}{}{}",
        if w["status"] == "stopped" {
            "◌"
        } else {
            mark(w["status"].as_str())
        },
        cell_fit(w["label"].as_str().unwrap_or_default(), label_width),
        clean(w["model"].as_str().unwrap_or_default()),
        if w.get("usage").is_some() {
            format!(" · {} tok", token_text(token_total(w)))
        } else {
            String::new()
        },
        status
    );
    if duration.is_empty() {
        prefix
    } else {
        format!(
            "{} {duration}",
            cell_fit(&prefix, size.saturating_sub(cell_width(&duration) + 2))
        )
    }
}
pub(crate) fn detail_rows(w: &Value, size: usize, expanded: bool) -> Vec<String> {
    let status = w["status"].as_str().unwrap_or_default();
    let heading = match status {
        "completed" => "Completed",
        "failed" => "Failed",
        "queued" | "preparing" => "Queued",
        "running" => "Running",
        "stopped" | "interrupted" => "Stopped",
        "skipped" => "Skipped",
        "blocked" => "Blocked",
        _ => status,
    };
    let attempt = w["attempt"]
        .as_u64()
        .filter(|attempt| *attempt > 1)
        .map(|attempt| {
            let reason = match w["lastAttemptReason"].as_str() {
                Some("user-retry") => "user retry",
                Some("throttled") => "throttled",
                _ => "stalled",
            };
            format!(" · attempt {attempt} ({reason})")
        })
        .unwrap_or_default();
    let mut metadata = Vec::new();
    let explicit_role = w
        .pointer("/options/agentType")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some();
    if let Some(role) = w["agentType"]
        .as_str()
        .map(clean)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .filter(|value| value != "general-purpose" || explicit_role)
    {
        metadata.push(role);
    }
    let isolation = w["isolation"]
        .as_str()
        .map(clean)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            w.pointer("/workspace/isolated")
                .and_then(Value::as_bool)
                .filter(|isolated| *isolated)
                .map(|_| "worktree".to_string())
        });
    if let Some(isolation) = isolation {
        metadata.push(isolation);
    }
    if w["cached"].as_bool() == Some(true) {
        metadata.push("from resume journal".to_string());
    }
    let metadata = if metadata.is_empty() {
        String::new()
    } else {
        format!(" · {}", metadata.join(" · "))
    };
    let glyph = match status {
        "stopped" | "interrupted" => "◌",
        "skipped" | "blocked" => "✘",
        _ => mark(Some(status)),
    };
    let model = w["model"]
        .as_str()
        .map(clean)
        .filter(|model| !model.is_empty());
    let model = model.map(|model| format!(" · {model}")).unwrap_or_default();
    let mut out = vec![format!("{glyph} {heading}{model}{metadata}{attempt}")];
    if let Some(metrics) = super::worker_timing::detail_metrics(w, chrono::Utc::now()) {
        out.push(metrics);
    }
    out.extend([String::new(), "Prompt".into()]);
    let prompt = wrap(
        w["prompt"].as_str().unwrap_or_default(),
        size.saturating_sub(2).max(1),
    );
    if prompt.len() > 2 {
        *out.last_mut().expect("prompt heading") = format!(
            "Prompt · {} lines{}",
            prompt.len(),
            if expanded { "" } else { " · ⏎ expand" }
        );
    }
    if w["prompt"].as_str().is_none_or(str::is_empty) {
        out.push(format!(
            "  {}",
            match status {
                "queued" | "preparing" => "Available once the agent starts.",
                "running" => "Not available yet (agent still running).",
                _ => "Transcript not available.",
            }
        ));
    } else {
        let visible = if expanded { prompt.len() } else { 2 };
        out.extend(prompt.iter().take(visible).map(|v| format!("  {v}")));
        if !expanded && prompt.len() > visible {
            let remaining = prompt.len() - visible;
            out.push(format!(
                "  … {remaining} more {}",
                if remaining == 1 { "line" } else { "lines" }
            ));
        }
    }
    if matches!(status, "queued" | "preparing") {
        out.extend([
            String::new(),
            "Outcome".into(),
            "  Waiting for an agent slot.".into(),
        ]);
        return out;
    }
    out.push(String::new());
    out.extend(super::worker_activity::activity_rows(w, size));
    out.extend([String::new(), "Outcome".into()]);
    let outcome = if matches!(status, "failed" | "blocked") {
        w["error"].as_str().unwrap_or("failed").to_string()
    } else {
        w["text"]
            .as_str()
            .filter(|text| !text.is_empty())
            .or_else(|| w["output"].as_str())
            .map(str::to_string)
            .unwrap_or_else(|| {
                if w["output"].is_null() {
                    String::new()
                } else {
                    w["output"].to_string()
                }
            })
    };
    let outcome = match status {
        "running" => "Still running…",
        "stopped" | "interrupted" => "The workflow stopped before this agent finished.",
        "skipped" => "Skipped by user.",
        "completed" if outcome.is_empty() => "(empty)",
        _ => &outcome,
    };
    out.extend(
        wrap(outcome, size.saturating_sub(2).max(1))
            .into_iter()
            .map(|v| format!("  {v}")),
    );
    out
}

pub(crate) fn render_overview(view: &WorkflowView, width: usize, height: usize) -> Vec<String> {
    let Some(run) = view.run() else {
        return render_picker(view, width);
    };
    let phases = view.phases();
    let workers = view.workers();
    let all = run["workers"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let done = all.iter().filter(|w| w["status"] == "completed").count();
    let summary = format!(
        "{}/{} {} · {} · {}",
        done,
        all.len(),
        if all.len() == 1 { "agent" } else { "agents" },
        elapsed(run),
        if run["status"] == "completed" {
            "done"
        } else {
            run["status"].as_str().unwrap_or_default()
        }
    );
    let left_items = if view.state.screen == WorkflowScreen::Detail {
        workers
            .iter()
            .map(|w| clean(w["label"].as_str().unwrap_or_default()))
            .collect::<Vec<_>>()
    } else {
        phases.clone()
    };
    let desired = left_items
        .iter()
        .map(|v| {
            cell_width(v)
                + if view.state.screen == WorkflowScreen::Detail {
                    6
                } else {
                    10
                }
        })
        .max()
        .unwrap_or(14)
        .max(14);
    let left_width = desired.min(((width as f64) * 0.4).floor() as usize).max(14);
    let inner = width.saturating_sub(10).max(1);
    let right_width = inner.saturating_sub(left_width + 1).max(1);
    let phase = phases.get(view.state.phase).cloned().unwrap_or_default();
    let title = if view.state.filter == "all" {
        format!("{} · {}", phase, plural(workers.len(), "agent"))
    } else {
        format!(
            "{} · showing {} {}",
            phase,
            workers.len(),
            match view.state.filter.as_str() {
                "completed" => "done",
                "stopped" => "interrupted",
                filter => filter,
            }
        )
    };
    let mut out = vec![
        "▔".repeat(width),
        String::new(),
        format!("  {}", "─".repeat(width.saturating_sub(6))),
        format!("   {}", clean(run["name"].as_str().unwrap_or_default())),
        format!(
            "   {}{}",
            cell_fit(
                run["description"].as_str().unwrap_or_default(),
                width.saturating_sub(cell_width(&summary) + 8)
            ),
            summary
        ),
        String::new(),
    ];
    let log_rows = run["logs"].as_array().map_or(0, |v| {
        if v.is_empty() {
            0
        } else {
            height.saturating_sub(13).clamp(1, 3)
        }
    });
    if log_rows > 0 {
        let log_lines = run["logs"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|v| {
                wrap(
                    v["value"]
                        .as_str()
                        .or_else(|| v["message"].as_str())
                        .unwrap_or_default(),
                    width.saturating_sub(6).max(1),
                )
            })
            .collect::<Vec<_>>();
        out.extend(
            log_lines
                .into_iter()
                .rev()
                .take(log_rows)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|v| format!("   {v}")),
        );
    }
    let top_left = if view.state.screen == WorkflowScreen::Detail {
        format!("╭ {} · {} ", phase, plural(workers.len(), "agent"))
    } else {
        "╭ Phases ".into()
    };
    let right_title = if view.state.screen == WorkflowScreen::Detail {
        clean(
            workers
                .get(view.state.worker)
                .and_then(|w| w["label"].as_str())
                .unwrap_or_default(),
        )
    } else {
        title
    };
    let clipped_title = cell_cut(&right_title, right_width.saturating_sub(2));
    out.push(format!(
        "   {}┬ {} {}╮",
        dash_fit(&top_left, left_width + 1),
        clipped_title,
        "─".repeat(right_width.saturating_sub(cell_width(&clipped_title) + 2))
    ));
    let body_rows = height.saturating_sub(11 + log_rows).max(1);
    let label_width = workers
        .iter()
        .map(|w| cell_width(w["label"].as_str().unwrap_or_default()))
        .max()
        .unwrap_or(12)
        .max(12);
    let right_lines = if view.state.screen == WorkflowScreen::Detail {
        workers.get(view.state.worker).map_or(vec![], |w| {
            detail_rows(w, right_width.saturating_sub(2), view.state.expanded)
        })
    } else {
        workers
            .iter()
            .map(|w| worker_row(w, right_width.saturating_sub(2), label_width))
            .collect()
    };
    let left_selected = if view.state.screen == WorkflowScreen::Detail {
        view.state.worker
    } else {
        view.state.phase
    };
    let left_count = if view.state.screen == WorkflowScreen::Detail {
        workers.len()
    } else {
        phases.len()
    };
    let left_offset = viewport(left_selected, left_count, body_rows);
    let detail_scroll_limit = if view.state.screen == WorkflowScreen::Detail {
        right_lines.len().saturating_sub(body_rows)
    } else {
        0
    };
    view.detail_scroll_limit.set(detail_scroll_limit);
    let right_offset = if view.state.screen == WorkflowScreen::Detail {
        view.state.scroll.min(detail_scroll_limit)
    } else if view.state.focus == WorkflowFocus::Workers {
        viewport(view.state.worker, workers.len(), body_rows)
    } else {
        0
    };
    view.detail_section
        .set(crate::workflow_view_style::detail::section_before(
            &right_lines,
            right_offset,
        ));
    let phase_width = phases.iter().map(|p| cell_width(p)).max().unwrap_or(0);
    for row in 0..body_rows {
        let index = row + left_offset;
        let left = if view.state.screen == WorkflowScreen::Detail {
            workers
                .get(index)
                .map(|w| {
                    format!(
                        "{} {} {}",
                        if index == view.state.worker {
                            "❯"
                        } else {
                            " "
                        },
                        mark(w["status"].as_str()),
                        clean(w["label"].as_str().unwrap_or_default())
                    )
                })
                .unwrap_or_default()
        } else {
            phases
                .get(index)
                .map(|p| {
                    let ws = all
                        .iter()
                        .filter(|w| w["phase"].as_str() == Some(p))
                        .collect::<Vec<_>>();
                    let status = phase_state(&ws);
                    let indicator = if status == "completed" || status == "failed" {
                        mark(Some(status)).to_string()
                    } else {
                        (index + 1).to_string()
                    };
                    format!(
                        "{} {} {}{}",
                        if index == view.state.phase && view.state.focus == WorkflowFocus::Phases {
                            "❯"
                        } else {
                            " "
                        },
                        indicator,
                        cell_fit(p, phase_width),
                        if ws.is_empty() {
                            String::new()
                        } else {
                            format!(
                                " {}/{}",
                                ws.iter().filter(|w| w["status"] == "completed").count(),
                                ws.len()
                            )
                        }
                    )
                })
                .unwrap_or_default()
        };
        let ri = row + right_offset;
        let prefix = if view.state.screen == WorkflowScreen::Detail {
            " "
        } else if view.state.focus == WorkflowFocus::Workers && ri == view.state.worker {
            " ❯"
        } else {
            "  "
        };
        out.push(format!(
            "   │{}│{}│",
            cell_fit(&format!(" {left}"), left_width),
            cell_fit(
                &format!(
                    "{}{}",
                    prefix,
                    right_lines.get(ri).cloned().unwrap_or_default()
                ),
                right_width
            )
        ));
    }
    let scroll_range = if detail_scroll_limit > 0 {
        format!(
            " {} {}–{} of {} {} ",
            if right_offset > 0 { "↑" } else { " " },
            right_offset + 1,
            right_offset + body_rows,
            right_lines.len(),
            if right_offset < detail_scroll_limit {
                "↓"
            } else {
                " "
            },
        )
    } else {
        String::new()
    };
    let scroll_range = cell_cut(&scroll_range, right_width);
    out.push(format!(
        "   ╰{}┴{}{}╯",
        "─".repeat(left_width),
        "─".repeat(right_width.saturating_sub(cell_width(&scroll_range))),
        scroll_range
    ));
    let restart = if run["status"] == "running"
        && (view.state.screen == WorkflowScreen::Detail
            || view.state.focus == WorkflowFocus::Workers)
        && workers.get(view.state.worker).is_some_and(|worker| {
            worker["status"] == "running" && worker["id"].as_str().is_some_and(|id| !id.is_empty())
        }) {
        " · r restart"
    } else {
        ""
    };
    let controls = if view.state.screen == WorkflowScreen::Detail {
        format!(
            "↑↓ agent{}{restart}",
            if detail_scroll_limit > 0 {
                " · j/k scroll"
            } else {
                ""
            },
        )
    } else {
        format!("↑↓ select{restart}")
    };
    let filter = if view.state.screen != WorkflowScreen::Overview
        || view.state.focus != WorkflowFocus::Workers
    {
        String::new()
    } else if view.state.filter == "all" {
        " · f filter".into()
    } else {
        format!(
            " · f filter: {}",
            match view.state.filter.as_str() {
                "completed" => "done",
                "stopped" => "interrupted",
                filter => filter,
            }
        )
    };
    let selected_worker_can_stop = workers.get(view.state.worker).is_some_and(|worker| {
        (matches!(
            worker["status"].as_str(),
            Some("running" | "queued" | "preparing")
        ) || (run["status"] == "paused"
            && matches!(worker["status"].as_str(), Some("stopped" | "interrupted"))))
            && worker["id"].as_str().is_some_and(|id| !id.is_empty())
    });
    let lifecycle = match run["status"].as_str() {
        Some("running")
            if view.state.screen == WorkflowScreen::Detail
                || view.state.focus == WorkflowFocus::Workers =>
        {
            if selected_worker_can_stop {
                " · p pause · x stop"
            } else {
                " · p pause"
            }
        }
        Some("running") => " · p pause · x stop",
        Some("paused")
            if selected_worker_can_stop
                && (view.state.screen == WorkflowScreen::Detail
                    || view.state.focus == WorkflowFocus::Workers) =>
        {
            " · p resume · x stop"
        }
        Some("paused") => " · p resume",
        _ => "",
    };
    out.push(format!(
        "   {controls}{filter}{lifecycle} · esc back · s save"
    ));
    out
}

impl Widget for &WorkflowView {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let label_width = (self.state.screen == WorkflowScreen::Overview).then(|| {
            self.workers()
                .iter()
                .map(|worker| cell_width(worker["label"].as_str().unwrap_or_default()))
                .max()
                .unwrap_or(0)
                .max(12)
        });
        let lines = self.lines(area.width as usize, area.height as usize);
        let lines = if self.styles_enabled {
            if self.state.screen == WorkflowScreen::Detail {
                let detail = self
                    .workers()
                    .get(self.state.worker)
                    .copied()
                    .map(|worker| crate::workflow_view_style::detail::Context {
                        status: worker["status"].as_str().unwrap_or_default(),
                        section: self.detail_section.get(),
                        empty_result: worker["text"].as_str().is_none_or(str::is_empty)
                            && (worker["output"].is_null() || worker["output"] == ""),
                    });
                crate::workflow_view_style::styled_lines_with_detail(lines, label_width, detail)
            } else {
                crate::workflow_view_style::styled_lines(lines, label_width)
            }
        } else {
            lines.into_iter().map(Line::raw).collect()
        };
        Paragraph::new(lines).render(area, buf)
    }
}

pub(crate) fn clean(value: &str) -> String {
    let mut out = String::new();
    let mut it = value.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            match it.peek() {
                Some('[') => {
                    it.next();
                    for x in it.by_ref() {
                        if ('@'..='~').contains(&x) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    it.next();
                    let mut esc = false;
                    for x in it.by_ref() {
                        if x == '\x07' || (esc && x == '\\') {
                            break;
                        }
                        esc = x == '\x1b';
                    }
                }
                _ => {}
            }
        } else if !c.is_control() {
            out.push(c)
        }
    }
    out
}

pub(crate) fn mark(status: Option<&str>) -> &'static str {
    match status {
        Some("completed") => "✔",
        Some("failed" | "stopped" | "interrupted") => "✘",
        Some("running") => "✻",
        Some("paused") => "Ⅱ",
        _ => "◌",
    }
}

pub(crate) fn elapsed(v: &Value) -> String {
    elapsed_at(v, chrono::Utc::now())
}

pub(crate) fn elapsed_at(v: &Value, now: chrono::DateTime<chrono::Utc>) -> String {
    let parse = |key| {
        v[key]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
    };
    let seconds = if let Some(duration) = v["durationMs"].as_u64() {
        let active = if v["status"] == "running" {
            parse("durationUpdatedAt")
                .map(|updated| now.signed_duration_since(updated).num_milliseconds().max(0) as u64)
                .unwrap_or(0)
        } else {
            0
        };
        duration.saturating_add(active) / 1000
    } else {
        match (
            parse("startedAt").or_else(|| parse("createdAt")),
            parse("endedAt").or_else(|| parse("updatedAt")),
        ) {
            (Some(a), Some(b)) => (b - a).num_seconds().max(0) as u64,
            _ => 0,
        }
    };
    if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}
