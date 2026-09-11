use codex_core::CodexThread;
use codex_core::NewThread;
use codex_core::StartIfIdleSubmission;
use codex_core::StartThreadOptions;
use codex_core::ThreadManager;
use codex_core::TurnInputRequest;
use codex_core::TurnStartOptions;
use codex_core::config::Config;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::ThreadSource;
use codex_protocol::protocol::W3cTraceContext;
use codex_protocol::user_input::UserInput;
use serde_json::Value;
use std::sync::Arc;
use std::sync::Weak;

/// A fully resolved agent invocation.
///
/// Agent discovery owns rendering `prompt`, including any selected skill
/// references. The runtime only starts that prompt in isolated forked context.
pub struct AgentInvocation {
    pub config: Config,
    pub prompt: String,
    pub parent_trace: Option<W3cTraceContext>,
    pub output_schema: Option<Value>,
    pub reserved_thread_id: Option<ThreadId>,
    pub thread_source: Option<ThreadSource>,
    pub start_gate: Option<tokio::sync::oneshot::Receiver<()>>,
}

/// A spawned agent whose initial turn has been submitted.
pub struct AgentRun {
    pub thread_id: ThreadId,
    pub turn_id: String,
    pub thread: Arc<CodexThread>,
}

/// Runs resolved agents in threads forked by the owning [`ThreadManager`].
#[derive(Clone)]
pub struct AgentRunner {
    thread_manager: Weak<ThreadManager>,
}

impl AgentRunner {
    pub fn new(thread_manager: Weak<ThreadManager>) -> Self {
        Self { thread_manager }
    }

    /// Starts a resolved agent in a fork of `parent_thread_id`.
    pub async fn start(
        &self,
        parent_thread_id: ThreadId,
        invocation: AgentInvocation,
    ) -> CodexResult<AgentRun> {
        let AgentInvocation {
            config,
            prompt,
            parent_trace,
            output_schema,
            reserved_thread_id,
            thread_source,
            start_gate,
        } = invocation;
        if prompt.trim().is_empty() {
            return Err(CodexErr::InvalidRequest(
                "agent prompt must not be empty".to_string(),
            ));
        }

        let thread_manager = self
            .thread_manager
            .upgrade()
            .ok_or_else(|| CodexErr::UnsupportedOperation("thread manager dropped".to_string()))?;
        let NewThread {
            thread_id, thread, ..
        } = thread_manager
            .spawn_subagent(
                parent_thread_id,
                StartThreadOptions {
                    parent_trace: parent_trace.clone(),
                    reserved_thread_id,
                    thread_source,
                    ..StartThreadOptions::new(config)
                },
            )
            .await?;
        if let Some(start_gate) = start_gate {
            tokio::time::timeout(std::time::Duration::from_secs(5), start_gate)
                .await
                .map_err(|_| {
                    CodexErr::InvalidRequest(
                        "agent event listener was not attached before start".to_string(),
                    )
                })?
                .map_err(|_| {
                    CodexErr::InvalidRequest("agent event listener gate was dropped".to_string())
                })?;
        }
        let turn_id = match Box::pin(
            thread.start_turn_if_idle(
                TurnInputRequest::user_input(vec![UserInput::Text {
                    text: prompt,
                    text_elements: Vec::new(),
                }])
                .on_start(TurnStartOptions {
                    final_output_json_schema: output_schema,
                    ..Default::default()
                })
                .with_trace(parent_trace),
            ),
        )
        .await?
        {
            StartIfIdleSubmission::Started { turn_id } => turn_id,
            StartIfIdleSubmission::NotSubmitted { reason } => {
                return Err(CodexErr::InvalidRequest(format!(
                    "agent prompt was not submitted: {reason:?}"
                )));
            }
        };

        Ok(AgentRun {
            thread_id,
            turn_id,
            thread,
        })
    }

    pub async fn resume(
        &self,
        thread: Arc<CodexThread>,
        prompt: String,
        output_schema: Option<Value>,
        parent_trace: Option<W3cTraceContext>,
    ) -> CodexResult<AgentRun> {
        let thread_id = thread.session_configured().thread_id;
        let turn_id = match Box::pin(
            thread.start_turn_if_idle(
                TurnInputRequest::user_input(vec![UserInput::Text {
                    text: prompt,
                    text_elements: Vec::new(),
                }])
                .on_start(TurnStartOptions {
                    final_output_json_schema: output_schema,
                    ..Default::default()
                })
                .with_trace(parent_trace),
            ),
        )
        .await?
        {
            StartIfIdleSubmission::Started { turn_id } => turn_id,
            StartIfIdleSubmission::NotSubmitted { reason } => {
                return Err(CodexErr::InvalidRequest(format!(
                    "agent repair prompt was not submitted: {reason:?}"
                )));
            }
        };
        Ok(AgentRun {
            thread_id,
            turn_id,
            thread,
        })
    }
}
