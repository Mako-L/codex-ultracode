use super::*;
use futures::SinkExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use tokio_tungstenite::tungstenite::Message;

// Tests provide their own engine fixtures; production resolves only packaged resources.
fn install_fixture_node(root: &Path) {
    let output = std::process::Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let source = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let target = root.join(if cfg!(windows) { "node.exe" } else { "node" });
    if std::fs::hard_link(&source, &target).is_err() {
        std::fs::copy(source, target).unwrap();
    }
}

#[derive(Clone, Copy)]
enum CompletionOrder {
    Manual,
    BeforeStartResponse,
    StaleBeforeStartResponse,
}

struct Fixture {
    _directory: tempfile::TempDir,
    runtime: Arc<Runtime>,
    parent_id: String,
    parent: Arc<Parent>,
    run_id: String,
    thread_id: String,
    daemon_messages: mpsc::UnboundedReceiver<Value>,
    daemon_send: mpsc::UnboundedSender<Value>,
    history: Arc<Mutex<Value>>,
    parent_listeners: Arc<Mutex<HashMap<String, String>>>,
    pump: tokio::task::JoinHandle<()>,
    daemon: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new(order: CompletionOrder) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let parent_id = Uuid::new_v4().to_string();
        let run_id = Uuid::new_v4().to_string();
        let thread_id = Uuid::new_v4().to_string();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (messages, daemon_messages) = mpsc::unbounded_channel();
        let (daemon_send, mut sends) = mpsc::unbounded_channel::<Value>();
        let daemon_thread = thread_id.clone();
        let daemon_cwd = directory.path().to_path_buf();
        let history = Arc::new(Mutex::new(
            json!({"data":[{"id":"parent-final","items":[],"status":"completed","error":null}],"nextCursor":null,"backwardsCursor":null}),
        ));
        let daemon_history = history.clone();
        let parent_listeners = Arc::new(Mutex::new(HashMap::<String, String>::new()));
        let daemon_listeners = parent_listeners.clone();
        let daemon = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            loop {
                tokio::select! {
                    Some(value) = sends.recv() => { socket.send(Message::Text(value.to_string().into())).await.unwrap(); }
                    frame = socket.next() => {
                        let Some(Ok(Message::Text(frame))) = frame else { break; };
                        let request: Value = serde_json::from_str(&frame).unwrap();
                        if request.get("id").is_none() { continue; }
                        let _ = messages.send(request.clone());
                        let result = match request["method"].as_str() {
                            Some("initialize") => json!({"userAgent":"workflow-test/1.0"}),
                            Some("workflow/authority/capture") => json!({"authorityRef":"authority","generation":1,"authorityDigest":"digest","cwd":daemon_cwd,"parentModel":"gpt-5.6-luna","parentEffort":"low","models":[],"plugins":[],"webSearchAvailable":false,"workflowHostUrl":daemon_listeners.lock().unwrap().get(request["params"]["parentThreadId"].as_str().unwrap()).cloned()}),
                            Some("workflow/worker/start") => json!({"threadId":daemon_thread,"sessionId":daemon_thread,"turnId":"turn-1","model":"gpt-5.6-luna","effort":"low"}),
                            Some("workflow/completion/inject") => json!({"turnId":"parent-final"}),
                            Some("thread/turns/list") => daemon_history.lock().unwrap().clone(),
                            Some(method) => panic!("unexpected native method {method}"),
                            None => continue,
                        };
                        if request["method"] == "workflow/worker/start" {
                            if matches!(order, CompletionOrder::BeforeStartResponse) {
                                socket.send(Message::Text(completed(&daemon_thread).to_string().into())).await.unwrap();
                                for _ in 0..600 {
                                    let progress = json!({"method":"item/agentMessage/delta","params":{"threadId":daemon_thread,"turnId":"turn-1","itemId":"message-1","delta":"progress"}});
                                    socket.send(Message::Text(progress.to_string().into())).await.unwrap();
                                }
                            } else if matches!(order, CompletionOrder::StaleBeforeStartResponse) {
                                socket.send(Message::Text(stale(&daemon_thread).to_string().into())).await.unwrap();
                            }
                        }
                        socket.send(Message::Text(json!({"id":request["id"],"result":result}).to_string().into())).await.unwrap();
                    }
                }
            }
        });
        let client = crate::connect_remote_app_server(
            codex_app_server_client::RemoteAppServerEndpoint::WebSocket {
                websocket_url: format!("ws://{address}"),
                auth_token: None,
            },
        )
        .await
        .unwrap();
        let (resolve, resolutions) = mpsc::unbounded_channel();
        let runtime = Arc::new(Runtime {
            home: directory.path().into(),
            handle: client.request_handle(),
            parents: tokio::sync::Mutex::new(HashMap::new()),
            workers: Mutex::new(Workers::default()),
            pending: Mutex::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
            resolve,
            status: broadcast::channel(8).0,
            mcp: tokio::sync::Mutex::new(HashMap::new()),
            headless_parents: Mutex::new(HashSet::new()),
            headless_runs: Mutex::new(HashSet::new()),
            headless_owners: Mutex::new(HashMap::new()),
        });
        let script = directory.path().join("peer.mjs");
        std::fs::write(&script, r#"import readline from 'node:readline';
import fs from 'node:fs';
const stateDir=process.argv[process.argv.indexOf('--state-dir')+1];
fs.mkdirSync(stateDir,{recursive:true});let run;
const send=value=>process.stdout.write(JSON.stringify(value)+'\n');
readline.createInterface({input:process.stdin}).on('line',line=>{
 const message=JSON.parse(line);
 if(message.method==='hello')send({id:message.id,ok:true,result:{protocolVersion:1}});
 else if(message.method==='validateSource')send({id:message.id,ok:true,result:{digest:'native-source'}});
 else if(message.method==='runSource'){
  const launch=message.params.worker?message.params:JSON.parse(message.params.source);
  run={id:launch.runId,status:'running',attempt:1,result:null};
  fs.writeFileSync(stateDir+'/run.json',JSON.stringify(run));
  send({id:message.id,ok:true,result:{runId:run.id}});
  send({id:'node-worker',method:'worker.start',params:launch.worker});
 }else if(message.id==='node-worker'){
  run.status=message.ok?'completed':'failed';run.result=message.result?.output;
  fs.writeFileSync(stateDir+'/run.json',JSON.stringify(run));
  send({event:'runChanged',runId:run.id,revision:1});
 }else if(message.method==='finishWithoutEvent'){
  run.status='completed';run.result='completed before registration snapshot';
  fs.writeFileSync(stateDir+'/run.json',JSON.stringify(run));
  send({id:message.id,ok:true,result:{completed:true}});
 }else if(message.method==='listRuns')send({id:message.id,ok:true,result:{runs:run?[run]:[],pluginRoot:process.cwd()}});
 else if(message.method==='inspectRun')send({id:message.id,ok:true,result:run});
 else if(message.method==='shutdown'){send({id:message.id,ok:true,result:{stopped:true}});process.exit(0);}
});"#).unwrap();
        std::fs::create_dir(directory.path().join("bin")).unwrap();
        std::fs::copy(&script, directory.path().join("bin/ultracode.mjs")).unwrap();
        install_fixture_node(directory.path());
        let state = directory.path().join("state");
        let bridge = UltracodeBridge::spawn(BridgeLaunch {
            node: "node".into(),
            script,
            plugin_root: directory.path().into(),
            cwd: directory.path().into(),
            state_dir: state.clone(),
            models: json!([]),
            plugins: json!([]),
            web_search_available: false,
        })
        .await
        .unwrap();
        let parent = Arc::new(Parent {
            bridge,
            attachment: Mutex::new(None),
            directory: state,
            plugin_root: directory.path().canonicalize().unwrap(),
            completions: tokio::sync::Mutex::new(()),
            frontend_access: Arc::new(tokio::sync::Mutex::new(())),
        });
        runtime
            .parents
            .lock()
            .await
            .insert(parent_id.clone(), parent.clone());
        runtime.listen(&parent_id, &parent).unwrap();
        let pump_runtime = runtime.clone();
        let pump = tokio::spawn(async move {
            pump_runtime.pump(client, resolutions).await;
        });
        Self {
            _directory: directory,
            runtime,
            parent_id,
            parent,
            run_id,
            thread_id,
            daemon_messages,
            daemon_send,
            history,
            parent_listeners,
            pump,
            daemon,
        }
    }

    fn headless_binding(&self) -> Arc<HeadlessBinding> {
        let binding =
            Arc::new(HeadlessBinding::new(Uuid::new_v4(), &self.parent.plugin_root).unwrap());
        let url = format!("http://localhost/{}", binding.connection_id);
        self.parent_listeners
            .lock()
            .unwrap()
            .insert(self.parent_id.clone(), url.clone());
        *binding.listener_url.lock().unwrap() = Some(url);
        binding
    }

    async fn attach(&self) -> (UltracodeBridge, tokio::task::JoinHandle<io::Result<()>>) {
        self.connect(Some(&self.parent_id)).await
    }

    async fn connect(
        &self,
        parent_id: Option<&str>,
    ) -> (UltracodeBridge, tokio::task::JoinHandle<io::Result<()>>) {
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        let server = tokio::net::UnixStream::from_std(server).unwrap();
        let runtime = self.runtime.clone();
        let task =
            tokio::spawn(async move { socket::serve(runtime, server, json!("secret")).await });
        let client = UltracodeBridge::from_socket(client).unwrap();
        client
            .request("authenticate", json!({"token":"secret"}), REQUEST_TIMEOUT)
            .await
            .unwrap();
        if let Some(parent_id) = parent_id {
            client
                .request(
                    "attach",
                    json!({"parentThreadId":parent_id,"pluginRoot":self.parent.plugin_root}),
                    REQUEST_TIMEOUT,
                )
                .await
                .unwrap();
        }
        (client, task)
    }

    async fn launch(&mut self, client: &UltracodeBridge) {
        let params = self.launch_params();
        client
            .request("runSource", params, REQUEST_TIMEOUT)
            .await
            .unwrap();
        self.wait_for_worker().await;
    }

    fn launch_params(&self) -> Value {
        json!({"runId":self.run_id,"worker":{
            "runId":self.run_id,"workerId":"worker-1","authorityRef":"authority","authorityDigest":"digest","authorityGeneration":1,
            "prompt":"Return result","model":"gpt-5.6-luna","effort":"low","agentType":"default","readOnly":true,
            "workspace":{"workspaceId":null,"cwd":self.parent.directory,"isolated":false,"baseCommit":null,"roleDigest":"native-role"},"schema":null,"resumeThreadId":null
        }})
    }

    async fn wait_for_worker(&mut self) {
        loop {
            let request = tokio::time::timeout(Duration::from_secs(5), self.daemon_messages.recv())
                .await
                .unwrap()
                .unwrap();
            if request["method"] == "workflow/worker/start" {
                assert_eq!(request["params"]["parentThreadId"], json!(self.parent_id));
                assert_eq!(request["params"]["roleDigest"], json!("native-role"));
                break;
            }
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let correlated = {
                    let workers = self.runtime.workers.lock().unwrap();
                    workers.pending.contains_key(&self.thread_id)
                        || workers.closed.contains_key(&self.thread_id)
                };
                if correlated {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    fn finish(&self) {
        self.daemon_send.send(completed(&self.thread_id)).unwrap();
    }

    async fn close(self) {
        let bridges: Vec<_> = self
            .runtime
            .parents
            .lock()
            .await
            .values()
            .map(|parent| parent.bridge.clone())
            .collect();
        for bridge in bridges {
            let _ = bridge.shutdown().await;
        }
        self.pump.abort();
        self.daemon.abort();
    }
}

#[tokio::test]
async fn detached_worker_completes_over_daemon_and_reconnect_returns_result_once() {
    let mut fixture = Fixture::new(CompletionOrder::Manual).await;
    let (client, connection) = fixture.attach().await;
    fixture.launch(&client).await;
    assert_eq!(
        client
            .request("detach", json!({}), REQUEST_TIMEOUT)
            .await
            .unwrap(),
        json!({"attached":false,"workersContinue":true})
    );
    connection.abort();
    drop(client);
    fixture.finish();
    let completion = tokio::time::timeout(Duration::from_secs(5), fixture.daemon_messages.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completion["method"], json!("workflow/completion/inject"));
    assert!(
        completion["params"]["summary"]
            .as_str()
            .unwrap()
            .contains("actual worker result")
    );
    let (reconnected, connection) = fixture.attach().await;
    let snapshot = reconnected.list_runs().await.unwrap();
    assert_eq!(snapshot["runs"][0]["result"], json!("actual worker result"));
    assert_eq!(snapshot["runs"][0]["status"], json!("completed"));
    fixture
        .runtime
        .complete(&fixture.parent_id, &fixture.parent, &fixture.run_id)
        .await
        .unwrap();
    assert!(fixture.daemon_messages.try_recv().is_err());
    let ledger: Value = serde_json::from_slice(
        &std::fs::read(
            fixture
                .parent
                .directory
                .join(format!("completion-{}-1.json", fixture.run_id)),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(ledger["status"], json!("delivered"));
    assert_eq!(ledger["turnId"], json!("parent-final"));
    connection.abort();
    fixture.close().await;
}

#[tokio::test]
async fn failed_detach_preserves_frontend_and_pending_worker() {
    let mut fixture = Fixture::new(CompletionOrder::Manual).await;
    let (client, connection) = fixture.attach().await;
    fixture.launch(&client).await;
    std::fs::create_dir(fixture.parent.directory.join("attachment.json")).unwrap();
    assert!(
        client
            .request("detach", json!({}), Duration::from_secs(1))
            .await
            .is_err()
    );
    // A failed durable handoff must not clear the owner before reporting failure.
    assert!(fixture.parent.attachment.lock().unwrap().is_some());
    assert_eq!(
        client.list_runs().await.unwrap()["runs"][0]["status"],
        json!("running")
    );
    fixture.finish();
    connection.abort();
    fixture.close().await;
}

#[tokio::test]
async fn detached_approval_remains_pending_and_replays_on_attach() {
    let mut fixture = Fixture::new(CompletionOrder::Manual).await;
    let (client, connection) = fixture.attach().await;
    fixture.launch(&client).await;
    client
        .request("detach", json!({}), REQUEST_TIMEOUT)
        .await
        .unwrap();
    let request: ServerRequest = serde_json::from_value(json!({"method":"item/commandExecution/requestApproval","id":77,"params":{"threadId":fixture.thread_id,"turnId":"turn-1","itemId":"command-1","startedAtMs":0,"command":"touch file","cwd":"/tmp"}})).unwrap();
    let mut stale_request = serde_json::to_value(&request).unwrap();
    stale_request["params"]["turnId"] = json!("prior-turn");
    fixture
        .runtime
        .approval(serde_json::from_value(stale_request).unwrap())
        .await;
    assert!(fixture.runtime.approvals.lock().unwrap().is_empty());
    fixture.runtime.approval(request).await;
    assert_eq!(fixture.runtime.approvals.lock().unwrap().len(), 1);
    assert!(fixture.daemon_messages.try_recv().is_err());
    let (attached, replay) = fixture.attach().await;
    let mut events = attached.take_events().unwrap();
    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event,BridgeEvent::Request { method, .. } if method == "supervisor.request"));
    assert_eq!(fixture.runtime.approvals.lock().unwrap().len(), 1);
    connection.abort();
    replay.abort();
    fixture.close().await;
}

fn completed(thread_id: &str) -> Value {
    json!({"method":"turn/completed","params":{"threadId":thread_id,"turn":{"id":"turn-1","status":"completed","items":[{"type":"agentMessage","id":"message-1","text":"actual worker result"}],"error":null}}})
}

#[tokio::test]
async fn completion_before_worker_start_response_is_replayed() {
    let mut fixture = Fixture::new(CompletionOrder::BeforeStartResponse).await;
    let (client, connection) = fixture.attach().await;
    fixture.launch(&client).await;
    let completion = tokio::time::timeout(Duration::from_secs(5), fixture.daemon_messages.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completion["method"], json!("workflow/completion/inject"));
    assert_eq!(
        client.list_runs().await.unwrap()["runs"][0]["result"],
        json!("actual worker result")
    );
    assert!(fixture.runtime.workers.lock().unwrap().pending.is_empty());
    connection.abort();
    fixture.close().await;
}

fn stale(thread_id: &str) -> Value {
    let mut value = completed(thread_id);
    value["params"]["turn"]["id"] = json!("prior-turn");
    value["params"]["turn"]["items"][0]["text"] = json!("stale result");
    value
}

#[tokio::test]
async fn stale_turns_before_and_after_start_response_cannot_finish_new_attempt() {
    let mut fixture = Fixture::new(CompletionOrder::StaleBeforeStartResponse).await;
    let (client, connection) = fixture.attach().await;
    fixture.launch(&client).await;
    fixture.daemon_send.send(stale(&fixture.thread_id)).unwrap();
    fixture.finish();
    let completion = tokio::time::timeout(Duration::from_secs(5), fixture.daemon_messages.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completion["method"], json!("workflow/completion/inject"));
    assert_eq!(
        client.list_runs().await.unwrap()["runs"][0]["result"],
        json!("actual worker result")
    );
    let terminal: ServerNotification =
        serde_json::from_value(completed(&fixture.thread_id)).unwrap();
    let mut workers = fixture.runtime.workers.lock().unwrap();
    workers.notification(terminal);
    assert!(workers.early.is_empty());
    assert!(workers.terminals.is_empty());
    drop(workers);
    connection.abort();
    fixture.close().await;
}

#[tokio::test]
async fn unbound_and_wrong_parent_replies_do_not_consume_consent() {
    let fixture = Fixture::new(CompletionOrder::Manual).await;
    let (rightful, rightful_connection) = fixture.attach().await;
    let (unbound, unbound_connection) = fixture.connect(None).await;
    let other_id = Uuid::new_v4().to_string();
    fixture.runtime.parents.lock().await.insert(
        other_id.clone(),
        Arc::new(Parent {
            bridge: fixture.parent.bridge.clone(),
            attachment: Mutex::new(None),
            directory: fixture.parent.directory.clone(),
            plugin_root: fixture.parent.plugin_root.clone(),
            completions: tokio::sync::Mutex::new(()),
            frontend_access: Arc::new(tokio::sync::Mutex::new(())),
        }),
    );
    let (wrong, wrong_connection) = fixture.connect(Some(&other_id)).await;
    let id = "workflow-supervisor:owned";
    let (reply, received) = oneshot::channel();
    fixture.runtime.pending.lock().unwrap().insert(
        id.into(),
        PendingConsent {
            parent_id: fixture.parent_id.clone(),
            connection_id: fixture
                .parent
                .attachment
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .0,
            response: reply,
        },
    );
    unbound.respond(id, Ok(json!({"unexpected":true}))).unwrap();
    assert!(
        unbound
            .request("resolve", json!({"id":id,"result":{}}), REQUEST_TIMEOUT)
            .await
            .is_err()
    );
    assert!(fixture.runtime.pending.lock().unwrap().contains_key(id));
    wrong.respond(id, Ok(json!({"unexpected":true}))).unwrap();
    assert!(
        wrong
            .request("resolve", json!({"id":id,"result":{}}), REQUEST_TIMEOUT)
            .await
            .is_err()
    );
    assert!(fixture.runtime.pending.lock().unwrap().contains_key(id));
    rightful
        .request(
            "resolve",
            json!({"id":id,"result":{"authorized":true}}),
            REQUEST_TIMEOUT,
        )
        .await
        .unwrap();
    assert_eq!(received.await.unwrap(), json!({"authorized":true}));
    rightful_connection.abort();
    unbound_connection.abort();
    wrong_connection.abort();
    fixture.close().await;
}

#[tokio::test]
async fn noncanonical_parent_aliases_are_rejected_before_runtime_lookup() {
    let fixture = Fixture::new(CompletionOrder::Manual).await;
    for alias in [
        fixture.parent_id.replace('-', ""),
        format!("{{{}}}", fixture.parent_id),
    ] {
        let error = match fixture
            .runtime
            .parent(&alias, &fixture.parent.plugin_root)
            .await
        {
            Ok(_) => panic!("accepted noncanonical parent"),
            Err(error) => error,
        };
        assert_eq!(error.message, "parent thread ID must be a canonical UUID");
    }
    assert_eq!(fixture.runtime.parents.lock().await.len(), 1);
    fixture.close().await;
}

#[tokio::test]
async fn different_frontend_templates_keep_separate_mcp_listeners() {
    let fixture = Fixture::new(CompletionOrder::Manual).await;
    let first = serde_json::to_value(ThreadStartParams {
        cwd: Some("/project-one".into()),
        model: Some("gpt-5.6-luna".into()),
        config: Some(HashMap::from([(
            "developer_instructions".into(),
            json!("project one policy"),
        )])),
        ..Default::default()
    })
    .unwrap();
    let second = serde_json::to_value(ThreadStartParams {
        cwd: Some("/project-two".into()),
        model: Some("gpt-5.6-terra".into()),
        config: Some(HashMap::from([(
            "developer_instructions".into(),
            json!("project two policy"),
        )])),
        ..Default::default()
    })
    .unwrap();
    let first = json!({"threadStartParams":first,"pluginRoot":fixture.parent.plugin_root});
    let second = json!({"threadStartParams":second,"pluginRoot":fixture.parent.plugin_root});
    let first_listener = fixture
        .runtime
        .configure(first.clone(), None)
        .await
        .unwrap();
    let second_listener = fixture
        .runtime
        .configure(second.clone(), None)
        .await
        .unwrap();
    assert_ne!(first_listener["url"], second_listener["url"]);
    assert_eq!(
        fixture.runtime.configure(first, None).await.unwrap(),
        first_listener
    );
    assert_eq!(
        fixture.runtime.configure(second, None).await.unwrap(),
        second_listener
    );
    fixture.close().await;
}

fn workflow_call(parent_id: &str) -> DynamicToolCallParams {
    DynamicToolCallParams {
        thread_id: parent_id.into(),
        turn_id: "parent-turn".into(),
        call_id: "call".into(),
        namespace: Some("codex_tui".into()),
        tool: "workflow".into(),
        arguments: json!({"script":"workflow source"}),
    }
}

#[tokio::test]
async fn parent_startup_contention_waits_for_attached_frontend() {
    let fixture = Fixture::new(CompletionOrder::Manual).await;
    let (client, connection) = fixture.attach().await;
    let mut events = client.take_events().unwrap();
    let frontend = WorkflowFrontend {
        runtime: fixture.runtime.clone(),
        plugin_root: fixture.parent.plugin_root.clone(),
        headless: None,
    };
    let params = workflow_call(&fixture.parent_id);
    let guard = fixture.runtime.parents.lock().await;
    let mut call = tokio::spawn(async move { frontend.call(params).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut call)
            .await
            .is_err()
    );
    drop(guard);
    let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .unwrap()
        .unwrap();
    let BridgeEvent::Request { id, method, .. } = event else {
        panic!("expected consent callback");
    };
    assert_eq!(method, "supervisor.workflow");
    let expected = crate::dynamic_tools::success_response(json!({"accepted":true})).unwrap();
    client
        .request(
            "resolve",
            json!({"id":id,"result":expected}),
            REQUEST_TIMEOUT,
        )
        .await
        .unwrap();
    assert_eq!(call.await.unwrap(), expected);
    connection.abort();
    fixture.close().await;
}

#[tokio::test]
async fn parent_creates_state_directory_before_bridge_spawn() {
    let fixture = Fixture::new(CompletionOrder::Manual).await;
    let plugin_root = fixture._directory.path().join("no-state-plugin");
    std::fs::create_dir_all(plugin_root.join("bin")).unwrap();
    let bridge_script = std::fs::read_to_string(fixture._directory.path().join("peer.mjs"))
        .unwrap()
        .replace("fs.mkdirSync(stateDir,{recursive:true});", "");
    std::fs::write(plugin_root.join("bin/ultracode.mjs"), bridge_script).unwrap();
    install_fixture_node(&plugin_root);

    let parent_id = Uuid::new_v4().to_string();
    let parent = fixture
        .runtime
        .parent(&parent_id, &plugin_root)
        .await
        .unwrap();

    assert!(parent.directory.is_dir());
    let attachment = parent.directory.join("attachment.json");
    socket::replace_private(
        &attachment,
        &json!({"parentThreadId":parent_id,"attached":false}),
    )
    .unwrap();
    assert_eq!(
        socket::read_private(&attachment).unwrap(),
        json!({"parentThreadId":parent_id,"attached":false})
    );

    parent.bridge.shutdown().await.unwrap();
    fixture.close().await;
}

#[tokio::test]
async fn old_connection_cleanup_preserves_new_frontend_consent() {
    let fixture = Fixture::new(CompletionOrder::Manual).await;
    let (old, old_connection) = fixture.attach().await;
    old.request("detach", json!({}), REQUEST_TIMEOUT)
        .await
        .unwrap();
    let (current, current_connection) = fixture.attach().await;
    let mut events = current.take_events().unwrap();
    let frontend = WorkflowFrontend {
        runtime: fixture.runtime.clone(),
        plugin_root: fixture.parent.plugin_root.clone(),
        headless: None,
    };
    let params = workflow_call(&fixture.parent_id);
    let call = tokio::spawn(async move { frontend.call(params).await });
    let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .unwrap()
        .unwrap();
    let BridgeEvent::Request { id, .. } = event else {
        panic!("expected consent callback");
    };
    old.notify(json!({"invalid":"connection closes after detach"}))
        .unwrap();
    assert!(old_connection.await.unwrap().is_err());
    assert!(fixture.runtime.pending.lock().unwrap().contains_key(&id));
    let expected = crate::dynamic_tools::success_response(json!({"accepted":true})).unwrap();
    current
        .request(
            "resolve",
            json!({"id":id,"result":expected}),
            REQUEST_TIMEOUT,
        )
        .await
        .unwrap();
    assert_eq!(call.await.unwrap(), expected);
    current_connection.abort();
    fixture.close().await;
}

#[tokio::test]
async fn parent_bridges_and_templates_use_their_explicit_plugin_roots() {
    let fixture = Fixture::new(CompletionOrder::Manual).await;
    let mut roots = Vec::new();
    for name in ["first-install", "second-install"] {
        let root = fixture._directory.path().join(name);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::copy(
            fixture._directory.path().join("peer.mjs"),
            root.join("bin/ultracode.mjs"),
        )
        .unwrap();
        install_fixture_node(&root);
        roots.push(root.canonicalize().unwrap());
    }
    let parent_a_id = Uuid::new_v4().to_string();
    let parent_a = fixture
        .runtime
        .parent(&parent_a_id, &roots[0])
        .await
        .unwrap();
    let parent_b = fixture
        .runtime
        .parent(&Uuid::new_v4().to_string(), &roots[1])
        .await
        .unwrap();
    assert_eq!(
        parent_a.bridge.list_runs().await.unwrap()["pluginRoot"],
        json!(roots[0])
    );
    assert_eq!(
        parent_b.bridge.list_runs().await.unwrap()["pluginRoot"],
        json!(roots[1])
    );
    assert!(
        fixture
            .runtime
            .parent(&parent_a_id, &roots[1])
            .await
            .is_err()
    );
    let template = serde_json::to_value(ThreadStartParams::default()).unwrap();
    let first = fixture
        .runtime
        .configure(
            json!({"threadStartParams":template,"pluginRoot":roots[0]}),
            None,
        )
        .await
        .unwrap();
    let second = fixture
        .runtime
        .configure(
            json!({"threadStartParams":template,"pluginRoot":roots[1]}),
            None,
        )
        .await
        .unwrap();
    assert_ne!(first["url"], second["url"]);
    fixture.close().await;
}

#[tokio::test]
async fn second_parent_detach_failure_reattaches_first_and_allows_retry() {
    let mut fixture = Fixture::new(CompletionOrder::Manual).await;
    let (first, first_connection) = fixture.attach().await;
    fixture.launch(&first).await;
    let second_id = Uuid::new_v4().to_string();
    let (second, second_connection) = fixture.connect(Some(&second_id)).await;
    let second_parent = fixture
        .runtime
        .parents
        .lock()
        .await
        .get(&second_id)
        .cloned()
        .unwrap();
    let blocked = second_parent.directory.join("attachment.json");
    std::fs::create_dir(&blocked).unwrap();
    let parents = vec![
        (fixture.parent_id.clone(), first.clone()),
        (second_id, second.clone()),
    ];
    assert!(crate::ultracode_bridge::detach_all(&parents).await.is_err());
    assert!(fixture.parent.attachment.lock().unwrap().is_some());
    assert!(second_parent.attachment.lock().unwrap().is_some());
    assert_eq!(
        first.list_runs().await.unwrap()["runs"][0]["status"],
        json!("running")
    );
    assert!(
        second.list_runs().await.unwrap()["runs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    std::fs::remove_dir(blocked).unwrap();
    crate::ultracode_bridge::detach_all(&parents).await.unwrap();
    assert!(fixture.parent.attachment.lock().unwrap().is_none());
    assert!(second_parent.attachment.lock().unwrap().is_none());
    first_connection.abort();
    second_connection.abort();
    fixture.close().await;
}

#[path = "ultracode_headless_lifecycle_tests.rs"]
mod headless_lifecycle;
