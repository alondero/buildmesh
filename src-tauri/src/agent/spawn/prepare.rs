//! Prepare-context phase of Agent Node spawn.
//!
//! Resolves everything the later phases need before touching git or a PTY:
//! the node row, session-id mode, mesh worktree policy, optional warm-pool
//! claim, and the host/spawn paths. The orchestrator acquires the in-flight
//! claim *before* calling this function and holds it across every phase.

use super::launch::LaunchParams;
use super::process::is_agent_already_running;
use super::provision::WorkspaceToProvision;
use super::reader::{SessionIdMode, SpawnTimer};
use super::WorktreePolicy;
use crate::models::{AgentNode, Provider};
use crate::{db, env};

/// Default `worktree_mode` when the mesh config leaves it unset. Pinned by
/// `default_worktree_mode_is_branched` in `prepare_tests.rs`.
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
///
/// `spawn_with_intent` acquires this before identity mutation and binds
/// it as a named local (`claim`, never `_claim`) so a future match cannot
/// drop it at the prepare seam and reopen the #650 race.
#[must_use = "dropping the claim releases the in-flight spawn slot"]
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
    /// absent at [`super::command::cascade_inputs_for`] so the cascade
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

/// Workspace inputs + launch knobs produced by prepare. Boxed in
/// [`PrepareOutcome::Ready`] so the Skipped unit variant doesn't trip
/// `large_enum_variant`.
pub(super) struct PreparedPhases {
    pub workspace: WorkspaceToProvision,
    pub launch: LaunchParams,
}

pub(super) enum PrepareOutcome {
    /// A live process already occupies the node (the orchestrator's
    /// in-flight claim still covers the skip so a racer cannot sneak in).
    Skipped,
    Ready(Box<PreparedPhases>),
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

/// Load the node, resolve worktree policy and paths. The orchestrator
/// The caller (`spawn_with_intent`) must already hold [`SpawnInFlightClaim`]
/// for `opts.session_id`.
pub(super) async fn prepare_context(
    app: &tauri::AppHandle,
    opts: SpawnOptions,
    timer: &SpawnTimer,
) -> Result<PrepareOutcome, String> {
    debug_assert!(
        SPAWNS_IN_FLIGHT.lock().contains(&opts.session_id),
        "prepare_context requires spawn_with_intent to hold SpawnInFlightClaim"
    );

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

    if is_agent_already_running(&session_id) {
        return Ok(PrepareOutcome::Skipped);
    }

    tracing::debug!(
        "prepare_context: killing stale processes for session {}",
        session_id
    );
    crate::agent::process::kill_agent(session_id).await.ok();

    // Load the node (skip the DB read if the caller already has it).
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
        "prepare_context: node path={}, env={:?}",
        node.path,
        node.env
    );
    timer.checkpoint("after_node_db_read");

    let adapter = provider.adapter();

    // Session-id mode: Assign writes the UUID before launch; Resume
    // reuses the captured id; None leaves capture to the adapter.
    let session_id_mode = if adapter.supports_resume() {
        match resume {
            Some(ref id) if !id.is_empty() => SessionIdMode::Resume(id.clone()),
            _ => {
                if adapter.self_assigns_session_id() {
                    SessionIdMode::None
                } else {
                    let cli_uuid = uuid::Uuid::new_v4().to_string();
                    db::update_cli_session_id(session_id, &cli_uuid).map_err(|e| e.to_string())?;
                    tracing::info!("prepare_context: assigned cli_session_id={}", cli_uuid);
                    SessionIdMode::Assign(cli_uuid)
                }
            }
        }
    } else {
        SessionIdMode::None
    };

    // Mesh row for use_worktree / worktree_mode (legacy
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

