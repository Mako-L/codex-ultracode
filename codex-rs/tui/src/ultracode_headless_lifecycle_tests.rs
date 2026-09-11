use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn headless_registration_does_not_query_unmaterialized_turn_history() {
    let mut fixture = Fixture::new(CompletionOrder::Manual).await;
    *fixture.history.lock().unwrap() = json!({"unavailable":"thread has no first user message"});
    let binding = fixture.headless_binding();
    let state = fixture
        .runtime
        .headless_state(
            &binding,
            json!({"parentThreadId":fixture.parent_id,"action":"register"}),
        )
        .await
        .unwrap();
    assert_eq!(state["active"], json!(false));
    assert_eq!(state["turnId"], Value::Null);
    while let Ok(request) = fixture.daemon_messages.try_recv() {
        assert_ne!(request["method"], json!("thread/turns/list"));
    }
    assert!(
        fixture
            .runtime
            .headless_parents
            .lock()
            .unwrap()
            .contains(&fixture.parent_id)
    );
}

#[tokio::test]
async fn registration_adopts_owned_run_that_finished_before_initial_snapshot() {
    let mut fixture = Fixture::new(CompletionOrder::Manual).await;
    let frontend = WorkflowFrontend {
        runtime: fixture.runtime.clone(),
        plugin_root: fixture.parent.plugin_root.clone(),
        headless: Some(fixture.headless_binding()),
    };
    let mut params = workflow_call(&fixture.parent_id);
    params.arguments = json!({"script":fixture.launch_params().to_string()});
    assert!(frontend.call(params).await.success);
    fixture.wait_for_worker().await;
    fixture
        .parent
        .bridge
        .request("finishWithoutEvent", json!({}), REQUEST_TIMEOUT)
        .await
        .unwrap();
    fixture.runtime.headless_runs.lock().unwrap().clear();
    frontend
        .headless
        .as_ref()
        .unwrap()
        .release(&fixture.runtime)
        .await;

    let state = fixture
        .runtime
        .headless_state(
            &fixture.headless_binding(),
            json!({"parentThreadId":fixture.parent_id,"action":"register"}),
        )
        .await
        .unwrap();
    assert_eq!(state["completionTurnIds"], json!(["parent-final"]));
    let completion = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(request) = fixture.daemon_messages.recv().await {
            if request["method"] == "workflow/completion/inject" {
                return request;
            }
        }
        panic!("daemon closed before completion injection");
    })
    .await
    .unwrap();
    assert_eq!(completion["method"], json!("workflow/completion/inject"));
    assert_eq!(completion["params"]["runId"], json!(fixture.run_id));
    assert!(
        !fixture
            .parent
            .directory
            .join(format!("headless-run-{}.json", fixture.run_id))
            .exists()
    );
    fixture.close().await;
}

#[tokio::test]
async fn accepted_completion_waits_for_actual_daemon_turn_persistence() {
    let mut fixture = Fixture::new(CompletionOrder::Manual).await;
    let (client, connection) = fixture.attach().await;
    fixture.launch(&client).await;
    fixture
        .runtime
        .headless_runs
        .lock()
        .unwrap()
        .insert((fixture.parent_id.clone(), fixture.run_id.clone()));
    client
        .request("detach", json!({}), REQUEST_TIMEOUT)
        .await
        .unwrap();
    fixture.finish();
    let completion = tokio::time::timeout(Duration::from_secs(5), fixture.daemon_messages.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completion["method"], json!("workflow/completion/inject"));
    fixture
        .runtime
        .complete(&fixture.parent_id, &fixture.parent, &fixture.run_id)
        .await
        .unwrap();
    let binding = fixture.headless_binding();
    let params = json!({"parentThreadId":fixture.parent_id,"action":"status"});
    {
        let mut history = fixture.history.lock().unwrap();
        history["data"][0]["id"] = json!("initial-turn");
    }
    let before_persistence = fixture
        .runtime
        .headless_state(&binding, params.clone())
        .await
        .unwrap();
    assert_eq!(before_persistence["turnId"], json!("initial-turn"));
    assert_eq!(before_persistence["completionPending"], json!(true));
    assert_eq!(
        before_persistence["pendingCompletionTurnIds"],
        json!(["parent-final"])
    );
    {
        let mut history = fixture.history.lock().unwrap();
        history["data"][0]["id"] = json!("parent-final");
        history["data"][0]["status"] = json!("inProgress");
    }
    assert_eq!(
        fixture
            .runtime
            .headless_state(&binding, params.clone())
            .await
            .unwrap()["completionPending"],
        json!(true)
    );
    fixture.history.lock().unwrap()["data"][0]["status"] = json!("completed");
    let completed = fixture
        .runtime
        .headless_state(&binding, params)
        .await
        .unwrap();
    assert_eq!(completed["completionPending"], json!(false));
    assert_eq!(completed["completionTurnIds"], json!(["parent-final"]));
    connection.abort();
    fixture.close().await;
}
#[tokio::test]
async fn explicit_headless_route_launches_worker_without_tui_consent() {
    let mut fixture = Fixture::new(CompletionOrder::Manual).await;
    let frontend = WorkflowFrontend {
        runtime: fixture.runtime.clone(),
        plugin_root: fixture.parent.plugin_root.clone(),
        headless: Some(fixture.headless_binding()),
    };
    let mut params = workflow_call(&fixture.parent_id);
    params.arguments = json!({"script":fixture.launch_params().to_string()});
    let response = frontend.call(params).await;
    assert!(response.success);
    assert!(fixture.parent.attachment.lock().unwrap().is_none());
    assert!(fixture.runtime.pending.lock().unwrap().is_empty());
    fixture.wait_for_worker().await;
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
    fixture.close().await;
}

