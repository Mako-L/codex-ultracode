use super::*;
use crate::app_event::WorkflowConsentChoice;
use crate::app_event::WorkflowEvent;

pub(crate) struct WorkflowSession {
    pub(crate) bridge: crate::workflow_bridge::WorkflowBridge,
    pub(crate) pending_workers: HashMap<String, PendingWorker>,
    pub(crate) consent: crate::workflow_consent::WorkflowConsentStore,
    pub(crate) reported_runs: HashSet<(String, u64)>,
    pub(crate) active_runs: bool,
}
pub(crate) struct PendingWorker {
    request_id: String,
    run_id: String,
    worker_id: String,
    turn_id: String,
    text: String,
    usage: serde_json::Value,
    activity: Vec<serde_json::Value>,
    model: String,
    effort: serde_json::Value,
    revision: u64,
    structured: bool,
    started: bool,
}

pub(super) fn append_workflow_feedback(
    response: &mut codex_app_server_protocol::DynamicToolCallResponse,
    feedback: Option<String>,
) {
    if let Some(feedback) = feedback {
        response.content_items.push(
            codex_app_server_protocol::DynamicToolCallOutputContentItem::InputText {
                text: format!("Workflow consent feedback: {feedback}"),
            },
        );
    }
}

fn terminal_output(
    status: &str,
    structured: bool,
    text: &str,
) -> Result<serde_json::Value, crate::workflow_bridge::BridgeError> {
    if status != "completed" {
        return Ok(serde_json::Value::Null);
    }
    if structured {
        return serde_json::from_str(text).map_err(|error| {
            crate::workflow_bridge::BridgeError::host_code(
                "INVALID_STRUCTURED_OUTPUT",
                error.to_string(),
            )
        });
    }
    Ok(serde_json::Value::String(text.to_string()))
}

fn workflow_state_dir(codex_home: &Path, cwd: &Path, thread_id: &str) -> std::io::Result<PathBuf> {
    let state_dir = codex_home.join("ultracode/sessions").join(thread_id);
    let legacy = cwd.join(".ultracode/native/sessions").join(thread_id);
    if !state_dir.exists() && legacy.exists() {
        std::fs::create_dir_all(codex_home.join("ultracode/sessions"))?;
        std::fs::rename(&legacy, &state_dir)?;
    }
    Ok(state_dir)
}

impl App {
    pub(super) async fn ensure_workflow_session(
        &mut self,
        authority: &codex_app_server_protocol::WorkflowAuthorityCaptureResponse,
        session_key: &str,
        app_server: &AppServerSession,
    ) -> Result<crate::workflow_bridge::WorkflowBridge> {
        if let Some(session) = self.workflow_sessions.get(session_key) {
            return Ok(session.bridge.clone());
        }
        let runtime = crate::workflow_runtime::WorkflowRuntime::bundled()?;
        let plugin_root = runtime.root;
        #[cfg(unix)]
        let supervised = app_server.workflow_host_enabled();
        #[cfg(not(unix))]
        let supervised = false;
        let bridge = if supervised {
            #[cfg(unix)]
            {
                crate::workflow_host::attach(&self.config.codex_home, session_key, &plugin_root)
                    .await?
            }
            #[cfg(not(unix))]
            {
                unreachable!()
            }
        } else {
            crate::workflow_bridge::WorkflowBridge::spawn(crate::workflow_bridge::BridgeLaunch {
                node: runtime.node,
                script: runtime.script,
                plugin_root,
                cwd: authority.cwd.clone().into(),
                state_dir: workflow_state_dir(
                    &self.config.codex_home,
                    Path::new(&authority.cwd),
                    session_key,
                )?,
                models: serde_json::to_value(&authority.models)?,
                plugins: serde_json::to_value(&authority.plugins)?,
                web_search_available: authority.web_search_available,
            })
            .await
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?
        };
        if let Some(mut events) = bridge.take_events() {
            let tx = self.app_event_tx.clone();
            let event_bridge = bridge.clone();
            tokio::spawn(async move {
                while let Some(event) = events.recv().await {
                    match event {
                        crate::workflow_bridge::BridgeEvent::RunChanged { run_id, .. } => {
                            tx.send(AppEvent::Workflow(WorkflowEvent::RunChanged {
                                bridge: event_bridge.clone(),
                                run_id,
                            }))
                        }
                        crate::workflow_bridge::BridgeEvent::Request { id, method, params } => tx
                            .send(AppEvent::Workflow(WorkflowEvent::HostRequest {
                                bridge: event_bridge.clone(),
                                id,
                                method,
                                params,
                            })),
                    }
                }
            });
        }
        self.workflow_sessions.insert(
            session_key.to_string(),
            WorkflowSession {
                bridge: bridge.clone(),
                pending_workers: HashMap::new(),
                consent: crate::workflow_consent::WorkflowConsentStore::load(
                    &self.config.codex_home,
                )?,
                reported_runs: HashSet::new(),
                active_runs: false,
            },
        );
        Ok(bridge)
    }

