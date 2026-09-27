//! Issue #1911 — the legacy retirement cleanup after a forced process crash.
//!
//! #1889 covered the deterministic startup cutover and a clean reopen. It did
//! not cover the wedge those two skip: a crash *during* owned-agent cleanup,
//! when part of the cleanup is durable and part is not. This module drives the
//! production sweep ([`crate::services::autopilot::retire_legacy_automation`],
//! the call `lib.rs` `setup` makes before crash recovery) in a child process
//! and kills that process at each durable boundary; a second child is the
//! relaunch that retries cleanup.
//!
//! # Windows-only
//!
//! The module is declared `#[cfg(all(test, windows))]`, for the same reason
//! `tests/job_object.rs` is `#![cfg(windows)]`. Its process claims depend on
//! the kill-on-close job object the real spawn contains an agent in:
//! [`JobHandle::contain`] returns `None` off Windows, and an uncontained
//! process *does* survive its parent's death (verified), so on Linux and macOS
//! the assertion this suite exists to make is false rather than merely
//! unimplemented. Gating the module is therefore the honest form — a
//! per-item gate would leave a suite that silently drops its central claim.
//! The #1889 retirement checks in `db::legacy_retirement` still run everywhere.
//!
//! Why a child process: the sweep reads the global database singleton
//! (`db::read_conn`/`db::write_conn`) and the global `PROCESS_REGISTRY`.
//! Running it in this test process would point the suite's shared database
//! singleton at a fixture the test later deletes, and a "restart" would be
//! indistinguishable from a continued pass. A child gives each phase its own
//! singleton, an empty process registry — the state a relaunch actually
//! starts from — and a real `std::process::abort()` rather than a modelled
//! one. The parent only ever opens a plain read/write `Connection`, so these
//! scenarios stay parallel-safe alongside the rest of the suite.
//!
//! What is forced, and what is not:
//!
//! * Real process kill. Every crash phase ends in `abort()` at a boundary
//!   between durable steps: before the cleanup intent, after it, between the
//!   owned-process kill and its acknowledgement, and after the acknowledgement.
//!   A crash is only accepted as covered when the child records that it reached
//!   the abort: a failed assertion also exits nonzero, so the status alone
//!   proves nothing.
//! * Injected statement abort. The two boundaries that live *inside* a single
//!   transaction (the cleanup intent and the acknowledgement) are forced with a
//!   `RAISE(ABORT)` trigger, the idiom the existing retirement tests use. A
//!   transaction that does not commit leaves the same durable state whether
//!   SQLite rolled it back or a killed process lost it, and a trigger is
//!   schema, so the parent drops it before the retry to model the transient
//!   failure clearing.
//!
//! The processes a crash phase holds are contained in [`JobHandle`]s, as the
//! real spawn contains them, and that kill-on-close job is what makes an app
//! crash reach them. That mechanism is Win32-only, which is why the whole module
//! is Windows-gated rather than the process assertions alone.

use rusqlite::{Connection, params};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const MESH_ID: i64 = 1;
const NODE_ID: i64 = 1;

const PHASE_ENV: &str = "BUILDMESH_TEST_RETIREMENT_PHASE";
const DB_ENV: &str = "BUILDMESH_TEST_RETIREMENT_DB";
const NODE_ENV: &str = "BUILDMESH_TEST_RETIREMENT_NODE";

/// Fails the acknowledgement inside `complete_stop`'s transaction, after the
/// owned process has been killed and before the stop is stamped durable.
const ACK_TRIGGER: &str = "injected_fail_retirement_ack";

/// Fails the cleanup intent inside `retire`'s transaction, after the
/// retirement rows are written and before the commit.
const INTENT_TRIGGER: &str = "injected_fail_retirement_intent";

const CHILD_TEST: &str = "retirement_crash_child";

