use anyhow::Result;
use codex_agent_extension::AgentInvocation;
use codex_agent_extension::AgentRunner;
use codex_protocol::protocol::EventMsg;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;

#[test]
fn starts_resolved_agent_prompt_in_forked_thread() -> Result<()> {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    skip_if_no_network!(Ok(()));

                    let server = responses::start_mock_server().await;
                    let response_mock = responses::mount_sse_once(
                        &server,
                        responses::sse(vec![
                            responses::ev_response_created("agent-response"),
                            responses::ev_completed("agent-response"),
                        ]),
                    )
                    .await;
                    let test = test_codex().build_with_auto_env(&server).await?;
                    let parent_thread_id = test.session_configured.session_id.into();
                    let agent_runner =
                        AgentRunner::new(std::sync::Arc::downgrade(&test.thread_manager));

                    let agent_run = agent_runner
                        .start(
                            parent_thread_id,
                            AgentInvocation {
                                config: test.config.clone(),
                                prompt: "Use $example-agent to inspect the current changes."
                                    .to_string(),
                                parent_trace: None,
                                output_schema: None,
                                reserved_thread_id: None,
                                thread_source: None,
                                start_gate: None,
                            },
                        )
                        .await?;

                    assert_ne!(agent_run.thread_id, parent_thread_id);
                    assert_eq!(
                        agent_run
                            .thread
                            .config_snapshot()
                            .await
                            .forked_from_thread_id,
                        Some(parent_thread_id)
                    );
                    let started = wait_for_event(&agent_run.thread, |event| {
                        matches!(event, EventMsg::TurnStarted(_))
                    })
                    .await;
                    let EventMsg::TurnStarted(started) = started else {
                        unreachable!("event predicate only matches turn started events");
                    };
                    assert_eq!(started.turn_id, agent_run.turn_id);
                    wait_for_event(&agent_run.thread, |event| {
                        matches!(event, EventMsg::TurnComplete(_))
                    })
                    .await;

                    let request = response_mock.single_request();
                    assert!(
                        request.message_input_texts("user").iter().any(
                            |text| text == "Use $example-agent to inspect the current changes."
                        )
                    );

                    Ok(())
                })
        })?
        .join()
        .map_err(|_| anyhow::anyhow!("agent start test panicked"))?
}

#[test]
fn resumes_owned_thread_with_structured_output() -> Result<()> {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    skip_if_no_network!(Ok(()));

                    let server = responses::start_mock_server().await;
                    let response_mock = responses::mount_sse_sequence(
                        &server,
                        vec![
                            responses::sse(vec![responses::ev_completed("first")]),
                            responses::sse(vec![responses::ev_completed("repair")]),
                        ],
                    )
                    .await;
                    let test = test_codex().build_with_auto_env(&server).await?;
                    let parent_thread_id = test.session_configured.session_id.into();
                    let runner = AgentRunner::new(std::sync::Arc::downgrade(&test.thread_manager));
                    let schema = serde_json::json!({
                        "type": "object",
                        "properties": {"passed": {"type": "boolean"}},
                        "required": ["passed"],
                        "additionalProperties": false
                    });
                    let first = runner
                        .start(
                            parent_thread_id,
                            AgentInvocation {
                                config: test.config.clone(),
                                prompt: "verify".into(),
                                parent_trace: None,
                                output_schema: Some(schema.clone()),
                                reserved_thread_id: Some(codex_protocol::ThreadId::new()),
                                thread_source: Some(
                                    codex_protocol::protocol::ThreadSource::Feature(
                                        "ultracode-worker".into(),
                                    ),
                                ),
                                start_gate: None,
                            },
                        )
                        .await?;
                    wait_for_event(&first.thread, |event| {
                        matches!(event, EventMsg::TurnComplete(_))
                    })
                    .await;
                    let repair = runner
                        .resume(
                            first.thread.clone(),
                            "repair".into(),
                            Some(schema.clone()),
                            None,
                        )
                        .await?;
                    assert_eq!(repair.thread_id, first.thread_id);
                    wait_for_event(&repair.thread, |event| {
                        matches!(event, EventMsg::TurnComplete(_))
                    })
                    .await;

                    let requests = response_mock.requests();
                    assert_eq!(requests.len(), 2);
                    for request in requests {
                        let body = request.body_json();
                        assert_eq!(body["text"]["format"]["schema"], schema);
                    }
                    Ok(())
                })
        })?
        .join()
        .map_err(|_| anyhow::anyhow!("structured-output worker test panicked"))?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waits_for_listener_gate_before_submitting_fast_turn() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![responses::ev_completed("fast")]),
    )
    .await;
    let test = test_codex().build_with_auto_env(&server).await?;
    let parent_thread_id = test.session_configured.session_id.into();
    let runner = AgentRunner::new(std::sync::Arc::downgrade(&test.thread_manager));
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
    let launch = tokio::spawn(async move {
        runner
            .start(
                parent_thread_id,
                AgentInvocation {
                    config: test.config.clone(),
                    prompt: "fast".into(),
                    parent_trace: None,
                    output_schema: None,
                    reserved_thread_id: Some(codex_protocol::ThreadId::new()),
                    thread_source: Some(codex_protocol::protocol::ThreadSource::Feature(
                        "ultracode-worker".into(),
                    )),
                    start_gate: Some(gate_rx),
                },
            )
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(response_mock.requests().is_empty());
    gate_tx.send(()).expect("release listener gate");
    let run = launch.await??;
    wait_for_event(&run.thread, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(response_mock.requests().len(), 1);
    Ok(())
}