    pub(super) fn handle_workflow_notification(&mut self, notification: &ServerNotification) {
        let thread_id = match notification {
            ServerNotification::AgentMessageDelta(value) => &value.thread_id,
            ServerNotification::ReasoningSummaryTextDelta(value) => &value.thread_id,
            ServerNotification::ReasoningTextDelta(value) => &value.thread_id,
            ServerNotification::ThreadTokenUsageUpdated(value) => &value.thread_id,
            ServerNotification::ItemStarted(value) => &value.thread_id,
            ServerNotification::ItemCompleted(value) => &value.thread_id,
            ServerNotification::TurnStarted(value) => &value.thread_id,
            ServerNotification::TurnCompleted(value) => &value.thread_id,
            _ => return,
        }
        .clone();
        let Some(session) = self
            .workflow_sessions
            .values_mut()
            .find(|session| session.pending_workers.contains_key(&thread_id))
        else {
            return;
        };
        let Some(worker) = session.pending_workers.get_mut(&thread_id) else {
            return;
        };
        match notification {
            ServerNotification::AgentMessageDelta(value) => worker.text.push_str(&value.delta),
            ServerNotification::ReasoningSummaryTextDelta(_)
            | ServerNotification::ReasoningTextDelta(_) => worker.started = true,
            ServerNotification::ThreadTokenUsageUpdated(value) => {
                worker.usage = serde_json::to_value(&value.token_usage).unwrap_or_default()
            }
            ServerNotification::ItemStarted(value) => {
                if !matches!(
                    value.item,
                    codex_app_server_protocol::ThreadItem::UserMessage { .. }
                ) {
                    worker.started = true;
                }
                worker
                    .activity
                    .push(serde_json::to_value(&value.item).unwrap_or_default())
            }
            ServerNotification::ItemCompleted(value) => {
                if let codex_app_server_protocol::ThreadItem::AgentMessage { text, .. } =
                    &value.item
                {
                    worker.text = text.clone();
                    worker.started = true;
                }
                worker
                    .activity
                    .push(serde_json::to_value(&value.item).unwrap_or_default())
            }
            ServerNotification::TurnStarted(value) => worker.turn_id = value.turn.id.clone(),
            ServerNotification::TurnCompleted(value) => {
                let mut worker = session.pending_workers.remove(&thread_id).unwrap();
                if let Some(text) = value.turn.items.iter().rev().find_map(|item| {
                    if let codex_app_server_protocol::ThreadItem::AgentMessage { text, .. } = item {
                        Some(text)
                    } else {
                        None
                    }
                }) {
                    worker.text = text.clone();
                }
                let status = format!("{:?}", value.turn.status).to_ascii_lowercase();
                let output = match terminal_output(&status, worker.structured, &worker.text) {
                    Ok(output) => output,
                    Err(error) => {
                        let _ = session.bridge.respond(&worker.request_id, Err(error));
                        return;
                    }
                };
                let result = serde_json::json!({"threadId":thread_id,"turnId":worker.turn_id,"status":status,"output":output,"text":worker.text,"usage":worker.usage,"activity":worker.activity,"model":worker.model,"effort":worker.effort,"error":value.turn.error.as_ref().map(|error|format!("{error:?}"))});
                let _ = session.bridge.respond(&worker.request_id, Ok(result));
                self.chat_widget.add_info_message(
                    format!("Workflow worker {} finished", worker.worker_id),
                    Some(format!("Run {} · {}", worker.run_id, status)),
                );
                return;
            }
            _ => {}
        }
        worker.revision += 1;
        if !worker.text.is_empty() {
            worker.started = true;
        }
        let _=session.bridge.notify(serde_json::json!({"event":"worker.updated","runId":worker.run_id,"workerId":worker.worker_id,"threadId":thread_id,"turnId":worker.turn_id,"status":"running","text":worker.text,"usage":worker.usage,"activity":worker.activity,"revision":worker.revision,"firstResponseStarted":worker.started}));
    }

