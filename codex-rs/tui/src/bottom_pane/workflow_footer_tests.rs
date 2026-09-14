use super::*;
use crate::app_event::WorkflowEvent;
use crossterm::event::KeyModifiers;
use tokio::sync::mpsc::unbounded_channel;

#[test]
fn workflow_footer_keyboard_focus_opens_and_returns_without_losing_input() {
    let (tx, mut rx) = unbounded_channel();
    let mut pane = super::tests::test_pane_with_disable_paste_burst(AppEventSender::new(tx), true);
    pane.set_status_line_enabled(true);
    pane.set_status_line(Some(Line::from("gpt-6-astra xhigh · project")));
    pane.set_workflow_status(Some(Line::from(vec![
        " ●".green(),
        " repair · 0/1 agents · running · /workflows".into(),
    ])));
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    pane.handle_key_event(key(KeyCode::Down));
    assert!(pane.workflow_status_focused);
    let area = Rect::new(0, 0, 96, 8);
    assert!(pane.cursor_pos(area).is_none());
    let mut buf = Buffer::empty(area);
    pane.render(area, &mut buf);
    insta::assert_snapshot!(
        "workflow_footer_focused",
        super::tests::snapshot_buffer(&buf)
    );
    pane.handle_key_event(key(KeyCode::Up));
    assert!(!pane.workflow_status_focused);
    pane.handle_key_event(key(KeyCode::Down));
    pane.handle_key_event(key(KeyCode::Esc));
    assert!(!pane.workflow_status_focused);
    pane.handle_key_event(key(KeyCode::Down));
    pane.handle_key_event(key(KeyCode::Enter));
    assert!(!pane.workflow_status_focused);
    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::Workflow(WorkflowEvent::Open { effort: None }))
    ));
    pane.handle_key_event(key(KeyCode::Down));
    pane.handle_key_event(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT));
    assert!(!pane.workflow_status_focused);
    assert_eq!(pane.composer.current_text(), "X");
    pane.handle_key_event(key(KeyCode::Down));
    assert!(!pane.workflow_status_focused);
    pane.handle_key_event(key(KeyCode::Backspace));
    pane.handle_key_event(key(KeyCode::Down));
    assert!(pane.workflow_status_focused);
    pane.set_workflow_status(None);
    assert!(!pane.workflow_status_focused);
    assert!(pane.cursor_pos(area).is_some());
}
