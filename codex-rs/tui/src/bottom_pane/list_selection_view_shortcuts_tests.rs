use super::*;
use crate::app_event::AppEvent;
use crossterm::event::KeyCode;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tokio::sync::mpsc::unbounded_channel;

#[test]
fn footer_shortcut_replaces_picker_without_accepting_or_cancelling() {
    let (sender, _receiver) = unbounded_channel::<AppEvent>();
    let shortcut_calls = Arc::new(AtomicUsize::new(0));
    let selection_calls = Arc::new(AtomicUsize::new(0));
    let cancellation_calls = Arc::new(AtomicUsize::new(0));
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            additional_shortcuts: vec![(crate::key_hint::ctrl(KeyCode::Char('g')).into(), {
                let calls = shortcut_calls.clone();
                Box::new(move |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                })
            })],
            items: vec![SelectionItem {
                name: "Yes, run it".into(),
                actions: vec![{
                    let calls = selection_calls.clone();
                    Box::new(move |_| {
                        calls.fetch_add(1, Ordering::SeqCst);
                    })
                }],
                ..Default::default()
            }],
            on_cancel: Some({
                let calls = cancellation_calls.clone();
                Box::new(move |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                })
            }),
            ..Default::default()
        },
        AppEventSender::new(sender),
        crate::keymap::RuntimeKeymap::defaults().list,
    );
    view.handle_key_event(KeyEvent::new_with_kind(
        KeyCode::Char('g'),
        KeyModifiers::CONTROL,
        KeyEventKind::Release,
    ));
    assert_eq!(shortcut_calls.load(Ordering::SeqCst), 0);
    assert!(view.completion.is_none());

    view.handle_key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
    assert_eq!(shortcut_calls.load(Ordering::SeqCst), 1);
    assert_eq!(selection_calls.load(Ordering::SeqCst), 0);
    assert_eq!(cancellation_calls.load(Ordering::SeqCst), 0);
    assert!(matches!(view.completion, Some(ViewCompletion::Accepted)));
}

fn feedback_item(
    name: &str,
    placeholder: &'static str,
    submissions: Arc<std::sync::Mutex<Vec<Option<String>>>>,
) -> SelectionItem {
    SelectionItem {
        name: name.into(),
        feedback: Some(SelectionFeedback::new(
            placeholder,
            String::new(),
            false,
            Box::new(move |feedback, _| {
                submissions.lock().unwrap().push(feedback);
            }),
        )),
        dismiss_on_select: true,
        ..Default::default()
    }
}

fn feedback_view(items: Vec<SelectionItem>) -> ListSelectionView {
    let (sender, _receiver) = unbounded_channel::<AppEvent>();
    ListSelectionView::new(
        SelectionViewParams {
            items,
            ..Default::default()
        },
        AppEventSender::new(sender),
        crate::keymap::RuntimeKeymap::defaults().list,
    )
}

