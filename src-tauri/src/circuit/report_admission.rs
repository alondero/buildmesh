//! Negative lifecycle evidence shared by report admission and durable commits.
//! The last harness report can veto a handoff; process projections cannot
//! release it, and a lifecycle report cannot authorize a handoff by itself.

use crate::agent::session_lifecycle::{LifecycleChangedPayload, LifecycleKind};
use crate::circuit::observation::CircuitObservationBlocker;

/// Generic attention is not a structured request; treating it as one would
/// prevent the report reconciler from recovering an otherwise finished turn.
pub(crate) fn lifecycle_blocker(
    lifecycle: Option<&LifecycleChangedPayload>,
) -> Option<CircuitObservationBlocker> {
    match lifecycle?.kind {
        LifecycleKind::PermissionRequested | LifecycleKind::QuestionRequested => {
            Some(CircuitObservationBlocker::HumanResponseRequired)
        }
        LifecycleKind::BackgroundRunning => Some(CircuitObservationBlocker::KnownWorkOutstanding),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::session_lifecycle::{HookSignalDetail, LifecycleChangedPayload};

    #[test]
    fn generic_input_required_does_not_block_a_finished_report() {
        let input_required = LifecycleChangedPayload::new(
            1,
            LifecycleKind::InputRequired,
            crate::models::SessionStatus::AwaitingInput,
            &HookSignalDetail::default(),
            "input may be required",
        );

        assert_eq!(lifecycle_blocker(Some(&input_required)), None);
    }
}
