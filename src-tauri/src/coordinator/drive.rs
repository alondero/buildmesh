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
use std::time::{Duration, Instant};

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
    #[allow(dead_code)] // used by the in-memory `FakeStore` in tests; the
                       // production `DbIdempotencyStore` goes through
                       // [`Verdict::as_verdict_str`] instead.
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

    /// Wrap the verdict in the typed [`crate::db::VerdictStr`] the DB layer
    /// uses for `finalize_drive_prompt_inner`. Keeps the call site clean and
    /// lets the DB layer evolve the column split without touching every
    /// caller.
    pub fn as_verdict_str(self) -> crate::db::VerdictStr<'static> {
        match self {
            Verdict::Delivered => crate::db::VerdictStr::Delivered,
            Verdict::Unverified => crate::db::VerdictStr::Unverified,
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
    /// Same idempotency key, *different* prompt payload — Stripe-style reject
    /// (issue #750, item 2). The route surfaces this as `409 Conflict` with
    /// `error: key_payload_mismatch`; the Coordinator should mint a fresh key
    /// rather than re-send a silently-dropped prompt.
    KeyPayloadMismatch,
    /// Another caller is currently driving this key (a `pending` row exists
    /// in the ledger). The orchestrator waited up to
    /// [`IN_PROGRESS_WAIT_TIMEOUT`] for the peer to finalize; the route
    /// surfaces this as `409 Conflict` with `Retry-After: 1`.
    InProgress,
}

/// Maximum time `drive_node_idempotent` will briefly wait for a peer holding
/// a `pending` claim to finalize before surfacing `DriveError::InProgress` to
/// the caller. 5 s is generous enough that a Coordinator's network-retry
/// collision resolves within one request handler (no caller-visible retry),
/// short enough that a peer stuck in a long drive doesn't pin the request
/// handler for too long. Tuned in tests via [`in_progress_poll_interval`].
pub const IN_PROGRESS_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// How often `drive_node_idempotent` re-checks the ledger while waiting on a
/// peer's `pending` claim to resolve. 50 ms is short enough to catch a
/// finalize within a few poll ticks yet slow enough that the wait loop
/// doesn't beat on the DB mutex.
pub fn in_progress_poll_interval(_idle: Duration) -> Duration {
    Duration::from_millis(50)
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
        // coordinator just drove. Status write + desktop emit route through
        // SessionLifecycle (issue #132); the mobile broadcast is a separate
        // channel kept here.
        crate::attention_autoclear::disarm(node_id);
        if let Some(app) = crate::http::app_handle() {
            let sink = crate::agent::session_lifecycle::AppSessionLifecycleSink { app };
            let _ = crate::agent::session_lifecycle::on_attention_cleared(&sink, node_id);
        } else {
            let _ = crate::agent::session_lifecycle::on_attention_cleared(
                &crate::agent::session_lifecycle::DbOnlySink,
                node_id,
            );
        }
        crate::http::events::emit(crate::http::events::EventMsg::AttentionCleared {
            session_id: node_id,
        });
    }
}

/// The idempotency ledger seam (issues #320 + #750). Remembers the verdict a
/// drive produced under a caller-supplied key, scoped to the node it drove, so
/// a retry replays rather than re-sends. Behind a trait so the orchestrator
/// is unit-testable without the process-global DB. Production wiring is
/// [`DbIdempotencyStore`].
///
/// v32 (issue #750) reshaped this from `lookup` + `record` to
/// `claim` + `finalize` + `release_claim` to close the concurrent-retry
/// double-send gap: a peer holding a `pending` claim short-circuits the second
/// caller's send instead of both racing through to the PTY.
pub trait IdempotencyStore {
    /// Atomically claim `(node_id, key)` for a drive with the given
    /// `prompt_hash`. Returns [`ClaimOutcome::Claimed`] (this caller owns the
    /// slot — proceed to drive), [`ClaimOutcome::Replay`] (a finalized peer
    /// row exists with the same prompt — return its verdict),
    /// [`ClaimOutcome::Mismatch`] (a finalized peer row exists with a
    /// *different* prompt — surface as 409), or [`ClaimOutcome::InProgress`]
    /// (a peer holds the slot in `pending` — the orchestrator will briefly
    /// wait for finalize before surfacing as 409).
    ///
    /// `Err` is propagated, never swallowed: a genuine read failure must not
    /// be mistaken for "key never seen" (fail-safe contract from issue #320
    /// review).
    fn claim(&self, node_id: i64, key: &str, prompt_hash: &str) -> Result<ClaimOutcome, DriveError>;

