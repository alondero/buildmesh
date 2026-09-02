//! Prepare-context phase of Agent Node spawn.
//!
//! Resolves everything the later phases need before touching git or a PTY:
//! the in-flight claim, the node row, session-id mode, mesh worktree policy,
//! optional warm-pool claim, and the host/spawn paths. The orchestrator holds
//! the claim for the rest of the pipeline.

use super::process::is_agent_already_running;
use super::reader::{SessionIdMode, SpawnTimer};
use super::WorktreePolicy;
use crate::models::{AgentNode, Provider};
use crate::{db, env};

/// Default `worktree_mode` when the mesh config leaves it unset. Pinned by
/// `default_worktree_mode_is_branched` in `provision_tests.rs`.
///
/// This was previously paired with a TS sentinel at `src/lib/worktreeMode.ts`,
/// deleted in #411 once the TS side lost its only consumer (a self-referential
/// test). If a future UI re-exposes a worktree-mode selector, re-introduce
/// the TS constant alongside it and re-couple by doc comment + paired test
/// (see [[feedback_cross-language-default-coupling]]). See
/// `docs/knowledge-primer.md` (Worktree Support) for the branched-vs-detached
/// rationale.
pub const DEFAULT_WORKTREE_MODE: &str = "branched";

pub(super) static SPAWNS_IN_FLIGHT: once_cell::sync::Lazy<
    parking_lot::Mutex<std::collections::HashSet<i64>>,
> = once_cell::sync::Lazy::new(|| parking_lot::Mutex::new(std::collections::HashSet::new()));

/// RAII claim on a session id in [`SPAWNS_IN_FLIGHT`]. Dropping releases
/// the claim on every exit path, including a cancelled async task.
pub(crate) struct SpawnInFlightClaim {
    session_id: i64,
}

impl SpawnInFlightClaim {
    /// `Some(claim)` if no spawn is in flight for this session, `None` if
    /// one already is (the caller should short-circuit as a duplicate).
    pub(crate) fn try_claim(session_id: i64) -> Option<Self> {
        // parking_lot::Mutex::lock is non-poisoning and a strict upgrade
        // over std here: no unwrap on contention, and `try_lock` lets
        // Drop fall back gracefully if the runtime is mid-shutdown.
        let mut guard = SPAWNS_IN_FLIGHT.lock();
        if guard.insert(session_id) {
            Some(Self { session_id })
        } else {
            None
        }
    }
}

impl Drop for SpawnInFlightClaim {
    fn drop(&mut self) {
        // `lock` (blocking) is correct here: the guard's lifetime is the
        // whole spawn function, so contention is at most a few µs of
        // contended HashSet::remove — never long enough to starve a
        // tokio worker. `try_lock` would silently leak the claim on
        // contention, which is the opposite of what the bug requires.
        SPAWNS_IN_FLIGHT.lock().remove(&self.session_id);
    }
}

/// Options for spawning or resuming an agent process.
pub(crate) struct SpawnOptions {
    pub session_id: i64,
    pub provider: Provider,
    pub resume: Option<String>,
    pub rows: u16,
    pub cols: u16,
    pub prefill: Option<String>,
    /// Pre-fetched node to avoid a redundant DB read when the caller already has it.
    pub node: Option<AgentNode>,
    /// Cascade layer-1 model override (issue #1155). Highest precedence
    /// in the spawn-config cascade — wins over the Mesh row and the
    /// application default. `None` or whitespace-only collapses to
    /// absent at [`super::orchestrator::cascade_inputs_for`] so the cascade
    /// falls through.
    pub explicit_model: Option<String>,
    /// Cascade layer-1 effort / reasoning override (issue #1155). Same
    /// semantics as [`Self::explicit_model`] — independent field, only
    /// matters when the harness's capability descriptor declares effort
    /// support (otherwise the resolver mask drops it).
    pub explicit_effort: Option<String>,
    /// Cascade layer-1 verbatim CLI flag string (issue #1358). No mesh
    /// / application layer carries per-spawn flags, so this is the only
    /// layer of supply. Capability-masked downstream — a harness whose
    /// descriptor reports `supports_extra_args = false` (Terminal is
    /// the only one) silently drops the value at the resolver rather
    /// than splicing a synthetic flag into its argv.
    pub explicit_extra_args: Option<String>,
    /// Caller-owned worktree policy. `ForceBranched` is used by issue-driven
    /// circuit runs; `RespectMesh` preserves the normal spawn behaviour.
    pub worktree_policy: WorktreePolicy,
}

