use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use super::session::Session;
use super::turn_context::TurnContext;

impl Session {
    pub(crate) async fn register_command_approval_cancellation(
        &self,
        turn_context: &TurnContext,
        call_id: &str,
    ) {
        self.state
            .lock()
            .await
            .command_approval_cancellations
            .entry((turn_context.sub_id.clone(), call_id.to_owned()))
            .or_insert_with(|| Arc::new(AtomicBool::new(false)));
    }

    pub(crate) async fn take_command_approval_cancellation(
        &self,
        turn_context: &TurnContext,
        call_id: &str,
    ) -> bool {
        self.state
            .lock()
            .await
            .command_approval_cancellations
            .remove(&(turn_context.sub_id.clone(), call_id.to_owned()))
            .is_some_and(|cancelled| cancelled.swap(false, Ordering::AcqRel))
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "pending approval removal and cancellation must belong to the same active turn"
    )]
    pub(super) async fn interrupt_for_exec_approval(self: &Arc<Self>, approval_id: &str) {
        let pending = {
            let active = self.active_turn.lock().await;
            if let Some(active) = active.as_ref() {
                let mut turn_state = active.turn_state.lock().await;
                let pending = turn_state.remove_pending_approval(approval_id);
                if let Some(pending) = pending.as_ref()
                    && let Some(cancelled) = pending.command_cancellation.as_ref()
                {
                    // Record the reason before interruption drops the escalation waiter.
                    // The process's canonical completion owns the final status and output.
                    cancelled.store(true, Ordering::Release);
                }
                pending
            } else {
                None
            }
        };
        self.interrupt_task().await;
        // Match abort_all_tasks: tasks observe cancellation before their approval
        // waiters close, preventing an early model-visible approval failure.
        drop(pending);
    }
}

#[cfg(test)]
#[path = "approval_cancellation_tests.rs"]
mod tests;