#[tokio::test]
async fn model_arguments_cannot_select_headless_permission_handling() {
    let fixture = Fixture::new(CompletionOrder::Manual).await;
    let frontend = WorkflowFrontend {
        runtime: fixture.runtime.clone(),
        plugin_root: fixture.parent.plugin_root.clone(),
        headless: None,
    };
    let mut params = workflow_call(&fixture.parent_id);
    params.arguments["frontend"] = json!("headless");
    params.arguments["native_workflow_host"] = json!(true);
    assert!(!frontend.call(params).await.success);
    assert!(fixture.runtime.headless_parents.lock().unwrap().is_empty());
    assert!(fixture.runtime.workers.lock().unwrap().pending.is_empty());
    fixture.close().await;
}

#[tokio::test]
async fn headless_worker_approval_uses_native_exec_rejection() {
    let mut fixture = Fixture::new(CompletionOrder::Manual).await;
    let frontend = WorkflowFrontend {
        runtime: fixture.runtime.clone(),
        plugin_root: fixture.parent.plugin_root.clone(),
        headless: Some(fixture.headless_binding()),
    };
    let mut params = workflow_call(&fixture.parent_id);
    params.arguments = json!({"script":fixture.launch_params().to_string()});
    assert!(frontend.call(params).await.success);
    fixture.wait_for_worker().await;
    let request: ServerRequest = serde_json::from_value(json!({"method":"item/commandExecution/requestApproval","id":88,"params":{"threadId":fixture.thread_id,"turnId":"turn-1","itemId":"command-1","startedAtMs":0,"command":"touch file","cwd":"/tmp"}})).unwrap();
    fixture.runtime.approval(request).await;
    let rejection = tokio::time::timeout(Duration::from_secs(5), fixture.daemon_messages.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rejection["id"], json!(88));
    assert!(
        rejection["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not supported in headless workflow execution")
    );
    assert!(fixture.runtime.approvals.lock().unwrap().is_empty());
    fixture.close().await;
}

#[tokio::test]
async fn headless_control_requires_explicit_authenticated_frontend_configuration() {
    let fixture = Fixture::new(CompletionOrder::Manual).await;
    let (client, connection) = fixture.connect(None).await;
    let error = client
        .request(
            "headless",
            json!({"parentThreadId":fixture.parent_id,"action":"stop","frontend":"headless"}),
            REQUEST_TIMEOUT,
        )
        .await
        .unwrap_err();
    assert!(
        error
            .message
            .contains("explicitly configured headless frontend")
    );
    assert!(fixture.runtime.headless_parents.lock().unwrap().is_empty());
    connection.abort();
    fixture.close().await;
}

#[tokio::test]
async fn headless_resume_does_not_wait_for_historical_completion_records() {
    let mut fixture = Fixture::new(CompletionOrder::Manual).await;
    let bridge = fixture.parent.bridge.clone();
    fixture.launch(&bridge).await;
    fixture.finish();
    let completion = tokio::time::timeout(Duration::from_secs(5), fixture.daemon_messages.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completion["method"], json!("workflow/completion/inject"));
    fixture
        .runtime
        .complete(&fixture.parent_id, &fixture.parent, &fixture.run_id)
        .await
        .unwrap();
    std::fs::remove_file(
        fixture
            .parent
            .directory
            .join(format!("completion-{}-1.json", fixture.run_id)),
    )
    .unwrap();
    let state = fixture
        .runtime
        .headless_state(
            &fixture.headless_binding(),
            json!({"parentThreadId":fixture.parent_id,"action":"status"}),
        )
        .await
        .unwrap();
    assert_eq!(
        state,
        json!({"active":false,"completionPending":false,"unresolved":[],"completionTurnIds":[],"pendingCompletionTurnIds":[],"turnId":"parent-final","turnStatus":"completed"})
    );
    fixture.close().await;
}

