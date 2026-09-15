use super::*;
use crate::workflow_source::WorkflowConsentPresentation;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::json;

fn preview(
    phases: serde_json::Value,
    args: serde_json::Value,
    withheld: bool,
) -> WorkflowSourcePreview {
    let mut preview = WorkflowSourcePreview::inline("thread", &json!({"script": "return 'raw';"}))
        .expect("valid consent fixture");
    preview.consent = Some(serde_json::from_value::<WorkflowConsentPresentation>(json!({
        "phases": phases,
        "args": args,
        "source": {"text": if withheld { "approval unavailable" } else { "return 'raw';" }, "withheld": withheld, "originalLength": 13}
    })).expect("valid consent fixture"));
    preview.metadata = Some(
        serde_json::from_value(
            json!({"name":"hidden-name","title":"hidden-title","description":"Inspect source."}),
        )
        .expect("valid consent fixture"),
    );
    preview
}

fn render(preview: &WorkflowSourcePreview, mode: WorkflowPreviewMode, width: u16) -> String {
    let view = workflow_preview_header(preview, mode);
    let area = Rect::new(7, 3, width, view.desired_height(width));
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);
    (area.y..area.bottom())
        .map(|y| {
            (area.x..area.right())
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn summary_shows_reference_description_phases_and_prompt_limit() {
    let preview = preview(
        json!([{"title":"Inspect","detail":"Read source","prompts":["one","two","three"]}]),
        serde_json::Value::Null,
        false,
    );
    let text = render(&preview, WorkflowPreviewMode::Summary, 100);
    assert!(text.starts_with("Inspect source.\n"));
    assert!(text.contains(
        "This dynamic workflow will spin up multiple subagents across the following phases:"
    ));
    assert!(text.contains("  1. Inspect — Read source"));
    assert!(text.contains("     · \"one\"  · \"two\"  +1 more"));
    assert!(!text.contains("hidden-name"));
    assert!(!text.contains("hidden-title"));
}

#[test]
fn no_summary_and_withheld_source_render_raw_without_phase_claims() {
    for preview in [
        preview(serde_json::Value::Null, serde_json::Value::Null, false),
        preview(
            json!([{"title":"Inspect","prompts":[]}]),
            serde_json::Value::Null,
            true,
        ),
    ] {
        let text = render(&preview, WorkflowPreviewMode::Summary, 70);
        assert!(!text.contains("following phases"));
        assert_eq!(
            text.contains("╌"),
            !preview
                .consent
                .as_ref()
                .expect("valid consent fixture")
                .source
                .withheld
        );
    }
}

#[test]
fn raw_long_lines_truncate_individually_with_dim_suffix() {
    let lines = raw_lines(&format!("{}\nreturn 1;", "x".repeat(2003)));
    assert_eq!(
        lines[0].to_string(),
        format!("{} … [+3 chars]", "x".repeat(2000))
    );
    assert_eq!(lines[1].to_string(), "return 1;");
    assert!(
        lines[0]
            .spans
            .last()
            .expect("valid consent fixture")
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::DIM)
    );
}

#[test]
fn prompt_excerpt_respects_utf16_boundary() {
    assert_eq!(prompt_excerpt(&"x".repeat(60)), "x".repeat(60));
    assert_eq!(
        prompt_excerpt(&"x".repeat(61)),
        format!("{}…", "x".repeat(59))
    );
    assert_eq!(
        prompt_excerpt(&"😀".repeat(31)),
        format!("{}…", "😀".repeat(29))
    );
}

#[test]
fn args_follow_body_and_raw_wraps_inside_border_at_nonzero_origin() {
    let preview = preview(
        serde_json::Value::Null,
        json!({"text":"first\nsecond","needsGutter":true,"withheld":false}),
        false,
    );
    let text = render(&preview, WorkflowPreviewMode::Raw, 18);
    assert!(text.contains("args:"));
    assert!(text.contains("│ args: first"));
    assert!(text.contains("│ second"));
    assert!(text.contains("return 'raw';"));
}
