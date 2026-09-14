use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct BridgeLaunch {
    pub node: PathBuf,
    pub script: PathBuf,
    pub plugin_root: PathBuf,
    pub cwd: PathBuf,
    pub state_dir: PathBuf,
    pub models: Value,
    pub plugins: Value,
    pub web_search_available: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BridgeEvent {
    RunChanged {
        run_id: String,
        revision: u64,
    },
    Request {
        id: String,
        method: String,
        params: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BridgeError {
    pub code: String,
    pub message: String,
    pub outcome_unresolved: bool,
}
impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for BridgeError {}

#[cfg(test)]
#[path = "ultracode_worker_configuration_tests.rs"]
mod worker_configuration_tests;
impl BridgeError {
    pub(crate) fn worker_start(error: codex_app_server_client::TypedRequestError) -> Self {
        if matches!(&error, codex_app_server_client::TypedRequestError::Server { source, .. }
            if matches!(source.code, -32600 | -32602))
        {
            Self::host_code("INVALID_WORKER_CONFIGURATION", error.to_string())
        } else {
            Self::host(error.to_string())
        }
    }

    fn internal(message: impl Into<String>, unresolved: bool) -> Self {
        Self {
            code: "INTERNAL".into(),
            message: message.into(),
            outcome_unresolved: unresolved,
        }
    }
    pub(crate) fn host(message: impl Into<String>) -> Self {
        Self {
            code: "HOST_ERROR".into(),
            message: message.into(),
            outcome_unresolved: false,
        }
    }
    pub(crate) fn host_code(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            outcome_unresolved: false,
        }
    }
}

struct Inner {
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<Box<dyn Write + Send>>>,
    #[cfg(unix)]
    socket: Option<std::os::unix::net::UnixStream>,
    supervised: bool,
    pending: Mutex<HashMap<String, oneshot::Sender<Result<Value, BridgeError>>>>,
    sequence: AtomicU64,
    closed: AtomicBool,
    events: Mutex<Option<mpsc::UnboundedReceiver<BridgeEvent>>>,
    stderr: Arc<Mutex<String>>,
}
#[derive(Clone)]
pub(crate) struct UltracodeBridge {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for UltracodeBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UltracodeBridge").finish_non_exhaustive()
    }
}

impl UltracodeBridge {
    pub(crate) fn same_peer(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    pub(crate) async fn spawn(launch: BridgeLaunch) -> Result<Self, BridgeError> {
        if !launch.plugin_root.is_absolute()
            || !launch.cwd.is_absolute()
            || !launch.state_dir.is_absolute()
        {
            return Err(BridgeError::internal(
                "bridge paths must be absolute",
                false,
            ));
        }
        let mut child = Command::new(&launch.node)
            .arg(&launch.script)
            .args(["bridge", "--stdio", "--cwd"])
            .arg(&launch.cwd)
            .arg("--state-dir")
            .arg(&launch.state_dir)
            .current_dir(&launch.plugin_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                BridgeError::internal(format!("failed to spawn Ultracode bridge: {e}"), false)
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| BridgeError::internal("bridge stdin unavailable", false))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| BridgeError::internal("bridge stdout unavailable", false))?;
        let stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| BridgeError::internal("bridge stderr unavailable", false))?;
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(Some(Box::new(stdin))),
            #[cfg(unix)]
            socket: None,
            supervised: false,
            pending: Mutex::new(HashMap::new()),
            sequence: AtomicU64::new(0),
            closed: AtomicBool::new(false),
            events: Mutex::new(Some(event_rx)),
            stderr: Arc::new(Mutex::new(String::new())),
        });
        let peer = Self {
            inner: inner.clone(),
        };
        let reader = peer.clone();
        thread::spawn(move || reader.read_loop(stdout, event_tx));
        let errors = inner.stderr.clone();
        thread::spawn(move || read_stderr(stderr_pipe, errors));
        if let Err(error) = peer
            .request(
                "hello",
                json!({"protocolVersion":1,"cwd":launch.cwd,"models":launch.models,"plugins":launch.plugins,"webSearchAvailable":launch.web_search_available}),
                Duration::from_secs(30),
            )
            .await
        {
            peer.disconnect("bridge handshake failed; remote outcome unresolved");
            if let Ok(mut child) = peer.inner.child.lock() {
                if let Some(child) = child.as_mut() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                child.take();
            }
            return Err(error);
        }
        Ok(peer)
    }
    pub(crate) fn is_supervised(&self) -> bool {
        self.inner.supervised
    }

    #[cfg(unix)]
    pub(crate) fn from_socket(stream: std::os::unix::net::UnixStream) -> Result<Self, BridgeError> {
        let writer = stream
            .try_clone()
            .map_err(|error| BridgeError::host(error.to_string()))?;
        let socket = stream
            .try_clone()
            .map_err(|error| BridgeError::host(error.to_string()))?;
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let peer = Self {
            inner: Arc::new(Inner {
                child: Mutex::new(None),
                stdin: Mutex::new(Some(Box::new(writer))),
                socket: Some(socket),
                supervised: true,
                pending: Mutex::new(HashMap::new()),
                sequence: AtomicU64::new(0),
                closed: AtomicBool::new(false),
                events: Mutex::new(Some(event_rx)),
                stderr: Arc::new(Mutex::new(String::new())),
            }),
        };
        let reader = peer.clone();
        thread::spawn(move || reader.read_loop(stream, event_tx));
        Ok(peer)
    }

    pub(crate) fn take_events(&self) -> Option<mpsc::UnboundedReceiver<BridgeEvent>> {
        self.inner.events.lock().ok()?.take()
    }
    pub(crate) async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, BridgeError> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(BridgeError::internal("bridge is closed", false));
        }
        let id = format!(
            "rust:{}",
            self.inner.sequence.fetch_add(1, Ordering::SeqCst) + 1
        );
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self
                .inner
                .pending
                .lock()
                .map_err(|_| BridgeError::internal("bridge pending lock poisoned", false))?;
            if pending.len() >= 1024 {
                return Err(BridgeError::internal(
                    "bridge pending request limit exceeded",
                    false,
                ));
            }
            pending.insert(id.clone(), tx);
        }
        let encoded = serde_json::to_vec(&json!({"id":id,"method":method,"params":params}))
            .map_err(|e| BridgeError::internal(e.to_string(), false))?;
        if encoded.len() > MAX_LINE_BYTES {
            self.inner
                .pending
                .lock()
                .ok()
                .and_then(|mut p| p.remove(&id));
            return Err(BridgeError::internal(
                "outgoing bridge line exceeds 8 MiB",
                false,
            ));
        }
        if let Err(error) = self.write_line(&encoded) {
            self.inner
                .pending
                .lock()
                .ok()
                .and_then(|mut p| p.remove(&id));
            return Err(error);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(BridgeError::internal(
                "bridge disconnected; remote outcome unresolved",
                true,
            )),
            Err(_) => {
                self.inner
                    .pending
                    .lock()
                    .ok()
                    .and_then(|mut p| p.remove(&id));
                Err(BridgeError::internal(
                    "bridge request timed out; remote outcome unresolved",
                    true,
                ))
            }
        }
    }
    fn write_line(&self, encoded: &[u8]) -> Result<(), BridgeError> {
        let mut guard = self
            .inner
            .stdin
            .lock()
            .map_err(|_| BridgeError::internal("bridge stdin lock poisoned", true))?;
        let stdin = guard.as_mut().ok_or_else(|| {
            BridgeError::internal("bridge stdin closed; remote outcome unresolved", true)
        })?;
        stdin
            .write_all(encoded)
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .map_err(|e| {
                BridgeError::internal(
                    format!("bridge write failed: {e}; remote outcome unresolved"),
                    true,
                )
            })
    }
    fn read_loop<R: Read>(&self, mut source: R, event_tx: mpsc::UnboundedSender<BridgeEvent>) {
        let mut pending = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match source.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => {
                    for byte in &chunk[..count] {
                        if *byte == b'\n' {
                            if let Ok(value) = serde_json::from_slice::<Value>(&pending) {
                                self.handle_message(value, &event_tx)
                            }
                            pending.clear()
                        } else if pending.len() >= MAX_LINE_BYTES {
                            self.disconnect(
                                "incoming bridge line exceeds 8 MiB; remote outcomes unresolved",
                            );
                            return;
                        } else {
                            pending.push(*byte)
                        }
                    }
                }
                Err(error) => {
                    self.disconnect(&format!(
                        "bridge read failed: {error}; remote outcomes unresolved"
                    ));
                    return;
                }
            }
        }
        self.disconnect("bridge disconnected; remote outcomes unresolved")
    }
    fn handle_message(&self, value: Value, event_tx: &mpsc::UnboundedSender<BridgeEvent>) {
        if let Some(event) = value["event"].as_str() {
            if event == "runChanged"
                && let (Some(run_id), Some(revision)) =
                    (value["runId"].as_str(), value["revision"].as_u64())
            {
                let _ = event_tx.send(BridgeEvent::RunChanged {
                    run_id: run_id.into(),
                    revision,
                });
            }
            return;
        }
        if let (Some(id), Some(method)) = (value["id"].as_str(), value["method"].as_str()) {
            let _ = event_tx.send(BridgeEvent::Request {
                id: id.into(),
                method: method.into(),
                params: value.get("params").cloned().unwrap_or_else(|| json!({})),
            });
            return;
        }
        let Some(id) = value["id"].as_str() else {
            return;
        };
        let Some(sender) = self
            .inner
            .pending
            .lock()
            .ok()
            .and_then(|mut p| p.remove(id))
        else {
            return;
        };
        let result = if value["ok"] == true {
            Ok(value.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(BridgeError {
                code: value
                    .pointer("/error/code")
                    .and_then(Value::as_str)
                    .unwrap_or("INTERNAL")
                    .into(),
                message: value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("invalid bridge error")
                    .into(),
                outcome_unresolved: false,
            })
        };
        let _ = sender.send(result);
    }
    pub(crate) fn respond(
        &self,
        id: &str,
        result: Result<Value, BridgeError>,
    ) -> Result<(), BridgeError> {
        let value = match result {
            Ok(result) => json!({"id":id,"ok":true,"result":result}),
            Err(error) => {
                json!({"id":id,"ok":false,"error":{"code":error.code,"message":error.message}})
            }
        };
        let encoded = serde_json::to_vec(&value)
            .map_err(|error| BridgeError::internal(error.to_string(), false))?;
        self.write_line(&encoded)
    }
    pub(crate) fn notify(&self, event: Value) -> Result<(), BridgeError> {
        let encoded = serde_json::to_vec(&event)
            .map_err(|error| BridgeError::internal(error.to_string(), false))?;
        self.write_line(&encoded)
    }
    pub(crate) fn disconnect(&self, message: &str) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        #[cfg(unix)]
        if let Some(socket) = &self.inner.socket {
            // The reader owns a bridge clone, so dropping the caller cannot close its socket.
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        self.inner.stdin.lock().ok().and_then(|mut s| s.take());
        let error = BridgeError::internal(message, true);
        if let Ok(mut pending) = self.inner.pending.lock() {
            for (_, sender) in pending.drain() {
                let _ = sender.send(Err(error.clone()));
            }
        }
    }
    pub(crate) async fn list_runs(&self) -> Result<Value, BridgeError> {
        self.request("listRuns", json!({}), Duration::from_secs(30))
            .await
    }
    pub(crate) async fn inspect_run(&self, run_id: &str) -> Result<Value, BridgeError> {
        self.request(
            "inspectRun",
            json!({"runId":run_id}),
            Duration::from_secs(30),
        )
        .await
    }
    pub(crate) async fn shutdown(&self) -> Result<(), BridgeError> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let result = self
            .request("shutdown", json!({}), Duration::from_secs(5))
            .await
            .map(|_| ());
        self.disconnect("bridge shutdown; remote outcomes unresolved");
        if let Ok(mut child) = self.inner.child.lock() {
            if let Some(child) = child.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
            child.take();
        }
        result
    }
}
/// Detach a frontend's parents as one recoverable handoff.
pub(crate) async fn detach_all(parents: &[(String, UltracodeBridge)]) -> Result<(), BridgeError> {
    if parents.iter().any(|(_, bridge)| !bridge.is_supervised()) {
        return Err(BridgeError::host(
            "Background workflows require the native workflow host; this frontend still owns execution.",
        ));
    }
    for (index, (_, bridge)) in parents.iter().enumerate() {
        let detached = bridge
            .request("detach", json!({}), Duration::from_secs(30))
            .await
            .and_then(|ack| {
                if ack["attached"] == false && ack["workersContinue"] == true {
                    Ok(())
                } else {
                    Err(BridgeError::host("invalid workflow detach acknowledgement"))
                }
            });
        if let Err(mut error) = detached {
            for (parent_id, attempted) in parents[..=index].iter().rev() {
                if let Err(recovery) = attempted
                    .request(
                        "attach",
                        json!({"parentThreadId":parent_id}),
                        Duration::from_secs(30),
                    )
                    .await
                {
                    error.message.push_str(&format!(
                        "; reattachment for {parent_id} failed: {recovery}"
                    ));
                }
            }
            return Err(error);
        }
    }
    Ok(())
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(child) = self.child.get_mut()
            && let Some(child) = child.as_mut()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn read_stderr<R: Read>(mut source: R, target: Arc<Mutex<String>>) {
    let mut bytes = [0u8; 4096];
    while let Ok(count) = source.read(&mut bytes) {
        if count == 0 {
            break;
        }
        if let Ok(mut text) = target.lock() {
            text.push_str(&String::from_utf8_lossy(&bytes[..count]));
            if text.len() > 64 * 1024 {
                let drain = text.len() - 64 * 1024;
                text.drain(..drain);
            }
        }
    }
}

#[cfg(all(test, unix))]
#[path = "ultracode_socket_disconnect_tests.rs"]
mod socket_disconnect_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;
    #[tokio::test]
    async fn real_node_sidecar_handshake_list_event_and_shutdown() {
        let temp = tempdir().unwrap();
        let script = temp.path().join("peer.mjs");
        fs::write(&script,r#"import readline from 'node:readline';let hello;const r=readline.createInterface({input:process.stdin});r.on('line',line=>{const m=JSON.parse(line);if(m.method==='hello'){hello=m.params;process.stdout.write(JSON.stringify({id:m.id,ok:true,result:{protocolVersion:1}})+'\n')}if(m.method==='listRuns'){process.stdout.write(JSON.stringify({event:'runChanged',runId:'one',revision:1})+'\n');process.stdout.write(JSON.stringify({id:m.id,ok:true,result:{runs:[],hello}})+'\n')}if(m.method==='shutdown'){process.stdout.write(JSON.stringify({id:m.id,ok:true,result:{stopped:true}})+'\n')}});"#).unwrap();
        let launch = BridgeLaunch {
            node: "node".into(),
            script,
            plugin_root: temp.path().into(),
            cwd: temp.path().into(),
            state_dir: temp.path().join("state"),
            models: json!([{"model":"gpt-test"}]),
            plugins: json!([{"name":"acme","root":"/plugin","workflows":["flows"]}]),
            web_search_available: true,
        };
        let expected_cwd = launch.cwd.clone();
        let bridge = UltracodeBridge::spawn(launch).await.unwrap();
        let mut events = bridge.take_events().unwrap();
        assert_eq!(
            bridge.list_runs().await.unwrap(),
            json!({"runs":[],"hello":{"protocolVersion":1,"cwd":expected_cwd,"models":[{"model":"gpt-test"}],"plugins":[{"name":"acme","root":"/plugin","workflows":["flows"]}],"webSearchAvailable":true}})
        );
        assert_eq!(
            events.recv().await,
            Some(BridgeEvent::RunChanged {
                run_id: "one".into(),
                revision: 1
            })
        );
        bridge.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn child_exit_rejects_pending_as_unresolved() {
        let temp = tempdir().unwrap();
        let script = temp.path().join("exit.mjs");
        fs::write(&script, "import readline from 'node:readline';readline.createInterface({input:process.stdin}).on('line',line=>{const m=JSON.parse(line);if(m.method==='hello')process.stdout.write(JSON.stringify({id:m.id,ok:true,result:{protocolVersion:1}})+'\\n');else process.exit(2)});").unwrap();
        let bridge = UltracodeBridge::spawn(BridgeLaunch {
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
        let error = bridge.list_runs().await.unwrap_err();
        assert!(error.outcome_unresolved);
    }
    #[tokio::test]
    async fn handshake_failure_is_reported_without_returning_a_live_controller() {
        let temp = tempdir().unwrap();
        let script = temp.path().join("fail.mjs");
        fs::write(&script, "process.exit(2)").unwrap();
        let result = UltracodeBridge::spawn(BridgeLaunch {
            node: "node".into(),
            script,
            plugin_root: temp.path().into(),
            cwd: temp.path().into(),
            state_dir: temp.path().join("state"),
            models: json!([]),
            plugins: json!([]),
            web_search_available: false,
        })
        .await;
        assert!(matches!(result,Err(error) if error.outcome_unresolved));
    }
}
