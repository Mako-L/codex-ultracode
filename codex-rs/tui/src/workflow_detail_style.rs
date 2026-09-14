//! Semantic styles for worker detail rows, including scrolled section content.

use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Span;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Section {
    #[default]
    Metadata,
    Prompt,
    Activity,
    Outcome,
}

impl Section {
    fn heading(text: &str) -> Option<(Self, &'static str)> {
        [
            (Self::Prompt, "Prompt"),
            (Self::Activity, "Activity"),
            (Self::Outcome, "Outcome"),
        ]
        .into_iter()
        .find(|(_, label)| {
            text == *label
                || text
                    .strip_prefix(*label)
                    .is_some_and(|tail| tail.starts_with(" ·"))
        })
    }
}

pub(crate) struct Context<'a> {
    pub(crate) status: &'a str,
    pub(crate) section: Section,
    pub(crate) empty_result: bool,
}

pub(crate) fn section_before(rows: &[String], offset: usize) -> Section {
    rows.iter()
        .take(offset)
        .filter_map(|row| Section::heading(row).map(|(section, _)| section))
        .next_back()
        .unwrap_or_default()
}

pub(super) fn cell_spans(cell: &str, context: &mut Context<'_>) -> Vec<Span<'static>> {
    let content = cell.trim_end_matches(' ');
    if content.is_empty() {
        return vec![Span::raw(cell.to_string())];
    }
    let plain = Style::default();
    let dim = plain.add_modifier(Modifier::DIM);
    let mut spans = Vec::new();
    if let Some((section, label)) = Section::heading(content) {
        context.section = section;
        spans.push(Span::styled(
            label.to_string(),
            dim.add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(content[label.len()..].to_string(), dim));
    } else if context.section == Section::Metadata && content.starts_with(['✔', '✘', '✻', '◌', 'Ⅱ'])
    {
        let color = match context.status {
            "completed" => Color::LightGreen,
            "failed" => Color::LightRed,
            "blocked" => Color::LightBlue,
            _ => Color::Gray,
        };
        let glyph_end = content.chars().next().map_or(0, char::len_utf8);
        let label_end = content.find(" · ").unwrap_or(content.len());
        let label_start = (glyph_end + content[glyph_end..].len()
            - content[glyph_end..].trim_start().len())
        .min(label_end);
        spans.push(Span::styled(
            content[..glyph_end].to_string(),
            plain.fg(color),
        ));
        spans.push(Span::raw(content[glyph_end..label_start].to_string()));
        spans.push(Span::styled(
            content[label_start..label_end].to_string(),
            plain.fg(color).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(content[label_end..].to_string(), dim));
    } else {
        let style = match context.section {
            Section::Outcome if matches!(context.status, "failed" | "blocked") => {
                plain.fg(Color::LightRed)
            }
            Section::Outcome if context.status == "completed" && !context.empty_result => plain,
            _ => dim,
        };
        spans.push(Span::styled(content.to_string(), style));
    }
    spans.push(Span::raw(cell[content.len()..].to_string()));
    spans
}