/// Rust's test harness exits 101 when a test panics, which is also a nonzero
/// exit — the reason a crash phase needs positive evidence instead of relying
/// on the status alone.
const PANIC_EXIT: i32 = 101;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Die during startup, before the sweep reaches the cleanup intent.
    CrashBeforeIntent,
    /// Die inside the intent transaction, after its rows are written.
    CrashIntentUncommitted,
    /// Die after the intent commits, before any owned process is stopped.
    CrashAfterIntent,
    /// Die after the owned process is killed, before the acknowledgement
    /// commits.
    CrashBeforeAck,
    /// Die after the acknowledgement commits.
    CrashAfterAck,
    /// The relaunch: run the sweep and exit normally.
    Retry,
    /// The relaunch, with a live process registered for the node. Every
    /// scenario that uses this phase puts ownership somewhere the retirement
    /// no longer holds it — the stop is already acknowledged, or a borrower or
    /// a new incarnation owns the process — so the child asserts that process
    /// survives the sweep.
    RetryBesideLiveProcess,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::CrashBeforeIntent => "crash-before-intent",
            Phase::CrashIntentUncommitted => "crash-intent-uncommitted",
            Phase::CrashAfterIntent => "crash-after-intent",
            Phase::CrashBeforeAck => "crash-before-ack",
            Phase::CrashAfterAck => "crash-after-ack",
            Phase::Retry => "retry",
            Phase::RetryBesideLiveProcess => "retry-beside-live-process",
        }
    }

    fn parse(value: &str) -> Self {
        [
            Phase::CrashBeforeIntent,
            Phase::CrashIntentUncommitted,
            Phase::CrashAfterIntent,
            Phase::CrashBeforeAck,
            Phase::CrashAfterAck,
            Phase::Retry,
            Phase::RetryBesideLiveProcess,
        ]
        .into_iter()
        .find(|phase| phase.as_str() == value)
        .unwrap_or_else(|| panic!("unknown retirement crash phase {value:?}"))
    }

    fn owns_live_process(self) -> bool {
        matches!(
            self,
            Phase::CrashAfterIntent
                | Phase::CrashBeforeAck
                | Phase::CrashAfterAck
                | Phase::RetryBesideLiveProcess
        )
    }

    fn is_crash(self) -> bool {
        !matches!(self, Phase::Retry | Phase::RetryBesideLiveProcess)
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// A temporary Mesh whose legacy Autopilot run is mid-wrap-up, plus the
/// worktree that retirement has to retain for inspection.
struct LegacyFixture {
    // Dropped last: the database file and the worktree live inside it.
    _dir: tempfile::TempDir,
    db: PathBuf,
    worktree: PathBuf,
}

impl LegacyFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("retirement fixture directory");
        let db = dir.path().join("legacy.db");
        let worktree = dir.path().join("worktrees").join("issue-18");
        std::fs::create_dir_all(&worktree).expect("retained worktree");
        std::fs::write(
            worktree.join("work-in-progress.txt"),
            "uncommitted legacy work\n",
        )
        .expect("worktree contents");
        {
            let conn = Connection::open(&db).expect("fixture database");
            crate::db::init_schema(&conn).expect("fixture schema");
            conn.execute(
                "INSERT INTO meshes(id,name,path,autopilot_enabled,autopilot_concurrency_limit,circuit_run_capacity)
                 VALUES(?1,'legacy-mesh',?2,1,7,3)",
                params![MESH_ID, dir.path().join("repo").to_string_lossy().to_string()],
            )
            .expect("legacy mesh");
            conn.execute(
                "INSERT INTO agent_nodes(id,mesh_id,name,path,status,session_started_at,worktree_path)
                 VALUES(?1,?2,'legacy-issue-18',?3,'running',NULL,?3)",
                params![NODE_ID, MESH_ID, worktree.to_string_lossy().to_string()],
            )
            .expect("legacy agent node");
            conn.execute(
                "INSERT INTO autopilot_runs(node_id,mesh_id,issue_number,state,pr_url,attempts,pr_number,loop_iteration)
                 VALUES(?1,?2,18,'finishing','https://example.test/pr/18',3,314,2)",
                params![NODE_ID, MESH_ID],
            )
            .expect("legacy autopilot run");
        }
        Self { _dir: dir, db, worktree }
    }

    fn open(&self) -> Connection {
        Connection::open(&self.db).expect("reopen the fixture database")
    }

    /// Run one phase in a fresh process. The child owns the global database
    /// singleton and the process registry for that phase only.
    ///
    /// Its output goes to a file beside the fixture database rather than to a
    /// pipe: a process that outlives the child — exactly what a containment
    /// regression leaves behind — would hold a pipe open and stall this
    /// scenario instead of being reported, and a hung test says less than a
    /// failed one.
    fn run(&self, phase: Phase) -> std::process::ExitStatus {
        let log = self.child_log(phase);
        let stdout = std::fs::File::create(&log).expect("create the child log");
        let stderr = stdout.try_clone().expect("clone the child log handle");
        Command::new(std::env::current_exe().expect("test binary path"))
            .arg("--ignored")
            .arg("--exact")
            .arg(child_test_name())
            .arg("--test-threads=1")
            .env(PHASE_ENV, phase.as_str())
            .env(DB_ENV, &self.db)
            .env(NODE_ENV, NODE_ID.to_string())
            .stdout(std::process::Stdio::from(stdout))
            .stderr(std::process::Stdio::from(stderr))
            .status()
            .unwrap_or_else(|error| panic!("could not spawn the {} child: {error}", phase.as_str()))
    }

    fn child_log(&self, phase: Phase) -> PathBuf {
        self.side_file(&format!("{}-child.log", phase.as_str()))
    }

    /// What the child said, so a failing scenario reports the child's own
    /// assertion rather than a bare exit code. A child test harness reports its
    /// own panic on stdout, so both streams of the log are searched.
    fn report(&self, phase: Phase, status: &std::process::ExitStatus) -> String {
        let log = std::fs::read(self.child_log(phase)).unwrap_or_default();
        match panic_line(&log) {
            Some(text) => format!("{status}: {text}"),
            None => format!("{status}: the child reported no diagnostics"),
        }
    }

    fn expect_crash(&self, phase: Phase) {
        let status = self.run(phase);
        assert_eq!(
            self.reached(phase).as_deref(),
            Some(phase.as_str()),
            "the {} child never reached its forced abort, so this crash window is not covered ({})",
            phase.as_str(),
            self.report(phase, &status)
        );
        assert_ne!(
            status.code(),
            Some(PANIC_EXIT),
            "the {} child failed an assertion instead of being killed ({})",
            phase.as_str(),
            self.report(phase, &status)
        );
        assert!(
            !status.success(),
            "the {} phase must die by a forced process kill instead of exiting cleanly ({})",
            phase.as_str(),
            self.report(phase, &status)
        );
    }

    /// What the crash child recorded immediately before `abort()`. A nonzero
    /// exit is not evidence of a crash — a failed assertion exits nonzero too —
    /// so the scenario only counts a boundary as covered when the child says it
    /// got there.
    fn reached(&self, phase: Phase) -> Option<String> {
        let marker = std::fs::read_to_string(self.side_file(&format!("reached-{}", phase.as_str())))
            .ok()?
            .trim()
            .to_string();
        Some(marker)
    }

    /// The pids the crash child contained: the registered process first, then
    /// the second process the retirement never touches, so the parent can watch
    /// more than the one entry the sweep knows about.
    fn owned_process_tree(&self) -> Vec<u32> {
        let recorded = std::fs::read_to_string(self.side_file("owned-pid")).unwrap_or_else(|error| {
            panic!("the crash child recorded no contained process: {error}")
        });
        let pids: Vec<u32> = recorded
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect();
        assert!(
            pids.len() == 2,
            "the crash child recorded both contained processes, not {pids:?}"
        );
        pids
    }

    /// Poll until the OS has finished killing a contained process, so "the app
    /// crash took the owned agent with it" is observed rather than assumed. The
    /// kill is asynchronous, so this is a bounded wait on an observable effect
    /// rather than a fixed sleep. A pid the probe cannot observe is a failure,
    /// not an exit.
    fn owned_process_exits(&self, pid: u32) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            match observe_exit(pid) {
                Some(true) => return true,
                Some(false) => {}
                None => panic!("could not observe whether process {pid} is still running"),
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// A file the crash child leaves beside the fixture database. The database
    /// path is what both processes are given, so it needs no second env var.
    fn side_file(&self, suffix: &str) -> PathBuf {
        PathBuf::from(format!("{}.{}", self.db.display(), suffix))
    }

    /// The relaunch. It must complete the sweep and exit cleanly; the crash
    /// scenarios above are the ones allowed to leave work outstanding.
    fn expect_recovery(&self, phase: Phase) {
        let status = self.run(phase);
        assert!(
            status.success(),
            "the relaunch after a crash must run the retirement sweep to completion ({}, {})",
            phase.as_str(),
            self.report(phase, &status)
        );
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot::read(&self.open())
    }

    fn worktree_retained(&self) -> bool {
        self.worktree.join("work-in-progress.txt").is_file()
    }

    fn trigger_present(&self, name: &str) -> bool {
        self.open()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND name=?1)",
                params![name],
                |row| row.get::<_, i64>(0),
            )
            .expect("query sqlite_master")
            != 0
    }

    /// Model the transient injected failure clearing, so the relaunch is not
    /// wedged on a trigger the test itself installed.
    fn drop_injected_triggers(&self) {
        let conn = self.open();
        for name in [ACK_TRIGGER, INTENT_TRIGGER] {
            conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {name}"))
                .expect("drop the injected trigger");
        }
    }

    /// A Circuit run that borrows the retained node as its source. The node is
    /// still owned by a live Circuit, so an old retirement must not stop it.
    fn borrow_node_from_running_circuit(&self) {
        self.open()
            .execute_batch(
                "INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
                 INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,source_agent_node_id)
                 VALUES(1,1,1,'running',1);",
            )
            .expect("borrow the retained node from a running Circuit");
    }

    /// A fresh session incarnation on the same node row. The retirement
    /// journalled the previous incarnation, so it no longer owns the process.
    fn start_new_session_incarnation(&self) {
        self.open()
            .execute(
                "UPDATE agent_nodes SET session_started_at=42 WHERE id=?1",
                params![NODE_ID],
            )
            .expect("start a new session incarnation");
    }
}