    /// Finalize a claim: UPDATE the `pending` row to its terminal status +
    /// verdict. Best-effort: a failed finalize leaves the row `pending` and
    /// a future claim will reclaim it after [`crate::db::PENDING_CLAIM_TIMEOUT_SECS`].
    fn finalize(&self, node_id: i64, key: &str, verdict: Verdict);

    /// Release a claim when the drive itself failed (so a retry can re-attempt
    /// rather than wait on a `pending` row the orchestrator never finalized).
    /// Only deletes `pending` rows — a finalized row stays put.
    fn release_claim(&self, node_id: i64, key: &str);
}

/// The outcome of an atomic claim attempt (issue #750, item 1). Mirrors
/// [`crate::db::ClaimOutcome`] but in this module's vocabulary so the
/// orchestrator can map it to [`DriveOutcome`] / [`DriveError`] without
/// importing `rusqlite`. The DB seam returns [`crate::db::ClaimOutcome`];
/// the production store converts it to this enum on its way out.
#[derive(Debug, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// This caller inserted the pending row; proceed to drive.
    Claimed,
    /// The row was already finalized with the same prompt; replay its verdict.
    Replay(Verdict),
    /// The row was already finalized but with a *different* prompt; reject.
    Mismatch,
    /// The row is `pending` — another caller is currently driving this key.
    InProgress,
}

/// Drive a node idempotently: claim-before-send so two concurrent same-key
/// requests cannot both write to the PTY (issue #750, item 1). On `Claimed`
/// the orchestrator sends the prompt via [`AgentDriver::send_prompt`] then
/// finalizes the row with the verdict. On `Replay` it returns the
/// peer-recorded verdict without touching the PTY. On `Mismatch` (same key +
/// different prompt) it surfaces `KeyPayloadMismatch` (issue #750, item 2).
/// On `InProgress` (peer holds a `pending` row) it briefly polls for the
/// peer to finalize, up to [`IN_PROGRESS_WAIT_TIMEOUT`], before surfacing
/// `InProgress` to the route.
///
/// Recording happens only *after* a successful send: a `NotLive` /
/// `WriteFailed` drive calls [`IdempotencyStore::release_claim`] so a genuine
/// failure is retried rather than cached, while a `Delivered` or `Unverified`
/// send finalizes the row — re-sending it is the double-delivery #178
/// forbids.
///
/// `drive_node_idempotent` is the pure orchestrator: no I/O of its own, just
/// the `(store, driver) -> outcome` decision tree, so the
/// "same-key-different-payload is a 409, not a silent replay" and
/// "concurrent same-key call = exactly one delivery" contracts are testable
/// against fakes.
pub fn drive_node_idempotent<S: IdempotencyStore, D: AgentDriver>(
    store: &S,
    driver: &D,
    node_id: i64,
    idempotency_key: &str,
    prompt: &str,
) -> Result<DriveOutcome, DriveError> {
    let prompt_hash = crate::db::hash_token(prompt);

    // The InProgress wait loop: poll the ledger for the peer's finalize, but
    // give up after `IN_PROGRESS_WAIT_TIMEOUT` so a stuck peer doesn't pin
    // the request handler forever.
    let wait_started = Instant::now();
    loop {
        match store.claim(node_id, idempotency_key, &prompt_hash)? {
            ClaimOutcome::Replay(verdict) => {
                return Ok(DriveOutcome { verdict, replayed: true });
            }
            ClaimOutcome::Mismatch => return Err(DriveError::KeyPayloadMismatch),
            ClaimOutcome::InProgress => {
                if wait_started.elapsed() >= IN_PROGRESS_WAIT_TIMEOUT {
                    return Err(DriveError::InProgress);
                }
                std::thread::sleep(in_progress_poll_interval(wait_started.elapsed()));
                continue;
            }
            ClaimOutcome::Claimed => break,
        }
    }

    // We won the race. Send the prompt; release the claim on a drive-side
    // failure so a retry can re-attempt rather than wait for the orphan
    // timeout.
    match driver.send_prompt(node_id, prompt) {
        Ok(prior) => {
            let verdict = driver.verify_delivery(prior);
            store.finalize(node_id, idempotency_key, verdict);
            Ok(DriveOutcome { verdict, replayed: false })
        }
        Err(e) => {
            store.release_claim(node_id, idempotency_key);
            Err(e)
        }
    }
}

