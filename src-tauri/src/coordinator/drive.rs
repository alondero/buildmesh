//! Coordinator drive (write) side (ADR-0008 §5, issue #319).
//!
//! `POST /nodes/{id}/prompt` writes a prompt into a live Agent Node's PTY and
//! returns an **honest verdict**. This module owns the [`AgentDriver`] trait —
//! #178's write-side seam, introduced here as its first consumer. #178's
//! scheduler will reuse the same trait rather than grow a parallel write path
//! (the single-execution-path rule, ADR-0008 §1).
//!
//! ## Why the verdict is honest
//!
//! There is no out-of-band "agent woke up" signal to wait on. In Buildmesh the
//! `attention-cleared` transition is driven *by the writer itself* — exactly as
//! the mobile relay does in [`crate::http::ws`]: a newline-terminated write to a
//! node that was `awaiting_input` clears its attention and flips it `Running`.
//! So the verdict reduces to the node's status captured *immediately before* the
//! write:
//!
//! - was `awaiting_input` → the write is the confirmed `awaiting → cleared`
//!   transition → [`Verdict::Delivered`].
//! - was anything else live (e.g. `running`, busy) → the prompt is queued to the
//!   agent's stdin with no transition to observe → [`Verdict::Unverified`].
//!
//! Claude Code queues stdin for a busy agent, so driving a non-`awaiting_input`
//! node is a legitimate "leave a follow-up" — hence *any live node* is drivable,
//! and `Unverified` (not an error) is the honest answer when consumption can't
//! be confirmed.

use crate::models::SessionStatus;

/// The honest delivery verdict (ADR-0008 §5). Serialized as `"delivered"` /
/// `"unverified"` in the `POST /nodes/{id}/prompt` response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// An `awaiting_input → cleared` transition confirmed the agent consumed
    /// the prompt.
    Delivered,
    /// The prompt was written but consumption could not be confirmed (queued to
    /// a busy agent — no attention transition to observe).
    Unverified,
}

impl Verdict {
    /// The wire/DB string form — the same token the response serializes to, so
    /// the idempotency ledger stores exactly what a replay returns.
    pub fn as_db_str(self) -> &'static str {
        match self {
            Verdict::Delivered => "delivered",
            Verdict::Unverified => "unverified",
        }
    }

    /// Parse a verdict back from the ledger. An unrecognised string (a
    /// hand-tampered row, or a future variant this build predates) is `None` so
    /// the caller can fall back to re-driving rather than trust garbage.
    pub fn from_db_str(s: &str) -> Option<Verdict> {
        match s {
            "delivered" => Some(Verdict::Delivered),
            "unverified" => Some(Verdict::Unverified),
            _ => None,
        }
    }
}

/// The result of an idempotent drive: the honest [`Verdict`] plus whether this
/// call actually sent (`replayed == false`) or replayed a prior key
/// (`replayed == true`, no second write). `replayed` makes a safe retry
/// *observable* to the Coordinator (ADR-0008 §6, story 6) rather than silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriveOutcome {
    pub verdict: Verdict,
    pub replayed: bool,
}

/// Why a drive attempt could not even reach a verdict.
#[derive(Debug, PartialEq, Eq)]
pub enum DriveError {
    /// The node has no live PTY process to write to (idle, suspended, errored,
    /// archived, or never spawned). Driving requires a *live* node.
    NotLive,
    /// The PTY write itself failed after the liveness check passed.
    WriteFailed(String),
    /// The idempotency ledger could not be consulted, so we cannot prove the
    /// prompt hasn't already landed. We refuse to send rather than risk a double
    /// delivery — fail *safe*, not fail open (issue #320 review). The caller
    /// should surface this as retryable (503).
    LedgerUnavailable(String),
}

