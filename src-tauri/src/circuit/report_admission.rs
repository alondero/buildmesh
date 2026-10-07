//! Negative lifecycle evidence shared by report admission and durable commits.
//! A display observation can veto a handoff, never authorize one.

use crate::agent::session_lifecycle::{LifecycleChangedPayload, LifecycleKind};
use crate::circuit::observation::CircuitObservationBlocker;

/// Callers supply only the current, revision-validated lifecycle snapshot.
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
