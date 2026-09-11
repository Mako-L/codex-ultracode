use super::*;
use codex_protocol::protocol::AskForApproval;
use serde_json::json;

#[tokio::test]
async fn request_profile_preserves_instructions_tools_and_policy_without_changing_defaults() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        r#"
developer_instructions = "default instructions"
approval_policy = "never"
[mcp_servers.selected_tool]
command = "default-tool"
"#,
    )
    .unwrap();
    std::fs::write(
        home.path().join("restricted.config.toml"),
        r#"
developer_instructions = "restricted instructions"
approval_policy = "on-request"
sandbox_mode = "read-only"
[mcp_servers.selected_tool]
command = "restricted-tool"
[mcp_servers.profile_only]
command = "profile-tool"
"#,
    )
    .unwrap();
    let manager = ConfigManager::without_managed_config_for_tests(home.path().to_path_buf());
    let overrides = HashMap::from([("user_config_profile".to_string(), json!("restricted"))]);
    let selected = manager
        .load_for_cwd(
            Some(overrides),
            ConfigOverrides::default(),
            Some(cwd.path().to_path_buf()),
        )
        .await
        .unwrap();
    let default = manager
        .load_for_cwd(
            None,
            ConfigOverrides::default(),
            Some(cwd.path().to_path_buf()),
        )
        .await
        .unwrap();
    // Exercise the actual refresh used by MCP/config refresh, including repeated refresh.
    let refreshed = manager
        .load_latest_config_for_thread(&selected)
        .await
        .unwrap();
    let refreshed_again = manager
        .load_latest_config_for_thread(&refreshed)
        .await
        .unwrap();
    for config in [&selected, &refreshed, &refreshed_again] {
        assert_eq!(
            config.developer_instructions.as_deref(),
            Some("restricted instructions")
        );
        assert_eq!(
            config.permissions.approval_policy.value(),
            AskForApproval::OnRequest
        );
        assert!(config.mcp_servers.get().contains_key("profile_only"));
        assert_eq!(
            config.config_layer_stack.effective_config()["mcp_servers"]["selected_tool"]["command"]
                .as_str(),
            Some("restricted-tool")
        );
        let layer = config.config_layer_stack.get_active_user_layer().unwrap();
        assert!(
            matches!(&layer.name,codex_config::ConfigLayerSource::User { profile:Some(profile),.. } if profile == "restricted")
        );
        assert_eq!(layer.hooks_config_folder().unwrap().as_path(), home.path());
    }
    let refreshed_default = manager
        .load_latest_config_for_thread(&default)
        .await
        .unwrap();
    for config in [&default, &refreshed_default] {
        assert_eq!(
            config.developer_instructions.as_deref(),
            Some("default instructions")
        );
        assert_eq!(
            config.permissions.approval_policy.value(),
            AskForApproval::Never
        );
        assert!(!config.mcp_servers.get().contains_key("profile_only"));
        assert_eq!(
            config.config_layer_stack.effective_config()["mcp_servers"]["selected_tool"]["command"]
                .as_str(),
            Some("default-tool")
        );
        assert!(matches!(
            &config
                .config_layer_stack
                .get_active_user_layer()
                .unwrap()
                .name,
            codex_config::ConfigLayerSource::User { profile: None, .. }
        ));
    }
}

#[tokio::test]
async fn request_profile_rejects_paths_and_non_string_values() {
    let home = tempfile::tempdir().unwrap();
    let manager = ConfigManager::without_managed_config_for_tests(home.path().to_path_buf());
    for invalid in [json!("../other"), json!(true)] {
        let result = manager
            .load_with_overrides(
                Some(HashMap::from([(
                    "user_config_profile".to_string(),
                    invalid,
                )])),
                ConfigOverrides::default(),
            )
            .await;
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    }
}

#[tokio::test]
async fn named_profile_edit_from_default_daemon_preserves_default_and_rejects_outside_home() {
    let home = tempfile::tempdir().unwrap();
    let default = home.path().join("config.toml");
    let profile = home.path().join("work.config.toml");
    std::fs::write(&default, "model_reasoning_effort = 'low'\n").unwrap();
    std::fs::write(&profile, "model_reasoning_effort = 'high'\n").unwrap();
    let manager = ConfigManager::without_managed_config_for_tests(home.path().to_path_buf());
    let params = codex_app_server_protocol::ConfigBatchWriteParams {
        edits: vec![codex_app_server_protocol::ConfigEdit {
            key_path: "model_reasoning_effort".into(),
            value: json!("max"),
            merge_strategy: codex_app_server_protocol::MergeStrategy::Replace,
        }],
        file_path: Some(profile.to_string_lossy().into_owned()),
        expected_version: None,
        reload_user_config: false,
    };
    manager.batch_write(params.clone()).await.unwrap();
    assert!(std::fs::read_to_string(&profile).unwrap().contains("max"));
    assert_eq!(
        std::fs::read_to_string(&default).unwrap(),
        "model_reasoning_effort = 'low'\n"
    );
    assert_eq!(
        manager.user_config_path().unwrap().as_path(),
        default.as_path()
    );
    let outside = tempfile::tempdir().unwrap();
    let mut outside_params = params;
    outside_params.file_path = Some(
        outside
            .path()
            .join("work.config.toml")
            .to_string_lossy()
            .into_owned(),
    );
    assert!(manager.batch_write(outside_params).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn implicit_named_profile_route_rejects_symlink_escape_without_changing_outside_file() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("settings.toml");
    let source = "model_reasoning_effort = 'low'\n";
    std::fs::write(&target, source).unwrap();
    let profile = home.path().join("work.config.toml");
    std::os::unix::fs::symlink(&target, &profile).unwrap();
    let manager = ConfigManager::without_managed_config_for_tests(home.path().to_path_buf());
    let params = codex_app_server_protocol::ConfigBatchWriteParams {
        edits: vec![codex_app_server_protocol::ConfigEdit {
            key_path: "model_reasoning_effort".into(),
            value: json!("max"),
            merge_strategy: codex_app_server_protocol::MergeStrategy::Replace,
        }],
        file_path: Some(profile.to_string_lossy().into_owned()),
        expected_version: None,
        reload_user_config: false,
    };
    assert!(
        manager
            .batch_write(params.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("inside Codex home")
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), source);
    assert!(
        std::fs::symlink_metadata(&profile)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    // A loader explicitly configured for this profile retains the preexisting write contract.
    manager
        .for_user_profile("work".parse().unwrap())
        .batch_write(params)
        .await
        .unwrap();
    assert!(std::fs::read_to_string(&target).unwrap().contains("max"));
}

#[cfg(unix)]
#[tokio::test]
async fn implicit_named_profile_route_rejects_dangling_external_symlink() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("new.toml");
    let profile = home.path().join("work.config.toml");
    std::os::unix::fs::symlink(&target, &profile).unwrap();
    let manager = ConfigManager::without_managed_config_for_tests(home.path().to_path_buf());
    assert!(
        manager
            .batch_write(codex_app_server_protocol::ConfigBatchWriteParams {
                edits: vec![],
                file_path: Some(profile.to_string_lossy().into_owned()),
                expected_version: None,
                reload_user_config: false,
            })
            .await
            .is_err()
    );
    assert!(!target.exists());
}
