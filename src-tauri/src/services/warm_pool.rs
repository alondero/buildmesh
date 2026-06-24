//! Pre-spawn Worktree Pool — the v21 tracer bullet (issue #609, PRD #608).
//!
//! Why this exists
//! ---------------
//! A cold worktree creation takes ~11s on a 400MB repo because Windows
//! Defender / NTFS search indexer / USN journal scan every freshly-written
//! file. ~97% of that time is the `git worktree add` checkout phase (not the
//! network `git fetch`). The pool keeps a detached-HEAD worktree already
//! checked out for every worktree-enabled mesh, so a manual spawn only has
//! to flip the worktree's mode (branched vs. detached) and start the agent —
//! no cold NTFS write cost, sub-500ms checkout in practice.
//!
//! Scope of the v21 tracer bullet
//! ------------------------------
//! * Exactly **1** warm entry per worktree-enabled mesh. Manual spawns adopt
//!   the preassigned slug as the node name (zero rename overhead). A
//!   successful claim triggers a background refill so the pool re-fills
//!   before the next spawn.
//! * Issue / PR spawns are NOT claimed — they would need a `git worktree
//!   move` rename (~50ms), and the user-facing benefit is dwarfed by the
//!   implementation cost. They're routed through the existing cold path.
//!   The PRD scopes this to a follow-up issue.
//! * Background refresh of stale SHAs (issue #608 §4) is NOT in this
//!   tranche. The pool worker just cuts the worktree once and trusts the
//!   spawn-time `git checkout -B` to bring it to the requested ref.
//!
//! Module layout
//! -------------
//! `reconcile_on_startup` — called from `lib.rs::setup`, runs after
//! `db::init`. Prunes stale rows (their on-disk dir is gone), then walks
//! every worktree-enabled mesh and ensures at least one `available` entry.
//!
//! `refill_after_claim` — kicked off by the spawn path immediately after a
//! successful claim so the pool is back at target by the next spawn. Runs
//! on `tokio::task::spawn_blocking` because it shells out to `git`.
//!
//! `prewarm_one` — does the actual on-disk work for one mesh. Idempotent:
//! if a row already points at the directory, the call is a no-op. The
//! PR-style `git fetch` step is intentionally omitted for the v21 tracer
//! — the spawn path's existing `git::sync::fetch_origin` keeps things fresh
//! just before a claim lands.

use crate::db::{self, WarmWorktreeStatus};
use crate::session_naming::on_spawn;
use std::path::Path;

/// Hardcoded pool target for the v21 tracer bullet. The PRD scopes per-mesh
/// `pre_spawn_pool_size` to a later phase (the column is intentionally not
/// added yet — see issue #608 §1.1). One warm entry per mesh is enough to
/// prove the design; the worker fills until it hits this number, then stands
/// down.
pub const POOL_TARGET_PER_MESH: i64 = 1;

/// Decide whether the spawn path should consult the pool for a given node.
///
/// Eligible == a **fresh manual worktree spawn**:
///   * no `source_pr` and no `source_issue`. Issue/PR spawns need the warm
///     entry renamed to `gh{N}-<slug>` / `pr{N}-<slug>` (a `git worktree
///     move`), which is out of scope for this tranche — the PRD scopes it
///     to a follow-up. They're served by the cold path.
///   * `existing_worktree_present == false` — the node's own worktree
///     directory is NOT already on disk. Resume / handover / re-spawn paths
///     re-enter `spawn_agent_inner` with a `worktree_name` whose directory
///     the original spawn already created; claiming a pool entry for one of
///     them would re-point the node at a *different* directory and abandon
///     the agent's existing work. The cold path keys its own "create the
///     worktree?" decision off the same `!host_path.exists()` check, so the
///     two stay in lockstep: if the cold path would create a worktree, the
///     pool is allowed to satisfy that creation; if it would reuse one, the
///     pool stays out of the way.
///
/// `existing_worktree_present` is computed by the caller from
/// `env::resolve_agent_path(node.path, node.worktree_name)` — the path the
/// node resolves to WITHOUT a pool claim.
///
/// Returns `false` for any spawn the cold path must serve. The caller falls
/// back to a cold `create_git_worktree` on `false` — exactly what it would
/// do if the pool were empty.
pub fn should_claim_for_spawn(
    node: &crate::models::AgentNode,
    existing_worktree_present: bool,
) -> bool {
    if node.source_pr.is_some() {
        return false;
    }
    if node.source_issue.is_some() {
        return false;
    }
    !existing_worktree_present
}

