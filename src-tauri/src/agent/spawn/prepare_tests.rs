use super::prepare::{
    resolve_base_ref_for_spawn, SpawnInFlightClaim, SpawnOptions, DEFAULT_WORKTREE_MODE,
};
use super::WorktreePolicy;
use crate::models::Provider;
use tempfile::TempDir;

/// Pin the spawn-time fallback. Sole pin of `DEFAULT_WORKTREE_MODE`
/// after #411 deleted the TS-side sentinel (it had no real consumer).
#[test]
fn default_worktree_mode_is_branched() {
    assert_eq!(DEFAULT_WORKTREE_MODE, "branched");
}

/// `SpawnOptions` must carry the explicit slots through to
/// `spawn_agent_inner` (issue #1155 AC #1). The orchestrator
/// destructures them out of `opts`; this test pins the struct
/// shape so a refactor that drops either field fails compilation.
#[test]
fn spawn_options_carries_explicit_slots() {
    let opts = SpawnOptions {
        session_id: -1,
        provider: Provider::Anthropic,
        resume: None,
        rows: 24,
        cols: 80,
        prefill: None,
        node: None,
        explicit_model: Some("sonnet-4".into()),
        explicit_effort: Some("low".into()),
        // Issue #1358: every transport that builds a `SpawnRequest`
        // and reaches `spawn_agent_inner` via `spawn_with_intent`
        // forwards `explicit_extra_args` from the v2 SpawnAgentNode
        // explicit slot. None is fine — the resolver then cascades
        // through mesh / app defaults and `default_prepare` only
        // forwards the string when `supports_extra_args = true`.
        explicit_extra_args: None,
        worktree_policy: WorktreePolicy::RespectMesh,
    };
    assert_eq!(opts.explicit_model.as_deref(), Some("sonnet-4"));
    assert_eq!(opts.explicit_effort.as_deref(), Some("low"));
    assert!(opts.explicit_extra_args.is_none());
}

// -----------------------------------------------------------------------
// Per-session spawn claim (duplicate-spawn fix). `is_agent_already_running`
// only sees registered processes and registration is seconds into the
// pipeline, so the claim must cover the whole `spawn_agent_inner` body.
// Test ids are unique across the suite (tests share the process-global
// set and run in parallel).
// -----------------------------------------------------------------------

#[test]
fn spawn_claim_rejects_concurrent_duplicate_for_same_session() {
    let first = SpawnInFlightClaim::try_claim(-917_0001);
    assert!(first.is_some(), "first claim must succeed");
    assert!(
        SpawnInFlightClaim::try_claim(-917_0001).is_none(),
        "second claim for the same session while the first is held must \
             be rejected — this is what stops a duplicate spawn_agent_inner \
             from killing the in-flight spawn's freshly-booted process"
    );
}

#[test]
fn spawn_claim_is_per_session() {
    let _a = SpawnInFlightClaim::try_claim(-917_0002).expect("claim a");
    assert!(
        SpawnInFlightClaim::try_claim(-917_0003).is_some(),
        "claims for different sessions must not contend"
    );
}

#[test]
fn spawn_claim_released_on_drop() {
    {
        let _claim = SpawnInFlightClaim::try_claim(-917_0004).expect("claim");
    }
    assert!(
        SpawnInFlightClaim::try_claim(-917_0004).is_some(),
        "dropping the claim must release the session for the next spawn \
             (RAII covers every return path, including cancelled tasks)"
    );
}

