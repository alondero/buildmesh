//! Fetch-failure classification and the per-Mesh backoff a permanent failure
//! earns (issue #2832).
//!
//! Why this exists
//! ---------------
//! The background mesh sync (ADR 0020) fetches each idle Mesh's base ref
//! once per `pool_worker::BACKGROUND_SYNC_INTERVAL` — three minutes. That
//! cadence suits a *transient* failure: an offline laptop, a slow
//! network or a wedged connection may well succeed on the next attempt, and
//! the Mesh must recover without the user doing anything.
//!
//! It does not suit a *permanent* one. A Mesh whose `base_ref` names a branch
//! the remote does not have fails on each attempt with `fatal: couldn't
//! find remote ref refs/heads/main`, and a retry is expected to keep failing
//! until the Mesh's configuration changes. The hub logs behind issue #2832
//! showed 1,303 such failures across the retained window — a wasted `git`
//! process tree per attempt, on a cadence of roughly three and a half
//! minutes, for a fetch that could not succeed.
//!
//! What this module owns
//! ---------------------
//! * [`classify_fetch_failure`] — is this failure permanent (a retry is
//!   expected to keep failing until the configuration changes) or transient
//!   (the network or the load was the problem)? Pure matching over git's own
//!   stderr, so it is unit-testable without a repository.
//! * [`missing_remote_branch`] — the branch the remote was asked for and does
//!   not have, when the failure says so.
//! * [`detect_remote_default_branch`] — the branch the remote's `HEAD` points
//!   at, so the message can offer the branch the user probably meant.
//! * [`describe_permanent_failure`] — the user-facing sentence, kept pure so
//!   the copy is testable without a repository or a network.
//! * The per-Mesh registry of permanent failures and the capped exponential
//!   backoff that follows one.
//!
//! The backoff
//! -----------
//! A permanent failure pauses the Mesh's background sync for at least
//! [`PERMANENT_BASE_INTERVAL`] (one hour), doubling on each further failure
//! and capped at [`PERMANENT_MAX_INTERVAL`] (24 hours). The record is keyed
//! on the Mesh's identity and its *configured* `base_ref`: when the user
//! changes that setting the recorded failure may stop describing the
//! situation, so it is dropped and the sync resumes at once. A successful
//! fetch of the Mesh drops it too, so a repaired remote is picked up by the
//! next sync rather than waiting out the interval.
//!
//! The record lives in memory on purpose, for the same reason
//! `services::warm_pool_backoff`'s does: the retry *policy* is a property of
//! this app run, and a restart is a natural reset — the user closed the app
//! and came back, which is the moment to try again.

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::process_util::{git_command, run_command_with_timeout};

/// How the retry policy treats a failed fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchFailureClass {
    /// A retry is expected to keep failing until the Mesh's configuration —
    /// or the remote itself — changes: the remote has no such branch, the
    /// remote is gone, or the credentials are refused.
    Permanent,
    /// The network, the load or a lock was the problem: a timeout, a DNS
    /// failure, a reset connection, an unreachable host, a TLS failure, or
    /// another git process holding the index or a ref. The same request may
    /// well succeed on the next attempt.
    Transient,
}

/// stderr fragments that mean "a retry is expected to keep failing".
///
/// Matched case-insensitively against the whole message. The first entry is
/// the exact shape issue #2832 reports:
///
/// ```text
/// fatal: couldn't find remote ref refs/heads/main
/// ```
const PERMANENT_MARKERS: &[&str] = &[
    // The remote does not have the branch we asked for.
    "find remote ref",
    // The remote itself is gone or was renamed. Git's wording puts the URL
    // between the two words ("repository 'https://…' not found"), so this
    // pair of shapes is matched separately — see
    // [`names_a_missing_repository`]. A bare "not found" would also match an
    // HTTP error body that happens to mention it, which says little about
    // the remote's fate.
    //
    // "does not appear to be a git repository" is deliberately NOT here: the
    // fetch path only reaches the fetch when the remote NAME is configured
    // (a missing name is `SkippedNoRemote`, not a failure), so the sentence
    // describes the remote's TARGET — a local path that is not there right
    // now (an unplugged drive) as often as a URL that is not a repository.
    // A gone URL arrives as "repository '…' not found", which is permanent.
    // Credentials refused — a wrong or expired token is still wrong next
    // time. `GIT_TERMINAL_PROMPT=0` (see `process_util::git_command`)
    // turns git's interactive credential prompt into this message.
    "authentication failed",
    "could not read username",
    "could not read password",
    "invalid username or password",
    "permission denied (publickey)",
    "terminal prompts disabled",
    // The same refusal in the wording of other hosting services. GitHub
    // says "Authentication failed"; GitLab and Gitea say "HTTP Basic:
    // Access denied"; Bitbucket says "repository access denied". A Mesh
    // on one of those would otherwise spend its retry budget on a token
    // the server has already refused.
    "http basic: access denied",
    "repository access denied",
    "remote: permission denied",
    // The hosting service's HTTP answers for a missing, private or
    // forbidden repository. Git wraps the status in "The requested URL
    // returned error: 403", so the marker is the "error: <status>" shape.
    "error: 401",
    "error: 403",
];

