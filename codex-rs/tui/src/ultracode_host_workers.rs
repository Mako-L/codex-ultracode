use super::*;
use std::collections::VecDeque;

#[derive(Default)]
pub(super) struct Workers {
    pending: HashMap<String, Worker>,
    early: VecDeque<ServerNotification>,
    terminals: HashMap<(String, String), ServerNotification>,
    closed: HashMap<String, String>,
    starting: usize,
    approvals: Vec<ServerRequest>,
}

struct Worker {
    parent_id: String,
    bridge: UltracodeBridge,
    request_id: String,
    run_id: String,
    worker_id: String,
    turn_id: String,
    text: String,
    usage: Value,
    activity: Vec<Value>,
    model: String,
    effort: Value,
    revision: u64,
    structured: bool,
    started: bool,
}

fn notification_thread(notification: &ServerNotification) -> Option<&str> {
    match notification {
        ServerNotification::AgentMessageDelta(value) => Some(&value.thread_id),
        ServerNotification::ReasoningSummaryTextDelta(value) => Some(&value.thread_id),
        ServerNotification::ReasoningTextDelta(value) => Some(&value.thread_id),
        ServerNotification::ThreadTokenUsageUpdated(value) => Some(&value.thread_id),
        ServerNotification::ItemStarted(value) => Some(&value.thread_id),
        ServerNotification::ItemCompleted(value) => Some(&value.thread_id),
        ServerNotification::TurnStarted(value) => Some(&value.thread_id),
        ServerNotification::TurnCompleted(value) => Some(&value.thread_id),
        _ => None,
    }
}

fn notification_turn(notification: &ServerNotification) -> Option<&str> {
    match notification {
        ServerNotification::AgentMessageDelta(value) => Some(&value.turn_id),
        ServerNotification::ReasoningSummaryTextDelta(value) => Some(&value.turn_id),
        ServerNotification::ReasoningTextDelta(value) => Some(&value.turn_id),
        ServerNotification::ThreadTokenUsageUpdated(value) => Some(&value.turn_id),
        ServerNotification::ItemStarted(value) => Some(&value.turn_id),
        ServerNotification::ItemCompleted(value) => Some(&value.turn_id),
        ServerNotification::TurnStarted(value) => Some(&value.turn.id),
        ServerNotification::TurnCompleted(value) => Some(&value.turn.id),
        _ => None,
    }
}

impl Workers {
    fn notification(&mut self, notification: ServerNotification) {
        let Some(thread_id) = notification_thread(&notification).map(str::to_string) else {
            return;
        };
        let Some(turn_id) = notification_turn(&notification).map(str::to_string) else {
            return;
        };
        if self.closed.get(&thread_id) == Some(&turn_id) {
            return;
        }
        let Some(worker) = self.pending.get_mut(&thread_id) else {
            if self.starting == 0 {
                return;
            }
            // Keep terminal outcomes independently of bounded progress until start correlation is known.
            if matches!(notification, ServerNotification::TurnCompleted(_)) {
                self.terminals.insert((thread_id, turn_id), notification);
            } else {
                if self.early.len() == 512 {
                    self.early.pop_front();
                }
                self.early.push_back(notification);
            }
            return;
        };
        if worker.turn_id != turn_id {
            return;
        }
        match notification {
            ServerNotification::AgentMessageDelta(value) => worker.text.push_str(&value.delta),
            ServerNotification::ReasoningSummaryTextDelta(_)
            | ServerNotification::ReasoningTextDelta(_) => worker.started = true,
            ServerNotification::ThreadTokenUsageUpdated(value) => {
                worker.usage = serde_json::to_value(value.token_usage).unwrap_or_default()
            }
            ServerNotification::ItemStarted(value) => {
                if !matches!(value.item, ThreadItem::UserMessage { .. }) {
                    worker.started = true;
                }
                worker
                    .activity
                    .push(serde_json::to_value(value.item).unwrap_or_default());
            }
            ServerNotification::ItemCompleted(value) => {
                if let ThreadItem::AgentMessage { text, .. } = &value.item {
                    worker.text = text.clone();
                    worker.started = true;
                }
                worker
                    .activity
                    .push(serde_json::to_value(value.item).unwrap_or_default());
            }
            ServerNotification::TurnStarted(_) => {}
            ServerNotification::TurnCompleted(value) => {
                self.closed.insert(thread_id.clone(), turn_id);
                let mut worker = self.pending.remove(&thread_id).unwrap();
                if let Some(text) = value.turn.items.iter().rev().find_map(|item| match item {
                    ThreadItem::AgentMessage { text, .. } => Some(text),
                    _ => None,
                }) {
                    worker.text = text.clone();
                }
                let status = format!("{:?}", value.turn.status).to_ascii_lowercase();
                let output = if status != "completed" {
                    Ok(Value::Null)
                } else if worker.structured {
                    serde_json::from_str(&worker.text).map_err(|error| {
                        BridgeError::host_code("INVALID_STRUCTURED_OUTPUT", error.to_string())
                    })
                } else {
                    Ok(Value::String(worker.text.clone()))
                };
                let result = output.map(|output| json!({"threadId":thread_id,"turnId":worker.turn_id,"status":status,"output":output,"text":worker.text,"usage":worker.usage,"activity":worker.activity,"model":worker.model,"effort":worker.effort,"error":value.turn.error.as_ref().map(|error|format!("{error:?}"))}));
                let _ = worker.bridge.respond(&worker.request_id, result);
                return;
            }
            _ => {}
        }
        worker.revision += 1;
        worker.started |= !worker.text.is_empty();
        if worker.activity.len() > 128 {
            worker.activity.drain(..worker.activity.len() - 128);
        }
        let _ = worker.bridge.notify(json!({"event":"worker.updated","runId":worker.run_id,"workerId":worker.worker_id,"threadId":thread_id,"turnId":worker.turn_id,"status":"running","text":worker.text,"usage":worker.usage,"activity":worker.activity,"revision":worker.revision,"firstResponseStarted":worker.started}));
    }
}