#[test]
fn tab_opens_feedback_and_enter_submits_trimmed_text() {
    let submissions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut view = feedback_view(vec![feedback_item(
        "Yes, run it",
        "tell Codex what to do next",
        submissions.clone(),
    )]);

    view.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    let feedback = view.items[0].feedback.as_ref().unwrap();
    assert!(feedback.expanded);
    assert_eq!(feedback.placeholder, "tell Codex what to do next");

    for c in "  continue here  ".chars() {
        view.handle_key_event(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(
        &*submissions.lock().unwrap(),
        &[Some("continue here".into())]
    );
    assert!(matches!(view.completion, Some(ViewCompletion::Accepted)));
}

#[test]
fn feedback_rows_keep_independent_text_and_collapse_only_empty_inactive_input() {
    let submissions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut view = feedback_view(vec![
        feedback_item(
            "Yes, run it",
            "tell Codex what to do next",
            submissions.clone(),
        ),
        feedback_item("No", "tell Codex what to do differently", submissions),
    ]);

    view.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    view.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    view.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert!(view.items[0].feedback.as_ref().unwrap().expanded);
    assert_eq!(view.items[0].feedback.as_ref().unwrap().text(), "a");

    view.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert!(view.items[1].feedback.as_ref().unwrap().expanded);
    view.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert!(!view.items[1].feedback.as_ref().unwrap().expanded);
    assert_eq!(view.items[0].feedback.as_ref().unwrap().text(), "a");
}

#[test]
fn escape_cancels_consent_without_submitting_feedback() {
    let submissions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut view = feedback_view(vec![feedback_item(
        "No",
        "tell Codex what to do differently",
        submissions.clone(),
    )]);

    view.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    view.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    view.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(submissions.lock().unwrap().is_empty());
    assert!(matches!(view.completion, Some(ViewCompletion::Cancelled)));
}

#[test]
fn paste_targets_active_feedback_and_tab_is_inert_on_plain_rows() {
    let submissions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let plain_calls = Arc::new(AtomicUsize::new(0));
    let mut view = feedback_view(vec![
        feedback_item("Yes, run it", "tell Codex what to do next", submissions),
        SelectionItem {
            name: "View raw script".into(),
            actions: vec![{
                let calls = plain_calls.clone();
                Box::new(move |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                })
            }],
            ..Default::default()
        },
    ]);

    view.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert!(view.handle_paste("pasted feedback".into()));
    assert_eq!(
        view.items[0].feedback.as_ref().unwrap().text(),
        "pasted feedback"
    );
    view.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    view.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(plain_calls.load(Ordering::SeqCst), 0);
    assert!(view.completion.is_none());
}

#[test]
fn constrained_height_keeps_choices_visible_below_long_consent_source() {
    let submissions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (sender, _receiver) = unbounded_channel::<AppEvent>();
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            header: Box::new(Paragraph::new(
                (0..80)
                    .map(|line| Line::from(format!("source line {line}")))
                    .collect::<Vec<_>>(),
            )),
            items: vec![
                feedback_item(
                    "Yes, run it",
                    "tell Codex what to do next",
                    submissions.clone(),
                ),
                feedback_item("No", "tell Codex what to do differently", submissions),
            ],
            ..Default::default()
        },
        AppEventSender::new(sender),
        crate::keymap::RuntimeKeymap::defaults().list,
    );
    view.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert!(view.handle_paste("修正 α".into()));
    let area = Rect::new(5, 3, 42, 8);
    let mut buffer = Buffer::empty(area);
    view.render(area, &mut buffer);
    let rendered = (area.y..area.y + area.height)
        .map(|row| {
            (area.x..area.x + area.width)
                .map(|col| buffer[(col, row)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Yes, run it"), "{rendered}");
    assert!(rendered.contains("No"), "{rendered}");
    let (wide_x, wide_y) = (area.y..area.y + area.height)
        .find_map(|row| {
            (area.x..area.x + area.width)
                .find(|col| buffer[(*col, row)].symbol() == "修")
                .map(|col| (col, row))
        })
        .expect("first wide feedback glyph");
    assert_eq!(buffer[(wide_x + 2, wide_y)].symbol(), "正");
    assert_eq!(buffer[(wide_x + 5, wide_y)].symbol(), "α");
    let cursor = view.cursor_pos(area).expect("visible feedback cursor");
    assert!(
        cursor.0 >= area.x
            && cursor.0 < area.x + area.width
            && cursor.1 >= area.y
            && cursor.1 < area.y + area.height,
        "{cursor:?}"
    );
}

#[test]
fn scrolled_feedback_overlay_clears_generic_description_tail() {
    let submissions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut view = feedback_view(vec![feedback_item(
        "Yes, run it",
        "tell Codex what to do next",
        submissions,
    )]);
    view.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert!(view.handle_paste(
        "first generic description line\nsecond generic description line\nthird generic description line\nfourth generic description line\nfifth generic description line\nz"
            .into()
    ));

    let area = Rect::new(7, 4, 36, 5);
    let mut buffer = Buffer::empty(area);
    view.render(area, &mut buffer);
    let feedback_area = view.feedback_area.get().expect("feedback render area");
    let cursor = view.cursor_pos(area).expect("scrolled feedback cursor");
    assert!(cursor.0 > feedback_area.x);
    assert_eq!(buffer[(cursor.0 - 1, cursor.1)].symbol(), "z");
    for col in cursor.0..feedback_area.x + feedback_area.width {
        assert_eq!(
            buffer[(col, cursor.1)].symbol(),
            " ",
            "stale cell at ({col}, {})",
            cursor.1
        );
    }
}