/// Whether the failure says the *repository* itself is gone.
///
/// Git's two shapes for that are `fatal: repository 'https://…/repo.git/'
/// not found` and the hosting service's `remote: Repository not found.`.
/// Both are matched as shapes rather than as the phrase "not found": an HTTP
/// error body that mentions "not found" (a 500 saying "upstream host not
/// found") says little about whether the remote is still there, and
/// reading it as permanent would pause a Mesh for hours over a blip.
fn names_a_missing_repository(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("repository not found")
        || (lower.contains("repository '") && lower.contains("' not found"))
}

/// Classify a failed fetch.
///
/// The permanent markers are checked FIRST, so a message that carries both a
/// transport prefix and a refusal still lands on the permanent reading — git
/// prefixes transport failures with `unable to access`, and
/// `fatal: unable to access 'https://…': The requested URL returned error:
/// 403` is a refused repository, not a flaky network.
///
/// Everything else is transient: the timeout, network and local-contention
/// shapes, and a message this function does not recognise alike. The
/// pre-#2832 behaviour is to retry, so an unrecognised failure must not
/// silently stop a Mesh's sync.
pub fn classify_fetch_failure(reason: &str) -> FetchFailureClass {
    let lower = reason.to_ascii_lowercase();
    if names_a_missing_repository(reason)
        || PERMANENT_MARKERS
            .iter()
            .any(|marker| lower.contains(marker))
    {
        FetchFailureClass::Permanent
    } else {
        FetchFailureClass::Transient
    }
}

/// Classify a failed fetch reported through the spawn-shaped
/// [`crate::git::sync::FetchError`].
pub fn classify_fetch_error(error: &crate::git::sync::FetchError) -> FetchFailureClass {
    match error {
        crate::git::sync::FetchError::FetchFailed(reason) => classify_fetch_failure(reason),
        // A repository that does not open is a LOCAL condition — an
        // unmounted drive, a folder mid-rename — and it can come back on
        // its own, so it stays on the transient schedule.
        crate::git::sync::FetchError::RepoUnusable { .. } => FetchFailureClass::Transient,
    }
}

/// The reason of a failed fetch, for the log line and the message.
pub fn fetch_failure_reason(error: &crate::git::sync::FetchError) -> String {
    match error {
        crate::git::sync::FetchError::FetchFailed(reason) => reason.clone(),
        crate::git::sync::FetchError::RepoUnusable { reason, .. } => reason.clone(),
    }
}

/// The branch a failed fetch asked for and the remote did not have.
///
/// Reads the branch out of git's own message
/// (`fatal: couldn't find remote ref refs/heads/main` → `main`), so the
/// answer is what the remote actually refused rather than what Buildmesh
/// believes it asked for.
pub fn missing_remote_branch(reason: &str) -> Option<String> {
    let lower = reason.to_ascii_lowercase();
    // "couldn't find remote ref" / "could not find remote ref" — match the
    // part that is the same in both spellings so git's choice of
    // apostrophe does not matter.
    let marker_at = lower.find("find remote ref")?;
    let rest = reason[marker_at + "find remote ref".len()..].trim();
    let rest = rest.trim_start_matches(':').trim();
    let branch = rest.strip_prefix("refs/heads/").unwrap_or(rest);
    (!branch.is_empty()).then(|| branch.to_string())
}

/// The wall-clock budget for the `git ls-remote` probe that asks a remote
/// which branch its `HEAD` points at.
///
/// Shorter than the spawn path's fetch budget on purpose: the probe runs on
/// the pool worker thread, which serves each Mesh's warm-pool fill, so a
/// stalled remote must not hold it for the full 30 s fetch timeout. A
/// single-ref query is one round-trip; 5 s is generous, and the local
/// symbolic ref answers the question without the network whenever it
/// exists.
const DEFAULT_BRANCH_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The branch the remote named `remote` points its `HEAD` at.
///
/// Read from the local symbolic ref `refs/remotes/<remote>/HEAD` first — it
/// costs no network round-trip — and from `git ls-remote --symref` when the
/// local ref is missing (a repository cloned before the remote's default
/// branch was known, or a remote re-pointed at another repository).
///
/// `None` when no source can answer, which is a legitimate answer: the
/// message then says what failed without claiming a default branch.
pub fn detect_remote_default_branch(host_path: &str, remote: &str) -> Option<String> {
    if remote.is_empty() {
        return None;
    }
    local_default_branch(host_path, remote).or_else(|| remote_default_branch(host_path, remote))
}