// ---------------------------------------------------------------------------
// Durable state
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    retirements: i64,
    prior_state: String,
    journalled_incarnation: Option<i64>,
    stopped_at: Option<String>,
    node_status: String,
    node_session_started_at: Option<i64>,
    run_state: String,
    run_pr_url: Option<String>,
    /// The run's inspection-relevant identity, excluding the fields a retry is
    /// allowed to move (`state`, `updated_at`). A retirement that changed any
    /// of these would rewrite history rather than preserve it.
    run_identity: String,
    autopilot_enabled: bool,
    legacy_concurrency: i64,
    circuit_capacity: i64,
    agent_nodes: i64,
    autopilot_runs: i64,
    circuit_runs: i64,
    pending_worktree_removals: i64,
    /// `updated_at` is the one run field a retry legitimately moves, so it is
    /// kept apart from the identity it must not disturb.
    updated_at: String,
}

/// One retained legacy run, read field by field so the identity a retry must
/// not disturb can be compared directly.
struct LegacyRun {
    state: String,
    pr_url: Option<String>,
    issue_number: i64,
    pr_number: Option<i64>,
    attempts: i64,
    loop_iteration: Option<i64>,
    created_at: String,
    updated_at: String,
}

impl Snapshot {
    fn read(conn: &Connection) -> Self {
        let retirement: (i64, String, Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT COUNT(*),COALESCE(MAX(prior_state),''),MAX(session_started_at),MAX(stopped_at)
                 FROM legacy_autopilot_retirements",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("read the retirement ledger");
        let node: (String, Option<i64>) = conn
            .query_row(
                "SELECT status,session_started_at FROM agent_nodes WHERE id=?1",
                params![NODE_ID],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read the retained node");
        let run: LegacyRun = conn
            .query_row(
                "SELECT state,pr_url,issue_number,pr_number,attempts,loop_iteration,created_at,updated_at
                 FROM autopilot_runs WHERE node_id=?1",
                params![NODE_ID],
                |row| {
                    Ok(LegacyRun {
                        state: row.get(0)?,
                        pr_url: row.get(1)?,
                        issue_number: row.get(2)?,
                        pr_number: row.get(3)?,
                        attempts: row.get(4)?,
                        loop_iteration: row.get(5)?,
                        created_at: row.get(6)?,
                        updated_at: row.get(7)?,
                    })
                },
            )
            .expect("read the retained legacy run");
        let mesh: (bool, i64, i64) = conn
            .query_row(
                "SELECT autopilot_enabled,autopilot_concurrency_limit,circuit_run_capacity FROM meshes WHERE id=?1",
                params![MESH_ID],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read retained mesh settings");
        Self {
            retirements: retirement.0,
            prior_state: retirement.1,
            journalled_incarnation: retirement.2,
            stopped_at: retirement.3,
            node_status: node.0,
            node_session_started_at: node.1,
            run_state: run.state.clone(),
            run_pr_url: run.pr_url.clone(),
            run_identity: format!(
                "issue={} attempts={} pr_number={:?} pr_url={:?} loop_iteration={:?} created_at={}",
                run.issue_number, run.attempts, run.pr_number, run.pr_url, run.loop_iteration, run.created_at
            ),
            updated_at: run.updated_at,
            autopilot_enabled: mesh.0,
            legacy_concurrency: mesh.1,
            circuit_capacity: mesh.2,
            agent_nodes: count(conn, "agent_nodes"),
            autopilot_runs: count(conn, "autopilot_runs"),
            circuit_runs: count(conn, "autopilot_circuit_runs"),
            pending_worktree_removals: count(conn, "pending_worktree_removals"),
        }
    }

    /// The retained side of the cutover, which no retry may alter. `before` is
    /// the fixture as the crash found it, so "unchanged" means unchanged across
    /// the crash and every retry that followed it.
    fn assert_retained(&self, before: &Snapshot, fixture: &LegacyFixture) {
        assert_eq!(self.prior_state, "finishing", "the retirement keeps the prior legacy state");
        assert_eq!(self.run_pr_url.as_deref(), Some("https://example.test/pr/18"), "the legacy PR identity is retained");
        assert_eq!(self.run_state, "cancelled", "the legacy run stays cancelled, never reopened");
        assert_eq!(
            self.run_identity, before.run_identity,
            "the retained run's identity survives every retry — only its state and updated_at may move"
        );
        assert_eq!(self.autopilot_runs, before.autopilot_runs, "a retry never splits or reopens the legacy run");
        assert_eq!(self.circuit_runs, before.circuit_runs, "no legacy run is converted into a Circuit");
        assert_eq!(self.agent_nodes, before.agent_nodes, "the Agent Node is retained");
        assert_eq!((self.legacy_concurrency, self.circuit_capacity), (7, 3), "capacity settings are independent and unchanged");
        assert_eq!(self.pending_worktree_removals, 0, "retirement never queues a worktree removal");
        assert!(fixture.worktree_retained(), "the retained worktree stays on disk for inspection");
    }

    /// A suspended retired node must stay out of the auto-resume list: Idle and
    /// Suspended are the frontend's fresh-spawn signals.
    fn assert_never_auto_resumes(&self, conn: &Connection) {
        assert!(
            crate::db::agent_node::list_suspended_nodes_inner(conn)
                .expect("list resumable nodes")
                .is_empty(),
            "retired work never auto-resumes, however far the cleanup got"
        );
    }
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap_or_else(|error| panic!("count {table}: {error}"))
}

/// While the retirement is outstanding, the stop still owns the node: a
/// replacement spawn must be refused, or the retry and the new session would
/// fight over the same worktree.
fn assert_replacement_spawn_refused(fixture: &LegacyFixture) {
    let conn = fixture.open();
    assert!(
        crate::db::legacy_retirement::pending_inner(&conn, NODE_ID).expect("pending read"),
        "the cleanup intent is still outstanding"
    );
    assert!(
        crate::db::circuit::leases::claim_agent_spawn_inner(&conn, NODE_ID)
            .expect("spawn claim")
            .is_none(),
        "an outstanding retirement refuses a replacement spawn"
    );
}

// ---------------------------------------------------------------------------
// Child process
// ---------------------------------------------------------------------------

/// Subprocess entry point for the scenarios above: it runs one retirement
/// phase with its own database singleton and process registry, then dies.
///
/// It is `#[ignore]`d because it is only meaningful as the process a scenario
/// kills — a scenario always supplies a phase, and this body does nothing at
/// all without one.
#[test]
#[ignore = "subprocess entry point: only the #1911 scenarios in this module run it"]
fn retirement_crash_child() {
    let Ok(phase) = std::env::var(PHASE_ENV) else {
        return;
    };
    let phase = Phase::parse(&phase);
    let db = std::env::var(DB_ENV).expect("the scenario supplies the fixture database path");
    let node_id: i64 = std::env::var(NODE_ENV)
        .expect("the scenario supplies the node id")
        .parse()
        .expect("a numeric node id");
    crate::db::init(std::path::Path::new(&db)).expect("child database init");

    let owned = phase
        .owns_live_process()
        .then(|| register_live_agent(node_id, MESH_ID, &db));
    if let Some(owned) = &owned {
        assert!(
            !process_has_exited(&owned.process),
            "the retired agent process must be alive before the sweep"
        );
    }

    match phase {
        // The app died during startup; the sweep never ran.
        Phase::CrashBeforeIntent => {}
        Phase::CrashIntentUncommitted => {
            install_trigger(
                INTENT_TRIGGER,
                "BEFORE UPDATE OF state ON autopilot_runs \
                 WHEN OLD.state IN ('implementing','finishing','suffix_pending') \
                 BEGIN SELECT RAISE(ABORT,'injected crash inside the cleanup intent'); END",
            );
            assert!(
                crate::services::autopilot::retire_legacy_automation().is_err(),
                "an aborted cleanup intent surfaces as a failed sweep"
            );
            assert!(
                !crate::db::legacy_retirement::pending(node_id).expect("pending read"),
                "an uncommitted intent records no pending stop"
            );
        }
        Phase::CrashAfterIntent => {
            assert_eq!(
                crate::db::legacy_retirement::retire().expect("commit the cleanup intent"),
                vec![node_id],
                "the intent names the outstanding stop"
            );
            let process = &owned.as_ref().expect("a live process for this phase").process;
            assert!(
                !process_has_exited(process),
                "committing the cleanup intent must not stop the process it is about to"
            );
        }
        Phase::CrashBeforeAck => {
            install_trigger(
                ACK_TRIGGER,
                "BEFORE UPDATE OF stopped_at ON legacy_autopilot_retirements \
                 BEGIN SELECT RAISE(ABORT,'injected crash before the retirement acknowledgement'); END",
            );
            crate::services::autopilot::retire_legacy_automation().expect("the sweep still runs");
            let process = &owned.as_ref().expect("a live process for this phase").process;
            assert!(
                crate::agent::process::PROCESS_REGISTRY.get(&node_id).is_none(),
                "the owned incarnation is torn down"
            );
            assert!(process_has_exited(process), "the owned process is really dead");
            assert!(
                crate::db::legacy_retirement::pending(node_id).expect("pending read"),
                "the failed acknowledgement leaves the stop outstanding"
            );
        }
        Phase::CrashAfterAck => {
            crate::services::autopilot::retire_legacy_automation().expect("the sweep runs");
            let process = &owned.as_ref().expect("a live process for this phase").process;
            assert!(process_has_exited(process), "the owned process is really dead");
            assert!(
                !crate::db::legacy_retirement::pending(node_id).expect("pending read"),
                "the acknowledgement is durable"
            );
        }
        Phase::Retry => {
            crate::services::autopilot::retire_legacy_automation()
                .expect("the relaunch completes the retirement sweep");
        }
        Phase::RetryBesideLiveProcess => {
            crate::services::autopilot::retire_legacy_automation()
                .expect("the relaunch completes the retirement sweep");
            let process = &owned.as_ref().expect("a live process for this phase").process;
            assert!(
                crate::agent::process::PROCESS_REGISTRY.get(&node_id).is_some(),
                "a stop that is not outstanding is not retried"
            );
            assert!(
                !process_has_exited(process),
                "a process the retirement no longer owns is never stopped by the retry"
            );
        }
    }

    if phase.is_crash() {
        forced_crash(phase, &db);
    }
}

/// A real process kill: no destructor runs, no connection closes, no lock is
/// released the way a clean exit would release it. The marker is written and
/// closed first, so the parent can see the child got here even though the
/// process that wrote it is gone.
fn forced_crash(phase: Phase, db: &str) -> ! {
    let marker = format!("{db}.reached-{}", phase.as_str());
    std::fs::write(&marker, phase.as_str()).expect("record that the child reached the forced abort");
    std::process::abort();
}

/// A child test harness reports its own panic on stdout, so the whole log is
/// searched for the line that says what went wrong.
fn panic_line(log: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(log);
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    while let Some(line) = lines.next() {
        if !line.contains("panicked at") {
            continue;
        }
        let message = lines.next()?;
        let location = line.split("panicked at").nth(1)?.trim();
        return Some(format!("{message} (child {location})"));
    }
    None
}

/// The child's name as libtest spells it.
///
/// `--exact` matches a test's *reported* name, which libtest builds from the
/// module path **without** the crate name, while `module_path!` includes it
/// (`buildmesh::services::autopilot::retirement_crash_tests`). Passing the
/// `module_path!` form verbatim would match nothing and the child would exit
/// 0 having run no test, so the leading segment is dropped here. If this
/// module is ever moved under a different harness, this is the line to revisit:
/// a wrong name shows up as a child that reports success without testing.
fn child_test_name() -> String {
    let module = module_path!();
    match module.split_once("::") {
        Some((_crate_name, rest)) => format!("{rest}::{CHILD_TEST}"),
        None => format!("{module}::{CHILD_TEST}"),
    }
}

fn install_trigger(name: &str, body: &str) {
    crate::db::write_conn()
        .execute_batch(&format!("CREATE TRIGGER {name} {body}"))
        .expect("install the injected trigger");
}

/// Whether the owned agent process has exited. A handle that cannot answer is
/// a failure rather than a state: treating an error as "still running" would
/// let a liveness assertion pass without establishing liveness.
fn process_has_exited(process: &crate::agent::process::AgentProcess) -> bool {
    process
        .child
        .lock()
        .expect("child handle")
        .try_wait()
        .unwrap_or_else(|error| panic!("cannot observe the retired agent process: {error}"))
        .is_some()
}

/// The processes a crash phase owns, all of which the app crash must take with
/// it.
struct OwnedTree {
    /// The registered process: the one the retirement stops, or refuses to
    /// stop when ownership has moved.
    process: Arc<crate::agent::process::AgentProcess>,
    /// A second process in its own kill-on-close job. The retirement never
    /// touches it, so finding it dead after the crash is what shows the app
    /// crash reaches every contained process and not merely the registry entry
    /// the sweep happens to know about. It is spawned by the app rather than by
    /// the owned process because a background child of a pseudoconsole-hosted
    /// shell does not survive on Windows, which would make the fixture prove
    /// nothing.
    _sleeper_job: Option<crate::process_util::JobHandle>,
}

/// Register a live PTY-backed agent process for `node_id` so the sweep runs
/// its production `kill_session_if_generation` path against a real OS process
/// instead of an empty registry. No reader or writer thread: the registry
/// entry and its child handle are what the retirement owns.
///
/// Each process is contained in a kill-on-close [`JobHandle`], exactly as the
/// real spawn contains it. That matters twice: the sweep's stop reaches what
/// the job holds, and the app crashing takes it with it instead of orphaning
/// it for the next launch to find.
fn register_live_agent(
    node_id: i64,
    mesh_id: i64,
    db: &str,
) -> OwnedTree {
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open a PTY for the retired agent process");
    let mut command = portable_pty::CommandBuilder::new(shell());
    shell_arguments(&mut command);
    let child = pair
        .slave
        .spawn_command(command)
        .expect("spawn the retired agent process");
    let pid = child.process_id().expect("the spawned process has a pid");
    let job = crate::process_util::JobHandle::contain(pid);
    assert!(job.is_some(), "the retired agent process must be job-contained");

    // A PowerShell sleeper rather than `ping`: a background child of the PTY's
    // shell does not survive on Windows, and this one only has to stay alive,
    // not write to a console. Windows-only like the module, so the program name
    // is fixed rather than resolved per platform.
    let mut sleeper_command = crate::process_util::command_no_window("powershell.exe");
    sleeper_command
        .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 600"])
        // Never inherit this process's stdio: the parent reads the crash
        // child's output, and a process that outlives the child would hold those
        // files open and stall the parent instead of being observed.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let sleeper = sleeper_command.spawn().expect("spawn a second contained process");
    let sleeper_pid = sleeper.id();
    // The job owns the sleeper's life: forgetting the child handle stops it from
    // ever reaping the process, and the job is closed by the crash. Without a
    // job this would leak a ten-minute sleeper per test — which is exactly why
    // the module is Windows-gated.
    std::mem::forget(sleeper);
    let sleeper_job = crate::process_util::JobHandle::contain(sleeper_pid);
    assert!(sleeper_job.is_some(), "the second process must be job-contained");

    // A command that rejects its own arguments exits within milliseconds, long
    // before the crash, and would make the exit observation meaningless — so
    // liveness is re-checked after the process has had time to die on its own.
    for pid in [pid, sleeper_pid] {
        assert!(observe_running(pid), "contained process {pid} must be running when it is registered");
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    for pid in [pid, sleeper_pid] {
        assert!(
            observe_running(pid),
            "contained process {pid} exited on its own; the fixture would prove nothing"
        );
    }
    assert!(
        observe_running(sleeper_pid),
        "the second contained process {sleeper_pid} must be running before the crash"
    );
    // The parent watches both pids to see what the app crash does to them.
    std::fs::write(format!("{db}.owned-pid"), format!("{pid}\n{sleeper_pid}\n"))
        .expect("record the contained processes");
    let (writer_tx, _writer_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(8);
    let process = crate::agent::process::AgentProcess::new(
        child,
        writer_tx,
        None,
        pair.master,
        Arc::new(AtomicBool::new(true)),
        Arc::new(AtomicBool::new(false)),
        job,
        None,
        std::time::Instant::now(),
        mesh_id,
    );
    crate::agent::process::PROCESS_REGISTRY.insert(node_id, process);
    let process = crate::agent::process::PROCESS_REGISTRY
        .get(&node_id)
        .expect("the spawned process is registered");
    OwnedTree { process, _sleeper_job: sleeper_job }
}

/// Whether a pid is still running. An unobservable pid fails rather than
/// answering, so a probe error can never stand in for either state.
fn observe_running(pid: u32) -> bool {
    match observe_exit(pid) {
        Some(exited) => !exited,
        None => panic!("could not observe whether process {pid} is running"),
    }
}

/// Three-way observation of a process this test no longer holds a handle to:
/// `Some(true)` exited, `Some(false)` still running, and `None` when the probe
/// could not tell. An unobservable process is a test failure — counting it as
/// either state would let a probe error pass as evidence.
fn observe_exit(pid: u32) -> Option<bool> {
    use std::os::raw::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn WaitForSingleObject(handle: *mut c_void, millis: u32) -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }

    const SYNCHRONIZE: u32 = 0x0010_0000;
    /// What Windows reports for a pid that does not exist, which is how a
    /// fully terminated process presents itself once nothing holds a handle.
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const WAIT_OBJECT_0: u32 = 0x0000_0000;
    const WAIT_TIMEOUT: u32 = 0x0000_0102;

    // SAFETY: the handle is checked for null, used once and closed before
    // returning; no out-param is passed.
    unsafe {
        let handle = OpenProcess(SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return (GetLastError() == ERROR_INVALID_PARAMETER).then_some(true);
        }
        let waited = WaitForSingleObject(handle, 0);
        CloseHandle(handle);
        match waited {
            WAIT_OBJECT_0 => Some(true),
            WAIT_TIMEOUT => Some(false),
            _ => None,
        }
    }
}

fn shell() -> &'static str {
    "cmd.exe"
}

/// A long-lived process for the retirement to own: a background child of a
/// pseudoconsole-hosted shell does not survive, so the owned process stays a
/// single sleeper.
fn shell_arguments(command: &mut portable_pty::CommandBuilder) {
    command.args(["/c", "ping", "-n", "600", "127.0.0.1"]);
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// A crash on either side of the cleanup-intent transaction leaves the legacy
/// run exactly as it was: no half-written retirement, no cancelled run without
/// a stop to back it. The relaunch then records the intent and completes the
/// stop it inherited.
#[test]
fn retirement_crash_before_the_cleanup_intent_recovers_without_partial_retirement() {
    let fixture = LegacyFixture::new();
    let untouched = fixture.snapshot();
    assert_eq!(untouched.retirements, 0);
    assert_eq!(untouched.run_state, "finishing");

    fixture.expect_crash(Phase::CrashBeforeIntent);
    assert_eq!(
        fixture.snapshot(),
        untouched,
        "a crash before the sweep must leave every durable fact alone"
    );

    fixture.expect_crash(Phase::CrashIntentUncommitted);
    assert_eq!(
        fixture.snapshot(),
        untouched,
        "a cleanup intent that never commits must not half-retire the legacy run"
    );

    fixture.drop_injected_triggers();
    fixture.expect_recovery(Phase::Retry);
    let recovered = fixture.snapshot();
    assert_eq!(recovered.retirements, 1, "the relaunch records the intent exactly once");
    assert_eq!(recovered.run_state, "cancelled", "legacy work is cancelled before any stop");
    assert!(!recovered.autopilot_enabled, "legacy scheduling is disabled");
    assert!(recovered.stopped_at.is_some(), "the relaunch completes the stop");
    assert_eq!(recovered.node_status, "suspended");
    recovered.assert_retained(&untouched, &fixture);
    recovered.assert_never_auto_resumes(&fixture.open());

    fixture.expect_recovery(Phase::RetryBesideLiveProcess);
    assert_eq!(
        fixture.snapshot(),
        recovered,
        "a further relaunch is idempotent: the sweep has nothing left to do"
    );
}

/// The durable wedge #1889 did not cover: the intent committed, the owned
/// process was never stopped, and the app died there. The relaunch resumes
/// the persisted intent instead of dispatching more work.
#[test]
fn retirement_crash_after_the_cleanup_intent_resumes_the_outstanding_stop() {
    let fixture = LegacyFixture::new();
    let untouched = fixture.snapshot();
    fixture.expect_crash(Phase::CrashAfterIntent);

    let wedged = fixture.snapshot();
    assert_eq!(wedged.retirements, 1, "the cleanup intent is durable");
    assert_eq!(wedged.run_state, "cancelled", "the legacy run is cancelled before the stop");
    assert!(!wedged.autopilot_enabled, "legacy scheduling is already disabled");
    assert!(
        wedged.stopped_at.is_none(),
        "the process stop is still outstanding when the app dies"
    );
    assert_eq!(
        wedged.node_status, "running",
        "a crash between the intent and the stop must not half-stop the node"
    );
    assert_replacement_spawn_refused(&fixture);
    wedged.assert_retained(&untouched, &fixture);

    // The processes the app contains are held in kill-on-close jobs, so the app
    // dying takes them with it: the relaunch inherits a stop whose process is
    // already gone, rather than an orphan it has no handle to. Both the
    // registered process and the second contained one must be gone — watching
    // only the registry entry would show the sweep's own victim, not the crash
    // reaching everything the app held.
    for pid in fixture.owned_process_tree() {
        assert!(
            fixture.owned_process_exits(pid),
            "an app crash must take every contained process with it (pid {pid})"
        );
    }

    fixture.expect_recovery(Phase::Retry);
    let recovered = fixture.snapshot();
    assert!(recovered.stopped_at.is_some(), "the retry finishes the stop it inherited");
    assert_eq!(recovered.node_status, "suspended");
    assert_eq!(recovered.retirements, 1, "the retry does not record a second retirement");
    recovered.assert_retained(&untouched, &fixture);
    recovered.assert_never_auto_resumes(&fixture.open());

    fixture.expect_recovery(Phase::RetryBesideLiveProcess);
    assert_eq!(fixture.snapshot(), recovered, "the retry is idempotent");
}

/// The external effect happens before its acknowledgement: the owned process
/// is really killed, then the process dies before the stop is stamped. The
/// relaunch must not read the dead process as a stop it already performed.
#[test]
fn retirement_crash_between_the_process_kill_and_its_acknowledgement_recovers() {
    let fixture = LegacyFixture::new();
    let untouched = fixture.snapshot();
    fixture.expect_crash(Phase::CrashBeforeAck);

    let wedged = fixture.snapshot();
    assert!(wedged.stopped_at.is_none(), "the kill is not acknowledged when the app dies");
    assert_eq!(
        wedged.node_status, "running",
        "the rolled-back acknowledgement leaves the node status alone"
    );
    assert_eq!(wedged.retirements, 1, "the intent survives the crash");
    assert!(fixture.trigger_present(ACK_TRIGGER), "the injected failure is what wedged the stop");
    assert_replacement_spawn_refused(&fixture);
    wedged.assert_retained(&untouched, &fixture);

    fixture.drop_injected_triggers();
    fixture.expect_recovery(Phase::Retry);
    let recovered = fixture.snapshot();
    assert!(
        recovered.stopped_at.is_some(),
        "the relaunch acknowledges the stop whose process is already gone"
    );
    assert_eq!(recovered.node_status, "suspended");
    recovered.assert_retained(&untouched, &fixture);
    recovered.assert_never_auto_resumes(&fixture.open());
}

/// The last durable boundary: the stop is acknowledged, then the app dies
/// before the sweep finishes. A relaunch has nothing to retry, so a process
/// that is no longer the retirement's to stop is left alone.
#[test]
fn retirement_crash_after_the_acknowledgement_leaves_nothing_to_retry() {
    let fixture = LegacyFixture::new();
    let untouched = fixture.snapshot();
    fixture.expect_crash(Phase::CrashAfterAck);

    let committed = fixture.snapshot();
    assert!(committed.stopped_at.is_some(), "the acknowledgement is durable");
    assert_eq!(committed.node_status, "suspended");
    committed.assert_retained(&untouched, &fixture);
    committed.assert_never_auto_resumes(&fixture.open());

    fixture.expect_recovery(Phase::RetryBesideLiveProcess);
    assert_eq!(
        fixture.snapshot(),
        committed,
        "a completed retirement is not re-run: nothing is stopped twice"
    );
}

/// Ownership can move while a stop is outstanding. A newer Circuit borrows the
/// retained node, so the retry closes its own intent instead of stopping a
/// process the Circuit owns.
#[test]
fn retirement_retry_protects_a_borrowed_process_and_the_retained_node() {
    let fixture = LegacyFixture::new();
    fixture.expect_crash(Phase::CrashAfterIntent);
    fixture.borrow_node_from_running_circuit();
    // The comparison point is after the borrow exists, so "unchanged" means the
    // retry added no Circuit run of its own.
    let borrowed = fixture.snapshot();
    assert_eq!(borrowed.circuit_runs, 1);
    assert_eq!(borrowed.retirements, 1, "the stop is still outstanding when the borrow appears");

    // The child asserts the live process survived the sweep.
    fixture.expect_recovery(Phase::RetryBesideLiveProcess);

    let after = fixture.snapshot();
    assert_eq!(
        after.node_status, "running",
        "a borrowing Circuit's process is not suspended by an old retirement"
    );
    assert!(
        after.stopped_at.is_some(),
        "the retirement closes its own intent instead of retrying a stop it does not own"
    );
    assert_eq!(
        circuit_run_state(&fixture),
        "running",
        "the borrowing run is neither reopened nor converted"
    );
    assert_eq!(after.circuit_runs, 1);
    after.assert_retained(&borrowed, &fixture);
}

/// A node that started a new session incarnation after the retirement
/// journalled the previous one is not the retirement's process to stop.
#[test]
fn retirement_retry_refuses_to_stop_a_new_session_incarnation() {
    let fixture = LegacyFixture::new();
    let untouched = fixture.snapshot();
    fixture.expect_crash(Phase::CrashAfterIntent);
    fixture.start_new_session_incarnation();

    // The child asserts the live process survived the sweep.
    fixture.expect_recovery(Phase::RetryBesideLiveProcess);

    let after = fixture.snapshot();
    assert_eq!(after.node_session_started_at, Some(42));
    assert_eq!(
        after.node_status, "running",
        "the new incarnation keeps running: the retirement stops only the incarnation it journalled"
    );
    assert!(after.stopped_at.is_some(), "the retirement closes its own intent");
    after.assert_retained(&untouched, &fixture);
}

fn circuit_run_state(fixture: &LegacyFixture) -> String {
    fixture
        .open()
        .query_row("SELECT state FROM autopilot_circuit_runs WHERE id=1", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("read the borrowing Circuit run")
}
