// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;
use crate::services::warm_pool_backoff::FillBackoff;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---- event name drift guard ----
//
// The `pool-count-changed` event is the IPC contract with the React
// `usePoolChanged` hook. The TS side exports `POOL_COUNT_CHANGED_EVENT`
// as a single source of truth — this test pins the Rust half so a
// future rename here (e.g. `pool-changed`, `warm-pool-changed`) fails
// loudly instead of silently breaking the badge's live updates.
// Symmetric test in `tests/unit/usePoolChanged.test.tsx`.

#[test]
fn pool_count_changed_event_name_matches_frontend_constant() {
    assert_eq!(
        POOL_COUNT_CHANGED_EVENT, "pool-count-changed",
        "the event name must match `POOL_COUNT_CHANGED_EVENT` in src/hooks/usePoolChanged.ts — \
         rename in lockstep on both sides"
    );
}

// ---- reconcile_warm_entries (the teardown + row-delete orchestration) ----
//
// These pin the two invariants the issue #610 review flagged as untested:
// teardown precedes row-delete, and the row survives a teardown failure so
// a future startup can retry rather than orphaning the directory forever.

fn entry(id: i64, dir_present: bool) -> db::WarmReconcileEntry {
    db::WarmReconcileEntry {
        id,
        path: format!("/repo/m/.claude/worktrees/slug-{}", id),
        dir_present,
        reason: db::WarmReconcileReason::CrashedFill,
    }
}

/// A live-directory entry: `remove` must be called BEFORE `delete_row`, and
/// the row must be deleted once teardown succeeds. The shared log proves the
/// ordering the doc comment promises (teardown-before-delete) — a refactor
/// that flipped the two calls would orphan the directory on a delete failure.
#[test]
fn reconcile_removes_worktree_before_deleting_row() {
    let log: Mutex<Vec<String>> = Mutex::new(Vec::new());
    reconcile_warm_entries(
        vec![entry(7, true)],
        |path| {
            log.lock().unwrap().push(format!("remove:{}", path));
            Ok(())
        },
        |id| {
            log.lock().unwrap().push(format!("delete:{}", id));
            Ok(())
        },
    );
    let log = log.lock().unwrap();
    assert_eq!(
        *log,
        vec![
            "remove:/repo/m/.claude/worktrees/slug-7".to_string(),
            "delete:7".to_string()
        ],
        "git teardown must run before the row delete"
    );
}

/// Teardown failure ⇒ the row is KEPT (delete_row never called) so the next
/// startup retries. Deleting it here would orphan the partial worktree
/// directory forever — the exact leak the review caught.
#[test]
fn reconcile_keeps_row_when_teardown_fails() {
    let deleted: Mutex<Vec<i64>> = Mutex::new(Vec::new());
    reconcile_warm_entries(
        vec![entry(9, true)],
        |_path| Err("worktree locked".to_string()),
        |id| {
            deleted.lock().unwrap().push(id);
            Ok(())
        },
    );
    assert!(
        deleted.lock().unwrap().is_empty(),
        "a failed teardown must NOT delete the row (it would orphan the on-disk worktree)"
    );
}

/// A row whose directory is already gone skips teardown entirely and is
/// dropped directly — there is nothing on disk to remove.
#[test]
fn reconcile_skips_teardown_for_missing_directory() {
    let removed: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let deleted: Mutex<Vec<i64>> = Mutex::new(Vec::new());
    reconcile_warm_entries(
        vec![entry(3, false)],
        |path| {
            removed.lock().unwrap().push(path.to_string());
            Ok(())
        },
        |id| {
            deleted.lock().unwrap().push(id);
            Ok(())
        },
    );
    assert!(
        removed.lock().unwrap().is_empty(),
        "a ghost row (directory already gone) must not invoke the git teardown"
    );
    assert_eq!(
        *deleted.lock().unwrap(),
        vec![3],
        "a ghost row must still have its bookkeeping row dropped"
    );
}

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

/// Warm pools use the same effective container as newly placed nodes.
#[test]
fn warm_worktree_host_path_uses_default_worktrees_container() {
    // No per-Mesh override and no app default (preferences uninitialised
    // in tests reads as `None`) → `.worktrees` container.
    let mesh = mesh_row(99, 1);
    let path = warm_worktree_host_path_for_mesh(&mesh, "bold-amber-fox");
    // Either forward-slash (POSIX host) or backslash (Windows host) is
    // acceptable as long as the default layout matches node placement.
    let normalized = path.replace('\\', "/");
    assert!(
        normalized.ends_with(".worktrees/bold-amber-fox"),
        "warm pool path must use the default container, got {normalized}"
    );
}

// ── Configurable directories (issue #1519) ──────────────────────────────

#[test]
fn warm_host_path_in_dir_joins_effective_dir_and_slug() {
    let p = warm_worktree_host_path_in_dir("/repo/mesh/custom-wt", "my-node");
    assert_eq!(p.replace('\\', "/"), "/repo/mesh/custom-wt/my-node");
    // Slug trimmed so a padded preassigned name never creates a spaced dir.
    let p = warm_worktree_host_path_in_dir("/repo/mesh/custom-wt/", "  my-node  ");
    assert_eq!(p.replace('\\', "/"), "/repo/mesh/custom-wt/my-node");
}

#[test]
fn merge_rebuild_request_collapses_rapid_saves() {
    // Issue #1519 review: type → correct → save bursts must not spawn a
    // thread per keystroke-save. `None` (rebuild all) subsumes mesh ids;
    // mesh ids accumulate deduplicated.
    let mut pending = super::RebuildPending::default();
    super::merge_rebuild_request(&mut pending, Some(1));
    super::merge_rebuild_request(&mut pending, Some(1));
    super::merge_rebuild_request(&mut pending, Some(2));
    assert!(!pending.all);
    assert_eq!(pending.meshes, vec![1, 2]);
    super::merge_rebuild_request(&mut pending, None);
    assert!(pending.all);
    assert!(pending.meshes.is_empty());
    // Once `all`, further mesh ids are absorbed silently.
    super::merge_rebuild_request(&mut pending, Some(3));
    assert!(pending.all);
    assert!(pending.meshes.is_empty());
}

#[test]
fn warm_path_under_effective_dir_detects_stale_locations() {
    assert!(warm_path_under_effective_dir(
        "/repo/mesh/custom/a",
        "/repo/mesh/custom"
    ));
    assert!(!warm_path_under_effective_dir(
        "/repo/mesh/.claude/worktrees/a",
        "/repo/mesh/custom"
    ));
    // The container itself is not a worktree entry.
    assert!(!warm_path_under_effective_dir(
        "/repo/mesh/custom",
        "/repo/mesh/custom"
    ));
    // Sibling prefix (`custom2`) must not match `custom`.
    assert!(!warm_path_under_effective_dir(
        "/repo/mesh/custom2/a",
        "/repo/mesh/custom"
    ));
}

// ---- should_claim_for_spawn (the activation gate) ----
//
// The gate keys solely off whether the node's CURRENT worktree directory is
// already on disk — `false` ⇒ fresh spawn (claim), `true` ⇒ resume /
// re-spawn reusing an existing worktree (don't claim). Issue/PR spawns are
// now claim-eligible too (issue #612): the spawn path `git worktree move`s
// the pool directory to their `gh{N}-`/`pr{N}-` name rather than excluding
// them.

#[test]
fn claims_for_a_fresh_spawn_when_worktree_absent() {
    assert!(
        should_claim_for_spawn(false),
        "a fresh spawn (worktree dir not yet on disk) must be claim-eligible"
    );
}

#[test]
fn does_not_claim_when_worktree_already_present() {
    // Resume / handover / re-spawn: the node's worktree already exists on
    // disk. Claiming would re-point it at a different directory and
    // abandon the agent's work.
    assert!(
        !should_claim_for_spawn(true),
        "a spawn reusing an existing on-disk worktree must NOT be claim-eligible"
    );
}