/// The local `refs/remotes/<remote>/HEAD` symbolic target, if the repository
/// has one.
fn local_default_branch(host_path: &str, remote: &str) -> Option<String> {
    let repo = super::project::open(host_path).ok()?;
    let reference = repo
        .find_reference(&format!("refs/remotes/{remote}/HEAD"))
        .ok()?;
    let target = reference.symbolic_target().ok().flatten()?;
    let prefix = format!("refs/remotes/{remote}/");
    let branch = target.strip_prefix(&prefix)?;
    (!branch.is_empty()).then(|| branch.to_string())
}

/// Ask the remote which branch its `HEAD` points at.
fn remote_default_branch(host_path: &str, remote: &str) -> Option<String> {
    let mut builder = git_command();
    builder
        .args(["ls-remote", "--symref", remote, "HEAD"])
        .current_dir(host_path);
    let output =
        run_command_with_timeout(builder, "git ls-remote", DEFAULT_BRANCH_PROBE_TIMEOUT).ok()?;
    if !output.status.success() {
        return None;
    }
    parse_ls_remote_symref(&String::from_utf8_lossy(&output.stdout))
}

/// Parse `git ls-remote --symref <remote> HEAD` output into the branch
/// `HEAD` points at.
///
/// The output's first line is the symref line
/// (`ref: refs/heads/master\tHEAD`), followed by the SHA line. Only the
/// symref line carries the branch name.
pub fn parse_ls_remote_symref(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("ref: ") else {
            continue;
        };
        let Some(target) = rest.split_whitespace().next() else {
            continue;
        };
        if let Some(branch) = target.strip_prefix("refs/heads/") {
            if !branch.is_empty() {
                return Some(branch.to_string());
            }
        }
    }
    None
}

/// The sentence the user sees when a Mesh's background sync fails
/// permanently.
///
/// Pure, so the copy is testable without a repository or a network. The
/// missing-branch case names the remote, the branch it does not have and
/// the remote's default branch — the issue's "origin has no `main`; its
/// default branch is `master`" — and offers the default branch as the
/// base ref to use.
///
/// `mesh_path` names the Mesh the failure is about: the toast that carries
/// this sentence is app-global, so without it the user would have to work
/// out which of their Meshes stopped syncing.
pub fn describe_permanent_failure(
    mesh_path: &str,
    remote: Option<&str>,
    missing_branch: Option<&str>,
    default_branch: Option<&str>,
    reason: &str,
) -> String {
    let remote = remote.unwrap_or("the remote");
    match (missing_branch, default_branch) {
        (Some(branch), Some(default)) if branch != default => format!(
            "{remote} has no branch '{branch}'; its default branch is '{default}'. \
             Background sync is paused for {mesh_path} — set its base ref to \
             {remote}/{default} to resume."
        ),
        (Some(branch), _) => format!(
            "{remote} has no branch '{branch}'. Background sync is paused for {mesh_path} — \
             set its base ref to a branch {remote} actually has, to resume."
        ),
        (None, _) => format!(
            "Background sync is paused for {mesh_path} — the fetch failed permanently \
             ({reason}). A retry is expected to keep failing until the remote or its \
             credentials are fixed."
        ),
    }
}

/// The wait after a Mesh's first permanent fetch failure.
///
/// One hour. Issue #2832's acceptance is a capped exponential backoff of at
/// least one hour: the failing fetch must not run on the three-minute sync
/// cadence for the rest of the app's life.
pub const PERMANENT_BASE_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// The longest the permanent-failure backoff ever waits between two attempts.
pub const PERMANENT_MAX_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// The interval to wait after the `failures`-th consecutive permanent
/// failure.
///
/// Doubles from [`PERMANENT_BASE_INTERVAL`] and is capped at
/// [`PERMANENT_MAX_INTERVAL`].
pub fn permanent_interval_for(failures: u32) -> Duration {
    if failures <= 1 {
        return PERMANENT_BASE_INTERVAL;
    }
    // An hour doubled 32 times already overflows a `u64` second count, so
    // the exponent is clamped *before* the shift. The cap makes the
    // exponents beyond the point where the interval saturates equivalent
    // anyway.
    let exponent = (failures - 1).min(32);
    let seconds = PERMANENT_BASE_INTERVAL
        .as_secs()
        .saturating_mul(1u64 << exponent)
        .min(PERMANENT_MAX_INTERVAL.as_secs());
    Duration::from_secs(seconds)
}

