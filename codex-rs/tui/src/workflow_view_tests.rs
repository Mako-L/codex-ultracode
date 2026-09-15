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
    let fixture: Value = serde_json::from_str(include_str!("workflow_view_fixtures.json")).unwrap();
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
                    serde_json::to_vec(&json!({"columns":160,"rows":48,"case":case,"stylesEnabled":v.styles_enabled,"snapshot":v.snapshot,"cells":cells}))
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
    v.snapshot["runs"][0]["status"] = json!("running");
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
fn terminal_lifecycle_keys_do_not_dispatch_or_change_the_view() {
    for status in ["completed", "failed", "stopped"] {
        for screen in [
            WorkflowScreen::Picker,
            WorkflowScreen::Overview,
            WorkflowScreen::Detail,
        ] {
            for focus in [WorkflowFocus::Phases, WorkflowFocus::Workers] {
                let mut item = run();
                item["status"] = json!(status);
                for worker in item["workers"].as_array_mut().unwrap() {
                    worker["status"] = json!(status);
                }
                let mut view = WorkflowView::new(json!({"runs":[item]}), None);
                view.state.screen = screen.clone();
                view.state.focus = focus.clone();
                let before_state = view.state.clone();
                let before = text(&view, 160, 48);
                for key in ['p', 'x', 'r'] {
                    assert_eq!(view.handle_key(KeyEvent::from(KeyCode::Char(key))), None);
                    assert_eq!(view.state, before_state);
                    assert_eq!(text(&view, 160, 48), before);
                }
            }
        }
    }
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
fn activity_summaries_clip_and_keep_unicode_borders_aligned() {
    let mut item = run();
    item["workers"][0]["label"] = json!("工😀worker");
    item["workers"][0]["activity"] = json!([{"type":"command_execution","command":["cargo","test","--very-long-option"],"status":"completed","exitCode":0,"input":{"unsafe":"\u{1b}[31mred"},"aggregatedOutput":"first line with many words that must wrap inside the detail pane\nsecond line"}]);
    let mut v = WorkflowView::new(json!({"runs":[item]}), None);
    v.state.screen = WorkflowScreen::Detail;
    v.state.expanded = true;
    let output = text(&v, 52, 28);
    assert!(output.contains("exec_command("), "{output}");
    assert!(!output.contains("--very-long-option"), "{output}");
    assert!(!output.contains("Output:"));
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
