use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn completed_outcome_uses_result_instead_of_retained_error() {
    let rows = detail_rows(
        &json!({"status": "completed", "error": "old failure",
        "text": "", "output": "actual result"}),
        100,
        false,
    );
    assert_eq!(rows.last().unwrap(), "  actual result");
}

#[test]
fn worker_list_can_restart_the_selected_running_worker() {
    let mut view = WorkflowView::new(
        json!({"runs": [{
            "id": "r", "status": "running", "phases": [{"name": "Work"}],
            "workers": [{"id": "active", "phase": "Work", "status": "running"}],
        }]}),
        None,
    );
    view.state.focus = WorkflowFocus::Workers;
    assert_eq!(
        view.handle_key(KeyEvent::from(KeyCode::Char('r'))),
        Some(WorkflowAction::RestartWorker {
            run_id: "r".into(),
            worker_id: "active".into()
        })
    );
    assert!(view.lines(160, 48).join("\n").contains("r restart"));
}

#[test]
fn activity_shows_only_last_three_tool_summaries() {
    let activity: Vec<_> = (1..=5)
        .map(|index| {
            json!({
                "type": "commandExecution", "id": index.to_string(),
                "toolName": "exec_command", "summary": format!("command {index}"),
            })
        })
        .collect();
    let activity: Vec<_> = activity
        .iter()
        .flat_map(|item| {
            let mut started = item.clone();
            started["summary"] = json!("pending");
            [started, item.clone()]
        })
        .collect();
    let rows = detail_rows(
        &json!({"status": "completed", "activity": activity}),
        100,
        false,
    );
    let start = rows
        .iter()
        .position(|row| row.starts_with("Activity"))
        .unwrap();
    let end = rows.iter().position(|row| row == "Outcome").unwrap();
    insta::assert_snapshot!(rows[start..end].join("\n").trim_end(), @r"
    Activity · last 3 of 5 tool calls
      exec_command(command 3)
      exec_command(command 4)
      exec_command(command 5)
    ");
}

#[test]
fn prompt_expansion_does_not_expand_tool_payloads() {
    let worker = json!({"status": "completed", "prompt": "short", "activity": [{
        "type": "commandExecution", "toolName": "exec_command", "command": "echo test",
        "input": {"command": "echo test"}, "aggregatedOutput": "test",
    }]});
    assert_eq!(
        detail_rows(&worker, 100, false),
        detail_rows(&worker, 100, true)
    );
}

#[test]
fn picker_lifecycle_keys_do_not_control_an_active_run() {
    let mut view = WorkflowView::new(
        json!({"runs": [
            {"id": "r", "status": "running"}, {"id": "other", "status": "paused"},
        ]}),
        None,
    );
    let before = view.state.clone();
    for key in ['p', 'x', 'r'] {
        assert_eq!(view.handle_key(KeyEvent::from(KeyCode::Char(key))), None);
        assert_eq!(view.state, before);
    }
}

#[test]
fn stopped_or_completed_worker_cannot_be_stopped_again_in_an_active_run() {
    for status in ["completed", "failed", "stopped", "skipped", "blocked"] {
        let mut view = WorkflowView::new(
            json!({"runs": [{
                "id": "r", "status": "running", "phases": [{"name": "Work"}],
                "workers": [
                    {"id": "done", "phase": "Work", "status": status},
                    {"id": "active", "phase": "Work", "status": "running"},
                ],
            }]}),
            None,
        );
        view.state.focus = WorkflowFocus::Workers;
        assert_eq!(view.handle_key(KeyEvent::from(KeyCode::Char('x'))), None);
        assert!(!view.lines(160, 48).join("\n").contains("x stop"));
        view.state.worker = 1;
        assert!(
            matches!(view.handle_key(KeyEvent::from(KeyCode::Char('x'))),
            Some(WorkflowAction::StopRun { worker_id: Some(id), .. }) if id == "active")
        );
    }
}

#[test]
fn long_prompt_collapses_to_two_lines_and_expands_on_request() {
    let worker = json!({"status": "queued", "prompt": "first\nsecond\nthird\nfourth"});
    let collapsed = detail_rows(&worker, 100, false);
    insta::assert_snapshot!(collapsed.join("\n"), @r"
    ◌ Queued

    Prompt · 4 lines · ⏎ expand
      first
      second
      … 2 more lines

    Outcome
      Waiting for an agent slot.
    ");
    let expanded = detail_rows(&worker, 100, true);
    assert!(expanded.iter().any(|line| line == "Prompt · 4 lines"));
    assert!(expanded.iter().any(|line| line == "  fourth"));
    assert!(!expanded.iter().any(|line| line.contains("more lines")));
}

#[test]
fn filter_cycles_present_statuses_in_reference_order() {
    let workers: Vec<_> = ["stopped", "completed", "queued", "failed", "running"]
        .into_iter()
        .map(|status| json!({"id": status, "phase": "Work", "status": status}))
        .collect();
    let mut view = WorkflowView::new(
        json!({"runs": [{
            "id": "r", "status": "running", "phases": [{"name": "Work"}], "workers": workers,
        }]}),
        None,
    );
    view.state.focus = WorkflowFocus::Workers;
    for expected in ["running", "queued", "failed", "completed", "stopped", "all"] {
        view.handle_key(KeyEvent::from(KeyCode::Char('f')));
        assert_eq!(view.state.filter, expected);
        if matches!(expected, "completed" | "stopped") {
            let label = if expected == "completed" {
                "done"
            } else {
                "interrupted"
            };
            assert!(
                view.lines(160, 48)
                    .join("\n")
                    .contains(&format!("f filter: {label}"))
            );
        }
    }
}

#[test]
fn queued_detail_omits_activity_and_explains_unavailable_prompt() {
    let rows = detail_rows(&json!({"status": "queued"}), 100, false);
    insta::assert_snapshot!(rows.join("\n"), @r"
    ◌ Queued

    Prompt
      Available once the agent starts.

    Outcome
      Waiting for an agent slot.
    ");
}

#[test]
fn running_detail_explains_pending_prompt_activity_and_outcome() {
    let rows = detail_rows(&json!({"status": "running"}), 100, false);
    insta::assert_snapshot!(rows.join("\n"), @r"
    ✻ Running

    Prompt
      Not available yet (agent still running).

    Activity
      No tool calls yet.

    Outcome
      Still running…
    ");
}

#[test]
fn terminal_details_explain_stopped_skipped_and_empty_results() {
    for (status, expected) in [
        (
            "stopped",
            "  The workflow stopped before this agent finished.",
        ),
        ("skipped", "  Skipped by user."),
        ("completed", "  (empty)"),
        ("failed", "  failed"),
    ] {
        let rows = detail_rows(&json!({"status": status}), 100, false);
        assert_eq!(rows.last().unwrap(), expected);
    }
}
