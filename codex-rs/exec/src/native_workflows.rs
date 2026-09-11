//! Backend injection for the integrated CLI's explicit native workflow route.
use codex_app_server_client::AppServerClient;
use codex_app_server_protocol::ThreadStartParams;
use codex_core::config::Config;
use serde_json::Value;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;

/// Asynchronous operations supplied by the native supervisor integration.
pub type NativeWorkflowFuture<T> = Pin<Box<dyn Future<Output = io::Result<T>> + Send>>;

/// A parent daemon connection and its explicitly configured workflow listener.
pub struct NativeWorkflowConnection {
    pub client: AppServerClient,
    pub mcp_config: Value,
    pub control: Arc<dyn Fn(String, String) -> NativeWorkflowFuture<Value> + Send + Sync>,
}

/// The integrated CLI supplies the supervisor without introducing a TUI dependency in exec.
pub type NativeWorkflowHostFactory =
    fn(Config, ThreadStartParams) -> NativeWorkflowFuture<NativeWorkflowConnection>;

pub(crate) fn finished(state: &Value, completed_turn_id: Option<&str>) -> io::Result<bool> {
    if state["unresolved"]
        .as_array()
        .is_some_and(|runs| !runs.is_empty())
    {
        return Err(io::Error::other(
            "native workflow completion acknowledgement is unresolved",
        ));
    }
    Ok(state["active"] == false
        && state["completionPending"] == false
        && state["pendingCompletionTurnIds"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && state["turnStatus"] != "inProgress"
        && completed_turn_id.is_some_and(|id| state["turnId"] == id))
}

#[cfg(test)]
#[path = "native_workflows_tests.rs"]
mod tests;

/// Preserve the CLI's supported loader selection on the fork-bound daemon route.
pub(crate) fn apply_loader_overrides(
    params: &mut ThreadStartParams,
    overrides: &codex_config::LoaderOverrides,
    codex_home: &std::path::Path,
) -> io::Result<()> {
    if overrides.ignore_user_config || overrides.ignore_user_and_project_exec_policy_rules {
        return Err(io::Error::other(
            "native workflows cannot discard user configuration or exec policy rules",
        ));
    }
    let expected_path = overrides
        .user_config_profile
        .as_ref()
        .map(|profile| codex_core::config::resolve_profile_v2_config_path(codex_home, profile));
    if overrides.user_config_path != expected_path {
        return Err(io::Error::other(
            "native workflows require a named user configuration profile",
        ));
    }
    params.config.get_or_insert_default().insert(
        "user_config_profile".into(),
        serde_json::to_value(
            overrides
                .user_config_profile
                .as_ref()
                .map(|profile| profile.as_str()),
        )?,
    );
    Ok(())
}