/// Production [`IdempotencyStore`] backed by the `coordinator_drive_prompts`
/// table.
struct DbIdempotencyStore;

impl IdempotencyStore for DbIdempotencyStore {
    fn claim(&self, node_id: i64, key: &str, prompt_hash: &str) -> Result<ClaimOutcome, DriveError> {
        // Same fail-safe contract as the pre-#750 `lookup`: a genuine DB
        // error propagates as `LedgerUnavailable`, never as a "key never
        // seen" that would re-deliver.
        match crate::db::claim_drive_prompt(node_id, key, prompt_hash) {
            Ok(crate::db::ClaimOutcome::Claimed) => Ok(ClaimOutcome::Claimed),
            Ok(crate::db::ClaimOutcome::Replay { verdict }) => {
                // A tampered/unknown verdict string is `None` (the DB seam
                // filters it to `Mismatch`); mirror that here as
                // `LedgerUnavailable` — we refuse to replay garbage, and
                // also refuse to silently retry.
                match Verdict::from_db_str(&verdict) {
                    Some(v) => Ok(ClaimOutcome::Replay(v)),
                    None => Err(DriveError::LedgerUnavailable(format!(
                        "unreadable verdict {verdict:?}"
                    ))),
                }
            }
            Ok(crate::db::ClaimOutcome::Mismatch) => Ok(ClaimOutcome::Mismatch),
            Ok(crate::db::ClaimOutcome::InProgress) => Ok(ClaimOutcome::InProgress),
            Err(e) => Err(DriveError::LedgerUnavailable(e.to_string())),
        }
    }

    fn finalize(&self, node_id: i64, key: &str, verdict: Verdict) {
        // Best-effort: a failed finalize leaves the row `pending` and the
        // orphan-recovery pass will reclaim it after
        // [`crate::db::PENDING_CLAIM_TIMEOUT_SECS`]. Logged so the rare
        // "delivered but not finalized" window is observable.
        if let Err(e) = crate::db::finalize_drive_prompt(node_id, key, verdict.as_verdict_str()) {
            tracing::warn!(
                node_id,
                error = %e,
                "failed to finalize coordinator drive claim; a retry may reclaim after the pending timeout"
            );
        }
    }

    fn release_claim(&self, node_id: i64, key: &str) {
        // Best-effort: a failed release leaves the row `pending` and the
        // orphan-recovery pass will reclaim it. The drive itself already
        // surfaced its `NotLive` / `WriteFailed` to the caller, so failing
        // here too would double-report.
        if let Err(e) = crate::db::release_drive_prompt_claim(node_id, key) {
            tracing::warn!(
                node_id,
                error = %e,
                "failed to release coordinator drive claim; orphan-recovery will reclaim after the pending timeout"
            );
        }
    }
}

