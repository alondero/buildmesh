//! Trigger ingestion for Autopilot Circuits (spec #1205 / milestone 3,
//! issue #1208): the two passes that START runs without a human —
//!
//! - **GitHub poll pass** (~[`GITHUB_POLL_INTERVAL`], on-demand capable):
//!   walks every enabled circuit whose blueprint declares a
//!   `GithubIssueLabel`/`GithubPullRequestLabel` trigger root, queries the
//!   labelled open issues/PRs on the circuit's mesh repo, and mints a
//!   pending run per unseen source. Deduplication lives in the schema
//!   (`UNIQUE (circuit_id, trigger_identity)` + `INSERT OR IGNORE`), so
//!   the identity string fully decides retrigger behaviour: different
//!   circuits process the same issue independently, and an
//!   already-processed `(circuit, source)` pair never fires twice.
//! - **Interval pass** (every fast tick): fires an Interval-triggered
//!   circuit when its configured cooldown has elapsed since its newest
//!   run. The newest-run timestamp is the anchor, so restarts cannot
//!   shortcut the cooldown.
//!
//! Both passes are thin impure skins over pure decision cores
//! ([`interval_should_fire`], [`parse_sqlite_datetime`], the identity
//! builders) unit-tested below — the established "pure core, thin impure
//! seam" split of `circuit_worker` / `stepper`. Threading note: both run
//! on the circuit worker's dedicated OS thread (blocking reqwest +
//! SQLite are fine there), invoked from its tick loop.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use crate::autopilot::circuit::context::CircuitContext;
use crate::autopilot::circuit::model::{CircuitGraph, CircuitNodeKind};
use crate::db;
use crate::models::AutopilotCircuit;
use crate::services::github::GitHubClient;

/// GitHub poll cadence (issue #1208: "~120s, on-demand capable").
pub const GITHUB_POLL_INTERVAL: Duration = Duration::from_secs(120);

/// Millis since the UNIX epoch of the last completed GitHub poll pass.
static LAST_GITHUB_POLL_MS: AtomicU64 = AtomicU64::new(0);

/// Set when something asks for an immediate poll (on-demand capability);
/// consumed by the next tick regardless of the cadence timer.
static POLL_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Ask for a GitHub poll on the worker's next tick instead of waiting
/// for [`GITHUB_POLL_INTERVAL`] to elapse.
pub fn request_github_poll() {
    POLL_REQUESTED.store(true, Ordering::SeqCst);
    super::circuit_worker::wake_circuit_worker();
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Called from the circuit worker's fast-tick loop: runs the GitHub poll
/// pass when the cadence timer has expired OR an on-demand request is
/// pending; otherwise a no-op.
pub fn maybe_poll_github() {
    let due = POLL_REQUESTED.swap(false, Ordering::SeqCst);
    if !due {
        let last = LAST_GITHUB_POLL_MS.load(Ordering::SeqCst);
        let now = now_millis();
        let elapsed = now.saturating_sub(last);
        if last != 0 && Duration::from_millis(elapsed) < GITHUB_POLL_INTERVAL {
            return;
        }
    }
    LAST_GITHUB_POLL_MS.store(now_millis(), Ordering::SeqCst);
    run_github_poll_pass();
}

// ---------------------------------------------------------------------------
// Pure decision cores.
// ---------------------------------------------------------------------------

/// Best-effort parser for SQLite `datetime('now')` output
/// (`YYYY-MM-DD HH:MM:SS`) into a wall-clock anchor. Shared logic with
/// `services::autopilot`'s private copy — kept local so the trigger core
/// has no cross-module test coupling; divergence would surface as a
/// wrong cooldown, caught by the round-trip test below.
pub(crate) fn parse_sqlite_datetime(s: &str) -> Option<SystemTime> {
    let mut parts = s.split(' ');
    let date = parts.next()?;
    let time = parts.next()?;
    let mut date_parts = date.split('-');
    let year: i32 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    let mut time_parts = time.split(':');
    let hour: u32 = time_parts.next()?.parse().ok()?;
    let minute: u32 = time_parts.next()?.parse().ok()?;
    let second: u32 = time_parts.next()?.parse().ok()?;
    let naive = chrono::NaiveDate::from_ymd_opt(year, month, day)?
        .and_hms_opt(hour, minute, second)?;
    Some(SystemTime::from(chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
        naive,
        chrono::Utc,
    )))
}

