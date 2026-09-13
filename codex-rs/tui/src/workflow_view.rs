use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use serde_json::Value;
use std::cell::Cell;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const FILTERS: [&str; 6] = ["all", "failed", "running", "completed", "stopped", "queued"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkflowScreen {
    Picker,
    Overview,
    Detail,
    Save,
    Effort,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkflowFocus {
    Phases,
    Workers,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkflowViewState {
    pub screen: WorkflowScreen,
    pub focus: WorkflowFocus,
    pub run: usize,
    pub phase: usize,
    pub worker: usize,
    pub filter: String,
    pub scroll: usize,
    pub expanded: bool,
    pub save_name: String,
    pub save_scope: String,
    pub effort: String,
    pub return_screen: Option<WorkflowScreen>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkflowAction {
    Close,
    InspectRun {
        run_id: String,
    },
    PauseRun {
        run_id: String,
    },
    ResumeRun {
        run_id: String,
    },
    StopRun {
        run_id: String,
        worker_id: Option<String>,
    },
    RestartWorker {
        run_id: String,
        worker_id: String,
    },
    Save {
        run_id: String,
        name: String,
        scope: String,
    },
    SetEffort {
        effort: String,
    },
}

pub(crate) struct WorkflowView {
    snapshot: Value,
    detail_scroll_limit: Cell<usize>,
    pub(crate) styles_enabled: bool,
    pub state: WorkflowViewState,
}

impl WorkflowView {
    pub(crate) fn new(snapshot: Value, view: Option<WorkflowViewState>) -> Self {
        let count = snapshot["runs"].as_array().map_or(0, Vec::len);
        Self {
            snapshot,
            detail_scroll_limit: Cell::new(0),
            styles_enabled: true,
            state: view.unwrap_or(WorkflowViewState {
                screen: if count == 1 {
                    WorkflowScreen::Overview
                } else {
                    WorkflowScreen::Picker
                },
                focus: WorkflowFocus::Phases,
                run: 0,
                phase: 0,
                worker: 0,
                filter: "all".into(),
                scroll: 0,
                expanded: false,
                save_name: String::new(),
                save_scope: "project".into(),
                effort: "medium".into(),
                return_screen: None,
            }),
        }
    }
    pub(crate) fn update(&mut self, snapshot: Value) {
        self.snapshot = snapshot;
        self.clamp();
    }
    fn runs(&self) -> &[Value] {
        self.snapshot["runs"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
    fn run(&self) -> Option<&Value> {
        self.runs().get(self.state.run)
    }
    fn phases(&self) -> Vec<String> {
        self.run()
            .and_then(|r| r["phases"].as_array())
            .map(|v| {
                v.iter()
                    .filter_map(|p| p.as_str().or_else(|| p["name"].as_str()))
                    .map(clean)
                    .collect()
            })
            .unwrap_or_default()
    }
    fn workers(&self) -> Vec<&Value> {
        let phase = self
            .phases()
            .get(self.state.phase)
            .cloned()
            .unwrap_or_default();
        self.run()
            .and_then(|r| r["workers"].as_array())
            .map(|v| {
                v.iter()
                    .filter(|w| {
                        w["phase"].as_str() == Some(&phase)
                            && (self.state.filter == "all"
                                || w["status"].as_str() == Some(&self.state.filter))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    fn clamp(&mut self) {
        self.state.run = self.state.run.min(self.runs().len().saturating_sub(1));
        self.state.phase = self.state.phase.min(self.phases().len().saturating_sub(1));
        self.state.worker = self
            .state
            .worker
            .min(self.workers().len().saturating_sub(1));
    }
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Option<WorkflowAction> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return None;
        }
        let run_id = self
            .run()
            .and_then(|r| r["id"].as_str())
            .unwrap_or_default()
            .to_string();
        if self.state.screen == WorkflowScreen::Save {
            match key.code {
                KeyCode::Esc => {
                    self.state.screen = self
                        .state
                        .return_screen
                        .clone()
                        .unwrap_or(WorkflowScreen::Overview)
                }
                KeyCode::Tab => {
                    self.state.save_scope = if self.state.save_scope == "user" {
                        "project"
                    } else {
                        "user"
                    }
                    .into()
                }
                KeyCode::Backspace => {
                    self.state.save_name.pop();
                }
                KeyCode::Char(c) => self.state.save_name.push(c),
                KeyCode::Enter => {
                    self.state.screen = self
                        .state
                        .return_screen
                        .clone()
                        .unwrap_or(WorkflowScreen::Overview);
                    return Some(WorkflowAction::Save {
                        run_id,
                        name: clean(&self.state.save_name).trim().into(),
                        scope: self.state.save_scope.clone(),
                    });
                }
                _ => {}
            }
            return None;
        }
        if self.state.screen == WorkflowScreen::Effort {
            const CHOICES: [&str; 6] = ["low", "medium", "high", "xhigh", "max", "ultracode"];
            let index = CHOICES
                .iter()
                .position(|value| *value == self.state.effort)
                .unwrap_or(1);
            match key.code {
                KeyCode::Left | KeyCode::Up => {
                    self.state.effort = CHOICES[index.saturating_sub(1)].into()
                }
                KeyCode::Right | KeyCode::Down => {
                    self.state.effort = CHOICES[(index + 1).min(CHOICES.len() - 1)].into()
                }
                KeyCode::Enter | KeyCode::Char('s') => {
                    return Some(WorkflowAction::SetEffort {
                        effort: self.state.effort.clone(),
                    });
                }
                KeyCode::Esc => return Some(WorkflowAction::Close),
                _ => {}
            }
            return None;
        }
        if key.code == KeyCode::Char('s') {
            self.state.return_screen = Some(self.state.screen.clone());
            self.state.screen = WorkflowScreen::Save;
            self.state.save_name = self
                .run()
                .and_then(|r| r["name"].as_str())
                .map(clean)
                .unwrap_or_default();
            return None;
        }
        let status = self
            .run()
            .and_then(|r| r["status"].as_str())
            .unwrap_or_default();
        if key.code == KeyCode::Char('p') {
            if status == "running" {
                return Some(WorkflowAction::PauseRun { run_id });
            }
            if status == "paused" {
                return Some(WorkflowAction::ResumeRun { run_id });
            }
        }
        if key.code == KeyCode::Char('x') && (status == "running" || status == "paused") {
            let worker_id = if self.state.screen == WorkflowScreen::Detail
                || self.state.focus == WorkflowFocus::Workers
            {
                self.workers()
                    .get(self.state.worker)
                    .and_then(|w| w["id"].as_str())
                    .map(str::to_string)
            } else {
                None
            };
            if (self.state.focus == WorkflowFocus::Workers
                || self.state.screen == WorkflowScreen::Detail)
                && worker_id.is_none()
            {
                return None;
            }
            return Some(WorkflowAction::StopRun { run_id, worker_id });
        }
        if key.code == KeyCode::Char('r')
            && self.state.screen == WorkflowScreen::Detail
            && let Some(w) = self.workers().get(self.state.worker)
            && w["status"] == "running"
            && w["id"].as_str().is_some_and(|id| !id.is_empty())
        {
            return Some(WorkflowAction::RestartWorker {
                run_id,
                worker_id: w["id"].as_str().unwrap_or_default().into(),
            });
        }
        match key.code {
            KeyCode::Char('f')
                if self.state.screen == WorkflowScreen::Overview
                    && self.state.focus == WorkflowFocus::Workers =>
            {
                let phase = self.phases().get(self.state.phase).cloned();
                let filters: Vec<_> = FILTERS
                    .into_iter()
                    .filter(|status| {
                        *status == "all"
                            || self
                                .run()
                                .and_then(|run| run["workers"].as_array())
                                .is_some_and(|workers| {
                                    workers.iter().any(|worker| {
                                        worker["phase"].as_str() == phase.as_deref()
                                            && worker["status"].as_str() == Some(*status)
                                    })
                                })
                    })
                    .collect();
                let i = filters
                    .iter()
                    .position(|v| *v == self.state.filter)
                    .unwrap_or(0);
                self.state.filter = filters[(i + 1) % filters.len()].into();
                self.state.worker = 0;
                self.state.scroll = 0;
            }
            KeyCode::Char('j') if self.state.screen == WorkflowScreen::Detail => {
                self.state.scroll = self
                    .state
                    .scroll
                    .saturating_add(1)
                    .min(self.detail_scroll_limit.get())
            }
            KeyCode::Char('k') if self.state.screen == WorkflowScreen::Detail => {
                self.state.scroll = self
                    .state
                    .scroll
                    .min(self.detail_scroll_limit.get())
                    .saturating_sub(1)
            }
            KeyCode::Down => {
                if self.state.screen == WorkflowScreen::Picker {
                    self.state.run += 1
                } else if self.state.screen == WorkflowScreen::Overview
                    && self.state.focus == WorkflowFocus::Phases
                {
                    if self.state.phase < self.phases().len().saturating_sub(1) {
                        self.state.filter = "all".into();
                    }
                    self.state.phase += 1
                } else {
                    self.state.worker += 1
                }
                self.clamp();
            }
            KeyCode::Up => {
                if self.state.screen == WorkflowScreen::Picker {
                    self.state.run = self.state.run.saturating_sub(1)
                } else if self.state.screen == WorkflowScreen::Overview
                    && self.state.focus == WorkflowFocus::Phases
                {
                    if self.state.phase > 0 {
                        self.state.filter = "all".into();
                    }
                    self.state.phase = self.state.phase.saturating_sub(1)
                } else {
                    self.state.worker = self.state.worker.saturating_sub(1)
                }
            }
            KeyCode::Enter | KeyCode::Right => {
                if self.state.screen == WorkflowScreen::Detail && key.code == KeyCode::Enter {
                    self.state.expanded = !self.state.expanded;
                    self.state.scroll = 0
                } else if self.state.screen == WorkflowScreen::Picker && self.run().is_some() {
                    self.state.screen = WorkflowScreen::Overview;
                    return Some(WorkflowAction::InspectRun { run_id });
                } else if self.state.screen == WorkflowScreen::Overview
                    && !self.workers().is_empty()
                {
                    if self.state.focus == WorkflowFocus::Workers {
                        self.state.screen = WorkflowScreen::Detail
                    } else {
                        self.state.focus = WorkflowFocus::Workers
                    }
                }
            }
            KeyCode::Esc | KeyCode::Left => {
                if self.state.screen == WorkflowScreen::Detail {
                    self.state.screen = WorkflowScreen::Overview;
                    self.state.focus = WorkflowFocus::Workers;
                    self.state.expanded = false
                } else if self.state.screen == WorkflowScreen::Overview
                    && self.state.focus == WorkflowFocus::Workers
                {
                    self.state.focus = WorkflowFocus::Phases
                } else if self.state.screen == WorkflowScreen::Overview && self.runs().len() > 1 {
                    self.state.screen = WorkflowScreen::Picker
                } else {
                    return Some(WorkflowAction::Close);
                }
            }
            _ => {}
        }
        None
    }
    fn lines(&self, width: usize, height: usize) -> Vec<String> {
        let mut out = match self.state.screen {
            WorkflowScreen::Picker => render_picker(self, width),
            WorkflowScreen::Save => render_save(self, width),
            WorkflowScreen::Effort => render_effort(self, width),
            WorkflowScreen::Overview | WorkflowScreen::Detail => {
                render_overview(self, width, height)
            }
        };
        for line in &mut out {
            *line = cell_cut(line, width);
        }
        out.truncate(height);
        while out.len() < height {
            out.insert(0, String::new())
        }
        out
    }
}

fn cell_width(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}
fn cell_cut(value: &str, size: usize) -> String {
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
fn cell_fit(value: &str, size: usize) -> String {
    let mut out = cell_cut(value, size);
    out.push_str(&" ".repeat(size.saturating_sub(cell_width(&out))));
    out
}
fn dash_fit(value: &str, size: usize) -> String {
    let mut out = cell_cut(value, size);
    out.push_str(&"─".repeat(size.saturating_sub(cell_width(&out))));
    out
}
fn viewport(selected: usize, count: usize, rows: usize) -> usize {
    selected
        .saturating_sub(rows.saturating_sub(1))
        .min(count.saturating_sub(rows))
}
fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}
fn wrap(value: &str, size: usize) -> Vec<String> {
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
fn dialog_at(width: usize, title: &str, body: Vec<String>, footer: &str) -> Vec<String> {
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
fn token_total(w: &Value) -> u64 {
    w.pointer("/usage/totalTokens")
        .or_else(|| w.pointer("/usage/total/totalTokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}
fn token_text(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.)
    } else {
        n.to_string()
    }
}

fn render_picker(view: &WorkflowView, width: usize) -> Vec<String> {
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
fn render_save(view: &WorkflowView, width: usize) -> Vec<String> {
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
fn render_effort(view: &WorkflowView, width: usize) -> Vec<String> {
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

fn phase_state(workers: &[&Value]) -> &'static str {
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
fn worker_row(w: &Value, size: usize, label_width: usize) -> String {
    let status = match w["status"].as_str() {
        Some("failed") => " · failed",
        Some("stopped") => " · stopped",
        _ => "",
    };
    let duration = if w["startedAt"].is_string() {
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
fn detail_rows(w: &Value, size: usize, expanded: bool) -> Vec<String> {
    let status = w["status"].as_str().unwrap_or_default();
    let heading = if status == "completed" {
        "Completed"
    } else if status == "failed" {
        "Failed"
    } else {
        status
    };
    let mut out = vec![
        format!(
            "{} {heading} · {}",
            mark(Some(status)),
            clean(w["model"].as_str().unwrap_or_default())
        ),
        format!("{} tok · {}", token_text(token_total(w)), elapsed(w)),
        String::new(),
        "Prompt".into(),
    ];
    let prompt = wrap(
        w["prompt"].as_str().unwrap_or_default(),
        size.saturating_sub(2).max(1),
    );
    out.extend(prompt.into_iter().map(|v| format!("  {v}")));
    out.extend([String::new(), "Activity".into()]);
    let activity = w["activity"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let mut activity_out = Vec::new();
    for item in activity {
        let kind = item["type"].as_str().unwrap_or_default();
        if !kind.contains("tool")
            && !kind.contains("Tool")
            && !kind.contains("command")
            && !kind.contains("Command")
            && !kind.contains("file")
            && !kind.contains("File")
        {
            continue;
        }
        let joined_command = item["command"].as_array().map(|parts| {
            parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        });
        let command = joined_command
            .as_deref()
            .or_else(|| item["command"].as_str())
            .or_else(|| item["toolName"].as_str())
            .or_else(|| item["name"].as_str())
            .or_else(|| item["title"].as_str())
            .or_else(|| item["message"].as_str())
            .unwrap_or(kind);
        activity_out.extend(wrap(command, size.saturating_sub(2).max(1)));
        let state = [
            item["status"].as_str().map(str::to_string),
            item["exitCode"].as_i64().map(|v| format!("exit {v}")),
        ]
        .into_iter()
        .flatten()
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
        if !state.is_empty() {
            activity_out.extend(wrap(&state, size.saturating_sub(2).max(1)))
        }
        if expanded {
            if let Some(input) = item.get("input") {
                activity_out.extend(wrap(
                    &format!("Input: {input}"),
                    size.saturating_sub(2).max(1),
                ))
            }
            if let Some(value) = item["aggregatedOutput"].as_str() {
                activity_out.push("Output:".into());
                activity_out.extend(
                    wrap(value, size.saturating_sub(2).max(1))
                        .into_iter()
                        .take(8),
                );
            }
        }
    }
    if activity_out.is_empty() {
        out.push("  No tool calls.".into())
    } else {
        out.extend(activity_out.into_iter().map(|v| format!("  {v}")))
    }
    out.extend([String::new(), "Outcome".into()]);
    let outcome = w["error"]
        .as_str()
        .or_else(|| w["text"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            w["output"].as_str().map(str::to_string).unwrap_or_else(|| {
                if w["output"].is_null() {
                    String::new()
                } else {
                    w["output"].to_string()
                }
            })
        });
    out.extend(
        wrap(&outcome, size.saturating_sub(2).max(1))
            .into_iter()
            .map(|v| format!("  {v}")),
    );
    out
}

fn render_overview(view: &WorkflowView, width: usize, height: usize) -> Vec<String> {
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
            view.state.filter
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
    let controls = if view.state.screen == WorkflowScreen::Detail {
        format!(
            "↑↓ agent{}{}",
            if detail_scroll_limit > 0 {
                " · j/k scroll"
            } else {
                ""
            },
            if workers
                .get(view.state.worker)
                .is_some_and(|w| w["status"] == "running")
            {
                " · r restart"
            } else {
                ""
            }
        )
    } else {
        "↑↓ select".into()
    };
    let filter = if view.state.screen != WorkflowScreen::Overview
        || view.state.focus != WorkflowFocus::Workers
    {
        String::new()
    } else if view.state.filter == "all" {
        " · f filter".into()
    } else {
        format!(" · f filter: {}", view.state.filter)
    };
    let lifecycle = match run["status"].as_str() {
        Some("running") => " · p pause · x stop",
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
            crate::workflow_view_style::styled_lines(lines, label_width)
        } else {
            lines.into_iter().map(Line::raw).collect()
        };
        Paragraph::new(lines).render(area, buf)
    }
}

fn clean(value: &str) -> String {
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

fn mark(status: Option<&str>) -> &'static str {
    match status {
        Some("completed") => "✔",
        Some("failed" | "stopped" | "interrupted") => "✘",
        Some("running") => "✻",
        Some("paused") => "Ⅱ",
        _ => "◌",
    }
}

fn elapsed(v: &Value) -> String {
    let parse = |key| {
        v[key]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
    };
    match (
        parse("startedAt").or_else(|| parse("createdAt")),
        parse("endedAt").or_else(|| parse("updatedAt")),
    ) {
        (Some(a), Some(b)) => {
            let s = (b - a).num_seconds().max(0);
            if s >= 60 {
                format!("{}m {}s", s / 60, s % 60)
            } else {
                format!("{s}s")
            }
        }
        _ => "0s".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::style::Modifier;
    use serde_json::json;
    fn run() -> Value {
        json!({"id":"run-1","name":"ui-reference","description":"UI inspection","status":"completed","startedAt":"2026-09-09T20:00:00Z","endedAt":"2026-09-09T20:00:32Z","phases":[{"name":"Inspect"},{"name":"Verify"}],"workers":[{"id":"a","label":"merge","phase":"Inspect","status":"completed","prompt":"prompt","model":"gpt-5","usage":{"totalTokens":40200},"output":"done","startedAt":"2026-09-09T20:00:00Z","endedAt":"2026-09-09T20:00:07Z"},{"id":"b","label":"quick","phase":"Inspect","status":"running","prompt":"bad\u{001b}[31mprompt","model":"gpt-5","activity":[],"output":"out"},{"id":"c","label":"compare","phase":"Verify","status":"completed","model":"gpt-5"}]})
    }
    fn text(v: &WorkflowView, w: u16, h: u16) -> String {
        let area = Rect::new(0, 0, w, h);
        let mut b = Buffer::empty(area);
        v.render(area, &mut b);
        (0..h)
            .map(|y| {
                let mut line = String::new();
                let mut x = 0;
                while x < w {
                    let symbol = b[(x, y)].symbol();
                    line.push_str(symbol);
                    x += UnicodeWidthStr::width(symbol).max(1) as u16;
                }
                line.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn golden(name: &str) -> &'static str {
        match name {
            "01-empty-workflows" => {
                include_str!("workflow_view_test/golden/01-empty-workflows.txt")
            }
            "02-effort-menu" => include_str!("workflow_view_test/golden/02-effort-menu.txt"),
            "03-running-workflow" => {
                include_str!("workflow_view_test/golden/03-running-workflow.txt")
            }
            "04-worker-detail-failed" => {
                include_str!("workflow_view_test/golden/04-worker-detail-failed.txt")
            }
            "05-save-project" => include_str!("workflow_view_test/golden/05-save-project.txt"),
            "06-save-user" => include_str!("workflow_view_test/golden/06-save-user.txt"),
            "07-filter-failed" => include_str!("workflow_view_test/golden/07-filter-failed.txt"),
            "08-run-picker" => include_str!("workflow_view_test/golden/08-run-picker.txt"),
            "09-completed-workflow" => {
                include_str!("workflow_view_test/golden/09-completed-workflow.txt")
            }
            "10-worker-detail-completed" => {
                include_str!("workflow_view_test/golden/10-worker-detail-completed.txt")
            }
            "11-effort-ultracode" => {
                include_str!("workflow_view_test/golden/11-effort-ultracode.txt")
            }
            "12-paused-color" => include_str!("workflow_view_test/golden/12-paused-color.txt"),
            "13-stopped-picker-color" => {
                include_str!("workflow_view_test/golden/13-stopped-picker-color.txt")
            }
            "14-stopped-workflow-color" => {
                include_str!("workflow_view_test/golden/14-stopped-workflow-color.txt")
            }
            _ => panic!("unknown golden {name}"),
        }
    }
    fn normalized(value: &str) -> String {
        let lines = value.lines().map(str::trim_end).collect::<Vec<_>>();
        let start = lines
            .iter()
            .position(|line| {
                line.trim_start().starts_with("──────────") || line.starts_with("▔▔▔▔▔▔▔▔▔▔")
            })
            .unwrap_or(0);
        lines[start..].join("\n").trim_end().to_string()
    }
    #[test]
    fn captured_workflow_styles_are_rendered_in_native_cells() {
        use ratatui::style::Color;
        use ratatui::style::Modifier;

        let view = WorkflowView::new(json!({"runs":[run()]}), None);
        let area = Rect::new(0, 0, 160, 48);
        let mut buffer = Buffer::empty(area);
        view.render(area, &mut buffer);
        let find = |needle: &str| {
            (0..area.height)
                .find_map(|y| {
                    let row: String = (0..area.width).map(|x| buffer[(x, y)].symbol()).collect();
                    row.find(needle).map(|offset| {
                        let x = UnicodeWidthStr::width(&row[..offset]) as u16;
                        &buffer[(x, y)]
                    })
                })
                .unwrap_or_else(|| panic!("missing rendered text: {needle}"))
        };
        assert_eq!(find("ui-reference").fg, Color::LightBlue);
        assert!(find("ui-reference").modifier.contains(Modifier::BOLD));
        assert_eq!(find("╭").fg, Color::White);
        assert_eq!(find("❯").fg, Color::LightBlue);
        assert_eq!(find("↑↓").fg, Color::Gray);
        assert!(find("↑↓").modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn narrow_unicode_wrap_always_advances() {
        assert_eq!(wrap("界 🌍", 1), vec!["界", "🌍"]);
        assert_eq!(wrap("界", 0), vec!["界"]);
    }

    #[test]
    fn clipping_preserves_complete_graphemes_and_cell_width() {
        assert_eq!(cell_cut("👩‍💻x", 2), "👩‍💻");
        assert_eq!(cell_fit("👩‍💻", 3), "👩‍💻 ");
        assert_eq!(wrap("👩‍💻", 1), vec!["👩‍💻"]);
    }

    #[test]
    fn overview_matches_captured_terminal_rows() {
        // Original Claude captures 03, 04, 07, 09, 10, 12, and 14 use these
        // dialog coordinates in a 160-column, 48-row terminal.
        let view = WorkflowView::new(json!({"runs":[run()]}), None);
        let lines = view.lines(160, 48);
        assert_eq!(lines[2], "▔".repeat(160));
        assert!(lines[5].contains("ui-reference"));
        assert!(lines[8].contains("╭ Phases"));
        assert!(lines[46].contains('╰'));
        assert!(lines[47].contains("esc back"));
        let area = Rect::new(0, 0, 160, 48);
        let mut buffer = Buffer::empty(area);
        (&view).render(area, &mut buffer);
        assert_eq!(buffer[(0, 2)].fg, ratatui::style::Color::LightBlue);
        assert_eq!(buffer[(2, 4)].fg, ratatui::style::Color::White);
    }

    #[test]
    fn effort_matches_captured_terminal_columns() {
        let mut view = WorkflowView::new(json!({"runs":[]}), None);
        view.state.screen = WorkflowScreen::Effort;
        for (effort, caret) in [("medium", 59), ("ultracode", 102)] {
            view.state.effort = effort.into();
            let lines = view.lines(160, 48);
            assert_eq!(lines[42].find("Smarter"), Some(104));
            assert_eq!(lines[43].chars().count(), 111);
            assert_eq!(lines[43].chars().position(|c| c == '▲'), Some(caret));
            assert_eq!(lines[45].find("xhigh + workflows"), Some(94));
        }
    }

    #[test]
    fn fourteen_reference_views_match_reviewed_goldens() {
        let fixture: Value =
            serde_json::from_str(include_str!("workflow_view_fixtures.json")).unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let mut run = fixture["run"].clone();
            if case["freeze"] == true {
                run["name"] = json!("ui-freeze");
                run["description"] = json!("Freeze probe: count then finish");
                run["endedAt"] = json!("2026-09-09T20:00:17Z");
                run["phases"] = json!([{"name":"Count"},{"name":"Finish"}]);
                let original = run["workers"].as_array().unwrap().clone();
                run["workers"] = json!([
                    {"id":"count","label":"count","phase":"Count","status":original[0]["status"],"prompt":original[0]["prompt"],"model":"gpt-5","effort":original[0]["effort"],"usage":{"totalTokens":40200},"activity":original[0]["activity"],"output":original[0]["output"],"startedAt":original[0]["startedAt"],"endedAt":"2026-09-09T20:00:07Z"},
                    {"id":"finish","label":"finish","phase":"Finish","status":original[1]["status"],"prompt":original[1]["prompt"],"model":"gpt-5","effort":original[1]["effort"],"usage":{"totalTokens":40200},"activity":original[1]["activity"],"output":original[1]["output"],"startedAt":original[1]["startedAt"],"endedAt":"2026-09-09T20:00:17Z"}
                ]);
                if case["status"] == "paused" {
                    run["workers"].as_array_mut().unwrap().truncate(1);
                }
            }
            if let Some(status) = case["status"].as_str() {
                run["status"] = json!(status)
            }
            if let Some(status) = case["workerStatus"].as_str() {
                run["workers"][0]["status"] = json!(status)
            }
            if case["failed"] == true {
                run["endedAt"] = json!("2026-09-09T20:00:09Z");
                for w in run["workers"].as_array_mut().unwrap() {
                    w["status"] = json!("failed");
                    w["error"] = json!("Provider error");
                    w["output"] = json!("");
                    w["usage"] = json!({"totalTokens":0});
                    w["startedAt"] = json!("2026-09-09T20:00:00Z");
                    w["endedAt"] = json!("2026-09-09T20:00:01Z");
                }
            }
            if case["status"] == "paused" {
                run["endedAt"] = run["startedAt"].clone();
                for w in run["workers"].as_array_mut().unwrap() {
                    w.as_object_mut().unwrap().remove("startedAt");
                    w.as_object_mut().unwrap().remove("endedAt");
                    w.as_object_mut().unwrap().remove("usage");
                }
            }
            if let Some(prompt) = case["workerPrompt"].as_str() {
                run["workers"][0]["prompt"] = json!(prompt);
            }
            if let Some(outcome) = case["workerOutcome"].as_str() {
                let field = if case["failed"] == true {
                    "error"
                } else {
                    "output"
                };
                run["workers"][0][field] = json!(outcome);
            }
            let count = case["runCount"].as_u64().unwrap_or(2) as usize;
            let runs = if case["kind"] == "empty" {
                vec![]
            } else if case["kind"] == "multi" {
                let mut values = (0..count)
                    .map(|i| {
                        let mut r = run.clone();
                        r["id"] = json!(format!("wf-fixture-{i}"));
                        if i > 0 {
                            r["name"] = json!(
                                ["ui-pause", "ui-controls", "ui-reference", "ui-reference"][i - 1]
                            );
                        }
                        r
                    })
                    .collect::<Vec<_>>();
                for i in 1..values.len() {
                    if case["freeze"] == true && i < 3 {
                        values[i]["status"] = json!("completed");
                        values[i]["workers"].as_array_mut().unwrap().truncate(1);
                        values[i]["endedAt"] = json!(if i == 1 {
                            "2026-09-09T20:00:11Z"
                        } else {
                            "2026-09-09T20:03:17Z"
                        });
                    } else {
                        let id = values[i]["id"].clone();
                        values[i] = fixture["run"].clone();
                        values[i]["id"] = id;
                        if i == count - 1 {
                            values[i]["endedAt"] = json!("2026-09-09T20:00:09Z");
                            for worker in values[i]["workers"].as_array_mut().unwrap() {
                                worker["status"] = json!("failed");
                                worker["usage"] = json!({"totalTokens":0});
                            }
                        }
                    }
                }
                values
            } else {
                vec![run]
            };
            let mut v = WorkflowView::new(json!({"runs":runs}), None);
            v.state.screen = match case["screen"].as_str() {
                Some("detail") => WorkflowScreen::Detail,
                Some("save") => WorkflowScreen::Save,
                Some("effort") => WorkflowScreen::Effort,
                _ => v.state.screen,
            };
            v.state.save_name = "ui-reference".into();
            if let Some(scope) = case["scope"].as_str() {
                v.state.save_scope = scope.into()
            }
            if let Some(filter) = case["filter"].as_str() {
                v.state.filter = filter.into();
                v.state.focus = WorkflowFocus::Workers
            }
            if let Some(effort) = case["effort"].as_str() {
                v.state.effort = effort.into()
            }
            v.styles_enabled = case["capture"].as_str().unwrap()[..2]
                .parse::<u8>()
                .unwrap()
                >= 12;
            let output = text(&v, 160, 48);
            let area = Rect::new(0, 0, 160, 48);
            let mut buffer = Buffer::empty(area);
            v.render(area, &mut buffer);
            if !v.styles_enabled {
                for cell in &buffer.content {
                    assert_eq!(cell.fg, ratatui::style::Color::Reset);
                    assert_eq!(cell.bg, ratatui::style::Color::Reset);
                    assert!(cell.modifier.is_empty());
                }
            }
            if let Some(directory) = std::env::var_os("ULTRACODE_NATIVE_CELL_OUTPUT") {
                let cells: Vec<_> = (0..48)
                    .flat_map(|row| {
                        let buffer = &buffer;
                        (0..160).map(move |column| {
                            let cell = &buffer[(column, row)];
                            json!({
                                "row": row, "column": column, "grapheme": cell.symbol(),
                                "foreground": format!("{:?}", cell.fg),
                                "background": format!("{:?}", cell.bg),
                                "bold": cell.modifier.contains(Modifier::BOLD),
                                "italic": cell.modifier.contains(Modifier::ITALIC),
                                "underline": cell.modifier.contains(Modifier::UNDERLINED),
                                "inverse": cell.modifier.contains(Modifier::REVERSED),
                            })
                        })
                    })
                    .collect();
                let directory = std::path::PathBuf::from(directory);
                std::fs::create_dir_all(&directory).unwrap();
                std::fs::write(
                    directory.join(format!("{}.json", case["capture"].as_str().unwrap())),
                    serde_json::to_vec(&json!({"columns":160,"rows":48,"case":case,"stylesEnabled":v.styles_enabled,"cells":cells}))
                        .unwrap(),
                )
                .unwrap();
            }
            assert_eq!(
                normalized(&output),
                normalized(golden(case["capture"].as_str().unwrap())),
                "{}",
                case["capture"]
            );
        }
    }
    #[test]
    fn filter_key_matches_reference_focus_and_available_statuses() {
        for status in ["completed", "failed"] {
            let mut v = WorkflowView::new(
                json!({"runs":[{"id":"reference","status":"completed","phases":["Count"],
                    "workers":[{"id":"count","phase":"Count","status":status}]}]}),
                None,
            );
            let initial = v.state.clone();
            assert_eq!(v.handle_key(KeyEvent::from(KeyCode::Char('f'))), None);
            assert_eq!(v.state, initial, "Phase focus ignores the filter key");
            v.handle_key(KeyEvent::from(KeyCode::Enter));
            assert!(text(&v, 160, 48).contains(" · f filter ·"));
            v.handle_key(KeyEvent::from(KeyCode::Char('f')));
            assert_eq!(v.state.filter, status);
            v.handle_key(KeyEvent::from(KeyCode::Char('f')));
            assert_eq!(v.state.filter, "all");
            v.handle_key(KeyEvent::from(KeyCode::Enter));
            let detail = v.state.clone();
            v.handle_key(KeyEvent::from(KeyCode::Char('f')));
            assert_eq!(v.state, detail, "Worker detail ignores the filter key");
        }
    }
    #[test]
    fn changing_phase_resets_worker_filter_like_reference() {
        let mut v = WorkflowView::new(
            json!({"runs":[{"id":"mixed","status":"completed","phases":["Count","Finish"],
                "workers":[{"id":"count","phase":"Count","status":"completed"},
                    {"id":"finish","phase":"Finish","status":"failed"}]}]}),
            None,
        );
        v.handle_key(KeyEvent::from(KeyCode::Enter));
        v.handle_key(KeyEvent::from(KeyCode::Char('f')));
        for (key, status) in [(KeyCode::Down, "failed"), (KeyCode::Up, "completed")] {
            v.handle_key(KeyEvent::from(KeyCode::Esc));
            v.handle_key(KeyEvent::from(key));
            assert_eq!(v.state.filter, "all");
            assert_eq!(v.workers().len(), 1);
            v.handle_key(KeyEvent::from(KeyCode::Enter));
            v.handle_key(KeyEvent::from(KeyCode::Char('f')));
            assert_eq!(v.state.filter, status);
        }
    }
    #[test]
    fn reducer_navigation_and_running_worker_restart() {
        let mut v = WorkflowView::new(json!({"runs":[run()]}), None);
        assert_eq!(v.handle_key(KeyEvent::from(KeyCode::Char('r'))), None);
        v.handle_key(KeyEvent::from(KeyCode::Right));
        v.state.worker = 1;
        v.handle_key(KeyEvent::from(KeyCode::Right));
        assert!(
            matches!(v.handle_key(KeyEvent::from(KeyCode::Char('r'))),Some(WorkflowAction::RestartWorker{worker_id,..})if worker_id=="b")
        );
        v.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(v.state.expanded);
    }
    #[test]
    fn detail_scroll_matches_reference_limits_and_range_hint() {
        let mut item = run();
        item["workers"][0]["output"] = json!(
            (1..=80)
                .map(|line| format!("line {line:03}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let mut view = WorkflowView::new(json!({"runs":[item]}), None);
        view.state.screen = WorkflowScreen::Detail;
        let top = text(&view, 160, 20);
        assert!(top.contains("j/k scroll"));
        assert!(top.contains("1–9 of 90 ↓"));
        for _ in 0..200 {
            view.handle_key(KeyEvent::from(KeyCode::Char('j')));
        }
        let bottom = text(&view, 160, 20);
        assert!(bottom.contains("line 080"));
        assert!(bottom.contains("↑ 82–90 of 90"));
        view.handle_key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(text(&view, 160, 20), bottom);
        view.handle_key(KeyEvent::from(KeyCode::Char('k')));
        let previous = text(&view, 160, 20);
        assert!(previous.contains("line 079"));
        assert!(!previous.contains("line 080"));
        assert!(previous.contains("↑ 81–89 of 90 ↓"));
        let expanded_viewport = text(&view, 160, 110);
        assert!(expanded_viewport.contains("line 080"));
        assert!(!expanded_viewport.contains("j/k scroll"));
        view.handle_key(KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(text(&view, 160, 110), expanded_viewport);
    }
    #[test]
    fn narrow_tall_and_sanitized() {
        let phases = (0..80)
            .map(|i| json!({"name":format!("phase-{i}")}))
            .collect::<Vec<_>>();
        let workers=(0..80).map(|i|json!({"id":format!("w-{i}"),"label":format!("worker-{i}"),"phase":format!("phase-{i}"),"status":"completed","model":"gpt\u{001b}]8;;bad\u{0007}-5"})).collect::<Vec<_>>();
        let mut r = run();
        r["phases"] = json!(phases);
        r["workers"] = json!(workers);
        let mut v = WorkflowView::new(json!({"runs":[r]}), None);
        v.state.phase = 73;
        let out = text(&v, 24, 8);
        assert_eq!(out.lines().count(), 8);
        assert!(out.lines().all(|l| l.chars().count() <= 24));
        assert!(!out.contains("bad"));
    }
    #[test]
    fn selected_items_remain_visible_and_invalid_restart_is_inert() {
        let workers=(0..80).map(|i|json!({"id":format!("w-{i}"),"label":format!("worker-{i}"),"phase":"Inspect","status":"completed","model":"gpt-5"})).collect::<Vec<_>>();
        let phases = (0..80)
            .map(|i| json!({"name":format!("phase-{i}")}))
            .collect::<Vec<_>>();
        let phase_workers=(0..80).map(|i|json!({"id":format!("p-{i}"),"label":format!("phase-worker-{i}"),"phase":format!("phase-{i}"),"status":"completed","model":"gpt-5"})).collect::<Vec<_>>();
        let mut v = WorkflowView::new(
            json!({"runs":[{"id":"r","status":"running","phases":phases,"workers":phase_workers}]}),
            None,
        );
        v.state.phase = 73;
        assert!(text(&v, 72, 16).contains("phase-73"));
        v = WorkflowView::new(
            json!({"runs":[{"id":"r","status":"running","phases":[{"name":"Inspect"}],"workers":workers}]}),
            None,
        );
        v.state.screen = WorkflowScreen::Detail;
        v.state.focus = WorkflowFocus::Workers;
        v.state.worker = 73;
        assert!(text(&v, 72, 16).contains("worker-73"));
        v.update(json!({"runs":[{"id":"r","status":"running","phases":[{"name":"Inspect"}],"workers":[{"id":"","label":"bad","phase":"Inspect","status":"running"}]}]}));
        v.state.worker = 0;
        assert_eq!(v.handle_key(KeyEvent::from(KeyCode::Char('r'))), None);
    }

    #[test]
    fn save_uses_resolved_path_and_returns_to_origin() {
        let mut v = WorkflowView::new(
            json!({"runs":[run()],"savePaths":{"project":"/repo/.codex/workflows/ui-reference.js","user":"/home/mako/.codex/workflows/ui-reference.js"}}),
            None,
        );
        v.state.screen = WorkflowScreen::Detail;
        v.handle_key(KeyEvent::from(KeyCode::Char('s')));
        assert!(text(&v, 100, 20).contains("/repo/.codex/workflows/ui-reference.js"));
        v.handle_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(v.state.screen, WorkflowScreen::Detail);

        v.handle_key(KeyEvent::from(KeyCode::Char('s')));
        v.handle_key(KeyEvent::from(KeyCode::Tab));
        assert!(text(&v, 100, 20).contains("/home/mako/.codex/workflows/ui-reference.js"));
        assert!(matches!(
            v.handle_key(KeyEvent::from(KeyCode::Enter)),
            Some(WorkflowAction::Save { scope, .. }) if scope == "user"
        ));
        assert_eq!(v.state.screen, WorkflowScreen::Detail);
    }

    #[test]
    fn effort_reducer_confirms_selection_and_ignores_release() {
        let mut view = WorkflowView::new(json!({"runs":[]}), None);
        view.state.screen = WorkflowScreen::Effort;
        view.state.effort = "medium".into();
        let mut release = KeyEvent::from(KeyCode::Right);
        release.kind = KeyEventKind::Release;
        view.handle_key(release);
        assert_eq!(view.state.effort, "medium");
        view.handle_key(KeyEvent::from(KeyCode::Right));
        assert_eq!(view.state.effort, "high");
        assert_eq!(
            view.handle_key(KeyEvent::from(KeyCode::Enter)),
            Some(WorkflowAction::SetEffort {
                effort: "high".into()
            })
        );
    }

    #[test]
    fn expanded_activity_wraps_and_keeps_unicode_borders_aligned() {
        let mut item = run();
        item["workers"][0]["label"] = json!("工😀worker");
        item["workers"][0]["activity"] = json!([{"type":"command_execution","command":["cargo","test","--very-long-option"],"status":"completed","exitCode":0,"input":{"unsafe":"\u{1b}[31mred"},"aggregatedOutput":"first line with many words that must wrap inside the detail pane\nsecond line"}]);
        let mut v = WorkflowView::new(json!({"runs":[item]}), None);
        v.state.screen = WorkflowScreen::Detail;
        v.state.expanded = true;
        let output = text(&v, 52, 28);
        assert!(output.contains("cargo test"), "{output}");
        assert!(output.contains("--very-long-option"), "{output}");
        assert!(output.contains("Output:"));
        assert!(!output.contains("\u{1b}"));
        assert!(
            output
                .lines()
                .all(|line| UnicodeWidthStr::width(line) <= 52)
        );
        for line in output.lines().filter(|line| line.contains('│')) {
            assert_eq!(UnicodeWidthStr::width(line), 47, "{output}");
        }
    }
}
