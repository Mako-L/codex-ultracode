//! Terminal styles measured from the Claude workflow reference captures.

use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

#[cfg(test)]
#[path = "workflow_view_style_tests.rs"]
mod tests;

fn padded_style(line: String, style: Style, margin: usize) -> Line<'static> {
    let prefix = line
        .bytes()
        .take(margin)
        .take_while(|byte| *byte == b' ')
        .count();
    Line::from(vec![
        Span::raw(line[..prefix].to_owned()),
        Span::styled(line[prefix..].to_owned(), style),
    ])
}

fn summary_style(line: String, style: Style, margin: usize) -> Line<'static> {
    let prefix = line
        .bytes()
        .take(margin)
        .take_while(|byte| *byte == b' ')
        .count();
    if let Some(end) = line.rfind("  ").map(|offset| offset + 2) {
        let start = line[..end].trim_end_matches(' ').len();
        if start > prefix && end - start >= 2 {
            return Line::from(vec![
                Span::raw(line[..prefix].to_owned()),
                Span::styled(line[prefix..start].to_owned(), style),
                Span::raw(line[start..end].to_owned()),
                Span::styled(format!("{} ", &line[end..]), style),
            ]);
        }
    }
    padded_style(line, style, margin)
}

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
    let margin = if dialog { 3 } else { 2 };
    let plain = Style::default();
    let gray = plain.fg(Color::Gray);
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let trimmed = line.trim_start();
            if trimmed.starts_with('╰') && line.contains(" of ") {
                let start = line.rfind('─').map_or(0, |offset| offset + '─'.len_utf8());
                let end = line.rfind('╯').unwrap_or(line.len());
                let prefix = line
                    .bytes()
                    .take(margin)
                    .take_while(|byte| *byte == b' ')
                    .count();
                return Line::from(vec![
                    Span::raw(line[..prefix].to_owned()),
                    Span::styled(line[prefix..start].to_owned(), plain.fg(Color::White)),
                    Span::styled(line[start..end].to_owned(), gray),
                    Span::styled(line[end..].to_owned(), plain.fg(Color::White)),
                ]);
            }
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
                if !dialog && divider.is_some_and(|start| index == start + 2) {
                    return summary_style(line, style, margin);
                }
                return padded_style(line, style, margin);
            }
            if trimmed.starts_with('│') {
                let mut spans = Vec::new();
                let cells: Vec<_> = line.split('│').collect();
                for (column, cell) in cells.iter().copied().enumerate() {
                    if column > 0 {
                        spans.push(Span::styled("│", plain.fg(Color::White)));
                    }
                    if column == 0 {
                        let prefix = cell.char_indices().last().map_or(0, |(offset, _)| offset);
                        spans.push(Span::raw(cell[..prefix].to_owned()));
                        spans.push(Span::styled(
                            cell[prefix..].to_owned(),
                            plain.fg(Color::White),
                        ));
                        continue;
                    }
                    let framed = column + 1 < cells.len();
                    let cell = if framed && cell.starts_with(' ') {
                        spans.push(Span::styled(" ", plain.fg(Color::White)));
                        &cell[1..]
                    } else {
                        cell
                    };
                    let trailing = framed && cell.ends_with(' ');
                    let cell = if trailing {
                        &cell[..cell.len() - 1]
                    } else {
                        cell
                    };
                    let cell = if column == 2
                        && worker_label_width.is_some()
                        && cell.starts_with(' ')
                        && cell.trim_start().starts_with(['✔', '✘', '✻', 'Ⅱ', '◌'])
                    {
                        spans.push(Span::styled(" ", plain.fg(Color::LightBlue)));
                        &cell[1..]
                    } else {
                        cell
                    };
                    if column == 1
                        && let Some(separator) = cell
                            .strip_prefix("❯ ")
                            .and_then(|rest| rest.find(' ').map(|offset| offset + "❯ ".len()))
                    {
                        let selected = plain.fg(Color::LightBlue);
                        spans.push(Span::styled(cell[..separator].to_owned(), selected));
                        spans.push(Span::raw(" "));
                        let rest = &cell[separator + 1..];
                        let content = rest.trim_end_matches(' ');
                        let count = content.split_whitespace().last().unwrap_or_default();
                        let count_offset = if count.split_once('/').is_some_and(|(a, b)| {
                            !a.is_empty()
                                && !b.is_empty()
                                && a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit())
                        }) {
                            content.len() - count.len()
                        } else {
                            content.len()
                        };
                        let label_end = content[..count_offset].trim_end_matches(' ').len();
                        spans.push(Span::styled(content[..label_end].to_owned(), selected));
                        spans.push(Span::raw(content[label_end..count_offset].to_owned()));
                        spans.push(Span::styled(content[count_offset..].to_owned(), selected));
                        spans.push(Span::raw(rest[content.len()..].to_owned()));
                    } else if column > 0 && cell.contains('❯') {
                        spans.push(Span::styled(cell.to_owned(), plain.fg(Color::LightBlue)));
                    } else if column == 1
                        && cell.trim_start().starts_with(|c: char| c.is_ascii_digit())
                    {
                        let leading = cell.len() - cell.trim_start().len();
                        let content = cell.trim();
                        let split = content.find(' ').unwrap_or(content.len());
                        spans.push(Span::raw(cell[..leading].to_owned()));
                        spans.extend(symbols(&content[..split], gray));
                        if split < content.len() {
                            spans.push(Span::raw(" "));
                            spans.extend(symbols(&content[split + 1..], gray));
                        }
                        spans.push(Span::raw(cell[leading + content.len()..].to_owned()));
                    } else if column == 2 && cell.contains(" · stopped") {
                        let content = cell.trim_end_matches(' ');
                        let mut start = 0;
                        for (offset, symbol) in content.char_indices() {
                            let width = UnicodeWidthStr::width(&content[..offset]);
                            if symbol == ' '
                                && worker_label_width.is_some()
                                && (width == 1
                                    || Some(width) == worker_label_width.map(|width| width + 2))
                            {
                                spans.extend(symbols(&content[start..offset], gray));
                                spans.push(Span::raw(" "));
                                start = offset + 1;
                            }
                        }
                        spans.extend(symbols(&content[start..], gray));
                        spans.push(Span::raw(cell[content.len()..].to_owned()));
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
                        let metadata = &cell[offset..];
                        if let Some(gap_end) = metadata
                            .trim_end_matches(' ')
                            .rfind("  ")
                            .map(|offset| offset + 2)
                        {
                            let gap_start = metadata[..gap_end].trim_end_matches(' ').len();
                            // Captured duration fields are right-aligned in at least six cells.
                            let duration_width =
                                UnicodeWidthStr::width(metadata[gap_end..].trim_end_matches(' '));
                            let gap_end = gap_end
                                .saturating_sub(6_usize.saturating_sub(duration_width))
                                .max(gap_start);
                            spans.push(Span::styled(metadata[..gap_start].to_owned(), gray));
                            spans.push(Span::raw(metadata[gap_start..gap_end].to_owned()));
                            spans.push(Span::styled(metadata[gap_end..].to_owned(), gray));
                        } else {
                            spans.push(Span::styled(metadata.to_owned(), gray));
                        }
                    } else {
                        spans.extend(symbols(cell, plain));
                    }
                    if trailing {
                        spans.push(Span::styled(" ", plain.fg(Color::White)));
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
                    let selected = line[..offset].contains('❯');
                    let styled_start = name_start - usize::from(selected);
                    let mut spans = symbols(&line[..styled_start], plain);
                    spans.push(Span::styled(
                        line[styled_start..name_start + split].to_owned(),
                        if selected {
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