// ---- refresh_warm_entries (ref-freshness orchestration, issue #613 AC3) ----
//
// The three side effects (reset, set_refreshing, set_available) are injected
// so the state-flip choreography is testable without git or a DB — the same
// DI shape as reconcile_warm_entries above. A shared log records the call
// order so the "refreshing-before-reset" invariant is provable.

fn warm(id: i64, base_sha: Option<&str>) -> db::WarmWorktree {
    db::WarmWorktree {
        id,
        path: format!("/repo/m/.claude/worktrees/slug-{}", id),
        preassigned_name: format!("slug-{}", id),
        base_sha: base_sha.map(str::to_string),
    }
}

/// An entry already parked on the new SHA is left completely untouched — no
/// reset, no status churn. This is the common case after a fetch that only
/// advanced an unrelated ref, so it must be a true no-op (a needless
/// `git reset --hard` would burn disk I/O on every fetch).
#[test]
fn refresh_skips_an_already_fresh_entry() {
    let log: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let n = refresh_warm_entries(
        "newsha",
        vec![warm(1, Some("newsha"))],
        |p, s| {
            log.lock().unwrap().push(format!("reset:{}:{}", p, s));
            Ok(())
        },
        |id| {
            log.lock().unwrap().push(format!("refreshing:{}", id));
            Ok(())
        },
        |id, sha| {
            log.lock()
                .unwrap()
                .push(format!("available:{}:{:?}", id, sha));
            Ok(())
        },
    );
    assert_eq!(n, 0, "an already-fresh entry counts as 0 refreshed");
    assert!(
        log.lock().unwrap().is_empty(),
        "a fresh entry must trigger no reset and no status flips"
    );
}

/// A stale entry is flipped to `refreshing` BEFORE the reset, then back to
/// `available` stamped with the NEW sha. The shared log proves the ordering:
/// flipping after the reset would let a concurrent claim grab a worktree
/// mid-`git reset --hard`.
#[test]
fn refresh_resets_a_stale_entry_and_stamps_new_sha() {
    let log: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let n = refresh_warm_entries(
        "newsha",
        vec![warm(7, Some("oldsha"))],
        |p, s| {
            log.lock().unwrap().push(format!("reset:{}:{}", p, s));
            Ok(())
        },
        |id| {
            log.lock().unwrap().push(format!("refreshing:{}", id));
            Ok(())
        },
        |id, sha| {
            log.lock()
                .unwrap()
                .push(format!("available:{}:{:?}", id, sha));
            Ok(())
        },
    );
    assert_eq!(n, 1, "one stale entry refreshed");
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            "refreshing:7".to_string(),
            "reset:/repo/m/.claude/worktrees/slug-7:newsha".to_string(),
            "available:7:Some(\"newsha\")".to_string(),
        ],
        "must mark refreshing, then reset, then flip back to available at the new sha"
    );
}

/// A reset failure restores the entry to `available` at its OLD sha (so it
/// stays claimable) and does NOT count it as refreshed. Stranding it at
/// `refreshing` would make it permanently unclaimable — the failure mode
/// this test guards against.
#[test]
fn refresh_restores_old_sha_when_reset_fails() {
    let restored: Mutex<Vec<(i64, Option<String>)>> = Mutex::new(Vec::new());
    let n = refresh_warm_entries(
        "newsha",
        vec![warm(9, Some("oldsha"))],
        |_p, _s| Err("reset blew up".to_string()),
        |_id| Ok(()),
        |id, sha| {
            restored.lock().unwrap().push((id, sha.map(str::to_string)));
            Ok(())
        },
    );
    assert_eq!(n, 0, "a failed reset must not count as refreshed");
    assert_eq!(
        *restored.lock().unwrap(),
        vec![(9, Some("oldsha".to_string()))],
        "a failed reset must restore the entry to available at its OLD sha"
    );
}

/// A successful reset whose flip-back to `available` fails must NOT be
/// counted as refreshed (issue #613 review): the worktree is correct on
/// disk but stuck at `refreshing` and so not yet claimable, so counting it
/// would over-report the pool's healthy size. The startup reconcile
/// recovers the stranded row later.
#[test]
fn refresh_does_not_count_when_flip_back_fails() {
    let n = refresh_warm_entries(
        "newsha",
        vec![warm(5, Some("oldsha"))],
        |_p, _s| Ok(()),                                // reset succeeds
        |_id| Ok(()),                                   // mark refreshing succeeds
        |_id, _sha| Err(rusqlite::Error::InvalidQuery), // flip-back fails
    );
    assert_eq!(
        n, 0,
        "a reset whose flip-back to available failed must not count as refreshed (row stranded at refreshing)"
    );
}

/// An entry with NO recorded base SHA (`base_sha = NULL`, e.g. the
/// fill's HEAD-SHA stamp failed) is treated as stale and
/// reset — it can't be proven fresh, so bringing it onto the known-good new
/// SHA is the safe choice.
#[test]
fn refresh_treats_null_base_sha_as_stale() {
    let reset_calls: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let n = refresh_warm_entries(
        "newsha",
        vec![warm(4, None)],
        |p, _s| {
            reset_calls.lock().unwrap().push(p.to_string());
            Ok(())
        },
        |_id| Ok(()),
        |_id, _sha| Ok(()),
    );
    assert_eq!(n, 1, "a NULL-sha entry must be refreshed");
    assert_eq!(
        *reset_calls.lock().unwrap(),
        vec!["/repo/m/.claude/worktrees/slug-4".to_string()],
        "a NULL-sha entry must be reset onto the new sha"
    );
}

// ---- recheck_after_claim (use-site guard, issue #653) ----
//
// The spawn path calls this immediately after `try_claim` to close the
// TOCTOU window between the claim's own existence check and the spawn's
// use of the path (which involves seconds of `fetch_origin` + git work).
// The tests pin all three observable branches: present→true, missing→
// false+row-dropped, and missing-row-idempotency (a delete during the
// recheck must not panic the spawn).

fn make_claimed_row(conn: &Connection, path: &str) -> i64 {
    // `insert_warm_worktree_inner` has a foreign key into `meshes`, so
    // the helper builds a v20-shape `meshes` table and inserts the parent
    // row before bringing the warm table forward. Mirrors
    // `db::warm_pool_tests::v20_schema_with_mesh(true)` — we replicate
    // it here because the `_inner` helpers we use don't ship their own
    // schema fixture (and the `db::warm_pool_tests` `v20_schema_with_mesh`
    // is private to that module).
    conn.execute_batch(
        "
        CREATE TABLE meshes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            layout TEXT NOT NULL DEFAULT 'grid',
            position INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            build_command TEXT,
            run_command TEXT,
            model TEXT,
            effort TEXT,
            use_worktree INTEGER NOT NULL DEFAULT 1,
            worktree_mode TEXT,
            default_provider TEXT,
            base_ref TEXT NOT NULL DEFAULT 'origin/main',
            scratchpad TEXT NOT NULL DEFAULT '',
            sandbox INTEGER NOT NULL DEFAULT 0
        );
        INSERT INTO meshes (name, path) VALUES ('m', '/repo/m');
        ",
    )
    .unwrap();
    db::migrations::evolve_to(db::migrations::SCHEMA_VERSION, conn).unwrap();
    db::insert_warm_worktree_inner(
        conn,
        1,
        path,
        "pool-warm-recheck",
        Some("aaaa"),
        db::WarmWorktreeStatus::Claimed,
    )
    .unwrap()
}

/// Happy path: the directory exists at recheck time. Returns `true`,
/// row is untouched, the spawn proceeds to use the warm entry.
#[test]
fn recheck_after_claim_returns_true_when_directory_present() {
    let conn = Connection::open_in_memory().unwrap();
    // `make_claimed_row` builds meshes + calls `evolve_to`; do not
    // pre-materialise baseline tables here (#1565) or CREATE TABLE meshes
    // collides.
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().to_str().unwrap().to_string();
    let id = make_claimed_row(&conn, &path);

    assert!(
        recheck_after_claim_inner(&conn, id, &path),
        "directory exists → recheck must say true so the spawn proceeds"
    );
    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM warm_worktrees WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 1, "row must survive when dir is present");
}

