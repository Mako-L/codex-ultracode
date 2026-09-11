use super::*;

impl Runtime {
    pub(super) async fn headless_state(
        self: &Arc<Self>,
        binding: &HeadlessBinding,
        params: Value,
    ) -> Result<Value, BridgeError> {
        let parent_id = params["parentThreadId"]
            .as_str()
            .ok_or_else(|| BridgeError::host("missing headless parent thread ID"))?;
        let canonical = Uuid::parse_str(parent_id)
            .map_err(|error| BridgeError::host(error.to_string()))?
            .to_string();
        if canonical != parent_id {
            return Err(BridgeError::host(
                "parent thread ID must be a canonical UUID",
            ));
        }
        let action = params["action"].as_str().unwrap_or_default();
        if !matches!(action, "register" | "status" | "stop") {
            return Err(BridgeError::host("unknown headless workflow action"));
        }
        let (parent, _access) = binding.authorize(self, parent_id).await?;
        let mut active = false;
        let mut completion_pending = false;
        let mut unresolved = Vec::new();
        let mut completion_turns = HashSet::new();
        {
            {
                let attachment = parent.attachment.lock().unwrap();
                if attachment.is_none() {
                    let mut pending = self.approvals.lock().unwrap();
                    let keys: Vec<_> = pending
                        .iter()
                        .filter(|(_, value)| value["parentThreadId"] == parent_id)
                        .map(|(key, _)| key.clone())
                        .collect();
                    for key in keys {
                        let entry = &pending[&key];
                        let mut encoded = entry["event"]["params"].clone();
                        encoded["id"] = entry["originalId"].clone();
                        let request: ServerRequest = serde_json::from_value(encoded)
                            .map_err(|error| BridgeError::host(error.to_string()))?;
                        self.resolve
                            .send(workers::headless_resolution(&request))
                            .map_err(|error| BridgeError::host(error.to_string()))?;
                        pending.remove(&key);
                    }
                }
            }
            let snapshot = parent.bridge.list_runs().await?;
            for run in snapshot["runs"].as_array().into_iter().flatten() {
                let run_id = run["id"]
                    .as_str()
                    .ok_or_else(|| BridgeError::host("workflow snapshot omitted its run ID"))?;
                let terminal = matches!(
                    run["status"].as_str(),
                    Some("completed" | "failed" | "stopped" | "interrupted")
                );
                if !terminal {
                    active = true;
                    Self::remember_headless_run(&parent, parent_id, run_id)?;
                    self.headless_runs
                        .lock()
                        .unwrap()
                        .insert((parent_id.to_string(), run_id.to_string()));
                    if action == "stop" {
                        parent
                            .bridge
                            .request("stopRun", json!({"runId":run_id}), REQUEST_TIMEOUT)
                            .await?;
                    }
                    continue;
                }
                // Historical terminal runs from an older frontend do not acquire a new delivery obligation.
                if !self
                    .headless_runs
                    .lock()
                    .unwrap()
                    .contains(&(parent_id.to_string(), run_id.to_string()))
                    && !Self::owns_headless_run(&parent, parent_id, run_id)?
                {
                    continue;
                }
                self.headless_runs
                    .lock()
                    .unwrap()
                    .insert((parent_id.to_string(), run_id.to_string()));
                let run_uuid = Uuid::parse_str(run_id)
                    .map_err(|error| BridgeError::host(error.to_string()))?;
                let attempt = run["attempt"].as_u64().unwrap_or(1);
                let ledger = parent
                    .directory
                    .join(format!("completion-{run_uuid}-{attempt}.json"));
                let record: Value = match std::fs::read(&ledger) {
                    Ok(bytes) => serde_json::from_slice(&bytes)
                        .map_err(|error| BridgeError::host(error.to_string()))?,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        self.complete(parent_id, &parent, run_id).await?;
                        let bytes = std::fs::read(&ledger)
                            .map_err(|error| BridgeError::host(error.to_string()))?;
                        serde_json::from_slice(&bytes)
                            .map_err(|error| BridgeError::host(error.to_string()))?
                    }
                    Err(error) => return Err(BridgeError::host(error.to_string())),
                };
                match record["status"].as_str() {
                    Some("delivered") => {
                        if let Some(turn_id) = record["turnId"].as_str() {
                            completion_turns.insert(turn_id.to_string());
                        } else {
                            unresolved.push(run_id.to_string());
                        }
                    }
                    Some("unresolved") => unresolved.push(run_id.to_string()),
                    _ => completion_pending = true,
                }
            }
        }
        // Admission can acknowledge a turn before hooks or persistence expose it in history.
        // Check every accepted completion turn, including older turns when several runs finish.
        let mut remaining = completion_turns.clone();
        let mut cursor = None;
        let mut latest = None;
        loop {
            // Registration precedes the first user message, so this thread may not
            // have materialized history yet. Later status polls check completion.
            if action == "register" {
                break;
            }
            let turns: ThreadTurnsListResponse = self
                .handle
                .request_typed(ClientRequest::ThreadTurnsList {
                    request_id: request_id(),
                    params: ThreadTurnsListParams {
                        thread_id: parent_id.to_string(),
                        cursor,
                        limit: Some(100),
                        sort_direction: Some(SortDirection::Desc),
                        items_view: Some(TurnItemsView::Summary),
                    },
                })
                .await
                .map_err(|error| BridgeError::host(error.to_string()))?;
            if latest.is_none() {
                latest = turns.data.first().cloned();
            }
            for turn in &turns.data {
                if turn.status != TurnStatus::InProgress {
                    remaining.remove(&turn.id);
                }
            }
            if remaining.is_empty() || turns.next_cursor.is_none() {
                break;
            }
            cursor = turns.next_cursor;
        }
        completion_pending |= !remaining.is_empty();
        Ok(
            json!({"active":active,"completionPending":completion_pending,"unresolved":unresolved,
            "completionTurnIds":completion_turns,"pendingCompletionTurnIds":remaining,
            "turnId":latest.as_ref().map(|turn|turn.id.as_str()),"turnStatus":latest.as_ref().map(|turn|&turn.status)}),
        )
    }
}