pub(super) fn headless_resolution(request: &ServerRequest) -> Resolution {
    match request {
        ServerRequest::McpServerElicitationRequest { .. } => {
            let response = serde_json::to_value(McpServerElicitationRequestResponse {
                action: McpServerElicitationAction::Cancel,
                content: None,
                meta: None,
            })
            .unwrap_or(Value::Null);
            Resolution::Accept(request.id().clone(), response)
        }
        _ => Resolution::Reject(
            request.id().clone(),
            JSONRPCErrorError {
                code: -32000,
                message:
                    "interactive approval or input is not supported in headless workflow execution"
                        .into(),
                data: None,
            },
        ),
    }
}

impl Runtime {
    pub(super) async fn start_worker(
        self: &Arc<Self>,
        parent_id: &str,
        parent: &Parent,
        id: String,
        mut params: Value,
    ) {
        let result = async {
            params["parentThreadId"] = json!(parent_id);
            params["roleDigest"] = params["workspace"]["roleDigest"].clone();
            let params: WorkflowWorkerStartParams =
                serde_json::from_value(params).map_err(|error| {
                    BridgeError::host(format!("invalid worker start request: {error}"))
                })?;
            let parent_id = params.parent_thread_id.clone();
            let run_id = params.run_id.clone();
            let worker_id = params.worker_id.clone();
            let structured = params.schema.is_some();
            self.workers.lock().unwrap().starting += 1;
            let started = self
                .handle
                .request_typed::<WorkflowWorkerStartResponse>(ClientRequest::WorkflowWorkerStart {
                    request_id: request_id(),
                    params,
                })
                .await;
            let pending_approvals = {
                let mut workers = self.workers.lock().unwrap();
                workers.starting -= 1;
                let started = match started {
                    Ok(started) => started,
                    Err(error) => {
                        if workers.starting == 0 {
                            workers.early.clear();
                            workers.terminals.clear();
                        }
                        return Err(BridgeError::host(error.to_string()));
                    }
                };
                let thread_id = started.thread_id.clone();
                let turn_id = started.turn_id.clone();
                workers.pending.insert(
                    thread_id.clone(),
                    Worker {
                        parent_id,
                        bridge: parent.bridge.clone(),
                        request_id: id.clone(),
                        run_id,
                        worker_id,
                        turn_id: started.turn_id,
                        text: String::new(),
                        usage: Value::Null,
                        activity: Vec::new(),
                        model: started.model,
                        effort: serde_json::to_value(started.effort).unwrap_or_default(),
                        revision: 0,
                        structured,
                        started: false,
                    },
                );
                let early = std::mem::take(&mut workers.early);
                for event in early {
                    if notification_thread(&event) == Some(thread_id.as_str()) {
                        workers.notification(event);
                    } else {
                        workers.early.push_back(event);
                    }
                }
                if let Some(terminal) = workers.terminals.remove(&(thread_id, turn_id)) {
                    workers.notification(terminal);
                }
                if workers.starting == 0 {
                    workers.early.clear();
                    workers.terminals.clear();
                }
                std::mem::take(&mut workers.approvals)
            };
            for request in pending_approvals {
                self.approval(request).await;
            }
            Ok::<(), BridgeError>(())
        }
        .await;
        if let Err(error) = result {
            let _ = parent.bridge.respond(&id, Err(error));
        }
    }