/// Regression guard for the user-visible "failed to start" symptom.
///
/// Spawn RACERS threads racing `try_claim` for the same session —
/// the first to acquire the HashSet entry wins, the rest see the
/// entry present and get `None`. Pins the entire atomicity story:
/// without it, two concurrent `spawn_agent_inner` calls for the
/// same node (backend stage-2 vs frontend Terminal auto-spawn on
/// `'idle'`) both passed the registry check and the loser's step-2
/// stale-kill destroyed the winner's freshly-booted process — the
/// "failed to start, yet it boots seconds later" symptom.
///
/// Uses a fresh session id per round so the test doesn't depend on
/// the racing threads' Drop ordering vs the next round's claim —
/// the global HashSet could in principle still hold a stale entry
/// from a previous round's racer that hasn't yet been observed as
/// dropped by the test thread (parking_lot's Drop is synchronous,
/// but the test thread's join() happens-before the next round).
#[test]
fn concurrent_spawn_claim_exactly_one_winner() {
    use std::sync::atomic::{AtomicUsize, Ordering as AOrd};
    use std::sync::Arc;

    const RACERS: usize = 8;
    const ROUNDS: usize = 200;

    for round in 0..ROUNDS {
        // Fresh session id per round so there's no cross-round
        // dependency on Drop ordering.
        let session: i64 = -917_1000 - round as i64;

        let winners = Arc::new(AtomicUsize::new(0));
        // Two barriers: gate the racers before the lock, AND gate
        // them before the drop. Without the second gate, a racer
        // that loses the lock race still releases its (empty) claim
        // path before the next racer even tries — the second
        // barrier forces every racer to attempt the lock with the
        // claim held until the round-end signal.
        let start_barrier = Arc::new(std::sync::Barrier::new(RACERS + 1));
        let end_barrier = Arc::new(std::sync::Barrier::new(RACERS + 1));

        let handles: Vec<_> = (0..RACERS)
            .map(|_| {
                let winners = winners.clone();
                let start = start_barrier.clone();
                let end = end_barrier.clone();
                std::thread::spawn(move || {
                    // Phase 1: align all racers at the lock.
                    start.wait();
                    let claim = SpawnInFlightClaim::try_claim(session);
                    if claim.is_some() {
                        winners.fetch_add(1, AOrd::SeqCst);
                    }
                    // Phase 2: hold the claim until the test thread
                    // signals round end. Any racer arriving at the
                    // lock now MUST see the existing entry (the
                    // insert returns false → claim is None).
                    end.wait();
                    drop(claim);
                })
            })
            .collect();

        // Fire the start gun — every racer races for the lock now.
        start_barrier.wait();
        // Give every racer time to acquire the lock, observe the
        // entry, and reach the end barrier.
        end_barrier.wait();
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(
            winners.load(AOrd::SeqCst),
            1,
            "exactly one racer must win the claim (round {round}, session {session})"
        );

        // After the last racing thread joined, its _claim dropped,
        // releasing the entry. Confirm by claiming it ourselves —
        // this exercises the post-drop "slot is empty" invariant
        // and prevents cross-round state pollution if a future
        // refactor accidentally leaks entries.
        assert!(
            SpawnInFlightClaim::try_claim(session).is_some(),
            "round {round}: racers all joined so their claims dropped — \
                 the next try_claim for session {session} must find the slot empty"
        );
    }
}

/// RAII must release on a *cancelled* async task too — the field doc
/// on `SpawnInFlightClaim` makes that an explicit guarantee. A
/// `tokio::time::timeout` racing a future that holds the claim is
/// the cheapest reproduction: the future is dropped at the await
/// point, the claim's Drop runs synchronously, and the next
/// `try_claim` must succeed.
#[test]
fn spawn_claim_released_when_async_task_is_cancelled() {
    // No real DB / PTY needed — the claim itself is what we're
    // pinning. Drive it on a runtime so the cancellation path
    // (Future::drop mid-await) actually runs.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let session = -917_0006;
    rt.block_on(async {
        // Spawn a task that holds the claim for "the whole pipeline"
        // (here, forever). Cancel it via timeout.
        let task = tokio::spawn(async move {
            let _claim = SpawnInFlightClaim::try_claim(session).expect("first claim must succeed");
            // Park forever. The test cancels this task below.
            std::future::pending::<()>().await;
        });

        // Let the task reach its pending await.
        tokio::task::yield_now().await;
        task.abort();
        // The abort drops the task's locals → Drop runs → claim released.
        let _ = task.await;

        assert!(
            SpawnInFlightClaim::try_claim(session).is_some(),
            "aborting the holding task must release the claim (RAII covers \
                 cancelled futures, not just successful return)"
        );
    });
}

// -----------------------------------------------------------------------
// base_ref resolution (master-trunk regression)
//
// Pre-fix, the spawn path hardcoded `"origin/main"` as the default
// `base_ref` when the `meshes.base_ref` DB column was `'origin/main'`
// (its COALESCE default) — meaning a master-trunk repo always hit
// `mesh-sync-warning` on every spawn (`fatal: couldn't find remote
// ref main`). These tests pin the resolution chain:
//
//   1. meshes.base_ref (BUT NOT the COALESCE default — that's
//      treated as "no config" so the detection chain runs)
//   2. refs/remotes/origin/HEAD read from the local repo
//   3. "origin/main" last resort
//
// The COALESCE-sentinel treatment is critical: the DB column is
// NOT NULL with default `'origin/main'`, so `Mesh.base_ref` is
// ALWAYS a non-empty `String` and `MeshRow.base_ref` is ALWAYS
// `Some(_)` — a naive `if let Some(b) = config_base_ref { return b }`
// would make the detection chain dead code in production. The
// `resolve_base_ref_treats_coalesce_sentinel_as_unset` test pins the
// production call path (`Some("origin/main")`).
// -----------------------------------------------------------------------

