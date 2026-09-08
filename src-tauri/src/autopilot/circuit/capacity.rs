//! Single Circuit capacity policy (issue #1660, extends ADR-0028).
//!
//! Admission (`circuit_run_capacity`), per-circuit step scheduling
//! (`concurrency_limit`), and the optional app-wide Autopilot pool all
//! live here. The worker observes live counts then calls these helpers;
//! the UI explains the same verdict via the generated [`CapacityBind`]
//! type. Limits are unchanged from ADR-0028.

use serde::{Deserialize, Serialize};

use super::model::{consumes_agent_slot, CircuitGraph};
use super::stepper::Capacity;
use super::vocabulary::RunState;

/// Which budget is binding a parked run or step. The UI renders this
/// verdict; it must not invent a fourth budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "CapacityBind.ts")]
#[serde(rename_all = "snake_case")]
pub enum CapacityBind {
    /// Mesh `circuit_run_capacity` — pending runs wait here.
    MeshRunAdmission,
    /// Per-circuit `concurrency_limit` — steps park as `pending_slot`.
    CircuitStepSlots,
    /// The run's durable SpawnAgentNode lease, optionally bounded by the
    /// app-wide Autopilot process pool.
    CircuitAgentLease,
}

/// Admit a pending run when the mesh still has a free run slot.
/// Already-admitted states (`running` / `paused`) always pass — they
/// hold the slot they were granted. Terminal states are not driven.
pub fn may_admit(state: RunState, active_runs: i64, mesh_run_capacity: i32) -> bool {
    if state != RunState::Pending {
        return true;
    }
    admit_pending_run(active_runs, mesh_run_capacity)
}

pub fn admit_pending_run(active_runs: i64, mesh_run_capacity: i32) -> bool {
    pending_run_bind(active_runs, mesh_run_capacity).is_none()
}

pub fn pending_run_bind(active_runs: i64, mesh_run_capacity: i32) -> Option<CapacityBind> {
    if active_runs < i64::from(mesh_run_capacity) {
        None
    } else {
        Some(CapacityBind::MeshRunAdmission)
    }
}

/// Why a queued step is parked. Matches the Probe copy: a saturated
/// per-circuit step budget binds first; otherwise the agent lease (or
/// optional host pool) is the remaining constraint.
pub fn queued_step_bind(concurrency_limit: i64, running_steps: i64) -> CapacityBind {
    if concurrency_limit > 0 && running_steps >= concurrency_limit {
        CapacityBind::CircuitStepSlots
    } else {
        CapacityBind::CircuitAgentLease
    }
}

/// Blueprint-declared SpawnAgentNode footprint reserved before admission.
pub fn declared_agent_slots(graph: &CircuitGraph) -> i64 {
    graph
        .nodes
        .iter()
        .filter(|node| consumes_agent_slot(&node.kind))
        .count() as i64
}

/// Free slots in the optional app-wide Autopilot pool after accounting for
/// agents outside a circuit lease and circuit-owned slots. Admission passes
/// durable worst-case lease reservations here; a running Tick passes the
/// live circuit-agent count. The snapshots intentionally differ even though
/// they protect the same host-wide resource.
pub fn global_agent_free_slots(
    global_pool: Option<u32>,
    unleased_slots: i64,
    circuit_occupied_slots: i64,
) -> i64 {
    global_pool
        .map(|pool| {
            i64::from(pool)
                .saturating_sub(unleased_slots)
                .saturating_sub(circuit_occupied_slots)
        })
        .unwrap_or(i64::MAX)
}

pub fn global_agent_reservation_fits(
    required: i64,
    reserved_circuit_slots: i64,
    unleased_slots: i64,
    global_pool: Option<u32>,
) -> bool {
    if required <= 0 {
        return true;
    }
    global_agent_free_slots(global_pool, unleased_slots, reserved_circuit_slots) >= required
}

/// Compose a Tick snapshot from already-observed counters. Fail-closed
/// callers pass `i64::MAX` running-steps / `0` global-free when a count
/// read fails.
pub fn tick_capacity(
    concurrency_limit: i64,
    circuit_running: i64,
    reserved_for_run: i64,
    owned_by_run: i64,
    global_free_slots: i64,
) -> Capacity {
    let lease_free = reserved_for_run.saturating_sub(owned_by_run);
    Capacity {
        circuit_free_slots: concurrency_limit - circuit_running,
        agent_free_slots: global_free_slots.min(lease_free),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn may_admit_passes_already_admitted_runs_without_counting() {
        assert!(may_admit(RunState::Running, 99, 1));
        assert!(may_admit(RunState::Paused, 99, 1));
        assert!(may_admit(RunState::Completed, 99, 1));
    }

    #[test]
    fn pending_run_defers_when_admitted_count_meets_cap() {
        assert!(may_admit(RunState::Pending, 1, 2));
        assert!(!may_admit(RunState::Pending, 2, 2));
        assert_eq!(pending_run_bind(2, 2), Some(CapacityBind::MeshRunAdmission));
        assert_eq!(pending_run_bind(1, 2), None);
    }

    #[test]
    fn queued_step_bind_prefers_step_slots_then_agent_lease() {
        assert_eq!(queued_step_bind(1, 1), CapacityBind::CircuitStepSlots);
        assert_eq!(queued_step_bind(2, 2), CapacityBind::CircuitStepSlots);
        assert_eq!(queued_step_bind(2, 1), CapacityBind::CircuitAgentLease);
        assert_eq!(queued_step_bind(0, 0), CapacityBind::CircuitAgentLease);
    }

    #[test]
    fn tick_capacity_uses_the_tighter_of_lease_and_global_pool() {
        let cap = tick_capacity(4, 1, 3, 1, 1);
        assert_eq!(cap.circuit_free_slots, 3);
        assert_eq!(
            cap.agent_free_slots, 1,
            "global free (1) tighter than lease free (2)"
        );
        let cap = tick_capacity(4, 1, 3, 2, 9);
        assert_eq!(
            cap.agent_free_slots, 1,
            "lease free (1) tighter than global (9)"
        );
    }

    #[test]
    fn global_pool_unset_is_unbounded() {
        assert_eq!(global_agent_free_slots(None, 50, 50), i64::MAX);
        assert!(global_agent_reservation_fits(8, 0, 0, None));
        assert!(!global_agent_reservation_fits(3, 2, 0, Some(4)));
        assert!(global_agent_reservation_fits(2, 2, 0, Some(4)));
    }
}
