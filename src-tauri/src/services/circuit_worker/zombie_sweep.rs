//! Zombie-agent reaper (issue #1793).
//!
//! The circuit watchdog cleans up the *run* it owns — kills the PTY reader
//! thread, tears down the watcher — but does not touch the `agent_nodes` row.
//! A piloted node whose harness crashed, exited, or was never observed is
//! therefore left `running` indefinitely, with no `cli_session_id` and no
//! readable assistant report. Nothing will ever re-observe it, and future
//! review creations / circuit steps keep attaching to it.
//!
//! This sweep centralises the "what counts as a zombie" rule in one place and
//! runs on the circuit worker's tick. It follows the three-phase DB pattern
//! (issue #1228): scan candidates under a reader, do the report filesystem I/O
//! and the notification dispatch **lock-free**, and batch-write the terminal
//! transitions under the writer.
//!
//! v1 shape (deliberately one): a zombie is transitioned to the terminal
//! [`SessionStatus::Lost`] status and surfaced to both clients through the
//! normalized `agent-lifecycle` event. The alternative offered by the issue —
//! flag-and-notify while leaving the row `running` — is not used, because the
//! node cannot legitimately be re-observed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use once_cell::sync::Lazy;
use tauri::AppHandle;

use crate::agent::session_lifecycle::{
    AppSessionLifecycleSink, HookSignalDetail, LifecycleChangedPayload, LifecycleKind,
    SessionLifecycleSink,
};
use crate::db;
use crate::models::{AgentNode, SessionStatus};

/// How long a piloted `running` node may go without a session identity or a
/// readable report before it is treated as lost. Mirrors the circuit stepper's
/// first-observation window (`UNOBSERVED_WAIT_MS`, issue #1791) so the two
/// surfaces agree on what "never observed" means.
pub(super) const ZOMBIE_THRESHOLD_MS: i64 = 15 * 60_000;

/// Cost bound on the sweep itself: the candidate scan runs at most this often,
/// independent of the 2s circuit tick.
pub(super) const ZOMBIE_SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Per-agent cooldown. A node observed once (reaped, or found healthy) is not
/// re-read — including its transcript — until this window elapses.
pub(super) const ZOMBIE_AGENT_COOLDOWN_MS: i64 = 15 * 60_000;

/// Epoch millis of the last sweep; `0` means "never", so the first tick runs.
static LAST_SWEEP_MS: AtomicI64 = AtomicI64::new(0);

/// Epoch millis each agent was last observed, for the per-agent cooldown.
/// Expired entries are pruned each sweep, so it is bounded by the agents
/// currently inside their window.
static LAST_OBSERVED: Lazy<Mutex<HashMap<i64, i64>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Fast-tick pass: reap circuit-piloted nodes that have gone observation-quiet
/// past [`ZOMBIE_THRESHOLD_MS`]. Cheap on the common no-op tick — the
/// [`ZOMBIE_SWEEP_INTERVAL`] gate returns before any DB access.
pub(super) fn zombie_sweep_pass(app: &AppHandle) {
    let now_ms = chrono::Utc::now().timestamp_millis();
    if !sweep_due(LAST_SWEEP_MS.load(Ordering::Relaxed), now_ms) {
        return;
    }
    LAST_SWEEP_MS.store(now_ms, Ordering::Relaxed);

    let sink = AppSessionLifecycleSink { app };
    let result = sweep_with(
        now_ms,
        ZOMBIE_THRESHOLD_MS,
        ZOMBIE_AGENT_COOLDOWN_MS,
        |node| crate::coordinator::enrichment::assistant_report(node).is_some(),
        |node_id| {
            let provider = db::get_agent_node_by_id(node_id)
                .ok()
                .map(|node| node.provider);
            sink.emit_lifecycle_changed(LifecycleChangedPayload::new(
                node_id,
                LifecycleKind::Lost,
                SessionStatus::Lost,
                &HookSignalDetail { provider, ..Default::default() },
                "Agent lost: it stayed running with no session identity or readable report.",
            ));
        },
    );
    match result {
        Ok(0) => {}
        Ok(count) => tracing::warn!(
            "circuits: zombie reaper transitioned {count} unobserved agent(s) to lost"
        ),
        Err(error) => tracing::warn!("circuits: zombie reaper failed: {error}"),
    }
}