    // Spawn path. The pool claim (issue #609/#612) decides whether
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
    // (in the provisioner) and by the SpawnContext built during provision.
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
        // Issue #1519: respects the persisted `worktree_path` (immutable) so
        // legacy rows keep their original legacy location and new rows keep
        // their creation-time effective dir.
        let existing = env::node_working_path(&node);
        let existing_present = std::path::Path::new(&existing.host_path).exists();
        if mesh_id > 0 && crate::services::warm_pool::should_claim_for_spawn(existing_present) {
            match crate::services::warm_pool::try_claim(app, mesh_id) {
                Ok(Some(entry)) => {
                    tracing::info!(
                        "prepare_context: claimed warm pool entry id={} path={} slug={} base_sha={}",
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
                        "prepare_context: warm pool empty for mesh {}; cold spawn",
                        mesh_id
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "prepare_context: warm pool claim failed (non-fatal, falling back to cold): {}",
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
    //    NOT exist yet; provision then `git worktree move`s the pool
    //    directory onto this target instead of a cold `git worktree add`
    //    (#612), after the PR-head fetch.
    //  * No claim: fall back to whatever the node row carries (resumes, or a
    //    cold issue/PR spawn).
    //
    // Owned (`Option<String>`, not `Option<&str>`) on purpose: provision
    // later `take()`s `warm_claimed`, so `spawn_worktree_name` must not
    // hold a borrow into it. The slugs are short, so the clone is
    // negligible.
    let spawn_worktree_name: Option<String> = if let Some(ref entry) = warm_claimed {
        if is_rename_spawn {
            node.worktree_name.clone()
        } else {
            Some(entry.preassigned_name.clone())
        }
    } else if use_worktree {
        node.worktree_name.clone()
    } else {
        tracing::info!("prepare_context: use_worktree=false, using repo root directly");
        None
    };

    // For a Manual warm claim, the pool's preassigned slug IS the node's
    // `worktree_name` once the spawn completes — the provisioner persists
    // that, but `provision_for_spawn` needs the right branch name in the
    // Spawn Context NOW so the manual `Upgraded` branch's `git checkout -B
    // <branch>` targets the pool's slug rather than the node's stage-1
    // throwaway. Mutate `node.worktree_name` in place here; the node
    // travels into WorkspaceToProvision.
    // Issue #1519: align the in-memory row with the claimed pool directory
    // so the single resolver below lands on it. The pool row holds the
    // HOST path (UNC on WSL) while `worktree_path` stores the RAW form, so
    // normalize UNC back to POSIX here — the DB adoption then persists the
    // same raw value `node_working_path` resolves.
    let mut node = node;
    if let (false, Some(ref entry)) = (is_rename_spawn, &warm_claimed) {
        node.worktree_name = Some(entry.preassigned_name.clone());
        node.worktree_path = Some(crate::env::normalize_unc_to_wsl(&entry.path).into_owned());
    }

    // Issue #1519: the node row is now authoritative for every spawn kind —
    // manual claims carry the adopted slug + normalized path above, Issue/PR
    // claims keep their own `gh{N}-`/`pr{N}-` identity whose stored path is
    // the move target, and unclaimed spawns resolve from the persisted
    // `worktree_path` (or the legacy fallback for pre-#1519 rows). One
    // resolver, no per-source branches: a second path here once fed the raw
    // host UNC form into `resolve_raw_path` and produced a UNC `spawn_path`
    // that `wsl.exe --cd` rejects on Windows.
    let resolved = env::node_working_path(&node);
    tracing::info!(
        "prepare_context: resolved spawn_path={}, host_path={}, env={:?}",
        resolved.spawn_path,
        resolved.host_path,
        resolved.env_type
    );

    let harness_id = node.provider.clone();
    let node_mesh_id = node.mesh_id;
    Ok(PrepareOutcome::Ready(Box::new(PreparedPhases {
        workspace: WorkspaceToProvision {
            session_id,
            provider,
            node,
            use_worktree,
            worktree_mode: worktree_mode.to_string(),
            base_ref,
            warm_claimed,
            pool_was_drained_by_this_spawn,
            spawn_worktree_name,
            resolved,
        },
        launch: LaunchParams {
            rows,
            cols,
            prefill,
            explicit_model,
            explicit_effort,
            explicit_extra_args,
            harness_id,
            node_mesh_id,
            registry_mesh_id: mesh_id,
            session_id_mode,
            sandbox,
        },
    })))
}
