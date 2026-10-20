//! Typed errors for the `db` layer (issue #2448).
//!
//! This is the *pilot* of the backend-wide error retrofit. `Result<_, String>`
//! made callers branch on error kinds by substring-matching prose somebody
//! spelled by hand (#2446 exists because those strings got scrambled), made
//! production error paths impossible to exercise without asserting on that
//! prose, and flattened every `?` bubble into an unstructured story. The
//! conversion slices that follow copy the pattern established here.
//!
//! # The frozen boundary
//!
//! Every variant below renders byte-identically to the `String` it replaced.
//!
//! Tauri command handlers serialize an error as a plain `String`, so the text
//! *is* what the user is shown — it is user-visible copy, and a refactor that
//! rewords it changes the product. The `tests` module is the pin: it copies the
//! exact phrasing that shipped, so "improve the wording" cannot ride along. A
//! new message is a behaviour change and belongs in its own slice.
//!
//! What the freeze is *not*: a hidden frontend contract. Spot-checking these
//! messages across `src/` and `tests/` during the conversion found no
//! TypeScript that branches on them, so the consumers are the person reading
//! the error and the Rust tests in `db/`. Re-check that before assuming a later
//! slice can reword freely — and before assuming it cannot, either.
//!
//! # What callers outside `db/` see
//!
//! [`impl From<DbError> for String`] is deliberate: it renders through
//! [`std::fmt::Display`], so every `?` at a `Result<_, String>` Tauri command
//! boundary keeps working untouched and produces exactly the text it produced
//! when the db layer returned `String`. That is why the conversion is almost
//! entirely inside `db/` — outside it, only the handful of sites that *spell out*
//! the error type (a `run_blocking` closure, a `map_err` into a typed wrapper)
//! needed an edit, and each of those is a type change, not a behaviour one. A
//! caller that *wants* the kind must ask for it (a `?` into a `DbError`-returning
//! function, or an explicit `match`) — that opt-in is the whole point.
//!
//! # Why there is no `PoolExhausted` or `CorruptSchema` variant
//!
//! Issue #2448 sketched both. Neither has a producer in the converted files,
//! and a variant nothing constructs cannot be exercised — a paper tiger.
//! `CorruptSchema` still does not: `db/migrations.rs` repairs and reports
//! schema drift, and it stays a `rusqlite::Error`.
//!
//! * **Pool exhaustion** — resolved in issue #1533, which is the slice that
//!   owns the condition. `ReaderPool::checkout` in `db/mod.rs` gives up after
//!   `READER_CHECKOUT_TIMEOUT` and returns SQLite's own `SQLITE_BUSY` code,
//!   so the condition is typed rather than prose and the caller can branch on
//!   it with [`is_busy_error`]. It stays a `rusqlite::Error` because
//!   `db/mod.rs` and its domain modules report `SqlResult`; a dedicated
//!   variant would need the `SqlResult`-reporting accessors to change error
//!   type, which is a different slice from the one that fixed the crash. A
//!   `DbError` holder reaches the same answer through its transparent `Sql`
//!   variant.
//!
//! The rendering of the *other* frozen messages is untouched: only the
//! reader-pool timeout changed, because "Invalid query" was never a truthful
//! description of it and the old copy is what made the failure look like a
//! bug in the query rather than a busy database.

use rusqlite::Error as SqlError;

/// Whether a raw SQLite error is the reader pool reporting itself busy.
///
/// The reader pool's checkout deadline expires as SQLite's `SQLITE_BUSY`
/// code (issue #1533), so this is a *typed* check rather than a match on
/// prose: a caller asks "is the database busy?" instead of asking whether an
/// error message happens to contain a word.
pub fn is_busy_error(error: &SqlError) -> bool {
    error
        .sqlite_error_code()
        .is_some_and(|code| code == rusqlite::ffi::ErrorCode::DatabaseBusy)
}

/// The error a reader-pool checkout reports when no reader was free by the
/// deadline.
///
/// SQLite's own `SQLITE_BUSY` code rather than an invented one: "the database
/// is busy" is exactly what happened, and the code is what lets a caller
/// branch on the condition ([`is_busy_error`]) instead of matching on prose.
/// The message is the user-facing copy, and it says a retry may work.
pub fn busy_error() -> SqlError {
    SqlError::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
        Some(crate::db::READER_POOL_BUSY_MESSAGE.to_string()),
    )
}