/// Pure time gate so the cadence is testable without a clock.
fn sweep_due(last_ms: i64, now_ms: i64) -> bool {
    last_ms == 0 || now_ms.saturating_sub(last_ms) >= ZOMBIE_SWEEP_INTERVAL.as_millis() as i64
}

/// Three-phase sweep with injected observations.
///
/// `has_report` is the transcript read (filesystem I/O); `notify` is the
/// post-commit user-facing dispatch. Both are injected so the lock discipline
/// is assertable in tests and so no I/O happens while a DB connection is held.
fn sweep_with<H, N>(
    now_ms: i64,
    threshold_ms: i64,
    cooldown_ms: i64,
    has_report: H,
    notify: N,
) -> Result<usize, String>
where
    H: Fn(&AgentNode) -> bool,
    N: FnMut(i64),
{
    // Phase 1 — candidate scan under a reader only. `list_zombie_candidates`
    // owns and releases its `read_conn`, and performs no I/O beyond SQLite.
    let candidates =
        db::list_zombie_candidates(threshold_ms, now_ms).map_err(|e| e.to_string())?;

    // Apply the per-agent cooldown first so a suppressed node never reaches the
    // filesystem read below. The map lock is released before that read runs.
    let due = {
        let mut observed = LAST_OBSERVED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        select_due_candidates(candidates, &mut observed, now_ms, cooldown_ms)
    };

    // Phase 2 — observation, lock-free. Each node is loaded (its read
    // connection dropped on return) before `has_report` touches the disk.
    let mut survivors = Vec::new();
    for id in due {
        let node = match db::get_agent_node_by_id(id) {
            Ok(node) => node,
            Err(_) => continue,
        };
        if is_zombie(&node, has_report(&node)) {
            survivors.push(id);
        }
    }

    // Phase 3 + 4 — batch transition under the writer, then surface.
    reap_and_surface(&survivors, notify)
}

/// Transition `ids` to [`SessionStatus::Lost`] in one writer-locked batch, then
/// dispatch one notification per *actually* transitioned node after the writer
/// lock is released. Split from [`sweep_with`] so the no-I/O-under-lock
/// invariant (issue #1228/#1793) is directly testable.
fn reap_and_surface(ids: &[i64], mut notify: impl FnMut(i64)) -> Result<usize, String> {
    let flipped = db::reap_zombie_agents(ids).map_err(|e| e.to_string())?;
    for id in &flipped {
        notify(*id);
    }
    Ok(flipped.len())
}

/// The "what counts as a zombie" rule, in one place: a node is a zombie when
/// it has neither a session identity nor a readable assistant report, so
/// nothing will ever re-observe it. The candidate scan already filters on the
/// session column, so today the report half is the deciding one — kept
/// explicit so the rule stays correct if a harness ever writes a transcript
/// without a captured session identity.
fn is_zombie(node: &AgentNode, has_report: bool) -> bool {
    let has_session = node
        .cli_session_id
        .as_deref()
        .is_some_and(|session| !session.trim().is_empty());
    !has_session && !has_report
}

/// Per-agent cooldown predicate: true when `last` is absent or older than
/// `cooldown_ms`.
fn cooldown_ready(last_ms: Option<i64>, now_ms: i64, cooldown_ms: i64) -> bool {
    last_ms.is_none_or(|last| now_ms.saturating_sub(last) >= cooldown_ms)
}

