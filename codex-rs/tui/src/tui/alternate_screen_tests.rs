use crossterm::cursor::MoveTo;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::execute;
use crossterm::terminal::EnterAlternateScreen;
use pretty_assertions::assert_eq;
use ratatui::layout::Rect;
use tokio::sync::mpsc::unbounded_channel;

use super::restore_main_screen;
use crate::app_event::AppEvent;
use crate::app_event::WorkflowEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::ListSelectionView;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::custom_terminal::Terminal;
use crate::pager_overlay::Overlay;
use crate::render::renderable::Renderable;
use crate::test_backend::VT100Backend;
use crate::tui::TuiEvent;
use std::io::Write;

#[tokio::test]
async fn source_pager_escape_repaints_numeric_selection_on_restored_main_screen() {
    let width = 120;
    let height = 40;
    let viewport = Rect::new(/*x*/ 0, /*y*/ 28, width, /*height*/ 12);
    let (tx, mut events) = unbounded_channel();
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            title: Some("Run workflow /proof?".into()),
            items: [
                "Run once",
                "Run and remember for this project",
                "View raw script",
                "Cancel",
            ]
            .into_iter()
            .map(|name| SelectionItem {
                name: name.into(),
                dismiss_on_select: false,
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::Workflow(WorkflowEvent::ViewScript {
                        thread_id: "thread".into(),
                        source: "\n\n\n\nreturn 'saved source';".into(),
                    }));
                })],
                ..Default::default()
            })
            .collect(),
            ..Default::default()
        },
        AppEventSender::new(tx),
        crate::keymap::RuntimeKeymap::defaults().list,
    );
    let new_main_screen = || {
        let mut terminal =
            Terminal::with_options(VT100Backend::new(width, height)).expect("terminal");
        terminal.set_viewport_area(viewport);
        execute!(terminal.backend_mut(), MoveTo(/*column*/ 0, /*row*/ 26))
            .expect("position saved history");
        write!(
            terminal.backend_mut(),
            "Workflow saved /repo/.codex/workflows/proof.js"
        )
        .expect("saved history");
        terminal
    };
    let draw_consent = |terminal: &mut Terminal<VT100Backend>, view: &ListSelectionView| {
        terminal
            .draw(|frame| view.render(viewport, frame.buffer_mut()))
            .expect("draw consent");
    };
    let mut terminal = new_main_screen();
    draw_consent(&mut terminal, &view);
    let initial = terminal.backend().vt100().screen().contents();
    assert!(initial.contains("› 1. Run once"));
    assert_eq!(initial.matches('›').count(), 1);

    // The shortcut changes selection before a main-screen frame can be painted.
    view.handle_key_event(KeyEvent::from(KeyCode::Char('3')));
    let AppEvent::Workflow(WorkflowEvent::ViewScript { source, .. }) =
        events.try_recv().expect("source preview event")
    else {
        panic!("Expected source preview");
    };
    assert!(!view.is_complete());
    let mut keymap = crate::keymap::RuntimeKeymap::defaults().pager;
    keymap.close.insert(0, crate::key_hint::plain(KeyCode::Esc));
    let Overlay::Static(mut pager) = Overlay::new_static_with_lines(
        source.lines().map(|line| line.to_owned().into()).collect(),
        "Workflow source".into(),
        keymap,
    ) else {
        panic!("Expected static pager");
    };
    execute!(terminal.backend_mut(), EnterAlternateScreen).expect("enter source pager");
    terminal.set_viewport_area(Rect::new(/*x*/ 0, /*y*/ 0, width, height));
    terminal.clear().expect("clear alternate screen");
    terminal
        .draw(|frame| pager.render(frame.area(), frame.buffer_mut()))
        .expect("draw pager");
    assert!(
        terminal
            .backend()
            .vt100()
            .screen()
            .contents()
            .contains("Workflow source")
    );

    let mut tui = crate::tui::test_support::make_test_tui().expect("key event tui");
    pager
        .handle_event(&mut tui, TuiEvent::Key(KeyEvent::from(KeyCode::Esc)))
        .expect("close source pager");
    assert!(pager.is_done());
    restore_main_screen(&mut terminal, Some(viewport));
    assert_eq!(terminal.backend().vt100().screen().contents(), initial);
    draw_consent(&mut terminal, &view);

    let mut expected = new_main_screen();
    draw_consent(&mut expected, &view);
    let restored = terminal.backend().vt100().screen().contents();
    assert_eq!(restored.matches('›').count(), 1);
    // An untouched blank and a written space occupy the same terminal cell.
    // Compare every displayed glyph and attribute, not vt100's written extent.
    let presentation = |cell: &vt100::Cell| {
        (
            if cell.contents().is_empty() {
                " "
            } else {
                cell.contents()
            }
            .to_owned(),
            cell.fgcolor(),
            cell.bgcolor(),
            [
                cell.bold(),
                cell.dim(),
                cell.italic(),
                cell.underline(),
                cell.inverse(),
                cell.is_wide(),
                cell.is_wide_continuation(),
            ],
        )
    };
    for row in 0..height {
        for column in 0..width {
            assert_eq!(
                presentation(
                    terminal
                        .backend()
                        .vt100()
                        .screen()
                        .cell(row, column)
                        .unwrap()
                ),
                presentation(
                    expected
                        .backend()
                        .vt100()
                        .screen()
                        .cell(row, column)
                        .unwrap()
                ),
                "restored cell ({column}, {row})",
            );
        }
    }
    assert!(restored.contains("› 3. View raw script"));
}