/// The minimal capabilities a driver needs from the running system, behind a
/// seam so the drive logic is unit-testable without a real PTY or the
/// process-global DB. Production wiring is [`RegistryTarget`].
pub trait DriveTarget {
    /// Is the node's agent process alive and able to receive stdin?
    fn is_live(&self, node_id: i64) -> bool;
    /// The node's current lifecycle status, or `None` if the node is unknown.
    fn status(&self, node_id: i64) -> Option<SessionStatus>;
    /// Is the node a plain terminal (shell) with no LLM attention state? Such a
    /// node must not have its status flipped on a write — see [`AgentDriver`].
    fn is_plain_terminal(&self, node_id: i64) -> bool;
    /// Write the (already newline-terminated) payload into the node's PTY.
    fn write_prompt(&self, node_id: i64, payload: &str) -> Result<(), String>;
    /// Clear attention for the node — flip it `Running` and fan out
    /// `attention-cleared`, mirroring the mobile drive primitive.
    fn clear_attention(&self, node_id: i64);
}

/// #178's write-side driver trait. v1 is `send_prompt` + `verify_delivery`; the
/// scheduler (#178) widens it with `provision`/`await_ready` when it lands.
pub trait AgentDriver {
    /// Write `prompt` into the node's PTY, returning the status captured
    /// *before* the write — the input to [`AgentDriver::verify_delivery`].
    /// Errors with [`DriveError::NotLive`] when the node has no live PTY.
    fn send_prompt(&self, node_id: i64, prompt: &str) -> Result<SessionStatus, DriveError>;

    /// Map the pre-write status to an honest verdict. Pure: a node that was
    /// `awaiting_input` is now cleared ([`Verdict::Delivered`]); any other live
    /// state queued the prompt with no observable transition
    /// ([`Verdict::Unverified`]).
    fn verify_delivery(&self, prior_status: SessionStatus) -> Verdict {
        match prior_status {
            SessionStatus::AwaitingInput => Verdict::Delivered,
            _ => Verdict::Unverified,
        }
    }
}

/// The one driver that exists today: writes through the live PTY registry. Holds
/// its [`DriveTarget`] so tests can swap in a fake.
pub struct PtyDriver<T: DriveTarget> {
    target: T,
}

impl<T: DriveTarget> PtyDriver<T> {
    pub fn new(target: T) -> Self {
        Self { target }
    }
}

impl<T: DriveTarget> AgentDriver for PtyDriver<T> {
    fn send_prompt(&self, node_id: i64, prompt: &str) -> Result<SessionStatus, DriveError> {
        // Liveness first, before capturing status, so a non-live node is a clean
        // `NotLive` rather than a confusing write failure.
        if !self.target.is_live(node_id) {
            return Err(DriveError::NotLive);
        }
        let prior = self.target.status(node_id).ok_or(DriveError::NotLive)?;
        // Newline submits the prompt, exactly as `send_to_agent` does for a
        // human keystroke — the PTY's stdin *is* the input box.
        let payload = format!("{prompt}\n");
        self.target
            .write_prompt(node_id, &payload)
            .map_err(DriveError::WriteFailed)?;
        // A plain terminal has no LLM attention state to clear; flipping it to
        // Running would paint a spurious "Running" badge on a shell sitting at a
        // prompt. This mirrors the desktop write path's
        // `commands::agent::should_skip_attention_signals` guard.
        if !self.target.is_plain_terminal(node_id) {
            self.target.clear_attention(node_id);
        }
        Ok(prior)
    }
}

/// Production [`DriveTarget`]: the live PTY registry + the DB + the event fan-out.
struct RegistryTarget;

impl DriveTarget for RegistryTarget {
    fn is_live(&self, node_id: i64) -> bool {
        crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id)
    }

    fn status(&self, node_id: i64) -> Option<SessionStatus> {
        crate::db::get_agent_node_by_id(node_id).ok().map(|n| n.status)
    }

    fn write_prompt(&self, node_id: i64, payload: &str) -> Result<(), String> {
        crate::agent::process::PROCESS_REGISTRY.write_bytes(node_id, payload.as_bytes())
    }

    fn is_plain_terminal(&self, node_id: i64) -> bool {
        crate::db::get_agent_node_by_id(node_id)
            .map(|n| {
                crate::preferences::resolve_harness_provider(&n.provider)
                    .adapter()
                    .is_plain_terminal()
            })
            .unwrap_or(false)
    }

    fn clear_attention(&self, node_id: i64) {
        // Mirror `http::ws::forward_mobile_input_with`: flip status to Running
        // and fan out `attention-cleared` to both the desktop webview and mobile
        // subscribers so neither shows a stale "awaiting" badge for a node the
        // coordinator just drove.
        crate::attention_autoclear::disarm(node_id);
        let _ = crate::db::update_agent_node_status(node_id, SessionStatus::Running);
        if let Some(app) = crate::http::app_handle() {
            use tauri::Emitter;
            let _ = app.emit(
                "attention-cleared",
                serde_json::json!({ "session_id": node_id }),
            );
        }
        crate::http::events::emit(crate::http::events::EventMsg::AttentionCleared {
            session_id: node_id,
        });
    }
}