/// What the spawn path needs to adopt a warm entry as a fresh node.
///
/// `path` and `preassigned_name` flow into `agent_nodes.path` and
/// `agent_nodes.worktree_name` respectively; `id` is the DB row id so the
/// spawn can `delete_warm_worktree(id)` after a successful claim (the row
/// outlives its purpose once the directory becomes a node's worktree).
/// `base_sha` is informational for the spawn-time freshness check.
#[derive(Debug, Clone)]
pub struct ClaimedWarmEntry {
    pub id: i64,
    pub path: String,
    pub preassigned_name: String,
    pub base_sha: Option<String>,
}

/// Try to claim a warm entry for `mesh_id`. Returns `None` when the pool is
/// empty / mid-fill — caller falls back to cold `create_git_worktree`.
///
/// After the atomic DB claim, the claimed row's own `path` is existence-
/// checked on disk to protect against claiming a row whose `git worktree
/// add` was rolled back by a crash between row insert and checkout
/// completion; a missing directory drops the row and reports `None`.
pub fn try_claim(mesh_id: i64) -> Result<Option<ClaimedWarmEntry>, String> {
    let claimed = match db::claim_warm_entry_for_mesh(mesh_id) {
        Ok(Some(row)) => row,
        Ok(None) => return Ok(None),
        Err(e) => return Err(format!("warm_pool claim db error: {}", e)),
    };

    // Stale-row guard: a previous crash might have left a row marked
    // `available` whose on-disk directory is gone (the worker wrote the row
    // first then crashed mid-checkout, OR the user deleted the
    // `.claude/worktrees/<slug>` directory by hand). In that case the
    // claim succeeded at the DB layer but the directory isn't there — drop
    // the row and report `None` so the spawn falls back to cold.
    if !Path::new(&claimed.path).exists() {
        tracing::warn!(
            "warm_pool: claimed row {} points at missing directory {}; dropping and falling back to cold",
            claimed.id,
            claimed.path
        );
        let _ = db::delete_warm_worktree(claimed.id);
        return Ok(None);
    }

    Ok(Some(ClaimedWarmEntry {
        id: claimed.id,
        path: claimed.path,
        preassigned_name: claimed.preassigned_name,
        base_sha: claimed.base_sha,
    }))
}

/// Drop a warm pool row after a successful spawn. The directory now lives
/// on as the node's worktree — the bookkeeping row's job is done.
///
/// Safe to call from the spawn path's success branch: a failure here just
/// leaves an orphan `claimed` row that the next startup reconcile prunes
/// (its `path` exists, so the row stays; but `status = 'claimed'` is never
/// claimed again, and we don't currently GC it — see issue #608 follow-up).
pub fn forget_after_spawn(id: i64) {
    if let Err(e) = db::delete_warm_worktree(id) {
        tracing::warn!(
            "warm_pool: failed to delete row {} after spawn (non-fatal): {}",
            id,
            e
        );
    }
}

/// Compose the directory-name slug for a warm entry. Used by the worker (and
/// by tests). It's a plain `session_naming::on_spawn` slug — deliberately NOT
/// prefixed — so a manual spawn can adopt it as BOTH the node's display name
/// and its worktree directory with zero rename (issue #609: directory name ==
/// node name). Pool entries are distinguished from live worktrees by the
/// `warm_worktrees` DB table, not by their on-disk name.
pub(crate) fn fresh_slug() -> String {
    on_spawn()
}

