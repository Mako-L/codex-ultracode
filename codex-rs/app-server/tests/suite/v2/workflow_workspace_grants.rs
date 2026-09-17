use anyhow::Context;
use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::WorkflowAuthorityCaptureResponse;
use codex_app_server_protocol::WorkflowWorkspacePrepareResponse;
use codex_protocol::models::PermissionProfile;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;

fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

struct Fixture {
    _home: TempDir,
    project: TempDir,
    server: TestAppServer,
    parent_id: String,
    authority: WorkflowAuthorityCaptureResponse,
}

impl Fixture {
    async fn new(policy: AskForApproval) -> Result<Self> {
        let home = TempDir::new()?;
        let project = TempDir::new()?;
        git(project.path(), &["init"])?;
        std::fs::create_dir(project.path().join("src"))?;
        std::fs::write(project.path().join("src/file.txt"), "base")?;
        for name in [".codex", ".agents"] {
            std::fs::create_dir(project.path().join(name))?;
            std::fs::write(project.path().join(name).join("fixture.txt"), "metadata")?;
        }
        git(
            project.path(),
            &["add", "src/file.txt", ".codex", ".agents"],
        )?;
        git(
            project.path(),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-m",
                "base",
            ],
        )?;
        MockResponsesConfig::new("http://127.0.0.1:1")
            .with_root_config("[worktree]\nbase_ref = \"head\"")
            .with_extra_config("[permissions.partial.filesystem]\n\"/\" = \"read\"\n\":project_roots\" = { src = \"write\" }")
            .write(home.path())?;
        let mut server = TestAppServer::builder()
            .with_codex_home(home.path())
            .build_initialized()
            .await?;
        let project_cwd = project.path().canonicalize()?;
        let mut environment = server.auto_env_params()?;
        environment.cwd =
            codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(&project_cwd)?.into();
        let start = server
            .send_thread_start_request(ThreadStartParams {
                cwd: Some(project_cwd.to_string_lossy().into_owned()),
                environments: Some(vec![environment]),
                permissions: Some("partial".into()),
                approval_policy: Some(policy),
                ..Default::default()
            })
            .await?;
        let parent: ThreadStartResponse = server.read_response(start).await?;
        let parent = parent.thread;
        let id = server
            .send_request(
                "workflow/authority/capture",
                Some(json!({"parentThreadId":parent.id,"allowIsolatedWorkspaces":true})),
            )
            .await?;
        let authority = server.read_response(id).await?;
        Ok(Self {
            _home: home,
            project,
            server,
            parent_id: parent.id,
            authority,
        })
    }

    async fn prepare(&mut self) -> Result<i64> {
        self.server.send_request("workflow/workspace/prepare",Some(json!({
            "runId":"grant-run","workerId":"grant-worker","agentType":"general-purpose",
            "authorityRef":self.authority.authority_ref,"authorityDigest":self.authority.authority_digest,
            "isolation":"worktree","requestedCwd":self.authority.cwd,"previous":null
        }))).await
    }

    fn assert_no_checkout_mutations(&self) -> Result<()> {
        assert!(!self.project.path().join(".ultracode/worktrees").exists());
        assert!(!self.project.path().join(".git/worktrees").exists());
        assert!(
            !self
                .project
                .path()
                .join(".git/ultracode-workspaces")
                .exists()
        );
        assert_eq!(
            git(
                self.project.path(),
                &[
                    "for-each-ref",
                    "--format=%(refname)",
                    "refs/heads/ultracode/"
                ]
            )?,
            ""
        );
        Ok(())
    }
}

#[tokio::test]
async fn native_policy_denies_checkout_expansion_without_host_writes() -> Result<()> {
    for policy in [
        AskForApproval::Never,
        AskForApproval::Granular {
            sandbox_approval: true,
            rules: true,
            skill_approval: true,
            request_permissions: false,
            mcp_elicitations: true,
        },
    ] {
        let mut fixture = Fixture::new(policy).await?;
        let id = fixture.prepare().await?;
        let error = tokio::time::timeout(
            Duration::from_secs(10),
            fixture
                .server
                .read_stream_until_error_message(RequestId::Integer(id)),
        )
        .await??;
        assert!(
            error.error.message.contains("prohibited by native policy"),
            "{}",
            error.error.message
        );
        fixture.assert_no_checkout_mutations()?;
    }
    Ok(())
}

