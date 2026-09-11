use super::*;

#[test]
fn native_workflow_warning_boundaries_and_projection() {
    let defaults = WorkflowWarningSettings::default();
    assert_eq!(large_workflow_warning(25, 1, 0, &defaults), None);
    assert_eq!(
        large_workflow_warning(26, 1, 0, &defaults).unwrap().axis,
        "agents"
    );
    assert_eq!(large_workflow_warning(21, 0, 0, &defaults), None);
    let warning = large_workflow_warning(22, 0, 0, &defaults).unwrap();
    assert_eq!(warning.axis, "tokens");
    assert_eq!(warning.projected_tokens, 1_540_000);
    assert_eq!(large_workflow_warning(10, 2, 300_000, &defaults), None);
    let warning = large_workflow_warning(10, 2, 300_001, &defaults).unwrap();
    assert_eq!(warning.projected_tokens, 1_500_005);
    assert_eq!(
        large_workflow_warning(1, 2, 1_500_001, &defaults)
            .unwrap()
            .projected_tokens,
        1_500_001
    );
    assert_eq!(
        large_workflow_warning(26, 0, 0, &defaults).unwrap().axis,
        "both"
    );
}

#[test]
fn native_workflow_warning_respects_explicit_guidance_and_ultracode() {
    for (size, threshold) in [
        ("small", 5),
        ("medium", 15),
        ("large", 50),
        ("unrestricted", 25),
    ] {
        let settings = WorkflowWarningSettings {
            size_guideline: Some(size),
            ..Default::default()
        };
        assert_eq!(large_workflow_warning(threshold, 1, 0, &settings), None);
        let warning = large_workflow_warning(threshold + 1, 1, 0, &settings).unwrap();
        assert_eq!(warning.agent_cap, threshold as f64);
        assert_eq!(warning.cap_from_guideline, size != "unrestricted");
    }
    let settings = WorkflowWarningSettings {
        ultracode_active: true,
        ..Default::default()
    };
    assert_eq!(large_workflow_warning(1000, 0, u64::MAX, &settings), None);
}

#[test]
fn native_workflow_warning_accepts_only_positive_finite_overrides() {
    let settings = WorkflowWarningSettings {
        size_guideline: Some("small"),
        agent_threshold: Some(8.5),
        token_threshold: Some(2_000_000.0),
        ..Default::default()
    };
    assert_eq!(large_workflow_warning(8, 1, 0, &settings), None);
    let warning = large_workflow_warning(9, 1, 0, &settings).unwrap();
    assert_eq!(warning.agent_cap, 8.5);
    assert!(!warning.cap_from_guideline);
    assert_eq!(large_workflow_warning(2, 2, 2_000_000, &settings), None);
    assert_eq!(
        large_workflow_warning(2, 2, 2_000_001, &settings)
            .unwrap()
            .axis,
        "tokens"
    );
    for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let settings = WorkflowWarningSettings {
            agent_threshold: Some(invalid),
            token_threshold: Some(invalid),
            ..Default::default()
        };
        assert_eq!(large_workflow_warning(25, 1, 0, &settings), None);
        let warning = large_workflow_warning(26, 1, 1_500_001, &settings).unwrap();
        assert_eq!((warning.agent_cap, warning.token_cap), (25.0, 1_500_000.0));
    }
}