/// Fully resolved spawn state at the prepare → provision seam.
pub(super) struct PreparedSpawn {
    pub session_id: i64,
    pub provider: Provider,
    pub rows: u16,
    pub cols: u16,
    pub prefill: Option<String>,
    pub explicit_model: Option<String>,
    pub explicit_effort: Option<String>,
    pub explicit_extra_args: Option<String>,
    pub node: AgentNode,
    pub session_id_mode: SessionIdMode,
    pub use_worktree: bool,
    pub sandbox: bool,
    pub worktree_mode: String,
    pub base_ref: String,
    pub mesh_id: i64,
    pub warm_claimed: Option<crate::services::warm_pool::ClaimedWarmEntry>,
    pub pool_was_drained_by_this_spawn: bool,
    pub spawn_worktree_name: Option<String>,
    pub resolved: env::ResolvedPath,
}

pub(super) enum PrepareOutcome {
    /// Duplicate in-flight spawn or a live process already occupies the node.
    Skipped,
    Ready {
        claim: SpawnInFlightClaim,
        spawn: Box<PreparedSpawn>,
    },
}

/// Resolve the `base_ref` string that `git::sync::fetch_origin` will use for
/// the spawn-time auto-sync. The chain (each tier only runs if the previous
/// one yields nothing useful):
///
/// 1. The mesh's `base_ref` column from the `meshes` DB row — explicit
///    user intent wins, even on a repo whose default branch disagrees.
///    **The COALESCE default `'origin/main'` is treated as "no config"**:
///    a fresh mesh whose `base_ref` column was never explicitly set reads
///    as `'origin/main'` from the DB (see `db::MESH_COLUMNS`), and a
///    user who never touched the field is functionally identical to a
///    user who has no config. Detecting both via the same path is what
///    closes the master-trunk regression. **There is no `mesh.toml`
///    file**: the value lives on the `meshes` SQLite row (and is
///    mirrored to `.claude/settings.json` at the mesh root for Claude
///    Code, see `commands::mesh_properties`).
/// 2. The repo's actual default branch read from
///    `refs/remotes/origin/HEAD` (populated by `git clone` / `git fetch`)
///    — closes the master-trunk regression where a repo whose default
///    branch is `master` was always fetched as `origin/main`.
/// 3. The literal `"origin/main"` as a last resort. Used only for a
///    non-repo / unconfigured path so the spawn path never blocks.
///
/// Extracted so the regression test can call it directly without standing
/// up the full async / PTY / DB machinery — the call site is a single
/// expression.
///
/// `pub(crate)` so the background mesh sync (`services::pool_worker`)
/// resolves its fetch target through the exact same 3-tier chain the spawn
/// uses — a worker that fetched a literal `origin/main` on a repo whose
/// default branch is `master` would fail every pass and never satisfy the
/// spawn-time freshness TTL.
pub(crate) fn resolve_base_ref_for_spawn(mesh_path: &str, config_base_ref: Option<&str>) -> String {
    const COALESCE_DEFAULT: &str = "origin/main";
    let user_set = config_base_ref.filter(|b| b.trim() != COALESCE_DEFAULT);
    if let Some(b) = user_set {
        let trimmed = b.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    // No explicit config (or the COALESCE sentinel) — read the repo's
    // actual default branch from `refs/remotes/origin/HEAD` (populated by
    // `git clone` / `git fetch`). `get_default_branch` falls back to
    // "main" if the repo can't be opened or the symbolic ref is missing,
    // so a non-repo / unconfigured mesh path still resolves to
    // "origin/main" — preserving pre-fix behaviour and never blocking the
    // spawn.
    //
    // Cheap repo-open + symbolic-ref read: use the sync core directly
    // (issue #762). Offloading this single lookup onto the blocking pool
    // would cost more than the work itself; the spawn pipeline already
    // `spawn_blocking`s the later git fetch / provision steps.
    let branch = crate::commands::git::get_default_branch_blocking(mesh_path.to_string())
        .unwrap_or_else(|_| "main".to_string());
    format!("origin/{}", branch)
}

/// Load the node, resolve worktree policy and paths, and claim the session
/// for the rest of the pipeline.
pub(super) async fn prepare_context(
    app: &tauri::AppHandle,
    opts: SpawnOptions,
    timer: &SpawnTimer,
) -> Result<PrepareOutcome, String> {
    let SpawnOptions {
        session_id,
        provider,
        resume,
        rows,
        cols,
        prefill,
        node: preloaded_node,
        explicit_model,
        explicit_effort,
        explicit_extra_args,
        worktree_policy,
    } = opts;

    // 0. Claim the session for the WHOLE pipeline. `is_agent_already_running`
    //    below only sees registered processes, and registration is seconds
    //    away (git fetch + worktree provisioning) — without this claim a
    //    concurrent duplicate call (backend stage-2 vs frontend Terminal
    //    auto-spawn) passes that check and its step-2 stale-kill destroys
    //    THIS call's freshly-booted process. Returning Ok mirrors the
    //    already-running short-circuit: the node is being brought up, the
    //    caller has nothing further to do.
    let claim = match SpawnInFlightClaim::try_claim(session_id) {
        Some(claim) => claim,
        None => {
            tracing::info!(
                "spawn_agent_inner: spawn already in flight for session {}, skipping duplicate call",
                session_id
            );
            return Ok(PrepareOutcome::Skipped);
        }
    };

    // 1. Check if already running
    if is_agent_already_running(&session_id) {
        return Ok(PrepareOutcome::Skipped);
    }

    // 2. Kill any stale process for this session
    tracing::debug!(
        "spawn_agent_inner: killing stale processes for session {}",
        session_id
    );
    crate::agent::process::kill_agent(session_id).await.ok();

    // 3. Get node and resolve paths (skip DB read if caller provided the node)
    let node = match preloaded_node {
        Some(n) => n,
        None => db::get_agent_node_by_id(session_id).map_err(|e| {
            let err = format!(
                "spawn_agent: failed to get agent node {}: {}",
                session_id, e
            );
            tracing::error!("{}", err);
            err
        })?,
    };
    tracing::info!(
        "spawn_agent_inner: node path={}, env={:?}",
        node.path,
        node.env
    );
    timer.checkpoint("after_node_db_read");

    let adapter = provider.adapter();

    // 4. Determine session ID mode
    let session_id_mode = if adapter.supports_resume() {
        match resume {
            Some(ref id) if !id.is_empty() => SessionIdMode::Resume(id.clone()),
            _ => {
                if adapter.self_assigns_session_id() {
                    SessionIdMode::None
                } else {
                    let cli_uuid = uuid::Uuid::new_v4().to_string();
                    db::update_cli_session_id(session_id, &cli_uuid).map_err(|e| e.to_string())?;
                    tracing::info!("spawn_agent_inner: assigned cli_session_id={}", cli_uuid);
                    SessionIdMode::Assign(cli_uuid)
                }
            }
        }
    } else {
        SessionIdMode::None
    };

    // 5. Read mesh row for use_worktree / worktree_mode (legacy
    // model/effort columns are no longer read as active spawn
    // configuration — the v33 migration copied any non-empty legacy
    // values into the new map; see issue #1151 acceptance criteria 6).
    let row = env::mesh_row(&std::path::PathBuf::from(&node.path));
    let use_worktree = row.as_ref().map(|r| r.use_worktree).unwrap_or(true);
    // OS-level sandbox toggle (macOS Seatbelt #497, Windows restricted token #498 / ADR-0014).
    // Off by default; the per-OS spawn policy is decided in `spawn_environment::wrap`
    // and `crate::sandbox::spawn::spawn_sandboxed`.
    let sandbox = row.as_ref().map(|r| r.sandbox).unwrap_or(false);
    let worktree_mode = row
        .as_ref()
        .and_then(|r| r.worktree_mode.as_deref())
        .unwrap_or(DEFAULT_WORKTREE_MODE);
    // Autopilot enforcement (issue #482, PRD #480): auto-spawned nodes must
    // always work on a real branch (and in a worktree) — the wrap-up sequence
    // pushes a branch and opens a PR, which a detached-HEAD worktree or a
    // shared mesh root cannot do. The ledger row is written before stage-2
    // starts, so this read is ordered correctly. The node row itself already
    // carries `use_worktree = true` (spawn override in `services::autopilot`).
    let is_autopilot = db::get_autopilot_run(session_id).ok().flatten().is_some();
    let force_branched = matches!(worktree_policy, WorktreePolicy::ForceBranched);
    let use_worktree = use_worktree || is_autopilot || force_branched;
    let worktree_mode = if is_autopilot || force_branched {
        "branched"
    } else {
        worktree_mode
    };
    let base_ref =
        resolve_base_ref_for_spawn(&node.path, row.as_ref().and_then(|r| r.base_ref.as_deref()));

    timer.checkpoint("after_mesh_row_read");

    // 6. Compute spawn path. The pool claim (issue #609/#612) decides whether
    //    the spawn adopts a pre-warmed worktree (Manual: pool slug IS the
    //    node name; Issue/PR: `git worktree move` the pool dir onto the
    //    `gh{N}-`/`pr{N}-` target) or falls through to a cold create. A
    //    claim failure is non-fatal — the spawn falls back to cold; it
    //    only fails on an actual worktree-create error.
    let mesh_id = db::get_mesh_by_path(&node.path).map(|m| m.id).unwrap_or(-1);
    // `is_rename_spawn` selects between the two warm-pool adoption modes
    // downstream: Manual adopts the pool's slug as the node name (issue #609);
    // Issue/PR keep their own `gh{N}-`/`pr{N}-` name and move the pool dir
    // to match (issue #612). Consumed by the post-spawn name adoption
    // (further below) and by the SpawnContext built at phase 7.
    let is_rename_spawn = node.source_issue.is_some() || node.source_pr.is_some();
    let mut warm_claimed: Option<crate::services::warm_pool::ClaimedWarmEntry> = None;
    // Issue #653: a successful `try_claim` that the use-site recheck later
    // dropped still drained the pool by one row — `warm_claimed` is None
    // (the spawn fell back to cold), but the mesh's pool inventory is one
    // short. Track "we claimed at least once this spawn" so the post-spawn
    // refill still fires (otherwise the pool stays at target-1 until the
    // next reconcile). Distinct from `warm_claimed` because `warm_claimed`
    // tracks "we adopted the warm entry as this node's worktree" — that's
    // what `forget_after_spawn` and the manual name adoption gate on.
    let mut pool_was_drained_by_this_spawn = false;
    if use_worktree {
        // The path the node resolves to WITHOUT a pool claim. If it's already
        // on disk this spawn is a resume / handover / re-spawn reusing an
        // existing worktree — never claim a pool entry for it (that would
        // re-point the node at a different directory and abandon its work).
        let existing = env::resolve_agent_path(&node.path, node.worktree_name.as_deref());
        let existing_present = std::path::Path::new(&existing.host_path).exists();
        if mesh_id > 0 && crate::services::warm_pool::should_claim_for_spawn(existing_present) {
            match crate::services::warm_pool::try_claim(app, mesh_id) {
                Ok(Some(entry)) => {
                    tracing::info!(
                        "spawn_agent_inner: claimed warm pool entry id={} path={} slug={} base_sha={}",
                        entry.id,
                        entry.path,
                        entry.preassigned_name,
                        entry.base_sha.as_deref().unwrap_or("none"),
                    );
                    // Issue #653 use-site guard: `try_claim` just checked
                    // the directory exists, but the spawn then waits
                    // seconds inside `fetch_origin` + git worktree move;
                    // another thread can delete the directory in that gap.
                    // Re-check immediately before committing to the warm
                    // path. On false, `recheck_after_claim` already dropped
                    // the row + tombstone; we just leave `warm_claimed`
                    // None so the existing `spawn_worktree_name` fallback
                    // resolves to the throwaway slug and the cold-create
                    // block runs naturally for both spawn modes (Issue/PR
                    // and manual).
                    if crate::services::warm_pool::recheck_after_claim(entry.id, &entry.path) {
                        warm_claimed = Some(entry);
                        pool_was_drained_by_this_spawn = true;
                    } else {
                        // Note: `recheck_after_claim` already logs the
                        // reason (claimed row N's directory disappeared...),
                        // so don't duplicate that WARN here.
                        // warm_claimed stays None — do NOT adopt. The row
                        // was already dropped by recheck_after_claim, but
                        // the pool inventory is still down by one; the
                        // post-spawn refill below must run regardless of
                        // the local `did_claim_warm` flag (which checks
                        // `warm_claimed.is_some()`, not the DB).
                        pool_was_drained_by_this_spawn = true;
                    }
                }
                Ok(None) => {
                    tracing::info!(
                        "spawn_agent_inner: warm pool empty for mesh {}; cold spawn",
                        mesh_id
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "spawn_agent_inner: warm pool claim failed (non-fatal, falling back to cold): {}",
                        e
                    );
                }
            }
        }
    }

    // The effective spawn_worktree_name + path.
    //
    //  * Manual warm claim (`!is_rename_spawn`): adopt the pool's preassigned
    //    slug as the node's `worktree_name`, so the rest of the pipeline
    //    resolves straight onto the already-on-disk pool directory (#609).
    //  * Issue/PR warm claim (`is_rename_spawn`): keep the node's own
    //    `gh{N}-`/`pr{N}-` `worktree_name`. It resolves to a path that does
    //    NOT exist yet, so we enter the cold-create block below — where the
    //    PR-head fetch runs — and there `git worktree move` the pool directory
    //    onto this target instead of a cold `git worktree add` (#612).
    //  * No claim: fall back to whatever the node row carries (resumes, or a
    //    cold issue/PR spawn).
    //
    // Owned (`Option<String>`, not `Option<&str>`) on purpose: the Issue/PR
    // path mutates `warm_claimed` (take / re-assign) inside the worktree block
    // below, so `spawn_worktree_name` must not hold a borrow into it. The slugs
    // are short, so the clone is negligible.
    let spawn_worktree_name: Option<String> = if let Some(ref entry) = warm_claimed {
        if is_rename_spawn {
            node.worktree_name.clone()
        } else {
            Some(entry.preassigned_name.clone())
        }
    } else if use_worktree {
        node.worktree_name.clone()
    } else {
        tracing::info!("spawn_agent_inner: use_worktree=false, using repo root directly");
        None
    };

    let resolved = env::resolve_agent_path(&node.path, spawn_worktree_name.as_deref());
    tracing::info!(
        "spawn_agent_inner: resolved spawn_path={}, host_path={}, env={:?}",
        resolved.spawn_path,
        resolved.host_path,
        resolved.env_type
    );

    // For a Manual warm claim, the pool's preassigned slug IS the node's
    // `worktree_name` once the spawn completes — the post-spawn DB write
    // (below, before `register_agent`) persists that, but `provision_for_spawn`
    // needs the right branch name in the Spawn Context NOW so the manual
    // `Upgraded` branch's `git checkout -B <branch>` targets the pool's slug
    // rather than the node's stage-1 throwaway. Mutate `node.worktree_name`
    // in place here; `node.clone()` carries the value into the Spawn Context.
    let mut node = node;
    if let (false, Some(ref entry)) = (is_rename_spawn, &warm_claimed) {
        node.worktree_name = Some(entry.preassigned_name.clone());
    }

    Ok(PrepareOutcome::Ready {
        claim,
        spawn: Box::new(PreparedSpawn {
            session_id,
            provider,
            rows,
            cols,
            prefill,
            explicit_model,
            explicit_effort,
            explicit_extra_args,
            node,
            session_id_mode,
            use_worktree,
            sandbox,
            worktree_mode: worktree_mode.to_string(),
            base_ref,
            mesh_id,
            warm_claimed,
            pool_was_drained_by_this_spawn,
            spawn_worktree_name,
            resolved,
        }),
    })
}
