use super::WorkflowView;
use serde_json::Value;

impl WorkflowView {
    pub(crate) fn update(&mut self, snapshot: Value) {
        let selected = self
            .run()
            .and_then(|run| run["id"].as_str())
            .map(str::to_owned);
        self.snapshot = snapshot;
        if let Some(selected) = selected
            && let Some(index) = self.runs().iter().position(|run| run["id"] == selected)
        {
            self.state.run = index;
        }
        self.clamp();
    }

    pub(crate) fn update_run(&mut self, run: Value) {
        let Some(id) = run["id"].as_str() else {
            return;
        };
        if let Some(current) = self.snapshot["runs"]
            .as_array_mut()
            .and_then(|runs| runs.iter_mut().find(|current| current["id"] == id))
        {
            *current = run;
        }
        self.clamp();
    }
}
