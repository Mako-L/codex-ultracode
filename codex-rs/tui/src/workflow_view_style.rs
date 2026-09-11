//! Terminal styles measured from the Claude workflow reference captures.

use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

fn symbols(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (offset, symbol) in text.char_indices() {
        let color = match symbol {
            '✔' => Color::LightGreen,
            '✘' => Color::LightRed,
            '✻' | 'Ⅱ' => Color::LightBlue,
            '◌' => Color::Gray,
            _ => continue,
        };
        if start < offset {
            spans.push(Span::styled(text[start..offset].to_owned(), base));
        }
        start = offset + symbol.len_utf8();
        spans.push(Span::styled(symbol.to_string(), base.fg(color)));
    }
    if start < text.len() {
        spans.push(Span::styled(text[start..].to_owned(), base));
    }
    spans
}

pub(crate) fn styled_lines(
    lines: Vec<String>,
    worker_label_width: Option<usize>,
) -> Vec<Line<'static>> {
    let divider = lines
        .iter()
        .position(|line| line.trim_start().starts_with("──────────"))
        .or_else(|| lines.iter().position(|line| line.starts_with("▔▔▔▔▔▔▔▔▔▔")));
    let dialog = divider.is_some_and(|index| lines[index].contains('▔'));
    let plain = Style::default();
    let gray = plain.fg(Color::Gray);
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let trimmed = line.trim_start();
            let style = if line.starts_with("▔▔▔▔▔▔▔▔▔▔") {
                Some(plain.fg(Color::LightBlue))
            } else if Some(index) == divider {
                Some(if dialog {
                    plain.fg(Color::LightBlue)
                } else {
                    plain.fg(Color::White)
                })
            } else if divider.is_some_and(|start| index == start + 1) {
                Some(
                    plain
                        .fg(if dialog {
                            Color::LightCyan
                        } else {
                            Color::LightBlue
                        })
                        .add_modifier(Modifier::BOLD),
                )
            } else if divider.is_some_and(|start| index == start + 2) {
                Some(gray)
            } else if trimmed.starts_with('↑') || trimmed.starts_with("Esc") {
                Some(gray.add_modifier(Modifier::ITALIC))
            } else if trimmed.starts_with('╭') || trimmed.starts_with('╰') {
                Some(plain.fg(Color::White))
            } else {
                None
            };
            if let Some(style) = style {
                return Line::from(Span::styled(line, style));
            }
            if trimmed.starts_with('│') {
                let mut spans = Vec::new();
                for (column, cell) in line.split('│').enumerate() {
                    if column > 0 {
                        spans.push(Span::styled("│", plain.fg(Color::White)));
                    }
                    if column > 0 && cell.contains('❯') {
                        spans.push(Span::styled(cell.to_owned(), plain.fg(Color::LightBlue)));
                    } else if (column == 1
                        && cell.trim_start().starts_with(|c: char| c.is_ascii_digit()))
                        || (column == 2 && cell.contains(" · stopped"))
                    {
                        spans.extend(symbols(cell, gray));
                    } else if column == 1 {
                        let count = cell.split_whitespace().last().unwrap_or_default();
                        if count.split_once('/').is_some_and(|(a, b)| {
                            !a.is_empty()
                                && !b.is_empty()
                                && a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit())
                        }) {
                            let offset = cell.rfind(count).unwrap_or(cell.len());
                            spans.extend(symbols(&cell[..offset], plain));
                            spans.push(Span::styled(cell[offset..].to_owned(), gray));
                        } else {
                            spans.extend(symbols(cell, plain));
                        }
                    } else if column == 2
                        && worker_label_width.is_some()
                        && cell.trim_start().starts_with(['✔', '✘', '✻', 'Ⅱ', '◌'])
                    {
                        let width = UnicodeWidthStr::width(cell)
                            - UnicodeWidthStr::width(cell.trim_start())
                            + 2
                            + worker_label_width.unwrap_or_default()
                            + 1;
                        let offset = cell
                            .char_indices()
                            .find_map(|(offset, _)| {
                                (UnicodeWidthStr::width(&cell[..offset]) >= width).then_some(offset)
                            })
                            .unwrap_or(cell.len());
                        spans.extend(symbols(&cell[..offset], plain));
                        spans.push(Span::styled(cell[offset..].to_owned(), gray));
                    } else {
                        spans.extend(symbols(cell, plain));
                    }
                }
                return Line::from(spans);
            }
            // Picker rows contain a status marker, name, then aligned agent metadata.
            if let Some((offset, marker)) = line
                .char_indices()
                .find(|(_, c)| matches!(c, '✔' | '✘' | '✻' | 'Ⅱ' | '◌'))
            {
                let name_start = offset + marker.len_utf8() + 1;
                if let Some(rest) = line.get(name_start..)
                    && let Some(split) = rest.find("  ")
                    && rest[split..].contains(" agent")
                {
                    let mut spans = symbols(&line[..name_start], plain);
                    spans.push(Span::styled(
                        rest[..split].to_owned(),
                        if line[..offset].contains('❯') {
                            plain.fg(Color::LightBlue)
                        } else {
                            plain
                        },
                    ));
                    spans.push(Span::styled(rest[split..].to_owned(), gray));
                    return Line::from(spans);
                }
            }
            Line::from(symbols(&line, plain))
        })
        .collect()
}