/// Race outcome: the directory vanished between `try_claim`'s check and
/// the spawn's use. Returns `false` AND drops the row so the pool's
/// bookkeeping stays consistent — without this, `claimed` rows would
/// accumulate pointing at missing directories (the exact symptom in
/// issue #653). The caller falls back to cold `create_git_worktree`.
#[test]
fn recheck_after_claim_returns_false_and_drops_row_when_dir_missing() {
    let conn = Connection::open_in_memory().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().to_str().unwrap().to_string();
    let id = make_claimed_row(&conn, &path);
    // Now delete the directory — simulate the race outcome.
    std::fs::remove_dir_all(&path).unwrap();

    assert!(
        !recheck_after_claim_inner(&conn, id, &path),
        "missing dir → recheck must say false so the spawn falls back to cold"
    );
    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM warm_worktrees WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        remaining, 0,
        "the dropped directory must drop its bookkeeping row — issue #653's stranded-row symptom"
    );
}

/// Idempotency guard: if some other path (the deleter-side guard,
/// startup reconcile, etc.) already dropped the row, the recheck's own
/// DELETE on a non-existent id must not panic. `rusqlite::Connection::
/// execute` returns `Ok(0)` for a missing row, so this is just a
/// regression guard against a future refactor that changed the
/// semantics.
#[test]
fn recheck_after_claim_is_idempotent_on_already_dropped_row() {
    let conn = Connection::open_in_memory().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().to_str().unwrap().to_string();
    let id = make_claimed_row(&conn, &path);
    std::fs::remove_dir_all(&path).unwrap();
    // First call drops the row (per the previous test).
    assert!(!recheck_after_claim_inner(&conn, id, &path));
    // Second call must not panic on the already-missing row.
    assert!(
        !recheck_after_claim_inner(&conn, id, &path),
        "idempotent recheck on a missing row + missing dir must still return false"
    );
}

// ---- the claim's use-site guard (issue #2533) ----
//
// The claim is the single carrier of the claimed row, so the guard's effect
// is observable on the claim's slot: after a recheck against a directory
// that disappeared, the claim holds no entry and the spawn falls back to a
// cold create instead of adopting a directory that is gone.
//
// `PoolClaim` itself needs a `tauri::AppHandle` (no test mock exists for
// one), which is why the guard lives in the free function `recheck_claim`
// and the tests below drive that.