/// Compute the absolute path of the warm worktree for a mesh. Matches
/// `env::resolve_agent_path`'s layout so the spawn path can `resolve_agent_path`
/// the result without special-casing. Uses `std::path::Path::join` on the
/// already-host-converted mesh path so the joined separator matches the host
/// (a hand-rolled `format!("{}/.claude/worktrees/{}", …)` produces a mixed
/// separator string on Windows when the mesh path is `C:\…`).
pub(crate) fn warm_worktree_host_path(mesh_path: &str, slug: &str) -> String {
    use std::path::Path;
    let host_mesh = crate::env::to_host_path(mesh_path);
    // `Path::join` always uses the platform's native separator on the
    // appended segment, so the result is `C:\repo\m\.claude\worktrees\<slug>`
    // on Windows and `/repo/m/.claude/worktrees/<slug>` on POSIX.
    Path::new(&host_mesh)
        .join(".claude")
        .join("worktrees")
        .join(slug)
        .to_string_lossy()
        .into_owned()
}

/// Cut a fresh warm worktree for `mesh` and insert a `warming → available`
/// row pair around it. Idempotent: if a row already points at the computed
/// path, the call is a no-op (so the startup reconcile is safe to re-run).
///
/// Returns `Ok(true)` when a new entry was warmed, `Ok(false)` when the
/// mesh was already at target (no work done). Errors from
/// `create_git_worktree` propagate so the worker can log + skip + try the
/// next mesh on the next reconcile pass.
pub fn prewarm_one(mesh: &db::WarmPoolMeshRow) -> Result<bool, String> {
    let available = match db::count_available_warm_for_mesh(mesh.id) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(
                "warm_pool: count_available for mesh {} failed (treating as 0): {}",
                mesh.id,
                e
            );
            return Ok(false);
        }
    };
    if available >= POOL_TARGET_PER_MESH {
        return Ok(false);
    }

    // Pick a fresh slug. Slug collisions are vanishingly rare (~3.9B combos
    // in the seed pool; #608 follow-up adds the `preassigned_name` UNIQUE
    // guard for the per-mesh case), but if the directory already exists on
    // disk we re-roll up to a small bounded number of times before bailing.
    let mut last_err: Option<String> = None;
    for _ in 0..4 {
        let slug = fresh_slug();
        let host_path = warm_worktree_host_path(&mesh.path, &slug);
        if Path::new(&host_path).exists() {
            // Lost the slug lottery on a stale leftover; retry with a new
            // slug so we don't try to `git worktree add` into an existing
            // path (which would no-op or fail).
            continue;
        }

        // Insert the row as `filling` BEFORE cutting the worktree so a
        // crash mid-checkout leaves a recoverable ghost (startup reconcile
        // deletes rows whose `path` doesn't exist).
        let row_id = db::insert_warm_worktree(
            mesh.id,
            &host_path,
            &slug,
            None,
            WarmWorktreeStatus::Filling,
        )
        .map_err(|e| format!("warm_pool insert filling: {}", e))?;

        // Cut the on-disk worktree. We pin mode = "detached" so a future
        // claim can `git checkout -B <branch>` to upgrade to branched mode
        // (or stay detached if the mesh says so). Detached also avoids
        // touching the mesh's branch refs.
        match crate::git::worktree::create_git_worktree(
            &mesh.path,
            &host_path,
            &slug,
            "detached",
            &mesh.base_ref,
        ) {
            Ok(()) => {
                // Flip to available + stamp the base SHA the worker just
                // checked out at. We compute it from the new worktree's
                // HEAD; failures here are non-fatal — the row stays
                // `available` and the spawn-time freshness check falls
                // back to "no SHA to compare" semantics.
                let base_sha = read_warm_head_sha(&host_path).unwrap_or_default();
                let base_sha_opt = if base_sha.is_empty() {
                    None
                } else {
                    Some(base_sha.as_str())
                };
                if let Err(e) = db::mark_warm_worktree_available(row_id, base_sha_opt) {
                    tracing::warn!(
                        "warm_pool: failed to flip row {} to available: {}",
                        row_id,
                        e
                    );
                }
                tracing::info!(
                    "warm_pool: prewarmed {} for mesh {} ({} available)",
                    slug,
                    mesh.id,
                    available + 1,
                );
                return Ok(true);
            }
            Err(e) => {
                // Roll back the filling row so the reconcile doesn't have
                // to garbage-collect it on the next pass.
                let _ = db::delete_warm_worktree(row_id);
                last_err = Some(e);
                continue;
            }
        }
    }

    Err(last_err.unwrap_or_else(|| "warm_pool: exhausted slug retries".to_string()))
}