#[test]
fn resolve_base_ref_uses_config_value_when_set() {
    // The config wins even on a non-repo / non-master path — explicit
    // user intent overrides any auto-detection. Empty / whitespace
    // config falls through to the detection chain (regression guard
    // for an empty-string value slipping through the COALESCE).
    let tmp = TempDir::new().unwrap();
    assert_eq!(
        resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), Some("origin/develop")),
        "origin/develop"
    );
    // Empty / whitespace strings are treated as "no config" so the
    // detection chain runs — mirrors the COALESCE-to-default contract
    // in the DB layer.
    assert_eq!(
        resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), Some("")),
        "origin/main",
        "empty config base_ref must fall through to detection, not propagate"
    );
    assert_eq!(
        resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), Some("   ")),
        "origin/main",
        "whitespace-only config base_ref must fall through to detection"
    );
}

#[test]
fn resolve_base_ref_falls_back_to_origin_main_for_non_repo() {
    // Non-repo path with no config — must not panic. Last-resort
    // behaviour preserved: `get_default_branch` returns "main" on a
    // failed `Repository::open`, and we prefix it with "origin/".
    // The spawn path itself short-circuits to `RepoUnusable` so the
    // auto-sync result is non-blocking.
    let tmp = TempDir::new().unwrap();
    let resolved = resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), None);
    assert_eq!(resolved, "origin/main");
}

#[test]
fn resolve_base_ref_detects_master_via_origin_head() {
    // Headline regression test: a master-trunk repo with no
    // `base_ref` in mesh config must produce "origin/master", not
    // the legacy "origin/main". Pre-fix, this always returned
    // "origin/main" and the spawn emitted a `mesh-sync-warning` on
    // every node.
    use crate::env::test_helpers::TestDir;
    use git2;

    let td = TestDir::new("base_ref_master");
    let parent = td.path();
    // Create a working repo on whatever default branch git picks.
    // The local branch name doesn't matter — what matters is that
    // `refs/remotes/origin/HEAD` points at `refs/remotes/origin/master`.
    crate::env::test_helpers::init_repo_with_commit(parent, &[("README.md", "v1\n")]);

    let repo = git2::Repository::open(parent).unwrap();
    let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    // Build the symbolic ref that `get_default_branch` reads.
    repo.reference("refs/remotes/origin/master", oid, true, "test setup")
        .unwrap();
    repo.reference_symbolic(
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/master",
        true,
        "test setup",
    )
    .unwrap();

    // Sanity: precondition for the test to be meaningful.
    let head_ref = repo
        .find_reference("refs/remotes/origin/HEAD")
        .unwrap()
        .symbolic_target()
        .unwrap()
        .to_string();
    assert_eq!(
        head_ref, "refs/remotes/origin/master",
        "precondition: origin/HEAD must point at refs/remotes/origin/master"
    );

    let resolved = resolve_base_ref_for_spawn(parent.to_str().unwrap(), None);
    assert_eq!(
        resolved, "origin/master",
        "master-trunk repo with no base_ref in config must yield origin/master, \
             not the legacy hardcoded origin/main (this is the master-trunk regression)"
    );
}

#[test]
fn resolve_base_ref_detects_main_via_origin_head() {
    // Sanity pin: the existing main-trunk behaviour (a repo whose
    // origin/HEAD points at `main`) must still resolve to
    // "origin/main" after the fix. Guards against the master fix
    // accidentally regressing the main case.
    use crate::env::test_helpers::TestDir;
    use git2;

    let td = TestDir::new("base_ref_main");
    let parent = td.path();
    crate::env::test_helpers::init_repo_with_commit(parent, &[("README.md", "v1\n")]);

    let repo = git2::Repository::open(parent).unwrap();
    let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    repo.reference("refs/remotes/origin/main", oid, true, "test setup")
        .unwrap();
    repo.reference_symbolic(
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        true,
        "test setup",
    )
    .unwrap();

    let resolved = resolve_base_ref_for_spawn(parent.to_str().unwrap(), None);
    assert_eq!(
        resolved, "origin/main",
        "main-trunk repo must still resolve to origin/main (no regression)"
    );
}

