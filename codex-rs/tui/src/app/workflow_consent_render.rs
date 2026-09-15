use crate::app_event::WorkflowPreviewMode;
use crate::render::highlight::highlight_code_to_lines;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use crate::workflow_source::WorkflowSourcePreview;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Padding;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use unicode_width::UnicodeWidthStr;

fn sanitize(text: &str) -> String {
    crate::history_cell::sanitize_user_text(text.to_owned().into()).into_owned()
}

fn prompt_excerpt(prompt: &str) -> String {
    if prompt.encode_utf16().count() <= 60 {
        return sanitize(prompt);
    }
    let mut units = 0;
    let prefix: String = prompt
        .chars()
        .take_while(|ch| {
            units += ch.len_utf16();
            units <= 59
        })
        .collect();
    format!("{}…", sanitize(&prefix))
}

fn raw_lines(source: &str) -> Vec<Line<'static>> {
    let mut suffixes = Vec::new();
    let truncated: Vec<String> = source
        .split('\n')
        .map(|line| {
            let mut units = 0;
            let prefix: String = line
                .chars()
                .take_while(|ch| {
                    units += ch.len_utf16();
                    units <= 2000
                })
                .collect();
            let omitted = line.encode_utf16().count() - prefix.encode_utf16().count();
            suffixes.push(omitted);
            prefix
        })
        .collect();
    let mut lines = highlight_code_to_lines(&truncated.join("\n"), "javascript");
    for (line, omitted) in lines.iter_mut().zip(suffixes) {
        if omitted > 0 {
            line.spans.push(format!(" … [+{omitted} chars]").dim());
        }
    }
    lines
}

pub(super) fn workflow_preview_header(
    preview: &WorkflowSourcePreview,
    mode: WorkflowPreviewMode,
) -> Box<dyn Renderable> {
    let mut children: Vec<Box<dyn Renderable>> = Vec::new();
    if let Some(metadata) = &preview.metadata {
        let description = sanitize(&metadata.description);
        if !description.is_empty() {
            let multiline = description.contains('\n') || description.width() > 80;
            let paragraph = Paragraph::new(description.bold()).wrap(Wrap { trim: false });
            children.push(Box::new(if multiline {
                paragraph.block(
                    Block::default()
                        .borders(Borders::LEFT)
                        .border_style(Style::default().dim())
                        .padding(Padding::new(1, 0, 0, 0)),
                )
            } else {
                paragraph
            }));
            children.push(Box::new(Paragraph::new(Line::default())));
        }
    }
    let consent = preview.consent.as_ref();
    let phases = consent.and_then(|consent| consent.phases.as_ref());
    if matches!(mode, WorkflowPreviewMode::Summary)
        && preview.validation_error.is_none()
        && !consent.is_some_and(|consent| consent.source.withheld)
        && let Some(phases) = phases
    {
        let mut lines = vec![
            "This dynamic workflow will spin up multiple subagents across the following phases:"
                .into(),
        ];
        for (index, phase) in phases.iter().enumerate() {
            let mut row = vec![format!("  {}. {}", index + 1, sanitize(&phase.title)).into()];
            if let Some(detail) = &phase.detail {
                row.push(format!(" — {}", sanitize(detail)).dim());
            }
            lines.push(row.into());
            if !phase.prompts.is_empty() {
                let prompts = phase
                    .prompts
                    .iter()
                    .take(2)
                    .map(|prompt| format!("· \"{}\"", prompt_excerpt(prompt)))
                    .collect::<Vec<_>>()
                    .join("  ");
                let remaining = phase.prompts.len().saturating_sub(2);
                let suffix = if remaining > 0 {
                    format!("  +{remaining} more")
                } else {
                    String::new()
                };
                lines.push(format!("     {prompts}{suffix}").dim().into());
            }
        }
        children.push(Box::new(Paragraph::new(lines).wrap(Wrap { trim: false })));
    } else {
        if let Some(error) = &preview.validation_error {
            children.push(Box::new(
                Paragraph::new(vec![
                    vec!["Validation error: ".red().bold(), sanitize(error).red()].into(),
                    Line::default(),
                ])
                .wrap(Wrap { trim: false }),
            ));
        }
        let source = consent.map_or_else(
            || sanitize(&preview.source),
            |consent| consent.source.text.clone(),
        );
        if consent.is_some_and(|consent| consent.source.withheld) {
            children.push(Box::new(
                Paragraph::new(vec![source.dim().into(), Line::default()])
                    .wrap(Wrap { trim: false }),
            ));
        } else {
            children.push(Box::new(
                Paragraph::new(raw_lines(&source))
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_set(ratatui::symbols::border::Set {
                                horizontal_top: "╌",
                                horizontal_bottom: "╌",
                                vertical_left: "╎",
                                vertical_right: "╎",
                                top_left: " ",
                                top_right: " ",
                                bottom_left: " ",
                                bottom_right: " ",
                            })
                            .border_style(Style::default().dim())
                            .padding(Padding::horizontal(1)),
                    )
                    .wrap(Wrap { trim: false }),
            ));
        }
    }
    if let Some(args) = consent.and_then(|consent| consent.args.as_ref())
        && !args.text.is_empty()
    {
        children.push(Box::new(Paragraph::new(Line::default())));
        let mut values = args.text.split('\n');
        let mut lines = vec![Line::from(vec![
            "args: ".bold().dim(),
            values.next().unwrap_or_default().to_owned().dim(),
        ])];
        lines.extend(values.map(|line| Line::from(line.to_owned().dim())));
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        children.push(Box::new(if args.needs_gutter && !args.withheld {
            paragraph.block(
                Block::default()
                    .borders(Borders::LEFT)
                    .border_style(Style::default().dim())
                    .padding(Padding::new(1, 0, 0, 0)),
            )
        } else {
            paragraph
        }));
    }
    Box::new(ColumnRenderable::with(children))
}

#[cfg(test)]
#[path = "workflow_consent_render_tests.rs"]
mod tests;
