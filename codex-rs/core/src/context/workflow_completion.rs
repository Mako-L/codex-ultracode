use codex_protocol::models::ContentItemKind;

use super::ContextualUserFragment;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkflowCompletion {
    run_id: String,
    summary: String,
}

impl WorkflowCompletion {
    pub(crate) fn new(run_id: impl Into<String>, summary: impl Into<String>) -> Self {
        let summary = summary.into();
        let summary = summary
            .char_indices()
            .take_while(|(index, _)| *index < 7_800)
            .last()
            .map_or("", |(index, ch)| &summary[..index + ch.len_utf8()])
            .to_string();
        Self {
            run_id: run_id.into(),
            summary,
        }
    }
}

impl ContextualUserFragment for WorkflowCompletion {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("workflow.completion".to_string())
    }

    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<workflow_completion>", "</workflow_completion>")
    }

    fn body(&self) -> String {
        format!("Run {} completed.\n{}", self.run_id, self.summary)
    }
}

#[cfg(test)]
#[path = "workflow_completion_tests.rs"]
mod tests;