/// The interval trigger's cooldown decision (pure): fire when the
/// circuit never fired (`None` anchor) or when `interval_seconds` have
/// elapsed since the last fire. A non-positive interval never fires —
/// the IPC boundary validates, but a hand-edited `graph_json` must not
/// turn into a hot loop. Clock comes from the caller so the decision is
/// deterministic under test.
pub(crate) fn interval_should_fire(
    last_fire_at: Option<SystemTime>,
    interval_seconds: i64,
    now: SystemTime,
) -> bool {
    if interval_seconds <= 0 {
        return false;
    }
    match last_fire_at {
        None => true,
        Some(t) => {
            now.duration_since(t).unwrap_or(Duration::ZERO)
                >= Duration::from_secs(interval_seconds as u64)
        }
    }
}

/// The dedupe identity of one GitHub-triggered source, scoped into the
/// run ledger as `trigger_identity` (the models doc pins this vocabulary:
/// `<issue|pr>:<number>:<label>`). Combined with the schema's
/// `UNIQUE (circuit_id, trigger_identity)` this makes "two circuits may
/// react to the same issue, one circuit never processes it twice"
/// structural rather than guarded.
pub(crate) fn issue_identity(number: i64, label: &str) -> String {
    format!("issue:{}:{}", number, label)
}

pub(crate) fn pr_identity(number: i64, label: &str) -> String {
    format!("pr:{}:{}", number, label)
}

/// Of the freshly-fetched sources, those whose identity is not already
/// in the circuit's stored set — the mint list. Pure so the
/// "already-processed pairs never retrigger" rule is pinned without
/// network or DB.
pub(crate) fn unseen_identities<'a>(
    fetched: &'a [(String, i64)],
    known: &HashSet<String>,
) -> Vec<&'a (String, i64)> {
    fetched.iter().filter(|(id, _)| !known.contains(id)).collect()
}

// ---------------------------------------------------------------------------
// Impure passes.
// ---------------------------------------------------------------------------

/// One GitHub poll pass over every enabled circuit. Per-circuit failures
/// are logged and isolated — one bad remote must not starve the others.
fn run_github_poll_pass() {
    let circuits = match db::list_enabled_circuits() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("circuits: github poll could not list enabled circuits: {}", e);
            return;
        }
    };
    for circuit in circuits {
        let graph = match CircuitGraph::from_json(&circuit.graph_json) {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!("circuits: circuit {} has unreadable graph_json: {}", circuit.id, e);
                continue;
            }
        };
        for node in &graph.nodes {
            match &node.kind {
                CircuitNodeKind::GithubIssueLabel { label } => {
                    ingest_issues(&circuit, label);
                }
                CircuitNodeKind::GithubPullRequestLabel { label } => {
                    ingest_pull_requests(&circuit, label);
                }
                _ => {}
            }
        }
    }
}

/// Query labelled open issues for the circuit's mesh repo and mint one
/// pending run per unseen source.
fn ingest_issues(circuit: &AutopilotCircuit, label: &str) {
    let Some((owner, repo, client)) = repo_client_for(circuit) else {
        return;
    };
    let issues = match client.list_open_issues_with_label(&owner, &repo, label) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(
                "circuits: issue-label poll for {} on {}/{} failed: {}",
                circuit.name,
                owner,
                repo,
                e
            );
            return;
        }
    };
    let fetched: Vec<(String, i64)> = issues
        .iter()
        .map(|i| (issue_identity(i.number, label), i.number))
        .collect();
    mint_unseen_runs(circuit, &fetched, |number| {
        issues
            .iter()
            .find(|i| i.number == number)
            .map(|i| {
                let mut ctx = base_context(circuit);
                ctx.with_issue(
                    i.number,
                    &i.title,
                    &i.body,
                    &i.author,
                    &i.html_url,
                    &i.labels,
                );
                ctx
            })
    });
}