/// One Mesh's permanent-failure record.
///
/// A *scheduling* record, not a log: it says when the next attempt may run
/// and what configuration the pause was recorded against. The reason
/// travels with the caller's own log line, so it is not duplicated here.
#[derive(Debug, Clone)]
pub struct PermanentFailure {
    /// Consecutive permanent failures since the last success or fingerprint
    /// change.
    pub failures: u32,
    /// How long the next attempt has to wait.
    pub interval: Duration,
    /// When the next attempt may run.
    pub next_attempt: Instant,
    /// The Mesh configuration the failure was recorded against.
    pub fingerprint: String,
}

/// What a recorded permanent failure means for the caller's log line.
#[derive(Debug, Clone, Copy)]
pub struct PermanentFailureReport {
    /// `true` for the Mesh's first permanent failure since its last success
    /// or configuration change — the line that tells the user their sync is
    /// broken, and the only one that surfaces the reason on the Mesh.
    pub first: bool,
    /// `true` when the retry interval changed. The interval stops changing
    /// once it reaches [`PERMANENT_MAX_INTERVAL`], so the retries after that
    /// are silent: the escalation has already been reported.
    pub interval_changed: bool,
    /// How many consecutive permanent failures have now happened.
    pub failures: u32,
    /// How long until the next attempt.
    pub interval: Duration,
}

/// The per-Mesh permanent-failure records.
static PERMANENT_FAILURES: Lazy<Mutex<HashMap<String, PermanentFailure>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn with_records<R>(f: impl FnOnce(&mut HashMap<String, PermanentFailure>) -> R) -> R {
    match PERMANENT_FAILURES.lock() {
        Ok(mut map) => f(&mut map),
        // A poisoned lock just means a prior writer panicked mid-insert;
        // the map has no cross-entry invariant to corrupt, so recover.
        Err(poisoned) => f(&mut poisoned.into_inner()),
    }
}

/// The key a Mesh's permanent-failure record is filed under.
///
/// The Mesh's own path, normalised — *not* the repository's coordination
/// key. Two Mesh rows can share a repository while naming different
/// `base_ref`s, and a shared record would let one Mesh's failure suppress
/// (or clear) the other's. Each Mesh owns its own record.
fn mesh_key(mesh_path: &str) -> String {
    let host = crate::env::to_host_path(mesh_path);
    let trimmed = host.trim_end_matches(['/', '\\']);
    let key = if trimmed.is_empty() {
        host
    } else {
        trimmed.to_string()
    };
    if cfg!(windows) {
        key.to_lowercase()
    } else {
        key
    }
}

/// Whether `mesh_path` may attempt a background fetch right now.
///
/// A record whose fingerprint differs is dropped first: the Mesh's
/// `base_ref` changed, so the recorded failure may stop describing the
/// situation, and the fetch is attempted at once.
pub fn allows_attempt(mesh_path: &str, fingerprint: &str) -> bool {
    allows_attempt_at(mesh_path, fingerprint, Instant::now())
}

/// [`allows_attempt`] with an explicit clock, so the backoff rule is
/// testable against a synthetic timeline.
pub fn allows_attempt_at(mesh_path: &str, fingerprint: &str, now: Instant) -> bool {
    let key = mesh_key(mesh_path);
    with_records(|map| match map.get(&key) {
        None => true,
        Some(record) if record.fingerprint != fingerprint => {
            map.remove(&key);
            true
        }
        Some(record) => now >= record.next_attempt,
    })
}

/// Record a permanent fetch failure and report what the caller should log.
pub fn record_permanent_failure(
    mesh_path: &str,
    fingerprint: &str,
    now: Instant,
) -> PermanentFailureReport {
    let key = mesh_key(mesh_path);
    with_records(|map| {
        let previous = map.get(&key);
        let failures = previous.map(|record| record.failures + 1).unwrap_or(1);
        let interval = permanent_interval_for(failures);
        let first = previous.is_none();
        let interval_changed = previous
            .map(|record| record.interval != interval)
            .unwrap_or(true);
        map.insert(
            key,
            PermanentFailure {
                failures,
                interval,
                next_attempt: now + interval,
                fingerprint: fingerprint.to_string(),
            },
        );
        PermanentFailureReport {
            first,
            interval_changed,
            failures,
            interval,
        }
    })
}

/// Record a successful fetch: the Mesh's permanent-failure record is over.
///
/// A fetch that reached the remote proves the permanent condition is gone,
/// so the Mesh's next background sync runs on the normal schedule instead
/// of waiting out a backoff that does not apply.
pub fn record_success(mesh_path: &str) -> bool {
    let key = mesh_key(mesh_path);
    with_records(|map| map.remove(&key).is_some())
}

#[cfg(test)]
#[path = "fetch_failure_tests.rs"]
mod tests;