    async fn approval(&self, request: ServerRequest) {
        let Ok(mut encoded) = serde_json::to_value(&request) else {
            return;
        };
        let thread_id = encoded["params"]["threadId"].as_str().unwrap_or_default();
        let direct_parent = self.parents.lock().await.contains_key(thread_id);
        let parent_id = if direct_parent {
            thread_id.to_string()
        } else {
            let mut workers = self.workers.lock().unwrap();
            match workers.pending.get(thread_id) {
                Some(worker) if encoded["params"]["turnId"] == worker.turn_id => {
                    worker.parent_id.clone()
                }
                Some(_) => return,
                None => {
                    if workers.starting > 0 && workers.approvals.len() < 256 {
                        workers.approvals.push(request);
                    }
                    return;
                }
            }
        };
        let attached = self
            .parents
            .lock()
            .await
            .get(&parent_id)
            .is_some_and(|parent| parent.attachment.lock().unwrap().is_some());
        if !attached && self.headless_parents.lock().unwrap().contains(&parent_id) {
            // Match exec's existing native behavior: interactive requests cannot be approved
            // by a missing user interface, while native automatic policy still runs upstream.
            let _ = self.resolve.send(headless_resolution(&request));
            return;
        }
        let key = format!("workflow-approval:{}", Uuid::new_v4());
        let original_id = request.id().clone();
        encoded["id"] = json!(key);
        let event = json!({"id":key,"method":"supervisor.request","params":encoded});
        self.approvals.lock().unwrap().insert(
            key.clone(),
            json!({"originalId":original_id,"parentThreadId":parent_id,"event":event}),
        );
        if let Some(parent) = self.parents.lock().await.get(&parent_id) {
            let snapshot: Vec<Value> = self
                .approvals
                .lock()
                .unwrap()
                .values()
                .filter(|value| value["parentThreadId"] == parent_id)
                .cloned()
                .collect();
            let _ = socket::replace_private(
                &parent.directory.join("pending-approvals.json"),
                &json!(snapshot),
            );
            if let Some((_, sender)) = parent.attachment.lock().unwrap().as_ref() {
                let _ = sender.send(event);
            }
        }
        // No frontend means no response. Native policy and its pending request remain authoritative.
    }

    pub(super) async fn pump(
        self: Arc<Self>,
        mut client: AppServerClient,
        mut resolutions: mpsc::UnboundedReceiver<Resolution>,
    ) {
        loop {
            tokio::select! {
                Some(resolution) = resolutions.recv() => {
                    let result = match resolution {
                        Resolution::Accept(id, result) => client.resolve_server_request(id, result).await,
                        Resolution::Reject(id, error) => client.reject_server_request(id, error).await,
                    };
                    if let Err(error) = result { tracing::warn!(%error, "workflow approval response failed"); }
                }
                event = client.next_event() => match event {
                    Some(AppServerEvent::ServerNotification(notification)) => {
                        if let ServerNotification::ThreadStatusChanged(status) = notification.as_ref() { let _ = self.status.send(status.clone()); }
                        self.workers.lock().unwrap().notification(*notification);
                    }
                    Some(AppServerEvent::ServerRequest(request)) => self.approval(*request).await,
                    Some(AppServerEvent::Lagged { skipped }) => tracing::warn!(skipped, "workflow daemon notifications lagged"),
                    Some(AppServerEvent::Disconnected { message }) => {
                        tracing::error!(%message, "workflow daemon disconnected; outcomes unresolved");
                        break;
                    }
                    None => break,
                }
            }
        }
        let mut workers = self.workers.lock().unwrap();
        for (_, worker) in workers.pending.drain() {
            let _ = worker.bridge.respond(
                &worker.request_id,
                Err(BridgeError::host_code(
                    "OUTCOME_UNRESOLVED",
                    "native daemon disconnected; worker outcome is unresolved",
                )),
            );
        }
    }
}

#[cfg(test)]
#[path = "ultracode_host_workers_tests.rs"]
mod tests;
