use super::*;
use clap::Parser;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn native_workflows_require_explicit_cli_selection() {
    let ordinary = crate::Cli::try_parse_from(["codex", "prompt"]).unwrap();
    let selected =
        crate::Cli::try_parse_from(["codex", "--native-workflow-host", "prompt"]).unwrap();
    assert_eq!(
        (ordinary.native_workflow_host, selected.native_workflow_host),
        (false, true)
    );
}

#[test]
fn headless_exit_waits_for_workers_and_the_delivered_parent_turn() {
    let finished_state = json!({"active":false,"completionPending":false,"unresolved":[],"pendingCompletionTurnIds":[],"turnId":"final-turn","turnStatus":"completed"});
    assert!(!finished(&finished_state, Some("initial-turn")).unwrap());
    assert!(finished(&finished_state, Some("final-turn")).unwrap());
    for field in ["active", "completionPending"] {
        let mut state = finished_state.clone();
        state[field] = json!(true);
        assert!(!finished(&state, Some("final-turn")).unwrap());
    }
    let mut unresolved = finished_state;
    unresolved["unresolved"] = json!(["run-1"]);
    assert!(finished(&unresolved, Some("final-turn")).is_err());
}

#[test]
fn accepted_completion_waits_for_delayed_admission_and_persistence() {
    let mut state = json!({"active":false,"completionPending":false,"unresolved":[],
        "completionTurnIds":["accepted-turn"],"pendingCompletionTurnIds":["accepted-turn"],
        "turnId":"initial-turn","turnStatus":"completed"});
    assert!(!finished(&state, Some("initial-turn")).unwrap());
    state["turnId"] = json!("accepted-turn");
    state["turnStatus"] = json!("inProgress");
    assert!(!finished(&state, Some("initial-turn")).unwrap());
    state["pendingCompletionTurnIds"] = json!([]);
    state["turnStatus"] = json!("completed");
    assert!(!finished(&state, Some("initial-turn")).unwrap());
    assert!(finished(&state, Some("accepted-turn")).unwrap());
}

#[test]
fn headless_template_preserves_explicit_profile_and_default_selection() {
    let home = tempfile::tempdir().unwrap();
    let profile = "restricted".parse().unwrap();
    let overrides = codex_config::LoaderOverrides {
        user_config_path: Some(codex_core::config::resolve_profile_v2_config_path(
            home.path(),
            &profile,
        )),
        user_config_profile: Some(profile),
        ..Default::default()
    };
    let mut params = ThreadStartParams::default();
    apply_loader_overrides(&mut params, &overrides, home.path()).unwrap();
    assert_eq!(
        params.config.as_ref().unwrap()["user_config_profile"],
        json!("restricted")
    );
    apply_loader_overrides(&mut params, &Default::default(), home.path()).unwrap();
    assert_eq!(
        params.config.as_ref().unwrap()["user_config_profile"],
        Value::Null
    );
}
