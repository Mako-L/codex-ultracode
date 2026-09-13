use super::styled_lines;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

#[test]
fn worker_duration_alignment_gap_is_plain() {
    let line = "   │   1 Count   │  ✔ count        gpt-5 · 40.2k tok        7s   │";
    let area = Rect::new(0, 0, 90, 1);
    let mut buffer = Buffer::empty(area);
    Paragraph::new(styled_lines(vec![line.into()], Some(12))).render(area, &mut buffer);
    let gap = unicode_width::UnicodeWidthStr::width(line.split("tok").next().unwrap()) as u16 + 3;
    for column in gap..gap + 4 {
        assert_eq!(buffer[(column, 0)].fg, Color::Reset, "{column}");
    }
    for column in gap + 4..gap + 8 {
        assert_eq!(buffer[(column, 0)].fg, Color::Gray, "{column}");
    }
    assert_eq!(buffer[(gap - 1, 0)].fg, Color::Gray);
    assert_eq!(buffer[(gap + 8, 0)].fg, Color::Gray);
}

#[test]
fn selected_phase_separators_are_plain() {
    let area = Rect::new(0, 0, 50, 1);
    let mut buffer = Buffer::empty(area);
    Paragraph::new(styled_lines(
        vec!["   │ ❯ 1 Count  0/1 │   │".into()],
        Some(12),
    ))
    .render(area, &mut buffer);
    for column in [8, 14, 15] {
        assert_eq!(buffer[(column, 0)].fg, Color::Reset, "{column}");
    }
    for column in [5, 6, 7, 9, 16] {
        assert_eq!(buffer[(column, 0)].fg, Color::LightBlue, "{column}");
    }
}

#[test]
fn stopped_worker_layout_padding_does_not_inherit_metadata_color() {
    for model in ["gpt-5", "gpt-5.6-luna", "模型"] {
        let content = format!("   │   1 Count   │  ◌ count        {model} · stopped");
        let line = format!("{content}     │");
        let area = Rect::new(0, 0, 90, 1);
        let mut buffer = Buffer::empty(area);
        Paragraph::new(styled_lines(vec![line], Some(12))).render(area, &mut buffer);
        let end = unicode_width::UnicodeWidthStr::width(content.as_str()) as u16;
        assert_eq!(buffer[(end - 1, 0)].fg, Color::Gray, "{model}");
        for column in end..end + 4 {
            assert_eq!(buffer[(column, 0)].fg, Color::Reset, "{model}: {column}");
        }
        assert_eq!(buffer[(end + 4, 0)].fg, Color::White, "{model}");
    }
}

#[test]
fn frame_padding_and_outer_margin_match_captured_cell_styles() {
    let lines = vec![
        "  ──────────".into(),
        "   title".into(),
        "   description".into(),
        "   │     │   │".into(),
    ];
    let area = Rect::new(0, 0, 20, 4);
    let mut buffer = Buffer::empty(area);
    Paragraph::new(styled_lines(lines, Some(12))).render(area, &mut buffer);
    for row in 0..4 {
        assert_eq!(buffer[(0, row)].fg, Color::Reset);
        assert_eq!(buffer[(1, row)].fg, Color::Reset);
    }
    for column in [2, 3, 4, 8, 9, 10, 12, 13] {
        assert_eq!(
            buffer[(column, 3)].fg,
            Color::White,
            "frame padding column {column}"
        );
    }
    for column in [5, 6, 7, 11] {
        assert_eq!(buffer[(column, 3)].fg, Color::Reset);
    }
}

#[test]
fn summary_field_padding_keeps_middle_gap_plain() {
    let lines = vec![
        "  ──────────".into(),
        "   title".into(),
        "   description    0/1".into(),
    ];
    let area = Rect::new(0, 0, 30, 3);
    let mut buffer = Buffer::empty(area);
    Paragraph::new(styled_lines(lines, Some(12))).render(area, &mut buffer);
    for column in [2, 13, 18, 21] {
        assert_eq!(buffer[(column, 2)].fg, Color::Gray);
    }
    for column in [0, 1, 14, 15, 16, 17] {
        assert_eq!(buffer[(column, 2)].fg, Color::Reset);
    }
}

#[test]
fn phase_and_picker_spacing_match_component_styles() {
    let lines = vec![
        "   │   2 Finish     │   │".into(),
        "   ❯ ✘ example  1 agent".into(),
    ];
    let area = Rect::new(0, 0, 40, 2);
    let mut buffer = Buffer::empty(area);
    Paragraph::new(styled_lines(lines, Some(12))).render(area, &mut buffer);
    for column in [5, 6, 8, 15, 16, 17, 18] {
        assert_eq!(buffer[(column, 0)].fg, Color::Reset);
    }
    assert_eq!(buffer[(7, 0)].fg, Color::Gray);
    assert_eq!(buffer[(19, 0)].fg, Color::White);
    assert_eq!(buffer[(6, 1)].symbol(), " ");
    assert_eq!(buffer[(6, 1)].fg, Color::LightBlue);
}

#[test]
fn summary_preserves_unicode_whitespace_before_layout_gap() {
    let lines = vec![
        "  ──────────".into(),
        "   title".into(),
        "   café\u{a0}    0/1".into(),
    ];
    let area = Rect::new(0, 0, 30, 3);
    let mut buffer = Buffer::empty(area);
    Paragraph::new(styled_lines(lines, Some(12))).render(area, &mut buffer);
    assert_eq!(buffer[(6, 2)].symbol(), "é");
    assert_eq!(buffer[(7, 2)].symbol(), "\u{a0}");
}

#[test]
fn frame_preserves_multibyte_indentation() {
    let area = Rect::new(0, 0, 10, 1);
    let mut buffer = Buffer::empty(area);
    Paragraph::new(styled_lines(vec!["\u{a0}│ │".into()], None)).render(area, &mut buffer);
    assert_eq!(buffer[(0, 0)].symbol(), "\u{a0}");
    assert_eq!(buffer[(1, 0)].symbol(), "│");
}