    pub(super) async fn handle_workflow_event(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        event: WorkflowEvent,
    ) -> Result<()> {
        match event {
            WorkflowEvent::LoadCatalog { thread_id } => {
                if self
                    .chat_widget
                    .thread_id()
                    .is_some_and(|id| id.to_string() == thread_id)
                {
                    self.chat_widget.set_workflow_commands(Vec::new());
                    self.chat_widget
                        .update_workflow_status(&serde_json::json!({"runs":[]}));
                }
                let authority = app_server
                    .workflow_authority_capture(
                        codex_app_server_protocol::WorkflowAuthorityCaptureParams {
                            parent_thread_id: thread_id.clone(),
                            allow_isolated_workspaces: false,
                        },
                    )
                    .await?;
                let bridge = self
                    .ensure_workflow_session(&authority, &thread_id, app_server)
                    .await?;
                let catalog = bridge
                    .request(
                        "listSavedWorkflows",
                        serde_json::json!({"cwd":authority.cwd}),
                        std::time::Duration::from_secs(30),
                    )
                    .await
                    .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
                let commands = catalog["workflows"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|item| {
                        Some(crate::bottom_pane::slash_commands::WorkflowCommand {
                            name: item.get("name")?.as_str()?.to_string(),
                            description: item
                                .get("description")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        })
                    })
                    .collect();
                if self
                    .chat_widget
                    .thread_id()
                    .is_some_and(|id| id.to_string() == thread_id)
                {
                    self.chat_widget.set_workflow_commands(commands);
                }
                let result = bridge.list_runs().await.map_err(|error| error.to_string());
                self.app_event_tx
                    .send(AppEvent::Workflow(WorkflowEvent::Snapshot {
                        bridge,
                        result,
                    }));
            }
            WorkflowEvent::RunSaved { name, args } => {
                let thread_id = self
                    .chat_widget
                    .thread_id()
                    .ok_or_else(|| color_eyre::eyre::eyre!("Parent session is unavailable"))?;
                let authority = app_server
                    .workflow_authority_capture(
                        codex_app_server_protocol::WorkflowAuthorityCaptureParams {
                            parent_thread_id: thread_id.to_string(),
                            allow_isolated_workspaces: false,
                        },
                    )
                    .await?;
                let key = thread_id.to_string();
                let bridge = self
                    .ensure_workflow_session(&authority, &key, app_server)
                    .await?;
                let catalog = bridge
                    .request(
                        "listSavedWorkflows",
                        serde_json::json!({"cwd":authority.cwd}),
                        std::time::Duration::from_secs(30),
                    )
                    .await
                    .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
                let digest = catalog["workflows"]
                    .as_array()
                    .and_then(|items| items.iter().find(|item| item["name"] == name))
                    .and_then(|item| item.get("sourceDigest").or_else(|| item.get("digest")))
                    .and_then(serde_json::Value::as_str);
                let allowed = matches!(
                    self.config.permissions.approval_policy.value(),
                    codex_protocol::protocol::AskForApproval::Never
                ) || digest.is_some_and(|digest| {
                    self.workflow_sessions.get(&key).is_some_and(|session| {
                        session
                            .consent
                            .allows_named(Path::new(&authority.cwd), &name, digest)
                            .unwrap_or(false)
                    })
                });
                if allowed {
                    self.app_event_tx
                        .send(AppEvent::Workflow(WorkflowEvent::RunSavedConsent {
                            thread_id: key,
                            name,
                            args,
                            choice: WorkflowConsentChoice::Run,
                            feedback: None,
                            preview: None,
                        }));
                } else {
                    let mut arguments = serde_json::json!({"name":name});
                    if let Some(args) = &args {
                        arguments["args"] = serde_json::Value::String(args.clone());
                    }
                    let preview = self
                        .prepare_workflow_preview(app_server, &key, &arguments)
                        .await?;
                    self.show_saved_workflow_consent(key, name, args, preview);
                }
            }
            WorkflowEvent::RunSavedConsent {
                thread_id,
                name,
                args,
                choice,
                feedback,
                preview,
            } => {
                if let Some(feedback) = feedback {
                    let thread_id = ThreadId::from_string(&thread_id)?;
                    app_server
                        .thread_inject_items(
                            thread_id,
                            vec![codex_protocol::models::ResponseItem::Message {
                                id: None,
                                role: "user".to_string(),
                                content: vec![codex_protocol::models::ContentItem::InputText {
                                    text: feedback,
                                }],
                                phase: None,
                                internal_chat_message_metadata_passthrough: None,
                            }],
                        )
                        .await?;
                }
                if matches!(choice, WorkflowConsentChoice::Cancel) {
                    return Ok(());
                }

                let authority = app_server
                    .workflow_authority_capture(
                        codex_app_server_protocol::WorkflowAuthorityCaptureParams {
                            parent_thread_id: thread_id.to_string(),
                            allow_isolated_workspaces: true,
                        },
                    )
                    .await?;
                let key = thread_id.to_string();
                let bridge = self
                    .ensure_workflow_session(&authority, &key, app_server)
                    .await?;
                let mut arguments = serde_json::json!({"name":name});
                if let Some(args) = args {
                    arguments["args"] = serde_json::Value::String(args);
                }
                if let Some(preview) = &preview
                    && preview.workflow_id.is_none()
                {
                    // An editor result is an inline launch, not the saved file's old bytes.
                    arguments.as_object_mut().unwrap().remove("name");
                    arguments["script"] = serde_json::Value::String(preview.source.clone());
                    if matches!(choice, WorkflowConsentChoice::Remember) {
                        return Err(color_eyre::eyre::eyre!(
                            "Edited workflows cannot inherit saved workflow permissions"
                        ));
                    }
                }
                let arguments = crate::workflow_launch::with_isolate_writes(
                    &arguments,
                    self.config.workflow_isolate_writes,
                );
                let launch = crate::workflow_launch::launch_with_preview(
                    &app_server.request_handle(),
                    &key,
                    &authority,
                    &bridge,
                    &arguments,
                    preview.as_ref(),
                )
                .await?;
                if matches!(choice, WorkflowConsentChoice::Remember) {
                    let digest = launch.source_digest.as_deref().ok_or_else(|| {
                        color_eyre::eyre::eyre!("Saved workflow digest is unavailable")
                    })?;
                    self.workflow_sessions
                        .get_mut(&key)
                        .unwrap()
                        .consent
                        .remember_named(Path::new(&authority.cwd), &name, digest)?;
                }
                let result = launch.result?;
                if self
                    .chat_widget
                    .thread_id()
                    .is_some_and(|current| current.to_string() == key)
                {
                    self.chat_widget.add_info_message(
                        if arguments.get("name").is_some() {
                            format!("Workflow /{name} launched")
                        } else {
                            "Edited workflow launched".to_string()
                        },
                        result
                            .get("scriptPath")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                    );
                }
            }
            WorkflowEvent::ToolCall { request_id, params } => {
                if self.config.disable_workflows {
                    self.app_event_tx.send(AppEvent::DynamicToolCallCompleted {
                        request_id,
                        response: crate::dynamic_tools::failure_response(
                            "Workflows are disabled by configuration.",
                        ),
                    });
                    return Ok(());
                }
                if let Err(error) = crate::workflow_launch::validate_arguments(&params.arguments) {
                    self.app_event_tx.send(AppEvent::DynamicToolCallCompleted {
                        request_id,
                        response: crate::dynamic_tools::failure_response(error.to_string()),
                    });
                    return Ok(());
                }
                let policy = self.config.permissions.approval_policy.value();
                let auto_review = self.config.approvals_reviewer == ApprovalsReviewer::AutoReview;
                let auto_allowed = auto_review
                    && (self.config.ultracode
                        || crate::workflow_consent::WorkflowConsentStore::load(
                            &self.config.codex_home,
                        )?
                        .allows_auto_first_launch());
                let mut named_allowed = false;
                if let Ok(crate::workflow_source::SourceLocation::Saved(name)) =
                    crate::workflow_source::source_location(&params.arguments)
                {
                    let authority = app_server
                        .workflow_authority_capture(
                            codex_app_server_protocol::WorkflowAuthorityCaptureParams {
                                parent_thread_id: params.thread_id.to_string(),
                                allow_isolated_workspaces: false,
                            },
                        )
                        .await?;
                    let key = params.thread_id.to_string();
                    let bridge = self
                        .ensure_workflow_session(&authority, &key, app_server)
                        .await?;
                    let catalog = bridge
                        .request(
                            "listSavedWorkflows",
                            serde_json::json!({"cwd":authority.cwd}),
                            std::time::Duration::from_secs(30),
                        )
                        .await
                        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
                    if let Some(saved) = catalog["workflows"]
                        .as_array()
                        .and_then(|items| items.iter().find(|item| item["name"] == name))
                        && let Some(digest) = saved
                            .get("sourceDigest")
                            .or_else(|| saved.get("digest"))
                            .and_then(serde_json::Value::as_str)
                    {
                        named_allowed = self
                            .workflow_sessions
                            .get(&key)
                            .unwrap()
                            .consent
                            .allows_named(Path::new(&authority.cwd), name, digest)?;
                    }
                }
                if matches!(policy, codex_protocol::protocol::AskForApproval::Never)
                    || auto_allowed
                    || named_allowed
                {
                    self.app_event_tx
                        .send(AppEvent::Workflow(WorkflowEvent::Consent {
                            request_id,
                            params,
                            choice: WorkflowConsentChoice::Run,
                            feedback: None,
                            preview: None,
                        }));
                } else {
                    let preview = self
                        .prepare_workflow_preview(app_server, &params.thread_id, &params.arguments)
                        .await?;
                    self.show_workflow_consent(request_id, params, preview)
                }
            }
            WorkflowEvent::ViewScript { thread_id, source } => {
                if self
                    .chat_widget
                    .thread_id()
                    .is_some_and(|current| current.to_string() == thread_id)
                {
                    let _ = tui.enter_alt_screen();
                    let source = crate::history_cell::sanitize_user_text(source.into());
                    let mut keymap = self.keymap.pager.clone();
                    keymap.close.insert(0, crate::key_hint::plain(KeyCode::Esc));
                    self.overlay = Some(Overlay::new_static_with_lines(
                        source
                            .lines()
                            .map(|line| ratatui::text::Line::from(line.to_owned()))
                            .collect(),
                        "Workflow source".into(),
                        keymap,
                    ));
                    tui.frame_requester().schedule_frame();
                }
            }
            WorkflowEvent::ToggleWorkflowPreview {
                consent,
                feedback_state,
                preview,
                mode,
            } => self.show_workflow_consent_context(consent, feedback_state, preview, mode),
            WorkflowEvent::EditWorkflowSource {
                consent,
                feedback_state,
                preview,
            } => {
                self.edit_workflow_source(tui, app_server, consent, feedback_state, preview)
                    .await;
            }
            WorkflowEvent::Consent {
                request_id,
                params,
                choice,
                feedback,
                preview,
            } => {
                if matches!(choice, WorkflowConsentChoice::Cancel) {
                    let mut response =
                        crate::dynamic_tools::failure_response("Workflow launch cancelled");
                    append_workflow_feedback(&mut response, feedback);
                    self.app_event_tx.send(AppEvent::DynamicToolCallCompleted {
                        request_id,
                        response,
                    });
                    return Ok(());
                }
                let response: Result<codex_app_server_protocol::DynamicToolCallResponse> = async {
                    let parent_thread_id = params.thread_id.clone();
                    let session_key = parent_thread_id.to_string();
                    let authority = app_server
                        .workflow_authority_capture(
                            codex_app_server_protocol::WorkflowAuthorityCaptureParams {
                                parent_thread_id,
                                allow_isolated_workspaces: true,
                            },
                        )
                        .await?;
                    self.ensure_workflow_session(&authority, &session_key, app_server)
                        .await?;
                    let bridge = self
                        .workflow_sessions
                        .get(&session_key)
                        .unwrap()
                        .bridge
                        .clone();
                    let arguments = crate::workflow_launch::with_isolate_writes(
                        &params.arguments,
                        self.config.workflow_isolate_writes,
                    );
                    let launch = crate::workflow_launch::launch_with_preview(
                        &app_server.request_handle(),
                        &session_key,
                        &authority,
                        &bridge,
                        &arguments,
                        preview.as_ref(),
                    )
                    .await?;
                    let source_digest = launch.source_digest;
                    if self.config.approvals_reviewer == ApprovalsReviewer::AutoReview
                        && !self.config.ultracode
                    {
                        self.workflow_sessions
                            .get_mut(&session_key)
                            .unwrap()
                            .consent
                            .remember_auto_first_launch()?;
                    }
                    if matches!(choice, WorkflowConsentChoice::Remember)
                        && let (Some(name), Some(digest)) = (
                            params
                                .arguments
                                .get("name")
                                .and_then(serde_json::Value::as_str),
                            source_digest.as_deref(),
                        )
                    {
                        self.workflow_sessions
                            .get_mut(&session_key)
                            .unwrap()
                            .consent
                            .remember_named(Path::new(&authority.cwd), name, digest)?;
                    }
                    Ok(crate::workflow_launch::response(launch.result))
                }
                .await;
                let mut response = response.unwrap_or_else(|error| {
                    crate::dynamic_tools::failure_response(error.to_string())
                });
                append_workflow_feedback(&mut response, feedback);
                self.app_event_tx.send(AppEvent::DynamicToolCallCompleted {
                    request_id,
                    response,
                });
            }
            WorkflowEvent::Open { effort } => {
                if let Some(label) = effort.as_deref().filter(|label| !label.is_empty()) {
                    self.apply_workflow_effort(app_server, label).await?;
                    return Ok(());
                }
                let effort = effort.map(|_| {
                    if self.config.ultracode {
                        "ultracode".to_string()
                    } else {
                        self.chat_widget
                            .current_reasoning_effort()
                            .map(|effort| effort.to_string())
                            .unwrap_or_else(|| "medium".to_string())
                    }
                });
                let Some(parent_thread_id) = self.chat_widget.thread_id() else {
                    self.chat_widget
                        .add_error_message("Session is still starting; try again shortly.".into());
                    return Ok(());
                };
                let session_key = parent_thread_id.to_string();
                if let Some(session) = self.workflow_sessions.get(&session_key) {
                    let bridge = session.bridge.clone();
                    let tx = self.app_event_tx.clone();
                    tokio::spawn(async move {
                        tx.send(AppEvent::Workflow(WorkflowEvent::Ready {
                            effort,
                            parent_thread_id: session_key,
                            result: bridge
                                .list_runs()
                                .await
                                .map(|snapshot| (bridge, snapshot))
                                .map_err(|error| error.to_string()),
                        }));
                    });
                    return Ok(());
                }
                let authority = app_server
                    .workflow_authority_capture(
                        codex_app_server_protocol::WorkflowAuthorityCaptureParams {
                            parent_thread_id: session_key.clone(),
                            allow_isolated_workspaces: false,
                        },
                    )
                    .await?;
                let bridge = self
                    .ensure_workflow_session(&authority, &session_key, app_server)
                    .await?;
                let snapshot = bridge.list_runs().await?;
                self.app_event_tx
                    .send(AppEvent::Workflow(WorkflowEvent::Ready {
                        parent_thread_id: session_key,
                        effort,
                        result: Ok((bridge, snapshot)),
                    }));
            }
            WorkflowEvent::Ready {
                parent_thread_id,
                effort,
                result,
            } => match result {
                Ok((bridge, snapshot)) => {
                    if !self.workflow_sessions.contains_key(&parent_thread_id)
                        && let Some(mut events) = bridge.take_events()
                    {
                        let tx = self.app_event_tx.clone();
                        let event_bridge = bridge.clone();
                        tokio::spawn(async move {
                            while let Some(event) = events.recv().await {
                                match event {
                                    crate::workflow_bridge::BridgeEvent::RunChanged {
                                        run_id,
                                        ..
                                    } => tx.send(AppEvent::Workflow(WorkflowEvent::RunChanged {
                                        bridge: event_bridge.clone(),
                                        run_id,
                                    })),
                                    crate::workflow_bridge::BridgeEvent::Request {
                                        id,
                                        method,
                                        params,
                                    } => tx.send(AppEvent::Workflow(WorkflowEvent::HostRequest {
                                        bridge: event_bridge.clone(),
                                        id,
                                        method,
                                        params,
                                    })),
                                }
                            }
                        });
                    }
                    let consent = crate::workflow_consent::WorkflowConsentStore::load(
                        &self.config.codex_home,
                    )?;
                    self.workflow_sessions
                        .entry(parent_thread_id)
                        .or_insert_with(|| WorkflowSession {
                            bridge: bridge.clone(),
                            pending_workers: HashMap::new(),
                            consent,
                            reported_runs: HashSet::new(),
                            active_runs: false,
                        });
                    if let Some(session) = self
                        .workflow_sessions
                        .values_mut()
                        .find(|session| session.bridge.same_peer(&bridge))
                    {
                        session.active_runs = snapshot["runs"].as_array().is_some_and(|runs| {
                            runs.iter().any(|run| {
                                !matches!(
                                    run["status"].as_str(),
                                    Some("completed" | "failed" | "stopped" | "interrupted")
                                )
                            })
                        });
                    }
                    let _ = tui.enter_alt_screen();
                    self.overlay = Some(Overlay::Workflow(Box::new(
                        crate::pager_overlay::WorkflowOverlay::new(snapshot, effort),
                    )));
                    tui.frame_requester().schedule_frame();
                }
                Err(error) => self
                    .chat_widget
                    .add_error_message(format!("Unable to open workflows: {error}")),
            },
            WorkflowEvent::Refresh => {
                let key = self.chat_widget.thread_id().map(|id| id.to_string());
                if let Some(session) = key.as_ref().and_then(|key| self.workflow_sessions.get(key))
                {
                    let bridge = session.bridge.clone();
                    let tx = self.app_event_tx.clone();
                    tokio::spawn(async move {
                        let result = bridge.list_runs().await.map_err(|error| error.to_string());
                        tx.send(AppEvent::Workflow(WorkflowEvent::Snapshot {
                            bridge,
                            result,
                        }));
                    });
                }
            }
            WorkflowEvent::RunChanged { bridge, run_id } => {
                let mut report = None;
                let session_key = self.workflow_sessions.iter().find_map(|(key, session)| {
                    session.bridge.same_peer(&bridge).then(|| key.clone())
                });
                if let Some(session) = session_key
                    .as_ref()
                    .and_then(|key| self.workflow_sessions.get_mut(key))
                {
                    if let Ok(state) = session.bridge.inspect_run(&run_id).await {
                        let status = state["status"].as_str().unwrap_or_default();
                        let attempt = state["attempt"].as_u64().unwrap_or(1);
                        if !session.bridge.is_supervised()
                            && ["completed", "failed", "stopped", "interrupted"].contains(&status)
                            && !session.reported_runs.contains(&(run_id.clone(), attempt))
                        {
                            let detail =
                                state.get("result").or_else(|| state.get("error")).map(|v| {
                                    if let Some(s) = v.as_str() {
                                        s.to_string()
                                    } else {
                                        v.to_string()
                                    }
                                });
                            let body = state
                                .get("result")
                                .or_else(|| state.get("error"))
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            let paths = format!(
                                " scriptPath={} transcriptDir={}",
                                state["scriptPath"], state["transcriptDir"]
                            );
                            report = Some((
                                attempt,
                                status.to_string(),
                                detail,
                                format!(
                                    "workflow {run_id} attempt {attempt} finished with status {status}.{paths} Consolidated result: {body}"
                                ),
                            ));
                        }
                    }
                    let bridge = session.bridge.clone();
                    let events = self.app_event_tx.clone();
                    tokio::spawn(async move {
                        let result = bridge.list_runs().await.map_err(|error| error.to_string());
                        events.send(AppEvent::Workflow(WorkflowEvent::Snapshot {
                            bridge,
                            result,
                        }));
                    });
                }
                if let Some((attempt, status, detail, report)) = report
                    && let Some(session_key) = session_key
                    && let Ok(thread_id) = ThreadId::from_string(&session_key)
                {
                    if self.chat_widget.thread_id() == Some(thread_id) {
                        self.chat_widget
                            .add_info_message(format!("Workflow {run_id} {status}"), detail);
                    }
                    let report: String = report.chars().take(8_000).collect();
                    app_server
                        .workflow_completion_inject(
                            codex_app_server_protocol::WorkflowCompletionInjectParams {
                                parent_thread_id: thread_id.to_string(),
                                run_id: run_id.clone(),
                                summary: report,
                            },
                        )
                        .await?;
                    if let Some(session) = self.workflow_sessions.get_mut(&session_key) {
                        session.reported_runs.insert((run_id, attempt));
                    }
                }
            }
            WorkflowEvent::SaveCompleted { thread_id, result } => match result {
                Ok(saved) => {
                    if self
                        .chat_widget
                        .thread_id()
                        .is_some_and(|current| current.to_string() == thread_id)
                    {
                        self.chat_widget
                            .add_info_message("Workflow saved".into(), Some(saved.path));
                    }
                    self.app_event_tx
                        .send(AppEvent::Workflow(WorkflowEvent::LoadCatalog { thread_id }));
                    self.app_event_tx
                        .send(AppEvent::Workflow(WorkflowEvent::Refresh));
                }
                Err(error) => {
                    if self
                        .chat_widget
                        .thread_id()
                        .is_some_and(|current| current.to_string() == thread_id)
                    {
                        self.chat_widget.add_error_message(error);
                    }
                }
            },
            WorkflowEvent::Snapshot { bridge, result } => match result {
                Ok(snapshot) => {
                    if let Some(session) = self
                        .workflow_sessions
                        .values_mut()
                        .find(|session| session.bridge.same_peer(&bridge))
                    {
                        session.active_runs = snapshot["runs"].as_array().is_some_and(|runs| {
                            runs.iter().any(|run| {
                                !matches!(
                                    run["status"].as_str(),
                                    Some("completed" | "failed" | "stopped" | "interrupted")
                                )
                            })
                        });
                    }
                    let active_matches = self
                        .chat_widget
                        .thread_id()
                        .and_then(|id| self.workflow_sessions.get(&id.to_string()))
                        .is_some_and(|session| session.bridge.same_peer(&bridge));
                    if active_matches {
                        self.chat_widget.update_workflow_status(&snapshot);
                    }
                    if active_matches
                        && let Some(Overlay::Workflow(overlay)) = self.overlay.as_mut()
                    {
                        overlay.update(snapshot);
                    }
                    tui.frame_requester().schedule_frame();
                }
                Err(error) => self
                    .chat_widget
                    .add_error_message(format!("Unable to refresh workflows: {error}")),
            },
            WorkflowEvent::HostRequest {
                bridge,
                id,
                method,
                params,
            } => {
                if method == "supervisor.workflow" {
                    app_server.register_workflow_response(id.clone(), bridge.clone());
                    let params = serde_json::from_value(params)?;
                    self.app_event_tx
                        .send(AppEvent::Workflow(WorkflowEvent::ToolCall {
                            request_id: codex_app_server_protocol::RequestId::String(id),
                            params,
                        }));
                    return Ok(());
                }
                if method == "supervisor.request" {
                    app_server.register_workflow_response(id.clone(), bridge.clone());
                    let request = serde_json::from_value(params)?;
                    self.handle_app_server_event(
                        app_server,
                        codex_app_server_client::AppServerEvent::ServerRequest(Box::new(request)),
                    )
                    .await;
                    return Ok(());
                }
                if method == "worker.start" {
                    let Some(parent_id) =
                        self.workflow_sessions.iter().find_map(|(id, session)| {
                            session.bridge.same_peer(&bridge).then(|| id.clone())
                        })
                    else {
                        bridge.respond(
                            &id,
                            Err(crate::workflow_bridge::BridgeError::host(
                                "parent workflow session is unavailable",
                            )),
                        )?;
                        return Ok(());
                    };
                    let mut params = params;
                    params["parentThreadId"] = serde_json::json!(parent_id);
                    params["roleDigest"] = params["workspace"]["roleDigest"].clone();
                    let parsed: codex_app_server_protocol::WorkflowWorkerStartParams =
                        match serde_json::from_value(params) {
                            Ok(parsed) => parsed,
                            Err(error) => {
                                bridge.respond(
                                    &id,
                                    Err(crate::workflow_bridge::BridgeError::host(format!(
                                        "invalid worker start request: {error}"
                                    ))),
                                )?;
                                return Ok(());
                            }
                        };
                    let run_id = parsed.run_id.clone();
                    let worker_id = parsed.worker_id.clone();
                    let structured = parsed.schema.is_some();
                    match app_server.workflow_worker_start(parsed).await {
                        Ok(started) => {
                            if let Some(session) = self
                                .workflow_sessions
                                .values_mut()
                                .find(|session| session.bridge.same_peer(&bridge))
                            {
                                session.pending_workers.insert(
                                    started.thread_id,
                                    PendingWorker {
                                        request_id: id,
                                        run_id,
                                        worker_id,
                                        turn_id: started.turn_id,
                                        text: String::new(),
                                        usage: serde_json::Value::Null,
                                        activity: Vec::new(),
                                        model: started.model,
                                        effort: serde_json::to_value(started.effort)
                                            .unwrap_or(serde_json::Value::Null),
                                        revision: 0,
                                        structured,
                                        started: false,
                                    },
                                );
                            } else {
                                bridge.respond(
                                    &id,
                                    Err(crate::workflow_bridge::BridgeError::host(
                                        "parent workflow session is unavailable",
                                    )),
                                )?;
                            }
                        }
                        Err(error) => bridge.respond(
                            &id,
                            Err(crate::workflow_bridge::BridgeError::worker_start(error)),
                        )?,
                    }
                    return Ok(());
                }
                let result: std::result::Result<serde_json::Value, String> = match method.as_str() {
                    "workspace.prepare" => match serde_json::from_value(params) {
                        Ok(params) => app_server
                            .workflow_workspace_prepare(params)
                            .await
                            .map_err(|error| error.to_string())
                            .and_then(|value| {
                                serde_json::to_value(value).map_err(|error| error.to_string())
                            }),
                        Err(error) => Err(error.to_string()),
                    },
                    "workspace.release" => match serde_json::from_value(params) {
                        Ok(params) => app_server
                            .workflow_workspace_release(params)
                            .await
                            .map_err(|error| error.to_string())
                            .and_then(|value| {
                                serde_json::to_value(value).map_err(|error| error.to_string())
                            }),
                        Err(error) => Err(error.to_string()),
                    },
                    "worker.interrupt" => {
                        let thread = params["threadId"]
                            .as_str()
                            .and_then(|id| ThreadId::from_string(id).ok());
                        let turn = params["turnId"].as_str().map(str::to_owned);
                        match (thread, turn) {
                            (Some(thread), Some(turn)) => {
                                crate::workflow_worker_interrupt::interrupt(
                                    app_server.request_handle(),
                                    codex_app_server_protocol::TurnInterruptParams {
                                        thread_id: thread.to_string(),
                                        turn_id: turn,
                                    },
                                )
                                .await
                                .map_err(|error| error.to_string())
                            }
                            _ => Err("invalid worker interrupt request".into()),
                        }
                    }
                    _ => Err(format!("unknown native host request: {method}")),
                };
                bridge.respond(
                    &id,
                    result.map_err(crate::workflow_bridge::BridgeError::host),
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn structured_non_completed_turns_preserve_native_status() {
        assert_eq!(
            terminal_output("interrupted", true, "").unwrap(),
            json!(null)
        );
        assert_eq!(terminal_output("failed", true, "").unwrap(), json!(null));
        assert_eq!(
            terminal_output("completed", true, r#"{"ok":true}"#).unwrap(),
            json!({"ok":true})
        );
        assert_eq!(
            terminal_output("completed", true, "").unwrap_err().code,
            "INVALID_STRUCTURED_OUTPUT"
        );
    }

    #[test]
    fn state_is_thread_scoped_outside_the_project_and_migrates_legacy_runs() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        let home = temp.path().join("codex-home");
        let legacy = cwd.join(".ultracode/native/sessions/thread-a");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("run.json"), "saved").unwrap();

        let migrated = workflow_state_dir(&home, &cwd, "thread-a").unwrap();
        assert_eq!(migrated, home.join("ultracode/sessions/thread-a"));
        assert_eq!(
            std::fs::read_to_string(migrated.join("run.json")).unwrap(),
            "saved"
        );
        assert_eq!(
            workflow_state_dir(&home, &cwd, "thread-b").unwrap(),
            home.join("ultracode/sessions/thread-b")
        );
    }

    #[tokio::test]
    async fn closing_and_reopening_view_retains_bridge_and_pending_worker() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("bridge.mjs");
        std::fs::write(&script, r#"import readline from 'node:readline';for await(const line of readline.createInterface({input:process.stdin})) {const m=JSON.parse(line);process.stdout.write(JSON.stringify({id:m.id,ok:true,result:m.method==='listRuns'?{runs:[]}:{protocolVersion:1}})+'\n');}"#).unwrap();
        let bridge =
            crate::workflow_bridge::WorkflowBridge::spawn(crate::workflow_bridge::BridgeLaunch {
                node: "node".into(),
                script,
                plugin_root: temp.path().into(),
                cwd: temp.path().into(),
                state_dir: temp.path().join("state"),
                models: json!([]),
                plugins: json!([]),
                web_search_available: false,
            })
            .await
            .unwrap();
        let mut session = WorkflowSession {
            bridge,
            pending_workers: HashMap::new(),
            consent: crate::workflow_consent::WorkflowConsentStore::load(temp.path()).unwrap(),
            reported_runs: HashSet::new(),
            active_runs: false,
        };
        session.pending_workers.insert(
            "thread".into(),
            PendingWorker {
                request_id: "request".into(),
                run_id: "run".into(),
                worker_id: "worker".into(),
                turn_id: "turn".into(),
                text: String::new(),
                usage: serde_json::Value::Null,
                activity: Vec::new(),
                model: "model".into(),
                effort: json!("low"),
                revision: 0,
                structured: false,
                started: false,
            },
        );
        let view = crate::pager_overlay::WorkflowOverlay::new(json!({"runs":[]}), None);
        drop(view);
        assert_eq!(
            session.bridge.list_runs().await.unwrap(),
            json!({"runs":[]})
        );
        assert!(session.pending_workers.contains_key("thread"));
        session.bridge.shutdown().await.unwrap();
    }
}