#[test]
fn resolve_base_ref_treats_coalesce_sentinel_as_unset() {
    // The production call path: `meshes.base_ref` is a NOT NULL
    // column with a COALESCE default of `'origin/main'` (see
    // `db::MESH_COLUMNS`). A fresh mesh whose base_ref was never
    // explicitly set reads as `Some("origin/main")` from the DB →
    // `MeshRow.base_ref = Some("origin/main")` →
    // `config.as_ref().and_then(|c| c.base_ref.as_deref())` returns
    // `Some("origin/main")`. The helper MUST treat this sentinel as
    // "no config" and fall through to the detection chain, otherwise
    // a master-trunk repo's spawn still hits `mesh-sync-warning`.
    // The earlier `_detects_master_via_origin_head` test passes
    // `None` (which never reaches production); THIS test pins the
    // actual production contract.
    use crate::env::test_helpers::TestDir;
    use git2;

    let td = TestDir::new("base_ref_coalesce_master");
    let parent = td.path();
    crate::env::test_helpers::init_repo_with_commit(parent, &[("README.md", "v1\n")]);

    let repo = git2::Repository::open(parent).unwrap();
    let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    repo.reference("refs/remotes/origin/master", oid, true, "test setup")
        .unwrap();
    repo.reference_symbolic(
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/master",
        true,
        "test setup",
    )
    .unwrap();

    // Production-shaped input: COALESCE default from the DB.
    let resolved = resolve_base_ref_for_spawn(parent.to_str().unwrap(), Some("origin/main"));
    assert_eq!(
        resolved, "origin/master",
        "the COALESCE default 'origin/main' from a fresh mesh's DB row \
             must be treated as 'no config' — fall through to origin/HEAD \
             detection. A master-trunk repo with an unconfigured mesh \
             produces origin/master, not origin/main. This is the actual \
             production contract; the test passing None never reaches \
             production."
    );
}

#[test]
fn resolve_base_ref_keeps_explicit_user_value_for_main_trunk() {
    // A user who LEGITIMATELY sets `base_ref = "origin/main"` (via
    // the 'Fresh' UI option) on a main-trunk repo must still get
    // "origin/main" back. The COALESCE-sentinel treatment must
    // apply to the *fresh* / *unconfigured* case, not penalize a
    // user who explicitly chose the same value. For a main-trunk
    // repo the auto-detect would return the same value, so this
    // test is mostly a documentation pin.
    use crate::env::test_helpers::TestDir;
    use git2;

    let td = TestDir::new("base_ref_explicit_main");
    let parent = td.path();
    crate::env::test_helpers::init_repo_with_commit(parent, &[("README.md", "v1\n")]);

    let repo = git2::Repository::open(parent).unwrap();
    let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    repo.reference("refs/remotes/origin/main", oid, true, "test setup")
        .unwrap();
    repo.reference_symbolic(
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        true,
        "test setup",
    )
    .unwrap();

    let resolved = resolve_base_ref_for_spawn(parent.to_str().unwrap(), Some("origin/main"));
    assert_eq!(
        resolved, "origin/main",
        "explicit user-set 'origin/main' on a main-trunk repo must resolve \
             to 'origin/main' (same as auto-detect — no behaviour change)"
    );
}

/// WorkspaceToProvision is git/disk inputs. Launch knobs live on LaunchParams.
#[test]
fn workspace_to_provision_has_no_pty_or_cascade_fields() {
    let src = include_str!("provision.rs");
    let start = src
        .find("pub(super) struct WorkspaceToProvision")
        .expect("WorkspaceToProvision must exist");
    let rest = &src[start..];
    let end = rest
        .find("pub(super) struct ProvisionedWorkspace")
        .expect("ProvisionedWorkspace follows");
    let body = &rest[..end];
    for needle in [
        "rows",
        "cols",
        "prefill",
        "explicit_model",
        "explicit_effort",
        "explicit_extra_args",
    ] {
        assert!(
            !body.contains(needle),
            "WorkspaceToProvision must not courier launch knob {needle}"
        );
    }
}

#[test]
fn prepare_does_not_acquire_the_in_flight_claim() {
    let src = include_str!("prepare.rs");
    let fn_start = src
        .find("pub(super) async fn prepare_context")
        .expect("prepare_context must exist");
    let body = &src[fn_start..];
    assert!(
        !body.contains("SpawnInFlightClaim::try_claim"),
        "prepare_context must not acquire the claim; spawn_agent_inner owns it"
    );
}
