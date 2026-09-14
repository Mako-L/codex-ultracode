use super::*;
use pretty_assertions::assert_eq;
use ratatui::style::Color;
use ratatui::style::Modifier;
use serde_json::json;

fn detail_view(status: &str) -> WorkflowView {
    let mut view = WorkflowView::new(
        json!({"runs": [{
            "id": "r", "status": "running", "phases": [{"name": "Work"}], "workers": [{
                "id": "w", "label": "worker", "phase": "Work", "status": status,
                "model": "gpt-5", "usage": {"totalTokens": 2}, "toolCalls": 1,
                "prompt": "visible prompt", "error": "visible failure", "output": "visible result",
                "activity": [{"id": "call", "type": "commandExecution", "toolName": "exec_command", "command": "echo example"}],
            }],
        }]}),
        None,
    );
    view.state.screen = WorkflowScreen::Detail;
    view
}

fn text_cell<'a>(buffer: &'a Buffer, text: &str) -> &'a ratatui::buffer::Cell {
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            if text.chars().enumerate().all(|(offset, character)| {
                x as usize + offset < buffer.area.width as usize
                    && buffer[(x + offset as u16, y)].symbol() == character.to_string()
            }) {
                return &buffer[(x, y)];
            }
        }
    }
    panic!("missing rendered text: {text}");
}

#[test]
fn worker_detail_styles_follow_reference_semantics() {
    let view = detail_view("failed");
    let area = Rect::new(0, 0, 160, 48);
    let mut buffer = Buffer::empty(area);
    view.render(area, &mut buffer);
    let facts = [
        "Failed",
        "gpt-5",
        "2 tok",
        "Prompt",
        "visible prompt",
        "Activity",
        "exec_command",
        "Outcome",
        "visible failure",
    ]
    .map(|text| {
        let cell = text_cell(&buffer, text);
        format!(
            "{text}: {:?}, bold={}, dim={}",
            cell.fg,
            cell.modifier.contains(Modifier::BOLD),
            cell.modifier.contains(Modifier::DIM)
        )
    })
    .join("\n");
    insta::assert_snapshot!(facts, @r"
    Failed: LightRed, bold=true, dim=false
    gpt-5: Reset, bold=false, dim=true
    2 tok: Reset, bold=false, dim=true
    Prompt: Reset, bold=true, dim=true
    visible prompt: Reset, bold=false, dim=true
    Activity: Reset, bold=true, dim=true
    exec_command: Reset, bold=false, dim=true
    Outcome: Reset, bold=true, dim=true
    visible failure: LightRed, bold=false, dim=false
    ");
}

#[test]
fn scrolled_failure_keeps_outcome_color_without_visible_heading() {
    let mut view = detail_view("failed");
    view.snapshot["runs"][0]["workers"][0]["error"] = json!(
        (1..=40)
            .map(|line| format!("failure line {line:02}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    view.state.scroll = 200;
    let area = Rect::new(0, 0, 160, 20);
    let mut buffer = Buffer::empty(area);
    view.render(area, &mut buffer);
    assert_eq!(text_cell(&buffer, "failure line 40").fg, Color::LightRed);
}

#[test]
fn empty_placeholder_is_dim_but_literal_result_is_not() {
    let mut view = detail_view("completed");
    let area = Rect::new(0, 0, 160, 48);
    for (output, expected_dim) in [(json!("(empty)"), false), (Value::Null, true)] {
        view.snapshot["runs"][0]["workers"][0]["output"] = output;
        let mut buffer = Buffer::empty(area);
        view.render(area, &mut buffer);
        assert_eq!(
            text_cell(&buffer, "(empty)")
                .modifier
                .contains(Modifier::DIM),
            expected_dim
        );
    }
}
