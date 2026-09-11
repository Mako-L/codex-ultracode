//! Adapt the existing native supervisor into exec's explicit backend factory.
pub(crate) fn connect(
    config: codex_core::config::Config,
    params: codex_app_server_protocol::ThreadStartParams,
) -> codex_exec::NativeWorkflowFuture<codex_exec::NativeWorkflowConnection> {
    Box::pin(async move {
        let (client, mcp_config, controller) =
            codex_tui::prepare_headless_workflow_host(config, params).await?;
        Ok(codex_exec::NativeWorkflowConnection {
            client,
            mcp_config,
            control: std::sync::Arc::new(move |parent, action| {
                let controller = controller.clone();
                Box::pin(async move { controller.control(parent, action).await })
            }),
        })
    })
}