/// Read the SHA the warm worktree is checked out at. Empty string on any
/// failure (the row stays `available` with `base_sha = NULL` and the
/// spawn-time freshness check degrades to "no SHA to compare").
fn read_warm_head_sha(worktree_path: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(worktree_path)
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

/// Run the startup reconcile pass: prune stale rows + fill any mesh that
/// is below the per-mesh target.
///
/// Called from `lib.rs::setup` once, after `db::init`. Failures are logged
/// and swallowed — a transient git error on one mesh must not block the
/// rest of the reconcile or the rest of app startup.
pub fn reconcile_on_startup() {
    // Step 1a: prune rows stuck in `claimed` after a previous crash (the
    // spawn path's `forget_after_spawn` failed after a successful spawn,
    // leaving the row alive with its directory present — the next claim
    // would skip it because the `status='available'` filter rejects it,
    // so without this prune the row leaks forever).
    match db::delete_orphaned_claimed_warm_worktrees() {
        Ok(n) if n > 0 => tracing::info!(
            "warm_pool: pruned {} orphan `claimed` row(s) from a prior crash",
            n
        ),
        Ok(_) => {}
        Err(e) => tracing::warn!("warm_pool: claimed-row prune failed: {}", e),
    }

    // Step 1b: prune rows whose directory is gone. These are crashes /
    // user-deletes between row-insert and the spawn-time claim — the row
    // would block a future `claim_warm_entry_for_mesh` (it'd return a
    // stale path that the spawn path then drops), but pruning early keeps
    // `list_worktree_enabled_meshes_for_warm`'s bookkeeping clean.
    match db::list_stale_warm_worktrees() {
        Ok(stale) => {
            for id in stale {
                if let Err(e) = db::delete_warm_worktree(id) {
                    tracing::warn!("warm_pool: failed to prune stale row {}: {}", id, e);
                } else {
                    tracing::info!("warm_pool: pruned stale row {}", id);
                }
            }
        }
        Err(e) => tracing::warn!("warm_pool: stale-row scan failed: {}", e),
    }

    // Step 2: walk every worktree-enabled mesh and fill to target. The
    // worker is best-effort: a single mesh failing to warm does not stop
    // the next mesh.
    let meshes = match db::list_worktree_enabled_meshes_for_warm() {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("warm_pool: list meshes failed: {}", e);
            return;
        }
    };
    for mesh in meshes {
        if let Err(e) = prewarm_one(&mesh) {
            tracing::warn!(
                "warm_pool: prewarm failed for mesh {} ({}): {}",
                mesh.id,
                mesh.path,
                e
            );
        }
    }
}