/// Keep only candidates outside their cooldown, stamping each kept id as
/// observed at `now_ms`. Extracted so the cooldown rule is unit-testable
/// against an explicit map rather than the process-global one.
fn select_due_candidates(
    candidates: Vec<i64>,
    observed: &mut HashMap<i64, i64>,
    now_ms: i64,
    cooldown_ms: i64,
) -> Vec<i64> {
    // Drop expired entries so the map stays bounded by the agents currently
    // inside their cooldown window, not every agent ever observed.
    observed.retain(|_, last| now_ms.saturating_sub(*last) < cooldown_ms);
    candidates
        .into_iter()
        .filter(|id| {
            if cooldown_ready(observed.get(id).copied(), now_ms, cooldown_ms) {
                observed.insert(*id, now_ms);
                true
            } else {
                false
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};

    const THRESHOLD: i64 = ZOMBIE_THRESHOLD_MS;
    const COOLDOWN: i64 = ZOMBIE_AGENT_COOLDOWN_MS;

    fn now_ms() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    fn rfc3339(ms: i64) -> String {
        chrono::DateTime::from_timestamp_millis(ms).unwrap().to_rfc3339()
    }

    fn isolated_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        conn
    }

    /// Create a mesh + node, optionally making it circuit-piloted (a step row
    /// references it). Returns the node id. The circuit/run/step chain is built
    /// on the same connection so the in-memory DB's FK targets are real.
    fn seed_node(
        conn: &mut Connection,
        name: &str,
        status: &str,
        changed_at_ms: i64,
        session: Option<&str>,
        piloted: bool,
    ) -> i64 {
        let mesh = db::create_mesh_inner(conn, name, &format!("/tmp/reap-{name}")).unwrap();
        conn.execute(
            "INSERT INTO agent_nodes (mesh_id, name, path, status, status_changed_at, cli_session_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![mesh.id, name, format!("/tmp/reap-{name}"), status, rfc3339(changed_at_ms), session],
        )
        .unwrap();
        let node = conn.last_insert_rowid();
        if piloted {
            let circuit =
                db::create_autopilot_circuit_inner(conn, mesh.id, "c", "", 2, "{}").unwrap();
            let run = db::create_circuit_run_locked(
                conn,
                circuit.id,
                mesh.id,
                "manual:test",
                "{}",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO autopilot_circuit_run_steps (run_id, node_id, agent_node_id, status) \
                 VALUES (?1, 'worker', ?2, 'running')",
                params![run, node],
            )
            .unwrap();
        }
        node
    }

    fn status_of(conn: &Connection, node_id: i64) -> SessionStatus {
        db::get_agent_node_by_id_inner(conn, node_id).unwrap().status
    }

    /// A piloted node stuck `running` with no session identity past the
    /// threshold is a zombie: it is scanned, then transitioned to terminal
    /// `Lost`.
    #[test]
    fn stale_sessionless_piloted_node_is_reaped() {
        let mut conn = isolated_conn();
        let node = seed_node(
            &mut conn,
            "stale",
            "running",
            now_ms() - THRESHOLD - 60_000,
            None,
            true,
        );

        let candidates = db::list_zombie_candidates_inner(&conn, THRESHOLD, now_ms()).unwrap();
        assert_eq!(candidates, vec![node], "the stale sessionless piloted node is a candidate");

        let flipped = db::reap_zombie_agents_inner(&mut conn, &candidates).unwrap();
        assert_eq!(flipped, vec![node]);
        assert_eq!(status_of(&conn, node), SessionStatus::Lost);
    }

    /// A node that entered `running` recently is not observation-quiet yet.
    #[test]
    fn recently_running_node_is_untouched() {
        let mut conn = isolated_conn();
        let node = seed_node(&mut conn, "recent", "running", now_ms() - 60_000, None, true);

        let candidates = db::list_zombie_candidates_inner(&conn, THRESHOLD, now_ms()).unwrap();
        assert!(candidates.is_empty(), "a recently-running node is not a candidate");
        assert_eq!(status_of(&conn, node), SessionStatus::Running);
    }

    /// A captured session identity is an observation, however old the row.
    #[test]
    fn session_identity_excludes_a_candidate() {
        let mut conn = isolated_conn();
        let node = seed_node(
            &mut conn,
            "sessioned",
            "running",
            now_ms() - THRESHOLD - 60_000,
            Some("test-session-1793"),
            true,
        );

        let candidates = db::list_zombie_candidates_inner(&conn, THRESHOLD, now_ms()).unwrap();
        assert!(candidates.is_empty(), "a session identity is proof of observation");
        assert_eq!(status_of(&conn, node), SessionStatus::Running);
    }

    /// A user-driven node (no circuit step references it) is never reaped —
    /// e.g. a `terminal` shell that never captures an identity.
    #[test]
    fn non_piloted_node_is_untouched() {
        let mut conn = isolated_conn();
        let node = seed_node(
            &mut conn,
            "user",
            "running",
            now_ms() - THRESHOLD - 60_000,
            None,
            false,
        );

        let candidates = db::list_zombie_candidates_inner(&conn, THRESHOLD, now_ms()).unwrap();
        assert!(candidates.is_empty(), "an interactive node is not circuit-piloted");
        assert_eq!(status_of(&conn, node), SessionStatus::Running);
    }

    /// Statuses other than `running` are out of scope even when piloted,
    /// sessionless, and stale.
    #[test]
    fn non_running_statuses_are_untouched() {
        let mut conn = isolated_conn();
        for (i, status) in ["awaiting_input", "ready", "completed"].iter().enumerate() {
            seed_node(
                &mut conn,
                &format!("done-{i}"),
                status,
                now_ms() - THRESHOLD - 60_000,
                None,
                true,
            );
        }

        let candidates = db::list_zombie_candidates_inner(&conn, THRESHOLD, now_ms()).unwrap();
        assert!(candidates.is_empty(), "only `running` is eligible: {candidates:?}");
    }

    /// The batch write re-checks `status` + session absence, so a node that
    /// captured an identity after the scan is not clobbered.
    #[test]
    fn reap_rechecks_session_absence_at_write() {
        let mut conn = isolated_conn();
        let node = seed_node(
            &mut conn,
            "race",
            "running",
            now_ms() - THRESHOLD - 60_000,
            None,
            true,
        );

        let candidates = db::list_zombie_candidates_inner(&conn, THRESHOLD, now_ms()).unwrap();
        assert_eq!(candidates, vec![node]);
        // A session identity lands between the scan and the write.
        conn.execute(
            "UPDATE agent_nodes SET cli_session_id = 'late-session' WHERE id = ?1",
            params![node],
        )
        .unwrap();

        let flipped = db::reap_zombie_agents_inner(&mut conn, &candidates).unwrap();
        assert!(flipped.is_empty(), "the conditional write must not clobber a now-observed node");
        assert_eq!(status_of(&conn, node), SessionStatus::Running);
    }

    /// The zombie rule: session OR report is an observation.
    #[test]
    fn is_zombie_requires_no_session_and_no_report() {
        let mut conn = isolated_conn();
        let plain = seed_node(&mut conn, "plain", "running", now_ms(), None, false);
        let sessioned =
            seed_node(&mut conn, "sessioned", "running", now_ms(), Some("s"), false);

        let plain = db::get_agent_node_by_id_inner(&conn, plain).unwrap();
        let sessioned = db::get_agent_node_by_id_inner(&conn, sessioned).unwrap();

        assert!(is_zombie(&plain, false), "no session and no report is a zombie");
        assert!(!is_zombie(&plain, true), "a readable report is an observation");
        assert!(!is_zombie(&sessioned, false), "a session identity is an observation");
        assert!(!is_zombie(&sessioned, true), "both observations");
    }

    /// Two ticks within the cooldown window observe each agent once.
    #[test]
    fn cooldown_suppresses_a_second_observation() {
        let mut observed = HashMap::new();
        let t = 1_000_000_000_000;

        assert_eq!(select_due_candidates(vec![7], &mut observed, t, COOLDOWN), vec![7]);
        assert!(
            select_due_candidates(vec![7], &mut observed, t + 60_000, COOLDOWN).is_empty(),
            "a tick inside the cooldown must not re-process the same agent"
        );
        assert_eq!(
            select_due_candidates(vec![7], &mut observed, t + COOLDOWN, COOLDOWN),
            vec![7],
            "the cooldown releases after the window"
        );
    }

    /// The sweep cadence itself is throttled independently of the worker tick.
    #[test]
    fn sweep_interval_gate_throttles_the_scan() {
        let t = 2_000_000_000_000;
        assert!(sweep_due(0, t), "the first sweep always runs");
        assert!(!sweep_due(t, t + 1_000), "a fast tick does not re-run the sweep");
        assert!(sweep_due(t, t + ZOMBIE_SWEEP_INTERVAL.as_millis() as i64));
    }

    // -----------------------------------------------------------------------
    // No-I/O-under-lock invariant (issues #1228/#1793). The first runs against
    // the process-global DB because the invariant is precisely "the global
    // writer mutex is released before the notification is dispatched", which a
    // per-test in-memory connection cannot express. Requires `--test-threads=1`
    // (the repo's standing contract for global-DB tests).
    // -----------------------------------------------------------------------

    /// End-to-end against the global DB: the notification fires only after the
    /// writer lock is released. The notifier panics if `try_write_conn` reports
    /// the lock is still held.
    #[test]
    fn notify_dispatches_after_the_writer_lock_is_released() {
        db::test_support::ensure_db_for_tests();
        let mesh = db::create_mesh("reap-global", "/tmp/reap-global").unwrap();
        let node = db::create_agent_node(
            mesh.id,
            "global-zombie",
            "/tmp/reap-global/node",
            "main",
            crate::models::EnvType::Windows,
            "anthropic",
            None,
            None,
            None,
            None,
            true,
            None,
            None,
            None,
        )
        .unwrap();
        db::update_agent_node_status(node.id, SessionStatus::Running).unwrap();

        let mut notified = Vec::new();
        let count = reap_and_surface(&[node.id], |id| {
            assert!(
                db::try_write_conn().is_some(),
                "notification must not run while the DB writer mutex is held (issue #1228)"
            );
            notified.push(id);
        })
        .unwrap();

        assert_eq!(count, 1);
        assert_eq!(notified, vec![node.id]);
        assert_eq!(
            db::get_agent_node_by_id(node.id).unwrap().status,
            SessionStatus::Lost
        );

        // Leave the shared process DB as we found it.
        let db = db::write_conn();
        let _ = db.execute("DELETE FROM agent_nodes WHERE id = ?1", params![node.id]);
        let _ = db.execute("DELETE FROM meshes WHERE id = ?1", params![mesh.id]);
    }

    /// A node that is not `running` is not transitioned and is not announced.
    #[test]
    fn no_notification_when_nothing_is_reaped() {
        db::test_support::ensure_db_for_tests();
        let mesh = db::create_mesh("reap-global-noop", "/tmp/reap-global-noop").unwrap();
        let node = db::create_agent_node(
            mesh.id,
            "global-idle",
            "/tmp/reap-global-noop/node",
            "main",
            crate::models::EnvType::Windows,
            "anthropic",
            None,
            None,
            None,
            None,
            true,
            None,
            None,
            None,
        )
        .unwrap();
        db::update_agent_node_status(node.id, SessionStatus::Idle).unwrap();

        let count = reap_and_surface(&[node.id], |id| panic!("must not notify for {id}")).unwrap();
        assert_eq!(count, 0);
        assert_eq!(
            db::get_agent_node_by_id(node.id).unwrap().status,
            SessionStatus::Idle
        );

        let db = db::write_conn();
        let _ = db.execute("DELETE FROM agent_nodes WHERE id = ?1", params![node.id]);
        let _ = db.execute("DELETE FROM meshes WHERE id = ?1", params![mesh.id]);
    }
}
