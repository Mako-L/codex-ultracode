use super::*;

#[test]
fn completion_retains_started_and_steered_turn_identity() {
    for submission in [
        TurnInputSubmission::Started {
            turn_id: "accepted".into(),
        },
        TurnInputSubmission::Steered {
            turn_id: "accepted".into(),
        },
    ] {
        assert_eq!(
            workflow_completion_response(submission).unwrap().turn_id,
            "accepted"
        );
    }
}

#[test]
fn unsubmitted_completion_is_not_acknowledged_as_delivered() {
    let response = workflow_completion_response(TurnInputSubmission::NotSubmitted {
        reason: codex_protocol::turn_input::NotSubmittedReason::PlanMode,
    });
    assert!(
        response
            .unwrap_err()
            .message
            .contains("not submitted: PlanMode")
    );
}
