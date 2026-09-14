use super::*;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::process::Stdio;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::UnixListener;
use tokio::net::UnixStream;

const MAX_FRAME: u64 = 8 * 1024 * 1024;

pub(super) fn authentication_result() -> Value {
    json!({
        "authenticated": true,
        "protocolVersion": 4,
        "supervisorProcessId": std::process::id(),
    })
}

pub(super) fn write_private_new(path: &Path, value: &Value) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.sync_all()
}

pub(super) fn read_private(path: &Path) -> io::Result<Value> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe workflow state file",
        ));
    }
    serde_json::from_reader(file).map_err(io::Error::other)
}

pub(super) fn replace_private(path: &Path, value: &Value) -> io::Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    write_private_new(&temporary, value)?;
    std::fs::rename(temporary, path)?;
    File::open(
        path.parent()
            .ok_or_else(|| io::Error::other("missing state directory"))?,
    )?
    .sync_all()
}

fn read_token(path: &Path) -> io::Result<Value> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe workflow host token",
        ));
    }
    serde_json::from_reader(file).map_err(io::Error::other)
}

pub(crate) async fn connect(home: &Path) -> io::Result<UltracodeBridge> {
    let directory =
        codex_app_server_daemon::workflow_backend_state_dir(home, &std::env::current_exe()?)
            .map_err(io::Error::other)?;
    std::fs::create_dir_all(&directory)?;
    let socket = directory.join("host.sock");
    let stream = match std::os::unix::net::UnixStream::connect(&socket) {
        Ok(stream) => stream,
        Err(_) => {
            let log = OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(directory.join("host.log"))?;
            let mut command = Command::new(std::env::current_exe()?);
            command
                .arg("--internal-workflow-supervisor")
                .arg(home)
                .stdin(Stdio::null())
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log));
            // SAFETY: setsid is async-signal-safe and does not access shared Rust state.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
            let mut child = command.spawn()?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
            loop {
                if let Ok(stream) = std::os::unix::net::UnixStream::connect(&socket) {
                    break stream;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "workflow host did not become ready; inspect {}",
                            directory.join("host.log").display()
                        ),
                    ));
                }
                // Another simultaneous frontend may own startup; its socket is equally usable.
                let _ = child.try_wait()?;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };
    let peer = UltracodeBridge::from_socket(stream).map_err(io::Error::other)?;
    let authentication = peer
        .request(
            "authenticate",
            json!({"token":read_token(&directory.join("host.token"))?}),
            REQUEST_TIMEOUT,
        )
        .await
        .map_err(io::Error::other)?;
    if authentication["protocolVersion"] != 4 {
        return Err(io::Error::other(
            "workflow host protocol version mismatch; reconnect with the matching native host version",
        ));
    }
    Ok(peer)
}

pub(crate) async fn attach(
    home: &Path,
    parent_id: &str,
    plugin_root: &Path,
) -> Result<UltracodeBridge, BridgeError> {
    let bridge = connect(home)
        .await
        .map_err(|error| BridgeError::host(error.to_string()))?;
    let attached = bridge
        .request(
            "attach",
            json!({"parentThreadId":parent_id,"pluginRoot":plugin_root}),
            REQUEST_TIMEOUT,
        )
        .await;
    if let Err(error) = attached {
        bridge.disconnect("workflow attachment failed");
        return Err(error);
    }
    Ok(bridge)
}