/// The idempotency ledger seam (issue #320): remembers the verdict a drive
/// produced under a caller-supplied key, scoped to the node it drove, so a retry
/// replays rather than re-sends. Behind a trait so [`drive_node_idempotent`] is
/// unit-testable without the process-global DB. Production wiring is
/// [`DbIdempotencyStore`].
pub trait IdempotencyStore {
    /// The verdict previously recorded for `(node_id, key)`, `Ok(None)` if the key
    /// is new for that node, or `Err` if the ledger could not be consulted at all.
    /// The `Err` case is distinct from `Ok(None)` on purpose: a caller must never
    /// treat "couldn't read" as "never sent" and re-deliver (issue #320 review).
    fn lookup(&self, node_id: i64, key: &str) -> Result<Option<Verdict>, DriveError>;
    /// Record `verdict` under `(node_id, key)`. First write wins; a later call for
    /// the same key must not overwrite it (see [`DbIdempotencyStore::record`]).
    fn record(&self, node_id: i64, key: &str, verdict: Verdict);
}

/// Drive a node idempotently: replay the recorded verdict for a duplicate key, or
/// send the prompt once and record the verdict under the key. This is the
/// headline guarantee (ADR-0008 §6) as a pure `(store, driver) -> outcome`
/// function — no I/O of its own, so the "same key twice = exactly one delivery"
/// contract is testable against fakes.
///
/// The lookup short-circuits *before* [`AgentDriver::send_prompt`], so a replay
/// never touches the node — it stays a no-op even if the node has since gone
/// away. A lookup that *errors* aborts with [`DriveError::LedgerUnavailable`]
/// without sending: if we can't prove the prompt is new, we refuse to risk a
/// second delivery. Recording happens only *after* a successful send: a
/// `NotLive` / `WriteFailed` drive leaves no ledger row, so a genuine failure is
/// retried rather than cached, while an `Unverified` (written-but-unconfirmed)
/// send *is* recorded — re-sending it is the double-delivery #178 forbids.
pub fn drive_node_idempotent<S: IdempotencyStore, D: AgentDriver>(
    store: &S,
    driver: &D,
    node_id: i64,
    idempotency_key: &str,
    prompt: &str,
) -> Result<DriveOutcome, DriveError> {
    if let Some(verdict) = store.lookup(node_id, idempotency_key)? {
        return Ok(DriveOutcome { verdict, replayed: true });
    }
    let prior = driver.send_prompt(node_id, prompt)?;
    let verdict = driver.verify_delivery(prior);
    store.record(node_id, idempotency_key, verdict);
    Ok(DriveOutcome { verdict, replayed: false })
}

/// Production [`IdempotencyStore`] backed by the `coordinator_drive_prompts`
/// table.
struct DbIdempotencyStore;

impl IdempotencyStore for DbIdempotencyStore {
    fn lookup(&self, node_id: i64, key: &str) -> Result<Option<Verdict>, DriveError> {
        match crate::db::lookup_drive_prompt_verdict(node_id, key) {
            Ok(None) => Ok(None),
            // A row exists but its verdict string is unreadable (tampered/corrupt):
            // we still know a send happened, so fail safe rather than re-deliver.
            Ok(Some(s)) => Verdict::from_db_str(&s).map(Some).ok_or_else(|| {
                DriveError::LedgerUnavailable(format!("unreadable verdict {s:?}"))
            }),
            Err(e) => Err(DriveError::LedgerUnavailable(e.to_string())),
        }
    }

    fn record(&self, node_id: i64, key: &str, verdict: Verdict) {
        // Best-effort: a failed ledger write must not fail a prompt that already
        // landed. The cost is that a retry of *this* key would then re-send —
        // acceptable versus reporting failure for a delivered prompt. Logged so
        // the (rare) "delivered but not recorded" window is observable.
        if let Err(e) = crate::db::record_drive_prompt_verdict(node_id, key, verdict.as_db_str()) {
            tracing::warn!(
                node_id,
                error = %e,
                "failed to record coordinator drive idempotency key; a retry may re-send"
            );
        }
    }
}