/// The vanished-directory half of the guard: the claim must end up with no
/// entry, and the row must be gone with it.
#[test]
fn recheck_claim_drops_the_entry_when_the_claimed_directory_disappeared() {
    let _db = crate::db::test_support::isolated();

    let tmp = tempfile::TempDir::new().unwrap();
    let repo_path = tmp.path().join("repo").to_string_lossy().to_string();
    let mesh_id = {
        let conn = crate::db::write_conn();
        insert_mesh(&conn, &repo_path)
    };
    // A path that does not exist — the "another thread deleted it" case.
    let missing = tmp.path().join("vanished").to_string_lossy().to_string();

    let row_id = db::insert_warm_worktree(
        mesh_id,
        &missing,
        "slug",
        None,
        db::WarmWorktreeStatus::Claimed,
    )
    .expect("insert the claimed row");

    let mut entry = Some(ClaimedWarmEntry {
        id: row_id,
        path: missing.clone(),
        preassigned_name: "slug".to_string(),
        base_sha: None,
    });
    recheck_claim(&mut entry);
    assert!(
        entry.is_none(),
        "a vanished directory must leave the claim with no entry, so the spawn \
         falls back to a cold create"
    );

    let remaining: i64 = {
        let conn = crate::db::write_conn();
        conn.query_row(
            "SELECT COUNT(*) FROM warm_worktrees WHERE id = ?1",
            rusqlite::params![row_id],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(remaining, 0, "the recheck must drop the claimed row too");
}

/// The other half of the guard: a claimed directory that is still there keeps
/// the claim's entry, so the spawn proceeds to adopt it.
#[test]
fn recheck_claim_keeps_the_entry_when_the_claimed_directory_is_present() {
    let _db = crate::db::test_support::isolated();

    let tmp = tempfile::TempDir::new().unwrap();
    let repo_path = tmp.path().join("repo").to_string_lossy().to_string();
    let mesh_id = {
        let conn = crate::db::write_conn();
        insert_mesh(&conn, &repo_path)
    };
    let present_dir = tmp.path().join("here");
    std::fs::create_dir_all(&present_dir).unwrap();
    let present = present_dir.to_string_lossy().to_string();

    let row_id = db::insert_warm_worktree(
        mesh_id,
        &present,
        "slug",
        None,
        db::WarmWorktreeStatus::Claimed,
    )
    .expect("insert the claimed row");

    let mut entry = Some(ClaimedWarmEntry {
        id: row_id,
        path: present.clone(),
        preassigned_name: "slug".to_string(),
        base_sha: None,
    });
    recheck_claim(&mut entry);
    assert!(
        entry.is_some(),
        "a directory that is still there must keep the claim's entry"
    );

    let remaining: i64 = {
        let conn = crate::db::write_conn();
        conn.query_row(
            "SELECT COUNT(*) FROM warm_worktrees WHERE id = ?1",
            rusqlite::params![row_id],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(remaining, 1, "a live claim must keep its row");
}

// ---- fill_mesh_to_target_with (orchestration, issues #634 / #1752) ----
//
// The `count` / `drain` / `warm_one` closures are injected so the test can
// drive the drain-then-fill-to-target contract without `tauri::AppHandle`
// (which is hard to mock) or real git worktree creation (which is slow +
// requires a real repo). The production `fill_mesh_to_target` wraps this
// with the real `count_available_warm_for_mesh` +
// `drain_excess_warm_entries` + `warm_one_body`.
//
// The per-mesh early-exits that used to live in a `prewarm_early_exit`
// helper (target == 0 ⇒ no work, count >= target ⇒ at target, count
// failure ⇒ stop loudly) are now the three branches of this orchestration,
// pinned by `fill_mesh_to_target_with_zero_target_runs_only_drain`,
// `..._returns_zero_when_at_target`, and
// `..._count_failure_aborts_before_warming`.

fn mesh_row(id: i64, target: i64) -> db::WarmPoolMeshRow {
    db::WarmPoolMeshRow {
        id,
        path: format!("/tmp/buildmesh_fill_test_{}", id),
        pre_spawn_pool_size: target,
        base_ref: "origin/main".to_string(),
        worktree_directory: None,
    }
}

/// The probe a fill sees when the mesh's folder is a working repository.
fn usable_probe() -> MeshRepoProbe {
    MeshRepoProbe {
        usable: true,
        head: Some("test-head".to_string()),
    }
}

/// The probe a fill sees when the mesh's folder cannot back a worktree.
fn unusable_probe() -> MeshRepoProbe {
    MeshRepoProbe {
        usable: false,
        head: None,
    }
}

/// The retry policy a test drives: a fresh backoff and a clock the test
/// owns (issue #2602).
fn test_fill_policy() -> (FillBackoff, Instant) {
    (FillBackoff::new(), Instant::now())
}

/// Happy path: drain runs once, then `warm_one` is called once per missing
/// entry. The `available < target` bound stops it after exactly `target`
/// calls for a mesh starting empty.
#[test]
fn fill_mesh_to_target_with_warms_exactly_target_times() {
    let mesh = mesh_row(1, 3);
    let warm_calls = std::cell::RefCell::new(0);
    let drain_calls = std::cell::RefCell::new(0);

    let (backoff, now) = test_fill_policy();

    let changes = fill_mesh_to_target_with(
        &mesh,
        &backoff,
        now,
        usable_probe,
        || Ok(0),
        |_| {
            *drain_calls.borrow_mut() += 1;
            Ok(0)
        },
        |_m| {
            *warm_calls.borrow_mut() += 1;
            Ok(())
        },
    );

    assert_eq!(*drain_calls.borrow(), 1, "drain must run exactly once");
    assert_eq!(
        *warm_calls.borrow(),
        3,
        "one cut per missing entry — a target-3 mesh starting empty needs 3"
    );
    assert_eq!(changes, 3, "3 cuts = 3 changes");
}

/// Issue #1752: the count is sampled **once** and a failure aborts the fill
/// before cutting anything — a broken count is no basis for warming. The
/// drain already ran, so its changes are still reported.
#[test]
fn fill_mesh_to_target_with_count_failure_aborts_before_warming() {
    let mesh = mesh_row(2, 5);
    let count_calls = std::cell::RefCell::new(0);
    let warm_calls = std::cell::RefCell::new(0);

    let (backoff, now) = test_fill_policy();

    let changes = fill_mesh_to_target_with(
        &mesh,
        &backoff,
        now,
        usable_probe,
        || {
            *count_calls.borrow_mut() += 1;
            Err("database is locked".into())
        },
        |_| Ok(2), // drain removed 2 excess rows
        |_m| {
            *warm_calls.borrow_mut() += 1;
            Ok(())
        },
    );

    assert_eq!(
        *count_calls.borrow(),
        1,
        "the count is sampled exactly once"
    );
    assert_eq!(
        *warm_calls.borrow(),
        0,
        "a failed count must not cut a worktree"
    );
    assert_eq!(changes, 2, "the drain's changes must still be reported");
}

/// `warm_one_body`'s `Err` (a `git worktree add` failure after slug
/// retries) also breaks the loop. Distinct from a count failure (which
/// aborts before any warm attempt).
#[test]
fn fill_mesh_to_target_with_prewarm_err_aborts_loop() {
    let mesh = mesh_row(3, 5);
    let responses: std::cell::RefCell<Vec<_>> = std::cell::RefCell::new(vec![
        Ok(()),
        Err("git worktree add: locked".into()),
        Ok(()), // must NOT be reached
    ]);
    let warm_calls = std::cell::RefCell::new(0);

    let (backoff, now) = test_fill_policy();

    fill_mesh_to_target_with(
        &mesh,
        &backoff,
        now,
        usable_probe,
        || Ok(0),
        |_| Ok(0),
        |_m| {
            *warm_calls.borrow_mut() += 1;
            responses.borrow_mut().remove(0)
        },
    );

    assert_eq!(
        *warm_calls.borrow(),
        2,
        "loop must stop on a git worktree add failure"
    );
}

/// `target == 0` ⇒ drain runs once and then the fill returns immediately,
/// **without even sampling the count** (issue #1752: a disabled pool has
/// nothing to warm, so the query would be pure overhead). This is the
/// common case for a worktree-disabled mesh.
#[test]
fn fill_mesh_to_target_with_zero_target_runs_only_drain() {
    let mesh = mesh_row(4, 0);
    let drain_calls = std::cell::RefCell::new(0);
    let count_calls = std::cell::RefCell::new(0);
    let warm_calls = std::cell::RefCell::new(0);

    let (backoff, now) = test_fill_policy();

    let changes = fill_mesh_to_target_with(
        &mesh,
        &backoff,
        now,
        usable_probe,
        || {
            *count_calls.borrow_mut() += 1;
            Ok(0)
        },
        |_| {
            *drain_calls.borrow_mut() += 1;
            Ok(0)
        },
        |_m| {
            *warm_calls.borrow_mut() += 1;
            Ok(())
        },
    );

    assert_eq!(*drain_calls.borrow(), 1);
    assert_eq!(
        *count_calls.borrow(),
        0,
        "a disabled pool must not query the available count"
    );
    assert_eq!(
        *warm_calls.borrow(),
        0,
        "a zero-target mesh must NOT warm (drain is the only useful work)"
    );
    assert_eq!(changes, 0);
}

/// Issue #1752: a mesh already partway to target only pays for the deficit.
/// The once-sampled count seeds `available`, so a target-3 mesh with one
/// entry already available cuts exactly two more.
#[test]
fn fill_mesh_to_target_with_warms_only_the_deficit() {
    let mesh = mesh_row(10, 3);
    let warm_calls = std::cell::RefCell::new(0);

    let (backoff, now) = test_fill_policy();

    let changes = fill_mesh_to_target_with(
        &mesh,
        &backoff,
        now,
        usable_probe,
        || Ok(1),
        |_| Ok(0),
        |_m| {
            *warm_calls.borrow_mut() += 1;
            Ok(())
        },
    );

    assert_eq!(
        *warm_calls.borrow(),
        2,
        "only the two missing entries are cut; the sample count seeds the running total"
    );
    assert_eq!(changes, 2);
}

/// Drain failure is logged at WARN and the loop PROCEEDS to warm (we
/// don't refuse to fill just because the downsize scan errored — the
/// pool is presumably already at or below target anyway, and a fill
/// is at worst a single extra worktree).
#[test]
fn fill_mesh_to_target_with_drain_failure_still_attempts_fill() {
    let mesh = mesh_row(5, 2);
    let warm_calls = std::cell::RefCell::new(0);

    let (backoff, now) = test_fill_policy();

    fill_mesh_to_target_with(
        &mesh,
        &backoff,
        now,
        usable_probe,
        || Ok(0),
        |_| Err("drain DB error".into()),
        |_m| {
            *warm_calls.borrow_mut() += 1;
            Ok(())
        },
    );

    assert_eq!(
        *warm_calls.borrow(),
        2,
        "a drain failure must NOT abort the fill — it still warms all the way to \
         target (drain err is logged + swallowed)"
    );
}

// ---- fill_mesh_to_target_with return value (issue #634 review) ----
//
// These tests pin the `usize` return contract that the idle worker
// uses for its adaptive-tick backoff signal. A return of `0` is the
// honest "nothing changed" signal; `> 0` means the pool's available
// count changed (drained or warmed). The pre-#634 review caught that
// the worker used a static `pre_spawn_pool_size > 0` heuristic
// instead — that always returned true for any worktree-enabled mesh,
// so the TICK_SLOW backoff never triggered in production.

/// 3 Warmed cuts + a drain that returned 0 → total changes = 3.
#[test]
fn fill_mesh_to_target_with_returns_count_of_warmed_entries() {
    let mesh = mesh_row(6, 3);

    let (backoff, now) = test_fill_policy();

    let changes = fill_mesh_to_target_with(
        &mesh,
        &backoff,
        now,
        usable_probe,
        || Ok(0),
        |_| Ok(0),
        |_m| Ok(()),
    );
    assert_eq!(changes, 3, "3 Warmed responses = 3 changes");
}

/// Drain returns 2 (two excess rows removed) + 1 Warmed = 3 total.
/// Pins that drain's count is added to the total — not just the warm
/// loop's contributions.
#[test]
fn fill_mesh_to_target_with_aggregates_drain_and_warm_changes() {
    let mesh = mesh_row(7, 1);
    let (backoff, now) = test_fill_policy();
    let changes = fill_mesh_to_target_with(
        &mesh,
        &backoff,
        now,
        usable_probe,
        || Ok(0),
        |_| Ok(2), // drain removed 2 excess rows
        |_m| Ok(()),
    );
    assert_eq!(changes, 3, "2 drain + 1 warm = 3 changes total");
}

/// Already-at-target mesh returns 0 — the honest "no work was
/// needed" signal the idle worker uses to count a no-op pass toward
/// its TICK_SLOW backoff. The once-sampled count (2, equal to target)
/// means the loop body never runs at all.
#[test]
fn fill_mesh_to_target_with_returns_zero_when_at_target() {
    let mesh = mesh_row(8, 2);
    let warm_calls = std::cell::RefCell::new(0);

    let (backoff, now) = test_fill_policy();

    let changes = fill_mesh_to_target_with(
        &mesh,
        &backoff,
        now,
        usable_probe,
        || Ok(2),
        |_| Ok(0),
        |_m| {
            *warm_calls.borrow_mut() += 1;
            Ok(())
        },
    );

    assert_eq!(changes, 0, "at-target mesh must report 0 changes");
    assert_eq!(
        *warm_calls.borrow(),
        0,
        "a mesh already at target must not cut anything"
    );
}

/// Disabled mesh (target = 0) returns 0 because the fill body never
/// executes; drain still ran once but reports 0 changes.
#[test]
fn fill_mesh_to_target_with_returns_zero_when_target_is_zero() {
    let mesh = mesh_row(9, 0);
    let (backoff, now) = test_fill_policy();
    let changes = fill_mesh_to_target_with(
        &mesh,
        &backoff,
        now,
        usable_probe,
        || Ok(0),
        |_| Ok(0),
        |_m| Ok(()),
    );
    assert_eq!(changes, 0);
}

// ---- Meshes that are not git working repositories (issue #2348) ----
//
// A Mesh whose folder cannot be opened as a git working repository can
// never back a warm worktree: `create_git_worktree` resolves
// `project_root` through `git::project::resolve` first and rejects the same
// folders (libgit2 `NotFound (-3)` for a plain folder). Before the gate,
// every idle tick cut a doomed worktree and logged a WARN for it (982 such
// lines in one day of hub log for one Mesh), and the worker re-attempted
// the background fetch on the same doomed premise.

/// A Mesh row pointing at a real on-disk folder. `mesh_row`'s synthetic
/// `/tmp/buildmesh_fill_test_*` path is not openable, which these tests
/// need to be exactly the opposite of.
fn mesh_row_at(id: i64, target: i64, path: &std::path::Path) -> db::WarmPoolMeshRow {
    db::WarmPoolMeshRow {
        id,
        path: path.to_str().unwrap().to_string(),
        pre_spawn_pool_size: target,
        base_ref: "origin/main".to_string(),
        worktree_directory: None,
    }
}

/// The gate stops the warm loop — the only step that shells out to git —
/// while the drain's bookkeeping still runs and is still reported, so a
/// downsize on an unpoolable mesh still converges. Asserting
/// `warm_calls == 0` is the regression: before #2348 the loop ran on every
/// idle tick and every attempt logged its own WARN.
#[test]
fn fill_mesh_to_target_with_does_nothing_for_a_non_repository_mesh() {
    let mesh = mesh_row(2348, 3);
    let drain_calls = std::cell::RefCell::new(0);
    let warm_calls = std::cell::RefCell::new(0);

    let (backoff, now) = test_fill_policy();

    let changes = fill_mesh_to_target_with(
        &mesh,
        &backoff,
        now,
        unusable_probe,
        || Ok(0),
        |_| {
            *drain_calls.borrow_mut() += 1;
            Ok(3)
        },
        |_m| {
            *warm_calls.borrow_mut() += 1;
            Ok(())
        },
    );

    assert_eq!(
        *warm_calls.borrow(),
        0,
        "a mesh whose folder is not a git working repository must never \
         reach `git worktree add` (issue #2348)"
    );
    assert_eq!(
        *drain_calls.borrow(),
        1,
        "the drain is pure bookkeeping and still runs"
    );
    assert_eq!(
        changes, 3,
        "the drain's changes are still reported, and no warm is counted"
    );
}

/// A mesh with nothing to fill never pays for the probe: an at-target (or
/// pool-disabled) mesh has no `git worktree add` that could fail, and on
/// an unreachable drive that repository open is the expensive call.
#[test]
fn fill_mesh_to_target_with_skips_the_probe_when_there_is_nothing_to_fill() {
    let mesh = mesh_row(2349, 2);
    let probe_calls = std::cell::RefCell::new(0);
    let warm_calls = std::cell::RefCell::new(0);

    let (backoff, now) = test_fill_policy();

    let changes = fill_mesh_to_target_with(
        &mesh,
        &backoff,
        now,
        || {
            *probe_calls.borrow_mut() += 1;
            unusable_probe() // would veto every fill if it were consulted
        },
        || Ok(2), // already at target
        |_| Ok(0),
        |_m| {
            *warm_calls.borrow_mut() += 1;
            Ok(())
        },
    );

    assert_eq!(
        *probe_calls.borrow(),
        0,
        "an at-target mesh must not pay for the repository probe"
    );
    assert_eq!(*warm_calls.borrow(), 0);
    assert_eq!(changes, 0);
}

// ---- the fill-failure backoff (issue #2602) ----
//
// A mesh whose `git worktree add` fails deterministically — a tracked path
// Windows refuses to check out (`playbook app icon.png:Zone.Identifier`) —
// used to fail on each worker tick: a `git worktree add`, a
// `warm_worktrees` row and a WARN line per 30-second tick for as long as the
// app stayed open. The backoff makes the mesh wait longer and longer between
// attempts, and a mesh that recovers is filled again at once.

/// One worker pass over a mesh, the way the pool worker runs it.
///
/// `head` is the repository's HEAD at this pass and `warm` is what the
/// `git worktree add` does.
fn fill_pass(
    mesh: &db::WarmPoolMeshRow,
    backoff: &FillBackoff,
    now: Instant,
    head: &str,
    warm: &dyn Fn() -> Result<(), String>,
) -> usize {
    fill_mesh_to_target_with(
        mesh,
        backoff,
        now,
        || MeshRepoProbe {
            usable: true,
            head: Some(head.to_string()),
        },
        || Ok(0),
        |_| Ok(0),
        |_m| warm(),
    )
}

/// The attempt interval grows with each consecutive failure, and the fill
/// is not even attempted inside the interval.
#[test]
fn a_failing_fill_waits_longer_between_attempts() {
    let mesh = mesh_row(2602, 1);
    let backoff = FillBackoff::new();
    let start = Instant::now();
    let attempts = std::cell::RefCell::new(0usize);
    let always_fails = || {
        *attempts.borrow_mut() += 1;
        Err("invalid path 'playbook app icon.png:Zone.Identifier'".to_string())
    };

    // Pass 1 — the failure the user needs to hear about.
    let changes = fill_pass(&mesh, &backoff, start, "head-1", &always_fails);
    assert_eq!(changes, 0, "a failed fill changes nothing");
    assert_eq!(*attempts.borrow(), 1);
    let record = backoff.failure(mesh.id).expect("the failure is recorded");
    assert_eq!(record.failures, 1);
    assert_eq!(record.interval, Duration::from_secs(30));

    // Pass 2, 29 s in: still inside the window, so the doomed attempt is
    // skipped and the pass reports no changes.
    let changes = fill_pass(
        &mesh,
        &backoff,
        start + Duration::from_secs(29),
        "head-1",
        &always_fails,
    );
    assert_eq!(changes, 0);
    assert_eq!(
        *attempts.borrow(),
        1,
        "the fill must not be attempted inside its backoff window"
    );

    // Pass 3, 30 s in: the window has passed, so the fill runs again — and
    // the interval it now waits grows.
    let changes = fill_pass(
        &mesh,
        &backoff,
        start + Duration::from_secs(30),
        "head-1",
        &always_fails,
    );
    assert_eq!(changes, 0);
    assert_eq!(
        *attempts.borrow(),
        2,
        "the fill retries once the backoff window has passed"
    );
    let record = backoff.failure(mesh.id).expect("the failure is recorded");
    assert_eq!(record.failures, 2);
    assert_eq!(
        record.interval,
        Duration::from_secs(60),
        "the attempt interval doubles"
    );
}

/// The interval keeps growing across passes until it reaches the cap, and
/// the mesh is attempted less and less often.
#[test]
fn the_attempt_interval_grows_across_worker_passes() {
    let mesh = mesh_row(2604, 1);
    let backoff = FillBackoff::new();
    let start = Instant::now();
    let attempts = std::cell::RefCell::new(0usize);
    let always_fails = || {
        *attempts.borrow_mut() += 1;
        Err("git worktree add failed".to_string())
    };

    // Each pass runs at the moment the previous failure's interval ends, so
    // each pass is a real attempt.
    let mut now = start;
    let mut expected_interval = Duration::from_secs(30);
    for _ in 0..4 {
        fill_pass(&mesh, &backoff, now, "head-1", &always_fails);
        let record = backoff.failure(mesh.id).expect("the failure is recorded");
        assert_eq!(record.interval, expected_interval);
        expected_interval *= 2;
        now += expected_interval / 2;
    }

    assert_eq!(
        *attempts.borrow(),
        4,
        "every pass that lands after the interval is an attempt"
    );
}

/// A successful fill clears the backoff, so the next deficit is filled
/// without waiting.
#[test]
fn a_successful_fill_clears_the_backoff() {
    let mesh = mesh_row(2603, 1);
    let backoff = FillBackoff::new();
    let start = Instant::now();
    let failing = std::cell::RefCell::new(true);

    fill_pass(&mesh, &backoff, start, "head-1", &|| {
        if *failing.borrow() {
            Err("git worktree add failed".to_string())
        } else {
            Ok(())
        }
    });
    assert!(
        backoff.failure(mesh.id).is_some(),
        "the failed fill is recorded"
    );

    // The mesh recovers: the next attempt succeeds.
    *failing.borrow_mut() = false;
    let changes = fill_pass(
        &mesh,
        &backoff,
        start + Duration::from_secs(30),
        "head-1",
        &|| Ok(()),
    );
    assert_eq!(changes, 1, "the recovered mesh is filled");
    assert!(
        backoff.failure(mesh.id).is_none(),
        "a successful fill clears the backoff"
    );

    // And the next failure starts a fresh streak rather than continuing the
    // old one.
    *failing.borrow_mut() = true;
    fill_pass(
        &mesh,
        &backoff,
        start + Duration::from_secs(31),
        "head-1",
        &|| Err("git worktree add failed".to_string()),
    );
    let record = backoff.failure(mesh.id).expect("the failure is recorded");
    assert_eq!(record.failures, 1);
    assert_eq!(record.interval, Duration::from_secs(30));
}

/// The user's fix is a commit, and a commit moves the HEAD. The recorded
/// failure described a repository that no longer exists, so the fill is
/// attempted at once instead of waiting out the interval.
#[test]
fn a_changed_head_retries_at_once() {
    let mesh = mesh_row(2605, 1);
    let backoff = FillBackoff::new();
    let start = Instant::now();
    let attempts = std::cell::RefCell::new(0usize);

    fill_pass(&mesh, &backoff, start, "head-1", &|| {
        *attempts.borrow_mut() += 1;
        Err("invalid path 'a:Zone.Identifier'".to_string())
    });
    assert_eq!(*attempts.borrow(), 1);

    // One second later the HEAD has moved: the fill is retried at once.
    let changes = fill_pass(
        &mesh,
        &backoff,
        start + Duration::from_secs(1),
        "head-2",
        &|| {
            *attempts.borrow_mut() += 1;
            Ok(())
        },
    );
    assert_eq!(changes, 1, "the repaired mesh is filled at once");
    assert_eq!(
        *attempts.borrow(),
        2,
        "a moved HEAD lifts the backoff — the user must not wait 30 s for a fix"
    );
}

/// The pool's fill failure rides on the mesh's health surface (issue #2602).
/// The backoff stopped the retry storm, but a backoff is not something a
/// user can see — this is.
#[test]
fn fill_failure_for_mesh_reports_the_recorded_failure() {
    let mesh = mesh_row(2606, 1);
    let now = Instant::now();

    FILL_BACKOFF.record_failure(
        mesh.id,
        "fingerprint",
        "invalid path 'playbook app icon.png:Zone.Identifier'",
        now,
    );

    let failure = fill_failure_for_mesh(mesh.id).expect("the failure is reported");
    assert_eq!(
        failure.reason,
        "invalid path 'playbook app icon.png:Zone.Identifier'"
    );
    assert_eq!(failure.attempts, 1);
    assert!(
        failure.retry_in_seconds <= 30,
        "the next attempt is at most the first interval away, got {}",
        failure.retry_in_seconds
    );

    // A healthy mesh reports nothing.
    assert!(fill_failure_for_mesh(mesh.id + 100_000).is_none());

    // And the record goes away once the mesh recovers.
    FILL_BACKOFF.record_success(mesh.id);
    assert!(fill_failure_for_mesh(mesh.id).is_none());
}

/// The probe is the real thing, not a stub: a plain folder reads as
/// unusable, a real `git init` repo does not. The repo half is what stops
/// the gate from silently disabling every pool in the app.
#[test]
fn mesh_repo_unusable_is_true_for_a_plain_folder_and_false_for_a_real_repo() {
    let (_repo_tmp, repo_path) = fresh_git_repo();
    let plain = tempfile::TempDir::new().expect("tempdir");
    let repo_mesh = mesh_row_at(9101, 1, std::path::Path::new(&repo_path));
    let plain_mesh = mesh_row_at(9102, 1, plain.path());

    assert!(
        !mesh_repo_unusable(&repo_mesh),
        "a real repository must stay poolable — the gate is not a pool kill switch"
    );

    // Claim this mesh's report first: the WARN-budget test below owns the
    // log line for its own paths, and this test is about the probe's truth
    // value, not about who logs it.
    claim_non_repo_report(plain_mesh.id, &plain_mesh.path);
    assert!(
        mesh_repo_unusable(&plain_mesh),
        "a folder that is not a git working repository must be reported unusable"
    );
}

/// The log latch: the FIRST report for a `(mesh, path)` wins, repeats are
/// silent, and a re-pointed mesh reports again. Deterministic (no
/// subscriber involved), so it pins the "at most one line per app run"
/// budget even where the tracing-capture test cannot run.
#[test]
fn non_repo_report_latch_covers_each_mesh_path_once() {
    let id = std::process::id() as i64 * 1000 + 9103;
    let first_path = format!("C:/nonexistent/mesh-{}/a", id);
    let moved_path = format!("C:/nonexistent/mesh-{}/b", id);

    assert!(
        claim_non_repo_report(id, &first_path),
        "the first sighting of a mesh path must be reportable"
    );
    assert!(
        !claim_non_repo_report(id, &first_path),
        "a repeat sighting must stay silent — this is the #2348 log flood"
    );
    assert!(
        claim_non_repo_report(id, &moved_path),
        "a mesh re-pointed at another folder is a new condition and must \
         report its own line"
    );
    assert!(
        claim_non_repo_report(id + 1, &first_path),
        "the latch is keyed per mesh AND path: a second mesh on the same \
         unusable folder still has to be reported"
    );
}

/// The observable half of the flood fix: a second pass over the SAME
/// non-repository mesh adds no log line at all. The idle worker runs this
/// path every tick, so "the second pass is silent" is exactly the
/// regression #2348 reported.
///
/// A hand-rolled capturing subscriber scoped via `with_default` (the same
/// shape as `agent::provider_menu`'s log-budget test) keeps this
/// parallel-safe without a global subscriber or a new dev-dependency.
/// The first pass also asserts that exactly one line arrives — `tracing`
/// caches per-callsite interest globally, so that assertion is what proves
/// the capture is live and makes the silence below non-vacuous.
#[test]
fn mesh_repo_unusable_stays_silent_on_repeat_passes() {
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata};

    struct MessageRecorder {
        messages: Vec<String>,
    }
    impl Visit for MessageRecorder {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                let rendered = format!("{value:?}");
                self.messages.push(
                    rendered
                        .strip_prefix('"')
                        .and_then(|inner| inner.strip_suffix('"'))
                        .unwrap_or(&rendered)
                        .to_string(),
                );
            }
        }
    }
    type Captured = Arc<Mutex<Vec<String>>>;
    struct Capture {
        messages: Captured,
    }
    impl tracing::Subscriber for Capture {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut recorder = MessageRecorder {
                messages: Vec::new(),
            };
            event.record(&mut recorder);
            self.messages.lock().unwrap().extend(recorder.messages);
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    let plain = tempfile::TempDir::new().expect("tempdir");
    let mesh = mesh_row_at(std::process::id() as i64 * 1000 + 9104, 1, plain.path());
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let run_pass = || {
        tracing::dispatcher::with_default(
            &tracing::dispatcher::Dispatch::new(Capture {
                messages: captured.clone(),
            }),
            || {
                tracing::callsite::rebuild_interest_cache();
                mesh_repo_unusable(&mesh)
            },
        )
    };
    let non_repo_lines = || {
        captured
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.contains("cannot be opened as a git working repository"))
            .count()
    };

    assert!(run_pass(), "precondition: a plain folder is not poolable");
    assert_eq!(
        non_repo_lines(),
        1,
        "the first report emits exactly one line — also what proves this \
         capture is live, so the silence asserted below is not vacuous"
    );

    tracing::callsite::rebuild_interest_cache();
    captured.lock().unwrap().clear();
    assert!(
        run_pass(),
        "the probe stays true on the second pass — the folder is still not a repo"
    );

    assert_eq!(
        non_repo_lines(),
        0,
        "a repeat pass over a non-repository mesh must add no log line (issue #2348)"
    );
}

// ---- End-to-end integration: drain-then-fill on a real temp git repo ----
//
// (Issue #634 sub-task #5.) The DB-layer drain/refresh logic is covered
// exhaustively by `db::warm_pool_tests`; the loop orchestration is
// covered by the `fill_mesh_to_target_with` tests above. What neither
// of those covers is the **end-to-end drain-then-fill-to-target
// contract** against a REAL git repo: real `git init` → real commit →
// real `git worktree add` (via `create_git_worktree`) → real
// directory on disk → real `git rev-parse HEAD` → real SHA stamped on
// the row. These tests lock that contract in place.
//
// We can't call `fill_mesh_to_target` directly — it needs a
// `tauri::AppHandle` for `emit_pool_changed`, and there's no test
// mock for that. Instead we drive the exact same `_inner` DB
// operations + `create_git_worktree` calls that `fill_mesh_to_target`
// uses internally. That's the "real git + real DB" coverage the issue
// asks for; the orchestration is unit-tested above.
//
// DB init routes through `db::test_support::isolated`
// — every test in this file (and the rest of the lib test binary)
// shares one global DB.

/// Fresh temp dir with a real git repo: `git init` + an empty initial
/// commit on `main`. Returns the temp dir + the repo path.
fn fresh_git_repo() -> (tempfile::TempDir, String) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().to_str().unwrap().to_string();

    let run = |args: &[&str]| {
        let status = std::process::Command::new("git") // allow-inline-process-spawn: test helper
            .args(args)
            .current_dir(&path)
            .status()
            .expect("git invocation failed");
        assert!(status.success(), "git {:?} must succeed", args);
    };

    run(&["init", "-q"]);
    run(&["config", "user.email", "test@buildmesh.local"]);
    run(&["config", "user.name", "Buildmesh Test"]);
    run(&["config", "commit.gpgsign", "false"]);
    run(&["checkout", "-q", "-b", "main"]);
    run(&["commit", "--allow-empty", "-q", "-m", "init"]);

    (tmp, path)
}