/// Drive a live node idempotently: write `prompt` to its PTY through the
/// [`AgentDriver`] under a caller-supplied `idempotency_key`, replaying the
/// original verdict on a duplicate key. The single production drive path
/// (issues #319 + #320 + #750) — the route is a thin transport skin over this.
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

    // --- Idempotency layer (issues #320 + #750) ---

    /// In-memory [`IdempotencyStore`] mirroring the DB ledger's
    /// claim/finalize/release protocol. `hold_pending` keeps the row in
    /// `pending` (simulates a peer mid-send); `fail_claim` simulates an
    /// unreadable ledger (the fail-safe path).
    #[derive(Default)]
    struct FakeStore {
        /// `(node_id, key, prompt_hash, status)`. Terminal rows carry
        /// `status="delivered"|"unverified"`; mid-send rows use
        /// `status="pending"`.
        recorded: RefCell<Vec<(i64, String, String, String)>>,
        /// When true, `claim` keeps returning `InProgress` for any
        /// pre-existing `pending` row even after time passes — used by the
        /// wait-timeout test to model a peer that never finalizes.
        hold_pending: bool,
        /// When true, `claim` always errors with `LedgerUnavailable` —
        /// models a DB read failure (the fail-safe path).
        fail_claim: bool,
    }

    impl FakeStore {
        fn lookup(&self, node_id: i64, key: &str) -> Option<(String, String)> {
            self.recorded
                .borrow()
                .iter()
                .find(|(n, k, _, _)| *n == node_id && k == key)
                .map(|(_, _, hash, status)| (hash.clone(), status.clone()))
        }
    }

    impl IdempotencyStore for FakeStore {
        fn claim(
            &self,
            node_id: i64,
            key: &str,
            prompt_hash: &str,
        ) -> Result<ClaimOutcome, DriveError> {
            if self.fail_claim {
                return Err(DriveError::LedgerUnavailable("boom".to_string()));
            }
            match self.lookup(node_id, key) {
                None => {
                    // No row yet — claim it.
                    self.recorded.borrow_mut().push((
                        node_id,
                        key.to_string(),
                        prompt_hash.to_string(),
                        "pending".to_string(),
                    ));
                    Ok(ClaimOutcome::Claimed)
                }
                Some((_, status)) if status == "pending" => Ok(ClaimOutcome::InProgress),
                Some((stored_hash, status)) if stored_hash == prompt_hash => {
                    // Same key + same prompt → replay.
                    match status.as_str() {
                        "delivered" => Ok(ClaimOutcome::Replay(Verdict::Delivered)),
                        "unverified" => Ok(ClaimOutcome::Replay(Verdict::Unverified)),
                        // An unknown terminal status string is treated as a
                        // tamper-and-mismatch (same as the production store):
                        // a garbage verdict must never be replayed.
                        _ => Ok(ClaimOutcome::Mismatch),
                    }
                }
                Some(_) => Ok(ClaimOutcome::Mismatch),
            }
        }

        fn finalize(&self, node_id: i64, key: &str, verdict: Verdict) {
            let mut rows = self.recorded.borrow_mut();
            if let Some(row) = rows
                .iter_mut()
                .find(|(n, k, _, _)| *n == node_id && k == key)
            {
                row.3 = verdict.as_db_str().to_string();
            }
        }

        fn release_claim(&self, node_id: i64, key: &str) {
            self.recorded
                .borrow_mut()
                .retain(|(n, k, _, status)| !(n == &node_id && k == key && status == "pending"));
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

    /// A failed drive releases the claim, so a retry genuinely re-attempts
    /// rather than replaying a phantom verdict for a prompt that never
    /// landed. (Issue #750 reshape: `release_claim` replaces the
    /// "leave no row" pre-#750 behaviour with an explicit DELETE.)
    #[test]
    fn failed_drive_releases_claim_for_retry() {
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
            "a non-live drive must not leave a pending row behind"
        );
    }

    /// Fail SAFE, not open: if the ledger can't be read, the drive aborts with
    /// `LedgerUnavailable` and the prompt is NEVER sent — we refuse to risk a
    /// second delivery we can't rule out (issue #320 review).
    #[test]
    fn unreadable_ledger_aborts_without_sending() {
        let store = FakeStore { fail_claim: true, ..Default::default() };
        let driver = awaiting_driver();

        let result = drive_node_idempotent(&store, &driver, 7, "k", "work on issue 23");
        assert!(matches!(result, Err(DriveError::LedgerUnavailable(_))));
        assert!(
            driver.target.writes.borrow().is_empty(),
            "a prompt must not be sent when idempotency cannot be verified"
        );
    }

    // --- Issue #750 hardening ---

    /// Item 2: a finalized row with a *different* prompt is Mismatch → 409,
    /// not a silent 200 replay of the original verdict. Stripe-style reject.
    #[test]
    fn same_key_different_prompt_returns_mismatch_error() {
        let store = FakeStore::default();
        let driver = awaiting_driver();

        // First call: prompt "v1" lands.
        drive_node_idempotent(&store, &driver, 7, "k", "v1").unwrap();
        // Retry with the same key but a *different* prompt.
        let result = drive_node_idempotent(&store, &driver, 7, "k", "v2");
        assert_eq!(result, Err(DriveError::KeyPayloadMismatch));
        // Exactly one write reached the PTY — v2 was rejected, not silently
        // accepted (and not silently ignored — a 200 with `replayed:true`).
        assert_eq!(driver.target.writes.borrow().len(), 1);
    }

    /// Item 1: the orchestrator briefly waits for a peer holding a `pending`
    /// row to finalize before giving up. The FakeStore's `hold_pending`
    /// models the peer's mid-send window. This test exercises the wait
    /// path's semantics: `claim` returns `InProgress` while pending, then
    /// `Replay` once we finalize.
    #[test]
    fn claim_returns_in_progress_until_peer_finalizes() {
        let store = FakeStore { hold_pending: true, ..Default::default() };
        // Pre-populate a pending row to simulate a peer mid-send.
        store
            .recorded
            .borrow_mut()
            .push((7, "k".to_string(), crate::db::hash_token("v1"), "pending".to_string()));

        // While the row is pending, the second caller sees InProgress.
        assert_eq!(
            store.claim(7, "k", &crate::db::hash_token("v1")).unwrap(),
            ClaimOutcome::InProgress
        );

        // Once the peer finalizes, the second caller sees Replay.
        store.finalize(7, "k", Verdict::Delivered);
        assert_eq!(
            store.claim(7, "k", &crate::db::hash_token("v1")).unwrap(),
            ClaimOutcome::Replay(Verdict::Delivered)
        );
    }

    /// Item 1: when a peer holds a `pending` row and never finalizes, the
    /// orchestrator surfaces `InProgress` rather than waiting forever. We
    /// drive the full orchestrator (not just the store) and assert the
    /// timeout fires. Real wall-clock wait is bounded by
    /// `IN_PROGRESS_WAIT_TIMEOUT`; this test runs that long once but does
    /// not exceed it.
    #[test]
    #[ignore = "exercises the real IN_PROGRESS_WAIT_TIMEOUT; run with `cargo test -- --ignored`"]
    fn in_progress_surfaces_after_wait_timeout() {
        let store = FakeStore { hold_pending: true, ..Default::default() };
        let driver = awaiting_driver();

        // Pre-populate a pending row.
        store
            .recorded
            .borrow_mut()
            .push((7, "k".to_string(), crate::db::hash_token("v1"), "pending".to_string()));

        let start = Instant::now();
        let result = drive_node_idempotent(&store, &driver, 7, "k", "v1");
        let elapsed = start.elapsed();
        assert_eq!(result, Err(DriveError::InProgress));
        assert!(
            elapsed >= IN_PROGRESS_WAIT_TIMEOUT,
            "the orchestrator must wait at least IN_PROGRESS_WAIT_TIMEOUT before giving up (waited {elapsed:?})"
        );
        assert!(
            elapsed < IN_PROGRESS_WAIT_TIMEOUT + Duration::from_millis(500),
            "the orchestrator must give up shortly after IN_PROGRESS_WAIT_TIMEOUT (waited {elapsed:?})"
        );
        // No write reached the PTY — the second caller refused to send
        // while a peer was mid-send.
        assert!(driver.target.writes.borrow().is_empty());
    }

    /// `release_claim` is a no-op for a finalized row (the row stays put so
    /// a retry sees the recorded verdict rather than silently re-driving).
    /// This pins the contract so a future refactor can't accidentally
    /// broaden it to "delete any row".
    #[test]
    fn release_claim_leaves_finalized_rows_intact() {
        let store = FakeStore::default();
        let driver = awaiting_driver();

        // Drive to a finalized state.
        drive_node_idempotent(&store, &driver, 1, "k", "hi").unwrap();
        assert_eq!(store.recorded.borrow().len(), 1);
        assert_eq!(store.recorded.borrow()[0].3, "delivered");

        // A release on the finalized row should be a no-op (the row's status
        // is no longer `pending`, so the DELETE filter rejects it).
        store.release_claim(1, "k");
        assert_eq!(store.recorded.borrow().len(), 1);
        assert_eq!(store.recorded.borrow()[0].3, "delivered");
    }
}