/// Drive a live node idempotently: write `prompt` to its PTY through the
/// [`AgentDriver`] under a caller-supplied `idempotency_key`, replaying the
/// original verdict on a duplicate key. The single production drive path
/// (issues #319 + #320) — the route is a thin transport skin over this.
pub fn drive_node_with_key(
    node_id: i64,
    idempotency_key: &str,
    prompt: &str,
) -> Result<DriveOutcome, DriveError> {
    let driver = PtyDriver::new(RegistryTarget);
    drive_node_idempotent(&DbIdempotencyStore, &driver, node_id, idempotency_key, prompt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeTarget {
        live: bool,
        status: Option<SessionStatus>,
        plain_terminal: bool,
        fail_write: bool,
        writes: RefCell<Vec<(i64, String)>>,
        cleared: RefCell<Vec<i64>>,
    }

    impl DriveTarget for FakeTarget {
        fn is_live(&self, _node_id: i64) -> bool {
            self.live
        }
        fn status(&self, _node_id: i64) -> Option<SessionStatus> {
            self.status
        }
        fn is_plain_terminal(&self, _node_id: i64) -> bool {
            self.plain_terminal
        }
        fn write_prompt(&self, node_id: i64, payload: &str) -> Result<(), String> {
            if self.fail_write {
                return Err("pty gone".to_string());
            }
            self.writes.borrow_mut().push((node_id, payload.to_string()));
            Ok(())
        }
        fn clear_attention(&self, node_id: i64) {
            self.cleared.borrow_mut().push(node_id);
        }
    }

    fn driver_with(target: FakeTarget) -> PtyDriver<FakeTarget> {
        PtyDriver::new(target)
    }

    /// The verdict-mapping AC: `awaiting → cleared = Delivered`,
    /// `running-with-no-transition = Unverified`. Idle is likewise unverifiable.
    #[test]
    fn verdict_maps_prior_status() {
        let driver = driver_with(FakeTarget::default());
        assert_eq!(
            driver.verify_delivery(SessionStatus::AwaitingInput),
            Verdict::Delivered
        );
        assert_eq!(
            driver.verify_delivery(SessionStatus::Running),
            Verdict::Unverified
        );
        assert_eq!(
            driver.verify_delivery(SessionStatus::Idle),
            Verdict::Unverified
        );
    }

    /// End-to-end through the driver: an `awaiting_input` live node yields
    /// `Delivered` and the prompt is written newline-terminated.
    #[test]
    fn awaiting_node_is_delivered_and_written() {
        let driver = driver_with(FakeTarget {
            live: true,
            status: Some(SessionStatus::AwaitingInput),
            ..Default::default()
        });
        let prior = driver.send_prompt(7, "work on issue 23").unwrap();
        assert_eq!(prior, SessionStatus::AwaitingInput);
        assert_eq!(driver.verify_delivery(prior), Verdict::Delivered);
        assert_eq!(
            *driver.target.writes.borrow(),
            vec![(7, "work on issue 23\n".to_string())],
            "the prompt is submitted with a trailing newline"
        );
        assert_eq!(
            *driver.target.cleared.borrow(),
            vec![7],
            "attention is cleared after the write"
        );
    }

    /// A busy (`running`) live node still accepts the prompt — Claude Code
    /// queues stdin — but the verdict is the honest `Unverified`.
    #[test]
    fn running_node_is_unverified_but_written() {
        let driver = driver_with(FakeTarget {
            live: true,
            status: Some(SessionStatus::Running),
            ..Default::default()
        });
        let prior = driver.send_prompt(9, "follow-up").unwrap();
        assert_eq!(driver.verify_delivery(prior), Verdict::Unverified);
        assert_eq!(driver.target.writes.borrow().len(), 1);
    }

    /// Driving a plain-terminal (shell) node writes the prompt but must NOT clear
    /// attention — a shell has no LLM attention state, and flipping it to Running
    /// paints a spurious badge (mirrors the desktop write path's guard).
    #[test]
    fn plain_terminal_node_is_written_but_attention_not_cleared() {
        let driver = driver_with(FakeTarget {
            live: true,
            status: Some(SessionStatus::Idle),
            plain_terminal: true,
            ..Default::default()
        });
        driver.send_prompt(5, "ls -la").unwrap();
        assert_eq!(
            driver.target.writes.borrow().len(),
            1,
            "the prompt is still written to the shell's PTY"
        );
        assert!(
            driver.target.cleared.borrow().is_empty(),
            "a plain terminal's status is never flipped on a write"
        );
    }

    /// A non-live node is rejected before any write — the "clear error" AC.
    #[test]
    fn non_live_node_is_rejected_without_writing() {
        let driver = driver_with(FakeTarget {
            live: false,
            status: Some(SessionStatus::Idle),
            ..Default::default()
        });
        assert_eq!(driver.send_prompt(3, "hi"), Err(DriveError::NotLive));
        assert!(
            driver.target.writes.borrow().is_empty(),
            "no write is attempted on a non-live node"
        );
        assert!(driver.target.cleared.borrow().is_empty());
    }

    /// A PTY write failure surfaces as `WriteFailed`, and attention is NOT
    /// cleared (the prompt never landed).
    #[test]
    fn write_failure_surfaces_and_skips_clear() {
        let driver = driver_with(FakeTarget {
            live: true,
            status: Some(SessionStatus::AwaitingInput),
            fail_write: true,
            ..Default::default()
        });
        assert_eq!(
            driver.send_prompt(1, "hi"),
            Err(DriveError::WriteFailed("pty gone".to_string()))
        );
        assert!(driver.target.cleared.borrow().is_empty());
    }

    /// The verdict serializes to the lowercase wire form the response uses.
    #[test]
    fn verdict_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&Verdict::Delivered).unwrap(), "\"delivered\"");
        assert_eq!(
            serde_json::to_string(&Verdict::Unverified).unwrap(),
            "\"unverified\""
        );
    }

    /// The DB string form round-trips and matches the wire form, so a replayed
    /// verdict is byte-identical to the original response.
    #[test]
    fn verdict_db_string_round_trips() {
        for v in [Verdict::Delivered, Verdict::Unverified] {
            assert_eq!(Verdict::from_db_str(v.as_db_str()), Some(v));
        }
        assert_eq!(Verdict::Delivered.as_db_str(), "delivered");
        assert_eq!(Verdict::Unverified.as_db_str(), "unverified");
        // A tampered / unknown row is rejected, not silently mapped.
        assert_eq!(Verdict::from_db_str("bogus"), None);
    }

    // --- Idempotency layer (issue #320) ---

    /// In-memory [`IdempotencyStore`] scoped by `(node_id, key)`, mirroring the
    /// DB ledger's primary key without a real database. `fail_lookup` simulates an
    /// unreadable ledger (the fail-safe path).
    #[derive(Default)]
    struct FakeStore {
        recorded: RefCell<Vec<(i64, String, Verdict)>>,
        fail_lookup: bool,
    }

    impl IdempotencyStore for FakeStore {
        fn lookup(&self, node_id: i64, key: &str) -> Result<Option<Verdict>, DriveError> {
            if self.fail_lookup {
                return Err(DriveError::LedgerUnavailable("boom".to_string()));
            }
            Ok(self
                .recorded
                .borrow()
                .iter()
                .find(|(n, k, _)| *n == node_id && k == key)
                .map(|(_, _, v)| *v))
        }
        fn record(&self, node_id: i64, key: &str, verdict: Verdict) {
            // First write wins, exactly like the DB's `INSERT OR IGNORE`.
            if self.lookup(node_id, key).ok().flatten().is_none() {
                self.recorded
                    .borrow_mut()
                    .push((node_id, key.to_string(), verdict));
            }
        }
    }

    fn awaiting_driver() -> PtyDriver<FakeTarget> {
        driver_with(FakeTarget {
            live: true,
            status: Some(SessionStatus::AwaitingInput),
            ..Default::default()
        })
    }

    /// THE headline test (ADR-0008 §6): the same key twice produces exactly one
    /// delivery to the agent, and the second call replays the original verdict.
    #[test]
    fn same_key_twice_delivers_once_and_replays_verdict() {
        let store = FakeStore::default();
        let driver = awaiting_driver();

        let first =
            drive_node_idempotent(&store, &driver, 7, "key-abc", "work on issue 23").unwrap();
        assert_eq!(first, DriveOutcome { verdict: Verdict::Delivered, replayed: false });

        // Retry with the identical key: a no-op that returns the original verdict.
        let second =
            drive_node_idempotent(&store, &driver, 7, "key-abc", "work on issue 23").unwrap();
        assert_eq!(second, DriveOutcome { verdict: Verdict::Delivered, replayed: true });

        // Exactly one write reached the PTY — the prompt never landed twice.
        assert_eq!(
            driver.target.writes.borrow().len(),
            1,
            "a duplicate key must not send the prompt a second time"
        );
    }

    /// A distinct key drives again — idempotency dedupes retries, not genuinely
    /// new prompts.
    #[test]
    fn a_new_key_drives_again() {
        let store = FakeStore::default();
        let driver = awaiting_driver();

        drive_node_idempotent(&store, &driver, 7, "key-1", "first").unwrap();
        let second = drive_node_idempotent(&store, &driver, 7, "key-2", "second").unwrap();

        assert!(!second.replayed);
        assert_eq!(driver.target.writes.borrow().len(), 2);
    }

    /// The same key on a *different* node is a fresh operation: scoping by node
    /// means an accidentally-reused key still drives each node once, rather than
    /// silently returning the other node's verdict.
    #[test]
    fn same_key_different_node_drives_each_once() {
        let store = FakeStore::default();
        let driver = awaiting_driver();

        drive_node_idempotent(&store, &driver, 7, "shared", "p").unwrap();
        let other = drive_node_idempotent(&store, &driver, 9, "shared", "p").unwrap();

        assert!(!other.replayed, "a new node is not a replay");
        let writes = driver.target.writes.borrow();
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].0, 7);
        assert_eq!(writes[1].0, 9);
    }

    /// An `Unverified` (written-but-unconfirmed) send is recorded and replayed —
    /// re-sending it is the double-delivery #178 forbids, so the honest answer is
    /// to return the original `Unverified` verdict, not try again.
    #[test]
    fn unverified_send_is_recorded_and_replayed() {
        let store = FakeStore::default();
        let driver = driver_with(FakeTarget {
            live: true,
            status: Some(SessionStatus::Running),
            ..Default::default()
        });

        let first = drive_node_idempotent(&store, &driver, 3, "k", "follow-up").unwrap();
        assert_eq!(first.verdict, Verdict::Unverified);
        let second = drive_node_idempotent(&store, &driver, 3, "k", "follow-up").unwrap();
        assert_eq!(second, DriveOutcome { verdict: Verdict::Unverified, replayed: true });
        assert_eq!(driver.target.writes.borrow().len(), 1);
    }

    /// A failed drive records nothing, so a retry genuinely re-attempts rather
    /// than replaying a phantom verdict for a prompt that never landed.
    #[test]
    fn failed_drive_is_not_recorded() {
        let store = FakeStore::default();
        let driver = driver_with(FakeTarget {
            live: false,
            status: Some(SessionStatus::Idle),
            ..Default::default()
        });

        assert_eq!(
            drive_node_idempotent(&store, &driver, 1, "k", "hi"),
            Err(DriveError::NotLive)
        );
        assert!(
            store.recorded.borrow().is_empty(),
            "a non-live drive must not populate the ledger"
        );
    }

    /// Fail SAFE, not open: if the ledger can't be read, the drive aborts with
    /// `LedgerUnavailable` and the prompt is NEVER sent — we refuse to risk a
    /// second delivery we can't rule out (issue #320 review).
    #[test]
    fn unreadable_ledger_aborts_without_sending() {
        let store = FakeStore { fail_lookup: true, ..Default::default() };
        let driver = awaiting_driver();

        let result = drive_node_idempotent(&store, &driver, 7, "k", "work on issue 23");
        assert!(matches!(result, Err(DriveError::LedgerUnavailable(_))));
        assert!(
            driver.target.writes.borrow().is_empty(),
            "a prompt must not be sent when idempotency cannot be verified"
        );
    }
}