/// Insert a worktree-enabled mesh row pointing at `repo_path`.
fn insert_mesh(conn: &rusqlite::Connection, repo_path: &str) -> i64 {
    conn.execute(
        "INSERT INTO meshes (name, path, use_worktree, pre_spawn_pool_size, base_ref) \
         VALUES ('m', ?1, 1, 0, 'origin/main')",
        rusqlite::params![repo_path],
    )
    .expect("insert mesh");
    conn.query_row::<i64, _, _>(
        "SELECT id FROM meshes WHERE path = ?1",
        rusqlite::params![repo_path],
        |row| row.get(0),
    )
    .expect("mesh id")
}

/// Cut a real git worktree via the production `create_git_worktree`
/// helper, mirroring `warm_one_body`'s body. Returns the on-disk path.
fn cut_real_warm_worktree(conn: &rusqlite::Connection, mesh_id: i64, mesh_path: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let slug = format!("wt-{:x}", nanos);
    // Legacy layout (no custom dir configured in these fixtures).
    let effective = crate::env::effective_worktree_dir_raw(mesh_path, None, None);
    let host_path = warm_worktree_host_path_in_dir(&effective, &slug);

    let row_id = db::insert_warm_worktree_inner(
        conn,
        mesh_id,
        &host_path,
        &slug,
        None,
        db::WarmWorktreeStatus::Filling,
    )
    .expect("insert filling row");

    crate::git::worktree::create_git_worktree(
        mesh_path,
        &host_path,
        &slug,
        "detached",
        "origin/main",
    )
    .unwrap_or_else(|e| panic!("create_git_worktree failed for {}: {}", slug, e));

    let sha = read_head_sha(&host_path);
    let sha_opt: Option<&str> = if sha.is_empty() { None } else { Some(&sha) };
    conn.execute(
        "UPDATE warm_worktrees SET status = 'available', base_sha = ?2 WHERE id = ?1",
        rusqlite::params![row_id, sha_opt],
    )
    .expect("flip to available");

    host_path
}