/// Query labelled open PRs for the circuit's mesh repo and mint one
/// pending run per unseen source.
fn ingest_pull_requests(circuit: &AutopilotCircuit, label: &str) {
    let Some((owner, repo, client)) = repo_client_for(circuit) else {
        return;
    };
    let prs = match client.list_open_pull_requests_with_label(&owner, &repo, label) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "circuits: pr-label poll for {} on {}/{} failed: {}",
                circuit.name,
                owner,
                repo,
                e
            );
            return;
        }
    };
    let fetched: Vec<(String, i64)> =
        prs.iter().map(|p| (pr_identity(p.number, label), p.number)).collect();
    mint_unseen_runs(circuit, &fetched, |number| {
        prs.iter().find(|p| p.number == number).map(|p| {
            let mut ctx = base_context(circuit);
            ctx.with_pr(
                p.number,
                &p.title,
                &p.body,
                &p.author,
                &p.html_url,
                &p.head_ref,
                &[],
            );
            ctx
        })
    });
}

/// Resolve the circuit's mesh origin to `(owner, repo, client)`, or log
/// and return `None` (no origin / non-GitHub origin / no token).
fn repo_client_for(circuit: &AutopilotCircuit) -> Option<(String, String, GitHubClient)> {
    let mesh = match db::get_mesh_by_id(circuit.mesh_id) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("circuits: circuit {} mesh lookup failed: {}", circuit.id, e);
            return None;
        }
    };
    let (owner, repo) = match crate::commands::pr::resolve_github_owner_repo(&mesh) {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!(
                "circuits: circuit {} ({}) has no GitHub repo: {}",
                circuit.id,
                circuit.name,
                e
            );
            return None;
        }
    };
    match GitHubClient::new() {
        Ok(client) => Some((owner, repo, client)),
        Err(e) => {
            tracing::warn!("circuits: GitHub client unavailable: {}", e);
            None
        }
    }
}

fn base_context(circuit: &AutopilotCircuit) -> CircuitContext {
    let mut ctx = CircuitContext::new();
    ctx.with_circuit(circuit.id, &circuit.name, circuit.mesh_id);
    ctx
}

/// Mint pending runs for every fetched source not already in the
/// circuit's ledger, seeding each run's context from its source. The
/// schema's UNIQUE constraint is the final backstop — this pre-filter
/// just avoids rewriting identical rows every pass.
fn mint_unseen_runs(
    circuit: &AutopilotCircuit,
    fetched: &[(String, i64)],
    context_for: impl Fn(i64) -> Option<CircuitContext>,
) {
    let known: HashSet<String> = match db::list_circuit_trigger_identities(circuit.id) {
        Ok(k) => k.into_iter().collect(),
        Err(e) => {
            tracing::warn!("circuits: identity listing failed for {}: {}", circuit.id, e);
            return;
        }
    };
    for (identity, number) in unseen_identities(fetched, &known) {
        let Some(context) = context_for(*number) else {
            continue;
        };
        match db::create_circuit_run(
            circuit.id,
            circuit.mesh_id,
            identity,
            &context.to_json().unwrap_or_else(|_| "{}".to_string()),
        ) {
            Ok(run_id) => {
                tracing::info!(
                    "circuits: circuit {} ({}) triggered by {} → run {}",
                    circuit.id,
                    circuit.name,
                    identity,
                    run_id
                );
            }
            Err(e) => {
                tracing::warn!("circuits: run creation for {} failed: {}", identity, e);
            }
        }
    }
    if !fetched.is_empty() {
        // Fresh runs start within milliseconds instead of waiting out the
        // fast tick.
        super::circuit_worker::wake_circuit_worker();
    }
}