async fn configure_headless(client: &UltracodeBridge, root: &Path) -> Value {
    client
        .request(
            "configure",
            json!({"frontend":"headless","pluginRoot":root,
        "threadStartParams":ThreadStartParams::default()}),
            REQUEST_TIMEOUT,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn headless_socket_requires_its_listener_root_parent_and_mode() {
    let fixture = Fixture::new(CompletionOrder::Manual).await;
    let (first, first_connection) = fixture.connect(None).await;
    let (second, second_connection) = fixture.connect(None).await;
    let first_mcp = configure_headless(&first, &fixture.parent.plugin_root).await;
    let second_mcp = configure_headless(&second, &fixture.parent.plugin_root).await;
    assert_ne!(first_mcp["url"], second_mcp["url"]);
    // Only the native daemon's effective config associates the actual parent with A.
    fixture.parent_listeners.lock().unwrap().insert(
        fixture.parent_id.clone(),
        first_mcp["url"].as_str().unwrap().into(),
    );
    let params = json!({"parentThreadId":fixture.parent_id,"action":"status"});
    assert!(
        second
            .request("headless", params.clone(), REQUEST_TIMEOUT)
            .await
            .unwrap_err()
            .message
            .contains("listener")
    );
    assert!(fixture.runtime.headless_parents.lock().unwrap().is_empty());
    assert!(
        first
            .request("headless", params.clone(), REQUEST_TIMEOUT)
            .await
            .is_ok()
    );
    let other_parent = Uuid::new_v4().to_string();
    fixture.parent_listeners.lock().unwrap().insert(
        other_parent.clone(),
        first_mcp["url"].as_str().unwrap().into(),
    );
    assert!(
        first
            .request(
                "headless",
                json!({"parentThreadId":other_parent,"action":"stop"}),
                REQUEST_TIMEOUT
            )
            .await
            .unwrap_err()
            .message
            .contains("another parent")
    );
    assert!(
        !fixture
            .runtime
            .parents
            .lock()
            .await
            .contains_key(&other_parent)
    );
    assert!(
        !fixture
            .runtime
            .headless_parents
            .lock()
            .unwrap()
            .contains(&other_parent)
    );
    let (interactive, interactive_connection) = fixture.attach().await;
    assert!(
        !fixture
            .runtime
            .headless_parents
            .lock()
            .unwrap()
            .contains(&fixture.parent_id)
    );
    let mut stop = params.clone();
    stop["action"] = json!("stop");
    assert!(
        first
            .request("headless", stop.clone(), REQUEST_TIMEOUT)
            .await
            .unwrap_err()
            .message
            .contains("interactive")
    );
    interactive
        .request("detach", json!({}), REQUEST_TIMEOUT)
        .await
        .unwrap();
    assert!(
        first
            .request("headless", stop, REQUEST_TIMEOUT)
            .await
            .unwrap_err()
            .message
            .contains("no longer owns")
    );
    assert!(
        !fixture
            .runtime
            .headless_parents
            .lock()
            .unwrap()
            .contains(&fixture.parent_id)
    );
    first_connection.abort();
    second_connection.abort();
    interactive_connection.abort();
    fixture.close().await;
}

#[tokio::test]
async fn headless_connection_cannot_adopt_parent_from_another_plugin_root() {
    let fixture = Fixture::new(CompletionOrder::Manual).await;
    let other_root = tempfile::tempdir().unwrap();
    let (client, connection) = fixture.connect(None).await;
    let mcp = configure_headless(&client, other_root.path()).await;
    fixture.parent_listeners.lock().unwrap().insert(
        fixture.parent_id.clone(),
        mcp["url"].as_str().unwrap().into(),
    );
    assert!(
        client
            .request(
                "headless",
                json!({"parentThreadId":fixture.parent_id,"action":"stop"}),
                REQUEST_TIMEOUT
            )
            .await
            .unwrap_err()
            .message
            .contains("different plugin root")
    );
    assert!(fixture.runtime.headless_parents.lock().unwrap().is_empty());
    connection.abort();
    fixture.close().await;
}

#[tokio::test]
async fn configured_headless_socket_cannot_change_frontend_mode() {
    let fixture = Fixture::new(CompletionOrder::Manual).await;
    let (client, connection) = fixture.connect(None).await;
    configure_headless(&client, &fixture.parent.plugin_root).await;
    assert!(
        client
            .request(
                "configure",
                json!({"frontend":"interactive","pluginRoot":fixture.parent.plugin_root,
        "threadStartParams":ThreadStartParams::default()}),
                REQUEST_TIMEOUT
            )
            .await
            .is_err()
    );
    assert!(fixture.parent.attachment.lock().unwrap().is_none());
    connection.abort();
    fixture.close().await;
}

#[tokio::test]
async fn closed_headless_listener_cannot_claim_a_parent_later() {
    let fixture = Fixture::new(CompletionOrder::Manual).await;
    let binding = fixture.headless_binding();
    binding.release(&fixture.runtime).await;
    let frontend = WorkflowFrontend {
        runtime: fixture.runtime.clone(),
        plugin_root: fixture.parent.plugin_root.clone(),
        headless: Some(binding),
    };
    assert!(
        !frontend
            .call(workflow_call(&fixture.parent_id))
            .await
            .success
    );
    assert!(fixture.runtime.headless_parents.lock().unwrap().is_empty());
    fixture.close().await;
}