pub async fn run(home: PathBuf) -> io::Result<()> {
    let directory =
        codex_app_server_daemon::workflow_backend_state_dir(&home, &std::env::current_exe()?)
            .map_err(io::Error::other)?;
    std::fs::create_dir_all(&directory)?;
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("host.lock"))?;
    // SAFETY: lock owns a valid file descriptor for the entire supervisor lifetime.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let token_path = directory.join("host.token");
    let fresh = json!(format!(
        "{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    ));
    match write_private_new(&token_path, &fresh) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let token = read_token(&token_path)?;
    let socket = directory.join("host.sock");
    match std::fs::remove_file(&socket) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let daemon_socket = codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(
        directory.join("control.sock"),
    )?;
    let client = crate::connect_remote_app_server(
        codex_app_server_client::RemoteAppServerEndpoint::UnixSocket {
            socket_path: daemon_socket,
        },
    )
    .await
    .map_err(|error| io::Error::other(error.to_string()))?;
    let (resolve, resolve_rx) = mpsc::unbounded_channel();
    let runtime = Arc::new(Runtime {
        home,
        handle: client.request_handle(),
        parents: tokio::sync::Mutex::new(HashMap::new()),
        workers: Mutex::new(workers::Workers::default()),
        pending: Mutex::new(HashMap::new()),
        approvals: Mutex::new(HashMap::new()),
        resolve,
        status: broadcast::channel(256).0,
        mcp: tokio::sync::Mutex::new(HashMap::new()),
        headless_parents: Mutex::new(HashSet::new()),
        headless_runs: Mutex::new(HashSet::new()),
        headless_owners: Mutex::new(HashMap::new()),
    });
    let events = runtime.clone();
    tokio::spawn(async move {
        events.pump(client, resolve_rx).await;
    });
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    loop {
        let (stream, _) = listener.accept().await?;
        let runtime = runtime.clone();
        let token = token.clone();
        tokio::spawn(async move {
            if let Err(error) = serve(runtime, stream, token).await {
                tracing::debug!(%error, "workflow frontend disconnected");
            }
        });
    }
}

pub(super) async fn serve(
    runtime: Arc<Runtime>,
    stream: UnixStream,
    token: Value,
) -> io::Result<()> {
    let connection_id = Uuid::new_v4();
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let (sender, mut outgoing) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        while let Some(value) = outgoing.recv().await {
            let mut frame = serde_json::to_vec(&value).map_err(io::Error::other)?;
            frame.push(b'\n');
            write.write_all(&frame).await?;
        }
        Ok::<_, io::Error>(())
    });
    let mut authenticated = false;
    let mut configured = false;
    let mut headless_control: Option<Arc<HeadlessBinding>> = None;
    let mut parent_id: Option<String> = None;
    let result = async {
        loop {
            let mut frame = Vec::new();
            let size = (&mut reader).take(MAX_FRAME + 1).read_until(b'\n', &mut frame).await?;
            if size == 0 { break; }
            if size as u64 > MAX_FRAME { return Err(io::Error::other("workflow host frame exceeds 8 MiB")); }
            let value: Value = serde_json::from_slice(&frame).map_err(io::Error::other)?;
            let id = value["id"].as_str().ok_or_else(|| io::Error::other("missing workflow request ID"))?.to_string();
            if !authenticated {
                if value["method"] != "authenticate" || value["params"]["token"] != token {
                    return Err(io::Error::new(io::ErrorKind::PermissionDenied, "invalid workflow host token"));
                }
                authenticated = true;
                let _ = sender.send(json!({"id":id,"ok":true,"result":authentication_result()}));
                continue;
            }
            if value.get("method").is_none() {
                let owned = if let Some(parent_id) = &parent_id {
                    runtime.parents.lock().await.get(parent_id).is_some_and(|parent| parent.attachment.lock().unwrap().as_ref().is_some_and(|(owner, _)| *owner == connection_id))
                } else { false };
                let mut pending = runtime.pending.lock().unwrap();
                if owned && pending.get(&id).is_some_and(|request| parent_id.as_ref() == Some(&request.parent_id) && request.connection_id == connection_id)
                    && let Some(reply) = pending.remove(&id)
                {
                    let _ = reply.response.send(value["result"].clone());
                }
                continue;
            }
            let method = value["method"].as_str().unwrap_or_default().to_string();
            let params = value["params"].clone();
            if method == "configure" {
                if configured || parent_id.is_some() { return Err(io::Error::other("frontend connection is already configured or attached")); }
                let selected_headless = if params["frontend"] == "headless" {
                    let root = params["pluginRoot"].as_str().ok_or_else(|| io::Error::other("missing frontend bundled workflow runtime"))?;
                    Some(Arc::new(HeadlessBinding::new(connection_id,Path::new(root)).map_err(io::Error::other)?))
                } else { None };
                let response = match runtime.configure(params,selected_headless.clone()).await {
                    Ok(config) => { configured = true; headless_control = selected_headless; json!({"id":id,"ok":true,"result":config}) },
                    Err(error) => json!({"id":id,"ok":false,"error":{"code":error.code,"message":error.message}}),
                };
                let _ = sender.send(response);
                continue;
            }
            if method == "attach" {
                let attachment = async {
                if headless_control.is_some() { return Err(io::Error::other("headless connection cannot attach interactively")); }
                let requested_parent = params["parentThreadId"].as_str().ok_or_else(|| io::Error::other("missing parent thread ID"))?;
                if parent_id.as_ref().is_some_and(|parent| parent != requested_parent) { return Err(io::Error::other("connection already belongs to another parent")); }
                let parent = if parent_id.as_deref() == Some(requested_parent) && params.get("pluginRoot").is_none() {
                    // This socket already authenticated and bound this parent/root pair.
                    runtime.parents.lock().await.get(requested_parent).cloned().ok_or_else(|| io::Error::other("parent runtime unavailable"))?
                } else {
                    let plugin_root = params["pluginRoot"].as_str().ok_or_else(|| io::Error::other("missing frontend bundled workflow runtime"))?;
                    runtime.parent(requested_parent, Path::new(plugin_root)).await.map_err(io::Error::other)?
                };
                let _access = parent.frontend_access.lock().await;
                let mut attachment = parent.attachment.lock().unwrap();
                if attachment.as_ref().is_some_and(|(owner, _)| *owner != connection_id) {
                    return Err(io::Error::new(io::ErrorKind::AlreadyExists, "workflow parent already has an attached frontend"));
                }
                *attachment = Some((connection_id, sender.clone()));
                drop(attachment);
                runtime.headless_owners.lock().unwrap().remove(requested_parent);
                runtime.headless_parents.lock().unwrap().remove(requested_parent);
                parent_id = Some(requested_parent.to_string());
                let _ = sender.send(json!({"id":id,"ok":true,"result":{"attached":true}}));
                let requests: Vec<Value> = runtime.approvals.lock().unwrap().values().cloned().collect();
                for request in requests {
                    if request["parentThreadId"] == requested_parent {
                        let _ = sender.send(request["event"].clone());
                    }
                }
                Ok::<_, io::Error>(())
                }.await;
                if let Err(error) = attachment {
                    let _ = sender.send(json!({"id":id,"ok":false,"error":{"code":"ATTACH_FAILED","message":error.to_string()}}));
                }
                continue;
            }
            if method == "detach" {
                let parent = parent_id.as_ref().ok_or_else(|| io::Error::other("no parent attached"))?;
                let owned = runtime.parents.lock().await.get(parent).cloned().ok_or_else(|| io::Error::other("parent runtime unavailable"))?;
                let mut attachment = owned.attachment.lock().unwrap();
                if attachment.as_ref().is_none_or(|(owner, _)| *owner != connection_id) { return Err(io::Error::other("frontend does not own attachment")); }
                if let Err(error) = replace_private(&owned.directory.join("attachment.json"), &json!({"parentThreadId":parent,"attached":false})) {
                    let _ = sender.send(json!({"id":id,"ok":false,"error":{"code":"DETACH_FAILED","message":error.to_string()}}));
                    continue;
                }
                *attachment = None;
                runtime.pending.lock().unwrap().retain(|_, request| request.connection_id != connection_id);
                let _ = sender.send(json!({"id":id,"ok":true,"result":{"attached":false,"workersContinue":true}}));
                continue;
            }
            let runtime = runtime.clone();
            let sender = sender.clone();
            let parent_id = parent_id.clone();
            let headless_control = headless_control.clone();
            tokio::spawn(async move {
                let result = async {
                    if method == "headless" {
                        let binding = headless_control.ok_or_else(|| BridgeError::host("headless control requires an explicitly configured headless frontend"))?;
                        return runtime.headless_state(&binding,params).await;
                    }
                    if method == "resolve" || method == "reject" {
                        let Some(parent_id) = &parent_id else { return Err(BridgeError::host("workflow response requires an attached parent")); };
                        let parents = runtime.parents.lock().await;
                        let owned = parents.get(parent_id).is_some_and(|parent| parent.attachment.lock().unwrap().as_ref().is_some_and(|(owner, _)| *owner == connection_id));
                        if !owned { return Err(BridgeError::host("workflow response connection does not own parent")); }
                        drop(parents);
                        let key = params["id"].as_str().ok_or_else(|| BridgeError::host("missing supervisor response ID"))?;
                        if key.starts_with("workflow-supervisor:") {
                            let mut pending_requests = runtime.pending.lock().unwrap();
                            if !pending_requests.get(key).is_some_and(|request| &request.parent_id == parent_id && request.connection_id == connection_id) { return Err(BridgeError::host("workflow response belongs to another parent")); }
                            if let Some(pending) = pending_requests.remove(key) {
                                let result = if method == "reject" { serde_json::to_value(crate::dynamic_tools::failure_response("Native frontend declined workflow launch")).map_err(|error| BridgeError::host(error.to_string()))? } else { params["result"].clone() };
                                let _ = pending.response.send(result);
                            }
                        } else {
                            let mut approvals = runtime.approvals.lock().unwrap();
                            if !approvals.get(key).is_some_and(|approval| approval["parentThreadId"] == parent_id.as_str()) { return Err(BridgeError::host("approval belongs to another parent")); }
                            let approval = approvals.remove(key).ok_or_else(|| BridgeError::host("unknown supervisor approval"))?;
                            drop(approvals);
                            let original: RequestId = serde_json::from_value(approval["originalId"].clone()).map_err(|error| BridgeError::host(error.to_string()))?;
                            let resolution = if method == "reject" {
                                Resolution::Reject(original, serde_json::from_value(params["error"].clone()).map_err(|error| BridgeError::host(error.to_string()))?)
                            } else { Resolution::Accept(original, params["result"].clone()) };
                            runtime.resolve.send(resolution).map_err(|error| BridgeError::host(error.to_string()))?;
                        }
                        return Ok(json!({"resolved":true}));
                    }
                    let parent_id = parent_id.ok_or_else(|| BridgeError::host("workflow parent is not attached"))?;
                    let parent = runtime.parents.lock().await.get(&parent_id).cloned().ok_or_else(|| BridgeError::host("parent workflow runtime is unavailable"))?;
                    if parent.attachment.lock().unwrap().as_ref().is_none_or(|(owner, _)| *owner != connection_id) { return Err(BridgeError::host("workflow frontend is detached")); }
                    if method == "shutdown" {
                        parent.bridge.shutdown().await?;
                        runtime.parents.lock().await.remove(&parent_id);
                        return Ok(json!({"stopped":true}));
                    }
                    parent.bridge.request(&method, params, REQUEST_TIMEOUT).await
                }.await;
                let response = match result {
                    Ok(result) => json!({"id":id,"ok":true,"result":result}),
                    Err(error) => json!({"id":id,"ok":false,"error":{"code":error.code,"message":error.message}}),
                };
                let _ = sender.send(response);
            });
        }
        Ok(())
    }.await;
    if let Some(parent_id) = parent_id {
        if let Some(parent) = runtime.parents.lock().await.get(&parent_id) {
            let mut attachment = parent.attachment.lock().unwrap();
            if attachment
                .as_ref()
                .is_some_and(|(owner, _)| *owner == connection_id)
            {
                *attachment = None;
            }
        }
        runtime
            .pending
            .lock()
            .unwrap()
            .retain(|_, request| request.connection_id != connection_id);
    }
    if let Some(binding) = headless_control {
        binding.release(&runtime).await;
    }
    writer.abort();
    result
}
