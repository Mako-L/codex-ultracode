use std::sync::atomic::Ordering;

use crate::session::SessionSettingsUpdate;
use crate::session::tests::make_session_and_context;
use crate::session::turn_context::NewTurnContextOptions;

#[tokio::test]
async fn command_cancellation_is_scoped_to_its_turn() {
    let (session, first_turn) = make_session_and_context().await;
    let (second_turn, _) = session
        .new_turn_with_sub_id(
            "second-turn".to_string(),
            SessionSettingsUpdate::default(),
            NewTurnContextOptions::default(),
        )
        .await
        .expect("create second turn");
    let call_id = "reused-call-id";

    session
        .register_command_approval_cancellation(&first_turn, call_id)
        .await;
    let first_cancellation = {
        let state = session.state.lock().await;
        assert_eq!(state.command_approval_cancellations.len(), 1);
        state
            .command_approval_cancellations
            .values()
            .next()
            .cloned()
            .expect("first turn cancellation marker")
    };
    first_cancellation.store(true, Ordering::Release);

    session
        .register_command_approval_cancellation(&second_turn, call_id)
        .await;

    assert!(
        !session
            .take_command_approval_cancellation(&second_turn, call_id)
            .await
    );
    assert!(
        session
            .take_command_approval_cancellation(&first_turn, call_id)
            .await
    );
    assert!(
        !session
            .take_command_approval_cancellation(&first_turn, call_id)
            .await
    );
}