#[tokio::test]
async fn declined_native_checkout_grant_never_creates_checkout_or_git_metadata() -> Result<()> {
    let mut fixture = Fixture::new(AskForApproval::OnRequest).await?;
    let id = fixture.prepare().await?;
    let request = tokio::time::timeout(
        Duration::from_secs(10),
        fixture.server.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::PermissionsRequestApproval { request_id, params } = request else {
        anyhow::bail!("expected checkout permission request");
    };
    assert_eq!(params.thread_id, fixture.parent_id);
    let reason = params.reason.context("checkout identity")?;
    assert!(reason.contains("run=grant-run") && reason.contains("worker=grant-worker"));
    fixture.assert_no_checkout_mutations()?;
    fixture
        .server
        .send_response(request_id, json!({"permissions":{},"scope":"turn"}))
        .await?;
    let error = fixture
        .server
        .read_stream_until_error_message(RequestId::Integer(id))
        .await?;
    assert!(
        error.error.message.contains("denied"),
        "{}",
        error.error.message
    );
    fixture.assert_no_checkout_mutations()?;
    Ok(())
}

#[tokio::test]
async fn declined_native_git_command_does_not_create_checkout() -> Result<()> {
    let mut fixture = Fixture::new(AskForApproval::OnRequest).await?;
    let id = fixture.prepare().await?;
    let request = tokio::time::timeout(
        Duration::from_secs(10),
        fixture.server.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::PermissionsRequestApproval { request_id, params } = request else {
        anyhow::bail!("expected checkout grant");
    };
    fixture
        .server
        .send_response(
            request_id,
            json!({"permissions":params.permissions,"scope":"turn"}),
        )
        .await?;
    let request = tokio::time::timeout(
        Duration::from_secs(10),
        fixture.server.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::CommandExecutionRequestApproval { request_id, params } = request else {
        anyhow::bail!("expected native Git approval");
    };
    assert_eq!(params.thread_id, fixture.parent_id);
    fixture.assert_no_checkout_mutations()?;
    fixture
        .server
        .send_response(request_id, json!({"decision":"decline"}))
        .await?;
    let error = fixture
        .server
        .read_stream_until_error_message(RequestId::Integer(id))
        .await?;
    assert!(
        error.error.message.contains("not approved"),
        "{}",
        error.error.message
    );
    fixture.assert_no_checkout_mutations()?;
    Ok(())
}

#[tokio::test]
async fn accepted_checkout_grant_stays_bounded_and_does_not_modify_parent_authority() -> Result<()>
{
    let mut fixture = Fixture::new(AskForApproval::OnRequest).await?;
    let id = fixture.prepare().await?;
    let mut grants = 0;
    let mut commands = 0;
    let prepared: WorkflowWorkspacePrepareResponse = loop {
        let message =
            tokio::time::timeout(Duration::from_secs(30), fixture.server.read_next_message())
                .await??;
        match message {
            JSONRPCMessage::Request(request) => {
                let request: ServerRequest = request.try_into()?;
                match request {
                    ServerRequest::PermissionsRequestApproval { request_id, params } => {
                        grants += 1;
                        assert_eq!(params.thread_id, fixture.parent_id);
                        let permissions = serde_json::to_value(&params.permissions)?;
                        let write = permissions["fileSystem"]["write"]
                            .as_array()
                            .context("exact checkout write request")?;
                        assert_eq!(write.len(), 1);
                        assert!(
                            write[0]
                                .as_str()
                                .context("checkout path")?
                                .ends_with("/.ultracode/worktrees/grant-run-grant-worker")
                        );
                        fixture
                            .server
                            .send_response(
                                request_id,
                                json!({"permissions":permissions,"scope":"session"}),
                            )
                            .await?;
                    }
                    ServerRequest::CommandExecutionRequestApproval { request_id, params } => {
                        commands += 1;
                        assert_eq!(params.thread_id, fixture.parent_id);
                        assert!(
                            params
                                .command
                                .context("native Git command")?
                                .starts_with("git ")
                        );
                        fixture
                            .server
                            .send_response(request_id, json!({"decision":"accept"}))
                            .await?;
                    }
                    request => anyhow::bail!("unexpected native request: {request:?}"),
                }
            }
            JSONRPCMessage::Response(response) if response.id == RequestId::Integer(id) => {
                break serde_json::from_value(response.result)?;
            }
            JSONRPCMessage::Error(error) if error.id == RequestId::Integer(id) => {
                anyhow::bail!("checkout preparation failed: {:?}", error.error)
            }
            _ => {}
        }
    };
    assert_eq!(grants, 1);
    assert!(commands >= 1);
    let workspace_id = prepared.workspace_id.context("owned checkout")?;
    let path = fixture
        .project
        .path()
        .join(".git/ultracode-workspaces")
        .join(format!("{workspace_id}.json"));
    let record: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    assert_eq!(
        record["checkout_grant"]["parent_thread_id"],
        fixture.parent_id
    );
    assert_eq!(record["checkout_grant"]["run_id"], "grant-run");
    assert_eq!(record["checkout_grant"]["worker_id"], "grant-worker");
    assert_eq!(record["checkout_grant"]["workspace_id"], workspace_id);
    assert_eq!(record["checkout_grant"]["path"], prepared.cwd);
    let profile: PermissionProfile = serde_json::from_value(record["permission_profile"].clone())?;
    let policy = profile.file_system_sandbox_policy();
    let checkout = Path::new(&prepared.cwd);
    assert!(policy.can_write_local_path_with_cwd(checkout.join("src/result").as_path(), checkout));
    assert!(!policy.can_write_local_path_with_cwd(checkout.join("outside-src").as_path(), checkout));
    for name in [".git", ".codex/fixture.txt", ".agents/fixture.txt"] {
        assert!(checkout.join(name).is_file());
        assert!(!policy.can_write_local_path_with_cwd(checkout.join(name).as_path(), checkout));
    }
    assert!(!policy.can_write_local_path_with_cwd(
        fixture.project.path().join(".git/config").as_path(),
        checkout
    ));
    let capture = fixture
        .server
        .send_request(
            "workflow/authority/capture",
            Some(json!({"parentThreadId":fixture.parent_id,"allowIsolatedWorkspaces":true})),
        )
        .await?;
    let after: WorkflowAuthorityCaptureResponse = fixture.server.read_response(capture).await?;
    assert_eq!(after.authority_digest, fixture.authority.authority_digest);
    Ok(())
}

#[tokio::test]
async fn failed_checkout_command_cleans_owned_worktree_and_branch() -> Result<()> {
    check_failed_checkout_cleanup("exit 17", -32010, /*retained*/ false).await
}

#[tokio::test]
async fn sandbox_denied_checkout_command_cleans_owned_worktree_and_branch() -> Result<()> {
    check_failed_checkout_cleanup(
        "printf blocked > ../../../outside-scope",
        -32011,
        /*retained*/ false,
    )
    .await
}

#[tokio::test]
async fn partial_checkout_failure_reports_retained_evidence_and_cleans_owned_branch() -> Result<()>
{
    check_failed_checkout_cleanup("rm .git; exit 17", -32010, /*retained*/ true).await
}

#[tokio::test]
async fn checkout_identity_failure_reports_retained_evidence_and_cleans_owned_branch() -> Result<()>
{
    check_failed_checkout_cleanup("rm .git", -32603, /*retained*/ true).await
}

async fn check_failed_checkout_cleanup(
    hook_command: &str,
    expected_error_code: i64,
    retained: bool,
) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut fixture = Fixture::new(AskForApproval::OnRequest).await?;
    let hook = fixture.project.path().join(".git/hooks/post-checkout");
    std::fs::write(&hook, format!("#!/bin/sh\n{hook_command}\n"))?;
    std::fs::set_permissions(hook, std::fs::Permissions::from_mode(0o755))?;
    let id = fixture.prepare().await?;
    let mut cleanup_commands = Vec::new();
    loop {
        let message =
            tokio::time::timeout(Duration::from_secs(30), fixture.server.read_next_message())
                .await??;
        match message {
            JSONRPCMessage::Request(request) => match ServerRequest::try_from(request)? {
                ServerRequest::PermissionsRequestApproval { request_id, params } => {
                    fixture
                        .server
                        .send_response(
                            request_id,
                            json!({"permissions":params.permissions,"scope":"turn"}),
                        )
                        .await?;
                }
                ServerRequest::CommandExecutionRequestApproval { request_id, params } => {
                    cleanup_commands.push(params.command.unwrap_or_default());
                    fixture
                        .server
                        .send_response(request_id, json!({"decision":"accept"}))
                        .await?;
                }
                request => anyhow::bail!("unexpected request: {request:?}"),
            },
            JSONRPCMessage::Error(error) if error.id == RequestId::Integer(id) => {
                assert_eq!(error.error.code, expected_error_code);
                assert_eq!(
                    error.error.message.contains("cleanup unresolved"),
                    retained,
                    "{}",
                    error.error.message
                );
                assert_eq!(error.error.message.contains("evidence retained"), retained);
                break;
            }
            JSONRPCMessage::Response(response) if response.id == RequestId::Integer(id) => {
                anyhow::bail!("failed post-checkout hook unexpectedly succeeded")
            }
            _ => {}
        }
    }
    assert_eq!(
        cleanup_commands
            .iter()
            .any(|command| command.contains("worktree remove")),
        !retained
    );
    assert!(
        cleanup_commands
            .iter()
            .any(|command| command.contains("update-ref -d"))
    );
    assert_eq!(
        fixture
            .project
            .path()
            .join(".ultracode/worktrees/grant-run-grant-worker")
            .exists(),
        retained
    );
    assert_eq!(
        git(
            fixture.project.path(),
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/ultracode/"
            ]
        )?,
        ""
    );
    assert!(
        !fixture
            .project
            .path()
            .join(".git/ultracode-workspaces")
            .exists()
    );
    assert!(!fixture.project.path().join("outside-scope").exists());
    Ok(())
}