/// Why a db-layer operation failed.
///
/// Every variant names the condition it carries, so a caller can branch on the
/// *kind* instead of substring-matching the rendered message. Rendering is
/// frozen to today's strings — see the module docs.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// SQLite itself failed: a bad statement, a constraint violation, a lock,
    /// an I/O failure. Produced by every `?` in the db layer now that `From` is
    /// implemented, which is why the 163 `.map_err(|e| e.to_string())` closures
    /// this slice deleted were all rendering exactly this text. Also carries
    /// the db layer's borrowed `rusqlite::Error` sentinels — see the module docs
    /// on pool exhaustion.
    #[error(transparent)]
    Sql(#[from] SqlError),

    /// A JSON column would not encode or decode. The db layer round-trips
    /// `agent_nodes.spawn_configuration`, circuit run `context_json`, circuit
    /// and review graph JSON, and pinned launch snapshots through `serde_json`;
    /// this is the encode/decode failure.
    #[error(transparent)]
    Json(#[from] serde_json::Error),

    /// A producer *outside* the db layer already rendered its own message and
    /// this passes it through untouched — `circuit::compatibility`'s reviewer
    /// refusal message, `circuit::context::CircuitContext::{from_json,to_json}`,
    /// `circuit::model::CircuitGraph::{from_json,validate}`, `preferences::load`,
    /// `preferences::launch_configurations::capture`, and the step-recovery
    /// refusal funnel that promotes a serialized `unavailable_reason` to an
    /// error. Those producers get their own typed enums in their own slices;
    /// carrying the message verbatim here keeps this slice's rendered output
    /// frozen and keeps the `?` bubbles from being re-wrapped in a second layer
    /// of prose.
    ///
    /// Not `#[error(transparent)]`: that requires the inner type to be a
    /// `std::error::Error`, and an already-rendered message has no source to
    /// chain. `{0}` renders it exactly as the producer did.
    #[error("{0}")]
    Rendered(String),

    /// A row the caller named by id or identity is gone. Produced by the
    /// "… no longer exists" guards throughout the circuit ledger: a run
    /// deleted mid-decision, a Circuit removed while a review was being
    /// started, a checkpoint a concurrent action already resolved, an agent
    /// row a `DELETE` had already removed.
    #[error("{message}")]
    NotFound { message: String },

    /// The write lost a race or contradicts current state: the run's evidence
    /// revision moved under the caller, the run went terminal mid-commit, a
    /// checkpoint changed between read and write, an agent is already owned by
    /// an active Circuit run, a review lineage no longer resolves. Their wording
    /// is the most heavily pinned surface in the layer — most of them tell the
    /// user to refresh and act again, so the sentence is the instruction.
    #[error("{message}")]
    Conflict { message: String },

    /// The request is well-formed but cannot be carried out as asked: no
    /// reviewer selected, a Circuit with no name, an effect target on a step
    /// kind that has none, a verdict with no evidence. Carries the full sentence
    /// because that sentence *is* the user-facing explanation.
    #[error("{reason}")]
    InvalidInput { reason: String },
}

impl DbError {
    /// A named row no longer exists. `message` is the frozen wire text.
    pub fn not_found(message: impl Into<String>) -> Self {
        DbError::NotFound {
            message: message.into(),
        }
    }

    /// The caller lost a race, or the write contradicts current state.
    /// `message` is the frozen wire text.
    pub fn conflict(message: impl Into<String>) -> Self {
        DbError::Conflict {
            message: message.into(),
        }
    }

    /// The request cannot be carried out as asked. `reason` is the frozen,
    /// user-facing sentence.
    pub fn invalid(reason: impl Into<String>) -> Self {
        DbError::InvalidInput {
            reason: reason.into(),
        }
    }

    /// The persistence layer was busy — the read may be retried.
    ///
    /// Issue #1533: a reader-pool checkout that found no free reader at its
    /// deadline reports SQLite's `SQLITE_BUSY`, so a caller can tell "come
    /// back in a moment" from "this query is broken" without matching on
    /// prose. The HTTP boundary turns a busy read into `503 Service
    /// Unavailable`; a command turns it into a retryable message.
    pub fn is_busy(&self) -> bool {
        match self {
            DbError::Sql(error) => is_busy_error(error),
            _ => false,
        }
    }
}

/// Carries a message another producer already rendered.
///
/// This is what lets `?` work directly on the `Result<_, String>` producers
/// listed on [`DbError::Rendered`] — including the step-recovery refusal
/// funnel, where the same sentence is serialized into the recovery view and
/// becomes an `Err` on the action path — without each site writing a `map_err`
/// that would re-wrap prose into prose.
impl From<String> for DbError {
    fn from(message: String) -> Self {
        DbError::Rendered(message)
    }
}

/// The frozen command boundary.
///
/// A `?` in a `Result<_, String>` Tauri command (or in `http/`, `services/`,
/// `agent/`) converts a [`DbError`] exactly as it converted the `String` this
/// slice replaced: through `Display`. Callers outside `db/` therefore need no
/// edit, and the user sees byte-identical text.
impl From<DbError> for String {
    fn from(error: DbError) -> Self {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which constructor built each frozen message. Tagging the kind instead of
    /// calling the constructor keeps the table a `const`, so the rows stay short
    /// enough for rustfmt to lay them out.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Kind {
        NotFound,
        Conflict,
        Invalid,
    }

    impl Kind {
        fn build(self, message: &str) -> DbError {
            match self {
                Kind::NotFound => DbError::not_found(message),
                Kind::Conflict => DbError::conflict(message),
                Kind::Invalid => DbError::invalid(message),
            }
        }

        fn matches(self, error: &DbError) -> bool {
            match self {
                Kind::NotFound => matches!(error, DbError::NotFound { .. }),
                Kind::Conflict => matches!(error, DbError::Conflict { .. }),
                Kind::Invalid => matches!(error, DbError::InvalidInput { .. }),
            }
        }
    }

    /// Every message this slice constructs *with* a kind, copied verbatim from
    /// the pre-#2448 `String` errors, grouped by the file that builds it.
    ///
    /// The phrasings are frozen rather than tidied: they are copy the user
    /// reads, and near-duplicates that differ only in punctuation - "Run no
    /// longer exists." against "Run no longer exists" - both ship. Normalising
    /// them here would be a behaviour change disguised as a refactor. Adding or
    /// rewording a row is a user-visible change and needs its own slice.
    const CONSTRUCTED: &[(&str, Kind)] = &[
        // db/circuit/ledger.rs
        ("Launch Configuration no longer exists; select another configuration", Kind::NotFound),
        ("Legacy retirement is still stopping the source node. Retry after cleanup finishes.", Kind::Conflict),
        ("The implementation agent is still being stopped. Try Continue review again in a moment.", Kind::Conflict),
        ("This agent is already controlled by an active Circuit run.", Kind::Conflict),
        ("Resume the agent before starting a review.", Kind::Invalid),
        ("Circuit no longer exists", Kind::NotFound),
        ("Select a manual Circuit from this agent's Mesh.", Kind::Invalid),
        ("Give the Circuit a name.", Kind::Invalid),
        ("Select a built-in Review Blueprint to copy.", Kind::Invalid),
        // db/circuit/evidence.rs
        ("Run no longer exists.", Kind::NotFound),
        ("Run no longer exists", Kind::NotFound),
        ("Give the evidence behind your review decision.", Kind::Invalid),
        ("The run changed. Refresh its evidence before acting.", Kind::Conflict),
        ("Only an active run can resolve a checkpoint.", Kind::Invalid),
        ("This checkpoint changed. Refresh before acting.", Kind::Conflict),
        ("The run changed before the verdict was recorded.", Kind::Conflict),
        ("Resolve outstanding work, input requests and conflicting evidence before recording completion.", Kind::Invalid),
        ("This action has no authoritative automatic evidence recheck. Inspect the external result.", Kind::Invalid),
        ("Circuit run became terminal before evidence recheck commit", Kind::Conflict),
        ("This checkpoint requires authoritative agent evidence; an external-action attestation cannot resolve it.", Kind::Invalid),
        ("Agent identity must be reconciled; attestation cannot establish a spawn attachment or completion.", Kind::Invalid),
        ("Resolve outstanding work, input requests and conflicting evidence before recording an outcome.", Kind::Invalid),
        ("The earlier action must be recorded as not performed before a deliberate retry.", Kind::Invalid),
        ("Circuit run became terminal before operator outcome commit", Kind::Conflict),
        ("Effect target is only supported for an OpenPr step.", Kind::Invalid),
        ("OpenPr dispatch claim changed before its target was recorded.", Kind::Conflict),
        // db/circuit/operator_push.rs
        ("The run changed. Refresh it before acting.", Kind::Conflict),
        ("Only a running run can be moved on.", Kind::Invalid),
        ("This step changed. Refresh it before acting.", Kind::Conflict),
        ("This step can no longer be moved on that way. Refresh it before acting.", Kind::Conflict),
        ("The run finished before this step could be moved on.", Kind::Conflict),
        // db/circuit/queue.rs
        ("queue order contains a duplicate run id - refresh and retry", Kind::Invalid),
        ("queue changed while reordering (stale or foreign run ids) - refresh and retry", Kind::Conflict),
        ("queue changed while reordering (expected 2 pending runs, got 1) - refresh and retry", Kind::Conflict),
        // db/circuit/recovery.rs
        ("This run no longer exists.", Kind::NotFound),
        ("Review lineage is inconsistent. Start a fresh review.", Kind::Conflict),
        ("Review lineage is inconsistent.", Kind::Conflict),
        ("Review lineage is incomplete.", Kind::Conflict),
        ("The continued review was cancelled. Start a fresh review.", Kind::Conflict),
        ("Review round limit reached.", Kind::Conflict),
        ("The implementation agent no longer belongs to this Mesh.", Kind::Conflict),
        ("The implementation agent is no longer retained.", Kind::NotFound),
        ("The reviewer definition is unavailable.", Kind::Invalid),
        ("Additional review rounds must be between 1 and 10.", Kind::Invalid),
        ("The review round limit is unavailable.", Kind::Invalid),
        ("Only failed reviews can be continued. Cancelled reviews require a fresh review.", Kind::Invalid),
        ("Only failed reviews can be continued. Open the active run to resume or cancel it.", Kind::Invalid),
        ("This custom circuit needs manual recovery. Open its implementation agent and inspect the failed step.", Kind::Invalid),
        ("This run stopped before review. Open the implementation agent and resolve its failure first.", Kind::Invalid),
        ("The implementation agent is no longer retained. Recover from the PR branch and start a new review.", Kind::NotFound),
        ("The implementation agent is still being stopped. Try Review again in a moment.", Kind::Conflict),
        ("The previous reviewer is still being cleaned up. Try Review again in a moment.", Kind::Conflict),
        ("The review feedback step has changed. Open the implementation agent to recover this custom circuit.", Kind::Conflict),
        // db/circuit/step_recovery.rs
        ("Only a failed run can be recovered this way.", Kind::Invalid),
        ("This failure changed. Refresh before acting.", Kind::Conflict),
        ("The recovery action is no longer available. Refresh the run and try again.", Kind::Conflict),
        ("The step is no longer in this blueprint.", Kind::NotFound),
        // db/agent_node.rs
        ("Agent Node no longer exists", Kind::NotFound),
    ];

    /// Every message this slice carries through [`DbError::Rendered`] because a
    /// producer rendered it first. Frozen for the same reason as
    /// [`CONSTRUCTED`]: these reach the user as-is.
    ///
    /// All of them are the step-recovery refusal funnel. `retry_refusal`,
    /// `continue_refusal` and `reopen_blocker` fill
    /// `RunRecoveryOption::unavailable_reason`, which the recovery view
    /// *serializes* - so it is `Option<String>` by design - and which
    /// `recover_failed_run_locked` then promotes to an `Err`. One `String`
    /// serves both paths, so the single promotion site cannot honestly label a
    /// heterogeneous set of refusals by kind: "the agent no longer exists" and
    /// "this step is a trigger" are different conditions behind one funnel.
    /// Type them when `circuit::model`'s recovery view is.
    const CARRIED: &[&str] = &[
        // retry_refusal / continue_refusal
        "This step is a trigger or a join, so there is nothing to run again.",
        "This step's agent is still open. Close it first, or carry on in the agent itself.",
        "The agent for this step was closed. Resume it from Archive, then retry.",
        "The agent for this step no longer exists, so it cannot be run again.",
        "The agent for this step no longer exists, so there is nothing to continue with.",
        "A review approval cannot be recorded on the reviewer's behalf. Use Review again.",
        "An agent cannot be recorded as started when it was not. Retry the step instead.",
        "The pull request must be found on GitHub, which cannot be attested. Retry once it exists.",
        "A verification result cannot be attested. Retry the step to run it again.",
        "This step cannot be marked done by hand.",
        // reopen_blocker - the first is format!("Run #{other} is already using this agent.")
        "Run #7 is already using this agent.",
        "Autopilot is still closing one of this run's agents. Try again in a moment.",
    ];

    /// Every message the db layer builds with a kind renders exactly as it did
    /// before the conversion.
    ///
    /// What this guards is `DbError`'s `Display`, not each call site's choice of
    /// kind: add a prefix to `#[error("{message}")]` and every row fails. The
    /// row *also* asserts the kind, so a message cannot be filed under the
    /// wrong constructor.
    #[test]
    fn render_stability_pins_every_constructed_message() {
        for (message, kind) in CONSTRUCTED {
            let error = kind.build(message);
            assert_eq!(error.to_string(), *message);
            assert!(kind.matches(&error), "{message:?} lost its kind");
        }
    }

    /// The same freeze for the messages carried in from other producers.
    #[test]
    fn render_stability_pins_every_carried_message() {
        for message in CARRIED {
            let error = DbError::from((*message).to_string());
            assert_eq!(error.to_string(), *message);
            assert!(matches!(error, DbError::Rendered(_)));
        }
    }

    /// The table still describes the layer: every row is reachable from a
    /// message some file in `db/` actually writes. A message that no longer
    /// exists, or a new one that was never added, is what this catches.
    #[test]
    fn frozen_rows_are_distinct_and_all_present_in_the_layer() {
        let mut all: Vec<&str> = CONSTRUCTED.iter().map(|(text, _)| *text).collect();
        all.extend(CARRIED.iter().copied());
        let mut sorted = all.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            all.len(),
            "two frozen rows render the same text, so the table no longer pins each one"
        );

        let db_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/db");
        let mut sources = String::new();
        let mut dirs = vec![db_root.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).expect("db/ source tree must be readable") {
                let path = entry.expect("db/ entry must be readable").path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    sources.push_str(&std::fs::read_to_string(&path).expect("db/ source readable"));
                }
            }
        }
        // `reopen_blocker` builds one of these with `format!("Run #{other} …")`,
        // so the source carries a template rather than a fixed id. Put the
        // template back before looking for it.
        fn source_literal(message: &str) -> String {
            let Some(rest) = message.strip_prefix("Run #") else {
                return message.to_string();
            };
            let Some((id, tail)) = rest.split_once(' ') else {
                return message.to_string();
            };
            if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
                return message.to_string();
            }
            format!("Run #{{other}} {tail}")
        }

        let absent: Vec<&str> = all
            .iter()
            .copied()
            .filter(|message| !sources.contains(&source_literal(message)))
            .collect();
        assert!(
            absent.is_empty(),
            "frozen messages no longer written anywhere under db/: {absent:?}"
        );
    }

    /// The reason a caller can branch on kind instead of substring-matching
    /// prose - the behaviour #2446 had to fake.
    #[test]
    fn kinds_are_distinguishable_without_reading_the_message() {
        let gone = DbError::not_found("Circuit no longer exists");
        let raced = DbError::conflict("The run changed. Refresh it before acting.");
        let refused = DbError::invalid("Give the Circuit a name.");
        let sql = DbError::from(SqlError::InvalidQuery);
        let json = DbError::from(serde_json::from_str::<i32>("not json").unwrap_err());
        let carried = DbError::from("Rendered elsewhere".to_string());

        assert!(matches!(gone, DbError::NotFound { .. }));
        assert!(matches!(raced, DbError::Conflict { .. }));
        assert!(matches!(refused, DbError::InvalidInput { .. }));
        assert!(matches!(sql, DbError::Sql(_)));
        assert!(matches!(json, DbError::Json(_)));
        assert!(matches!(carried, DbError::Rendered(_)));
        // The point: two errors with different prose are separable, and the
        // kinds do not overlap.
        assert!(!matches!(gone, DbError::Conflict { .. }));
        assert!(!matches!(raced, DbError::InvalidInput { .. }));
    }

    /// The transparent variants render exactly as the source error did, which is
    /// what made deleting the 163 `.map_err(|e| e.to_string())` closures in this
    /// slice behaviour-preserving.
    #[test]
    fn transparent_variants_render_exactly_as_their_source() {
        let sql = SqlError::QueryReturnedNoRows;
        let expected = sql.to_string();
        assert_eq!(DbError::from(sql).to_string(), expected);

        let json = serde_json::from_str::<i32>("not json").unwrap_err();
        let expected = json.to_string();
        assert_eq!(DbError::from(json).to_string(), expected);
    }

    /// The command boundary: the `?` at a `Result<_, String>` Tauri command
    /// keeps producing the text the user sees today.
    ///
    /// `From<DbError> for String` is exactly what `?` runs, so converting
    /// directly is the same assertion without building an `Err` only to unwrap
    /// it — which also keeps `clippy::err_expect` quiet.
    #[test]
    fn converts_to_string_through_display() {
        for (message, kind) in CONSTRUCTED {
            let at_boundary: String = kind.build(message).into();
            assert_eq!(at_boundary, *message);
        }
        for message in CARRIED {
            let at_boundary: String = DbError::from((*message).to_string()).into();
            assert_eq!(at_boundary, *message);
        }
    }
}
