use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
#[cfg(test)]
pub(crate) use ratatui::buffer::Buffer;
#[cfg(test)]
pub(crate) use ratatui::layout::Rect;
#[cfg(test)]
pub(crate) use ratatui::widgets::Widget;
use serde_json::Value;
use std::cell::Cell;
#[cfg(test)]
pub(crate) use unicode_width::UnicodeWidthStr;

#[path = "workflow_worker_timing.rs"]
mod worker_timing;

#[path = "workflow_worker_activity.rs"]
mod worker_activity;

const FILTERS: [&str; 8] = [
    "all",
    "running",
    "queued",
    "failed",
    "completed",
    "skipped",
    "blocked",
    "stopped",
];

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

#[path = "workflow_view_updates.rs"]
mod updates;

#[path = "workflow_view_render.rs"]
mod render;
#[cfg(test)]
#[path = "workflow_view_tests.rs"]
mod view_tests;

pub(crate) use render::cell_cut;
#[cfg(test)]
pub(crate) use render::cell_fit;
pub(crate) use render::clean;
#[cfg(test)]
pub(crate) use render::detail_rows;
pub(crate) use render::elapsed_at;
pub(crate) use render::render_effort;
pub(crate) use render::render_overview;
pub(crate) use render::render_picker;
pub(crate) use render::render_save;
pub(crate) use render::token_text;
pub(crate) use render::token_total;
#[cfg(test)]
pub(crate) use render::worker_row;
#[cfg(test)]
pub(crate) use render::wrap;

pub(crate) struct WorkflowView {
    snapshot: Value,
    detail_scroll_limit: Cell<usize>,
    detail_section: Cell<crate::workflow_view_style::detail::Section>,
    pub(crate) styles_enabled: bool,
    pub state: WorkflowViewState,
}

impl WorkflowView {
    pub(crate) fn new(snapshot: Value, view: Option<WorkflowViewState>) -> Self {
        let count = snapshot["runs"].as_array().map_or(0, Vec::len);
        Self {
            snapshot,
            detail_scroll_limit: Cell::new(0),
            detail_section: Cell::new(crate::workflow_view_style::detail::Section::Metadata),
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
        if self.state.screen == WorkflowScreen::Picker
            && matches!(key.code, KeyCode::Char('p' | 'x' | 'r'))
        {
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
                    .filter(|w| {
                        matches!(
                            w["status"].as_str(),
                            Some("running" | "queued" | "preparing")
                        ) || (status == "paused"
                            && matches!(w["status"].as_str(), Some("stopped" | "interrupted")))
                    })
                    .and_then(|w| w["id"].as_str())
                    .filter(|id| !id.is_empty())
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
            && status == "running"
            && (self.state.screen == WorkflowScreen::Detail
                || (self.state.screen == WorkflowScreen::Overview
                    && self.state.focus == WorkflowFocus::Workers))
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

#[cfg(test)]
#[path = "workflow_view_restart_tests.rs"]
mod restart_tests;

#[cfg(test)]
#[path = "workflow_view_metadata_tests.rs"]
mod metadata_tests;

#[cfg(test)]
#[path = "workflow_view_reference_states_tests.rs"]
mod reference_states_tests;

#[cfg(test)]
#[path = "workflow_detail_style_tests.rs"]
mod detail_style_tests;