/// One interval pass (fast-tick cadence): fire every enabled
/// Interval-triggered circuit whose cooldown has elapsed.
pub fn run_interval_pass() {
    let circuits = match db::list_enabled_circuits() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("circuits: interval pass could not list enabled circuits: {}", e);
            return;
        }
    };
    let now = SystemTime::now();
    for circuit in circuits {
        let graph = match CircuitGraph::from_json(&circuit.graph_json) {
            Ok(g) => g,
            Err(_) => continue, // loudly logged by the poll pass; stay silent here
        };
        let interval_seconds = graph.nodes.iter().find_map(|n| match &n.kind {
            CircuitNodeKind::Interval { interval_seconds } => Some(*interval_seconds),
            _ => None,
        });
        let Some(interval_seconds) = interval_seconds else {
            continue;
        };
        let last = db::latest_circuit_run_created_at(circuit.id).ok().flatten();
        let last_anchor = last.as_deref().and_then(parse_sqlite_datetime);
        if interval_should_fire(last_anchor, interval_seconds, now) {
            let identity = format!("interval:{}", now_millis());
            let context = base_context(&circuit);
            match db::create_circuit_run(
                circuit.id,
                circuit.mesh_id,
                &identity,
                &context.to_json().unwrap_or_else(|_| "{}".to_string()),
            ) {
                Ok(run_id) => {
                    tracing::info!(
                        "circuits: circuit {} ({}) interval fired ({}s cadence) → run {}",
                        circuit.id,
                        circuit.name,
                        interval_seconds,
                        run_id
                    );
                    super::circuit_worker::wake_circuit_worker();
                }
                Err(e) => {
                    tracing::warn!("circuits: interval run creation failed: {}", e);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn epoch_plus(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    // -- interval_should_fire -------------------------------------------------

    #[test]
    fn interval_fires_immediately_when_the_circuit_never_fired() {
        assert!(interval_should_fire(None, 300, epoch_plus(0)));
    }

    #[test]
    fn interval_respects_the_cooldown_window() {
        // Fired at t=1000, interval 300: not at t=1299, yes at t=1300.
        let last = epoch_plus(1_000);
        assert!(!interval_should_fire(Some(last), 300, epoch_plus(1_299)));
        assert!(interval_should_fire(Some(last), 300, epoch_plus(1_300)));
    }

    #[test]
    fn interval_never_fires_on_a_non_positive_interval() {
        // Hand-edited graph_json guard: no hot loops.
        assert!(!interval_should_fire(None, 0, epoch_plus(9_999)));
        assert!(!interval_should_fire(None, -5, epoch_plus(9_999)));
    }

    #[test]
    fn interval_treats_a_clock_rolled_back_anchor_as_not_yet_elapsed() {
        // A future anchor (clock rolled back after firing) must not
        // hot-loop: duration_since saturates to zero < interval, so the
        // trigger waits out the window again. Conservative by design.
        let future = epoch_plus(9_999);
        assert!(!interval_should_fire(Some(future), 60, epoch_plus(1_000)));
    }

    // -- sqlite datetime parsing ----------------------------------------------

    #[test]
    fn parses_sqlite_datetime_output() {
        // "1970-01-02 12:34:56" must land exactly 45,296 seconds after
        // the epoch — pinned so any SQLite format change surfaces here
        // rather than as silently-shortened cooldowns.
        let parsed = parse_sqlite_datetime("1970-01-02 12:34:56").expect("parses");
        let expected =
            SystemTime::UNIX_EPOCH + Duration::from_secs(86_400 + 12 * 3_600 + 34 * 60 + 56);
        assert_eq!(parsed, expected);
    }

    #[test]
    fn garbage_datetimes_degrade_to_none_not_panic() {
        assert!(parse_sqlite_datetime("").is_none());
        assert!(parse_sqlite_datetime("not a date").is_none());
        assert!(parse_sqlite_datetime("2026-13-99 99:99:99").is_none());
    }

    // -- identity builders + unseen filter --------------------------------------

    #[test]
    fn identities_match_the_models_doc_vocabulary() {
        assert_eq!(issue_identity(42, "buildmesh:run"), "issue:42:buildmesh:run");
        assert_eq!(pr_identity(7, "review-me"), "pr:7:review-me");
    }

    #[test]
    fn unseen_filter_keeps_only_sources_without_a_stored_identity() {
        let fetched = vec![
            (issue_identity(1, "go"), 1),
            (issue_identity(2, "go"), 2),
            (pr_identity(3, "go"), 3),
        ];
        let known: HashSet<String> = [issue_identity(2, "go")].into_iter().collect();
        let planned = unseen_identities(&fetched, &known);
        assert_eq!(planned.len(), 2);
        assert_eq!(planned[0].1, 1, "unseen issue stays");
        assert_eq!(planned[1].1, 3, "PRs dedupe independently of issues");
    }

    #[test]
    fn the_same_source_is_seen_again_only_through_its_own_circuit() {
        // The dedupe set is per-circuit (the caller passes that circuit's
        // identities only), so the same issue identity appearing in two
        // passes for DIFFERENT circuits is filtered by neither — pinned
        // here by showing the filter itself is scoped to what it's given.
        let fetched = vec![(issue_identity(42, "buildmesh:run"), 42)];
        let other_circuits_known: HashSet<String> = HashSet::new();
        assert_eq!(unseen_identities(&fetched, &other_circuits_known).len(), 1);
    }
}