/// Background refill: re-warm a mesh after a successful claim so the pool
/// is back at target by the next spawn. Called from the spawn path's
/// success branch via `tokio::task::spawn_blocking`.
///
/// Failures are non-fatal and logged — the next startup reconcile (or the
/// next post-claim refill) will retry.
pub fn refill_after_claim(mesh_id: i64) {
    let mesh = match db::list_worktree_enabled_meshes_for_warm() {
        Ok(rows) => rows.into_iter().find(|m| m.id == mesh_id),
        Err(e) => {
            tracing::warn!("warm_pool: refill list failed for mesh {}: {}", mesh_id, e);
            return;
        }
    };
    let Some(mesh) = mesh else {
        // Mesh was disabled or deleted between the spawn and the refill —
        // nothing to do.
        return;
    };
    if let Err(e) = prewarm_one(&mesh) {
        tracing::warn!(
            "warm_pool: refill failed for mesh {} ({}): {}",
            mesh.id,
            mesh.path,
            e
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the slug format: a plain `on_spawn` adj-adj-noun slug with NO
    /// prefix. The manual-spawn fast path adopts this slug as the node's
    /// display name, so a refactor that re-introduced a `pool-warm-` prefix
    /// (or otherwise made the slug un-adoptable as a user-facing name) must
    /// surface as a test failure, not a confusing node name in the UI.
    #[test]
    fn fresh_slug_is_a_plain_adoptable_slug() {
        let slug = fresh_slug();
        assert!(
            !slug.starts_with("pool-warm-"),
            "fresh slug must NOT carry an implementation-detail prefix (issue #609 full name adoption), got `{}`",
            slug
        );
        // Three-word hyphenated slug (matches `on_spawn`'s adj-adj-noun) so it
        // is a valid node name AND a valid git branch / directory name.
        assert!(
            slug.split('-').count() == 3,
            "slug must be three hyphenated words, got `{}`",
            slug
        );
    }

    /// `warm_worktree_host_path` mirrors `env::resolve_agent_path`'s layout
    /// exactly so the spawn-time resolution is a no-op on the claimed path.
    #[test]
    fn warm_worktree_host_path_matches_resolve_agent_path_layout() {
        let path = warm_worktree_host_path("/repo/my-mesh", "bold-amber-fox");
        // Either forward-slash (POSIX host) or backslash (Windows host) is
        // acceptable as long as the layout matches `resolve_agent_path`.
        let normalized = path.replace('\\', "/");
        assert_eq!(
            normalized, "/repo/my-mesh/.claude/worktrees/bold-amber-fox",
            "warm pool path must follow the same layout env::resolve_agent_path uses"
        );
    }

    // ---- should_claim_for_spawn (the activation gate) ----
    //
    // These pin the fix for the activation bug: the gate originally returned
    // `node.worktree_name.is_none()`, but `agent_node::create` always assigns
    // a slug to a worktree-enabled node, so the pool was never claimed. The
    // gate now keys off whether the node's CURRENT worktree directory is
    // already on disk — `false` ⇒ fresh spawn (claim), `true` ⇒ resume /
    // re-spawn reusing an existing worktree (don't claim).

    fn node_with(source_pr: Option<i64>, source_issue: Option<i64>) -> crate::models::AgentNode {
        crate::models::AgentNode {
            path: "/repo/m".to_string(),
            use_worktree: true,
            // A worktree-enabled node always carries a slug (set at create
            // time); the gate no longer cares about its value, only about
            // whether the directory exists.
            worktree_name: Some("gentle-amber-fox".to_string()),
            source_pr,
            source_issue,
            ..Default::default()
        }
    }

    #[test]
    fn claims_for_fresh_manual_spawn_when_worktree_absent() {
        let node = node_with(None, None);
        assert!(
            should_claim_for_spawn(&node, false),
            "a fresh manual spawn (worktree dir not yet on disk) must be claim-eligible"
        );
    }

    #[test]
    fn does_not_claim_when_worktree_already_present() {
        // Resume / handover / re-spawn: the node's worktree already exists on
        // disk. Claiming would re-point it at a different directory and
        // abandon the agent's work.
        let node = node_with(None, None);
        assert!(
            !should_claim_for_spawn(&node, true),
            "a spawn reusing an existing on-disk worktree must NOT be claim-eligible"
        );
    }

    #[test]
    fn does_not_claim_pr_or_issue_spawns() {
        // Issue/PR spawns need a renamed directory (out of scope for v21) —
        // never claimed, even when their worktree dir is absent.
        assert!(
            !should_claim_for_spawn(&node_with(Some(420), None), false),
            "PR spawns must route through the cold path"
        );
        assert!(
            !should_claim_for_spawn(&node_with(None, Some(609)), false),
            "issue spawns must route through the cold path"
        );
    }
}