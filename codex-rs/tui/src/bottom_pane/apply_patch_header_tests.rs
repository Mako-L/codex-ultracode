use super::*;
use codex_protocol::ThreadId;
use codex_utils_absolute_path::test_support::PathBufExt;
use codex_utils_absolute_path::test_support::test_path_buf;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::collections::HashMap;

#[test]
fn save_approval_without_patch_shows_known_destination_directory() {
    let root = test_path_buf("/project/.codex/workflows");
    let target = root.join("fix-fixture.js");
    let request = ApplyPatchApprovalRequest {
        thread_id: ThreadId::new(),
        thread_label: None,
        id: "save".into(),
        reason: Some(format!(
            "Save workflow 'fix-fixture' to {}",
            target.display()
        )),
        grant_root: Some(root.clone()),
        cwd: test_path_buf("/project").abs(),
        changes: HashMap::new(),
    };
    let area = Rect::new(0, 0, 120, 4);
    let mut buffer = Buffer::empty(area);
    build_header(&request).render(area, &mut buffer);
    let rendered: String = buffer
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(rendered.contains(&target.display().to_string()));
    assert!(rendered.contains(&format!("Destination directory: {}", root.display())));
    assert!(!rendered.contains("unavailable"));
    let lines = buffer
        .content
        .chunks(area.width as usize)
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .replace(&target.display().to_string(), "<workflow-file>")
        .replace(&root.display().to_string(), "<workflow-directory>");
    insta::assert_snapshot!(lines);
}