fn read_head_sha(worktree_path: &str) -> String {
    let output = std::process::Command::new("git") // allow-inline-process-spawn: test helper
        .args(["-C", worktree_path, "rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse");
    if !output.status.success() {
        return String::new();
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Pin the drain-then-fill-to-target contract end-to-end against a real
/// temp git repo + real `create_git_worktree` + real DB. Mirrors the
/// issue #634 acceptance: drain removes the oldest excess (FIFO order),
/// fill cuts real worktrees via the production helper, at-target is a
/// no-op.
///
/// Uses unique slug suffixes so multiple invocations in the same test
/// don't collide on a stale directory.
#[test]
fn maintenance_drain_then_fill_end_to_end_on_real_repo() {
    let _db = crate::db::test_support::isolated();

    let (_tmp, repo_path) = fresh_git_repo();
    let conn = crate::db::write_conn();
    let mesh_id = insert_mesh(&conn, &repo_path);

    // Cut three warm worktrees in order — the first is the OLDEST.
    let path_oldest = cut_real_warm_worktree(&conn, mesh_id, &repo_path);
    std::thread::sleep(std::time::Duration::from_millis(10));
    let path_middle = cut_real_warm_worktree(&conn, mesh_id, &repo_path);
    std::thread::sleep(std::time::Duration::from_millis(10));
    let path_newest = cut_real_warm_worktree(&conn, mesh_id, &repo_path);
    assert!(std::path::Path::new(&path_oldest).exists());
    assert!(std::path::Path::new(&path_middle).exists());
    assert!(std::path::Path::new(&path_newest).exists());

    // Set target = 1 so the drain should drop 2 of 3 (FIFO).
    conn.execute(
        "UPDATE meshes SET pre_spawn_pool_size = 1 WHERE id = ?1",
        rusqlite::params![mesh_id],
    )
    .unwrap();

    // Drain phase — mirrors `drain_excess_warm_entries` body.
    let target: i64 = conn
        .query_row(
            "SELECT pre_spawn_pool_size FROM meshes WHERE id = ?1",
            rusqlite::params![mesh_id],
            |row| row.get(0),
        )
        .unwrap();
    let count = db::count_droppable_warm_entries_for_mesh_inner(&conn, mesh_id).unwrap();
    assert_eq!(count, 3, "precondition: 3 droppable rows");
    let excess = (count - target).max(0);
    let candidates = db::list_oldest_warm_entries_for_mesh_inner(&conn, mesh_id, excess).unwrap();
    assert_eq!(candidates.len(), 2);
    // FIFO: the OLDEST two are the drop candidates.
    assert_eq!(candidates[0].1, path_oldest);
    assert_eq!(candidates[1].1, path_middle);
    for (id, path) in &candidates {
        db::delete_warm_worktree_inner(&conn, *id).unwrap();
        let _ = crate::git::worktree::remove_one_worktree(path);
    }

    // Postcondition: 1 row left, only newest dir on disk.
    let remaining = db::count_available_warm_for_mesh_inner(&conn, mesh_id).unwrap();
    assert_eq!(remaining, 1);
    assert!(!std::path::Path::new(&path_oldest).exists());
    assert!(!std::path::Path::new(&path_middle).exists());
    assert!(std::path::Path::new(&path_newest).exists());

    // Fill phase — bump target to 2 and run the fill loop, which
    // pins the issue #634 contract: when count < target, we cut
    // exactly ONE worktree per iteration (mirrors `warm_one_body`).
    conn.execute(
        "UPDATE meshes SET pre_spawn_pool_size = 2 WHERE id = ?1",
        rusqlite::params![mesh_id],
    )
    .unwrap();
    let target: i64 = conn
        .query_row(
            "SELECT pre_spawn_pool_size FROM meshes WHERE id = ?1",
            rusqlite::params![mesh_id],
            |row| row.get(0),
        )
        .unwrap();

    let mut new_paths: Vec<String> = Vec::new();
    for _ in 0..target {
        let count = db::count_available_warm_for_mesh_inner(&conn, mesh_id).unwrap();
        if count >= target {
            break;
        }
        new_paths.push(cut_real_warm_worktree(&conn, mesh_id, &repo_path));
    }

    let post = db::count_available_warm_for_mesh_inner(&conn, mesh_id).unwrap();
    assert_eq!(post, 2, "fill must reach target");
    for p in &new_paths {
        assert!(std::path::Path::new(p).exists(), "new dir {} must exist", p);
    }

    // At-target no-op: running the fill loop again must not create
    // anything new (the `available >= target` early-exit).
    let mut new_paths_2 = 0;
    for _ in 0..target {
        let count = db::count_available_warm_for_mesh_inner(&conn, mesh_id).unwrap();
        if count >= target {
            break;
        }
        cut_real_warm_worktree(&conn, mesh_id, &repo_path);
        new_paths_2 += 1;
    }
    assert_eq!(new_paths_2, 0, "at-target fill loop must early-exit");
    let final_count = db::count_available_warm_for_mesh_inner(&conn, mesh_id).unwrap();
    assert_eq!(final_count, 2, "at-target no-op must leave count unchanged");
}

/// The fill's crash-recovery path (issue #2533).
///
/// The fill writes its crash-recovery marker (`filling`) BEFORE it cuts
/// the worktree, and flips the row to `available` afterwards. A process
/// killed in that window leaves the `filling` row behind — and that row is
/// the only record that a half-built directory exists. This test kills
/// the fill there for real (real repo, real checkout, real DB) and pins
/// the two halves of the contract:
///
///   1. the marker survives the kill, and
///   2. the startup reconcile finds it and tears the half-built worktree
///      down.
///
/// It also pins the age guard: a fill that is *still running* must not be
/// reconciled, or the startup pass would destroy a worktree a worker is
/// cutting right now.
#[test]
fn a_fill_killed_mid_way_is_found_by_the_startup_reconcile() {
    let _db = crate::db::test_support::isolated();

    let (tmp, repo_path) = fresh_git_repo();
    // Scope every write guard tightly: `db::write_conn` locks a
    // non-reentrant mutex, so holding a guard across a `db::…` call that
    // takes its own guard would deadlock the test.
    let mesh_id = {
        let conn = crate::db::write_conn();
        insert_mesh(&conn, &repo_path)
    };
    // Keep the worktree inside the temp dir so the test never touches a
    // real worktree container.
    let worktrees_dir = tmp.path().join("warm");
    std::fs::create_dir_all(&worktrees_dir).expect("worktrees dir");

    let mesh = db::WarmPoolMeshRow {
        id: mesh_id,
        path: repo_path.clone(),
        base_ref: "HEAD".to_string(),
        pre_spawn_pool_size: 1,
        worktree_directory: Some(worktrees_dir.to_string_lossy().to_string()),
    };

    // The kill: the checkout succeeds, then the flip to `available` fails
    // — the process would have died right there. The row must stay at
    // `filling`.
    let repo_for_cut = repo_path.clone();
    let deps = FillDeps {
        insert_marker: Box::new(move |path, slug| db::begin_warm_fill(mesh_id, path, slug)),
        create_worktree: Box::new(move |path, slug| {
            crate::git::worktree::create_git_worktree(&repo_for_cut, path, slug, "detached", "HEAD")
        }),
        mark_available: Box::new(|_row_id, _base_sha| Err(rusqlite::Error::InvalidQuery)),
        delete_row: Box::new(db::delete_warm_worktree),
        head_sha: Box::new(|path| {
            crate::git::sha::resolve_sha_at(path, &crate::git::sha::ShaSource::LocalHead)
                .ok()
                .map(|oid| oid.to_string())
        }),
        emit_changed: Box::new(|| {}),
    };
    warm_one_with(&mesh, deps).expect("the fill's own failure is not fatal");

    // The marker survived the kill, and the half-built directory is on
    // disk — the exact state a killed fill leaves behind.
    let (row_id, status, path): (i64, String, String) = {
        let conn = crate::db::write_conn();
        conn.query_row(
            "SELECT id, status, path FROM warm_worktrees WHERE mesh_id = ?1",
            rusqlite::params![mesh_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("the filling row must still be there")
    };
    assert_eq!(
        status, "filling",
        "a killed fill must leave the crash-recovery marker behind"
    );
    assert!(
        std::path::Path::new(&path).exists(),
        "the half-built worktree must still be on disk"
    );

    // A fill that is still in flight must NOT be reconciled — the age
    // guard is what keeps the startup pass from destroying a worktree a
    // worker is cutting right now.
    let in_flight = db::list_warm_worktrees_to_reconcile(WARM_FILL_STALE_AFTER_MINUTES)
        .expect("reconcile scan");
    assert!(
        in_flight.is_empty(),
        "a fresh `filling` row must not be reconciled: {in_flight:?}"
    );

    // Now the app was closed and relaunched: the marker is older than
    // the stale window, so it IS a crash orphan.
    {
        let conn = crate::db::write_conn();
        conn.execute(
            "UPDATE warm_worktrees SET created_at = datetime('now', '-1 hour') WHERE id = ?1",
            rusqlite::params![row_id],
        )
        .expect("backdate the marker");
    }

    let orphans = db::list_warm_worktrees_to_reconcile(WARM_FILL_STALE_AFTER_MINUTES)
        .expect("reconcile scan");
    assert_eq!(
        orphans.len(),
        1,
        "the startup reconcile must find the killed fill's marker"
    );
    assert_eq!(orphans[0].id, row_id);
    assert_eq!(orphans[0].path, path);
    assert!(
        orphans[0].dir_present,
        "the half-built worktree is still on disk, so it must be torn down"
    );
    assert_eq!(
        orphans[0].reason,
        db::WarmReconcileReason::CrashedFill,
        "the reconcile must say WHY it is reclaiming the row"
    );

    // The reconcile tears the worktree down and only then drops the row.
    reconcile_warm_entries(
        orphans,
        crate::git::worktree::remove_one_worktree,
        db::delete_warm_worktree,
    );

    let remaining: i64 = {
        let conn = crate::db::write_conn();
        conn.query_row(
            "SELECT COUNT(*) FROM warm_worktrees WHERE id = ?1",
            rusqlite::params![row_id],
            |row| row.get(0),
        )
        .expect("row count")
    };
    assert_eq!(remaining, 0, "the crashed fill's row must be gone");
    assert!(
        !std::path::Path::new(&path).exists(),
        "the half-built worktree must be gone from disk"
    );
}
