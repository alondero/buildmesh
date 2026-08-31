#![allow(unused_imports)]

use super::{orchestrator::*, provision::*, reader::*, *};
use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};
use crate::agent::provider::Platform;
use crate::models::{EnvType, Provider};
// The eight worktree-provision helpers were moved to
// `crate::git::worktree::provision` in PR #676 / issue #677, and #698
// added `locked_fetch_pr_head` on top. The tests here exercise them by
// name, so re-import at the test-module scope.
use crate::agent::capabilities::ResolvedAgentConfig;
use crate::git::worktree::provision::{
    adopt_warm_worktree_by_move, fetch_fork_head, fetch_single_ref, fork_remote_alias,
    locked_fetch_pr_head, read_origin_ref_sha, upgrade_warm_to_mode,
};
use tempfile::TempDir;

fn read_injected_settings(project: &std::path::Path) -> serde_json::Value {
    let path = project.join(".claude").join("settings.local.json");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("settings.local.json not written: {}", e));
    serde_json::from_str(&content).expect("settings.local.json is not valid JSON")
}

/// The Notification hook must fire on EVERY notification type, not just
/// `idle_prompt`. An empty matcher is Claude Code's "match all" — without it
/// the hook ignores `permission_prompt` notifications, so the user is never
/// alerted when an agent asks to run a tool or otherwise needs a decision.
/// Regression guard for the "only alerted after the agent finishes" gap.
#[test]
fn attention_hook_notification_matcher_is_catch_all() {
    let temp = TempDir::new().unwrap();
    inject_attention_hook(temp.path()).unwrap();

    let settings = read_injected_settings(temp.path());
    let notification = &settings["hooks"]["Notification"][0];
    assert_eq!(
        notification["matcher"], "",
        "Notification matcher must be empty (catch-all) so permission_prompt \
             notifications alert the user, not just idle_prompt"
    );
    let command = notification["hooks"][0]["command"]
        .as_str()
        .expect("notification hook command should be a string");
    assert!(
        command.contains("/api/attention/"),
        "notification hook should POST to the attention endpoint, got: {command}"
    );
}

/// A `Stop` hook fires the instant the agent finishes a turn, so the user is
/// alerted immediately rather than waiting for the `idle_prompt` idle timer.
#[test]
fn attention_hook_includes_stop_event() {
    let temp = TempDir::new().unwrap();
    inject_attention_hook(temp.path()).unwrap();

    let settings = read_injected_settings(temp.path());
    let command = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .expect("Stop hook command should be present so turn-end alerts fire immediately");
    assert!(
        command.contains("/api/attention/"),
        "Stop hook should POST to the attention endpoint, got: {command}"
    );
}

/// Both hooks must forward the hook's stdin JSON as the POST body (issue
/// #878). Claude Code pipes `{hook_event_name, transcript_path, …}` into
/// the command; without `--data-binary @-` the backend gets an empty body
/// and cannot tell "turn ended, user needed" from "turn ended, waiting on
/// background tasks".
#[test]
fn attention_hook_forwards_stdin_payload() {
    let temp = TempDir::new().unwrap();
    inject_attention_hook(temp.path()).unwrap();

    let settings = read_injected_settings(temp.path());
    for (event, path) in [
        (
            "Notification",
            &settings["hooks"]["Notification"][0]["hooks"][0],
        ),
        ("Stop", &settings["hooks"]["Stop"][0]["hooks"][0]),
    ] {
        let command = path["command"].as_str().unwrap();
        assert!(
            command.contains("--data-binary @-"),
            "{event} hook must forward stdin as the POST body, got: {command}"
        );
        assert!(
            command.contains("Content-Type: application/json"),
            "{event} hook must declare a JSON body, got: {command}"
        );
    }
}

/// Injection is idempotent: a second call over an already-correct file must
/// not rewrite it (the early-return guard) and must leave it parseable.
#[test]
fn attention_hook_injection_is_idempotent() {
    let temp = TempDir::new().unwrap();
    inject_attention_hook(temp.path()).unwrap();
    let first = read_injected_settings(temp.path());
    inject_attention_hook(temp.path()).unwrap();
    let second = read_injected_settings(temp.path());
    assert_eq!(first, second, "second injection should be a no-op");
}

/// Injection must preserve unrelated keys already present in the user's
/// settings.local.json (e.g. `permissions`) — it only owns `hooks`.
#[test]
fn attention_hook_preserves_other_settings() {
    let temp = TempDir::new().unwrap();
    let claude_dir = temp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.local.json"),
        r#"{"permissions":{"allow":["Bash(ls:*)"]}}"#,
    )
    .unwrap();

    inject_attention_hook(temp.path()).unwrap();

    let settings = read_injected_settings(temp.path());
    assert_eq!(
        settings["permissions"]["allow"][0], "Bash(ls:*)",
        "pre-existing permissions must survive hook injection"
    );
    assert_eq!(settings["hooks"]["Notification"][0]["matcher"], "");
}

// ----- fork alias + fetch_fork_head (issue #443) ---------------------

/// `fork-<login>` is the human-readable alias used in `git remote -v` and
/// the worktree `base_ref` string. The `fork-` prefix keeps our entries
/// easy to spot in the remote list and trivial to clean up if we ever
/// need to. Pin the format so a future refactor that swaps the prefix
/// surfaces as a test failure rather than a silent rename in user
/// worktrees.
#[test]
fn fork_remote_alias_uses_fork_prefix() {
    assert_eq!(fork_remote_alias("alice"), "fork-alice");
    assert_eq!(fork_remote_alias("alondero"), "fork-alondero");
}

/// Build a bare "fork" repo (a real local clone target so the test
/// doesn't need a network round-trip) and a regular repo that will
/// register the fork as a remote. The fork has a single commit on
/// `main` plus a `feat/443-fork` branch so the fetch can target a
/// non-default ref. Returns `(local, fork_bare_dir, fork_path)` —
/// the caller holds the dirs for the duration of the test.
fn init_fork_fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    // Source: a regular repo with a feature branch we can fetch.
    let src = TempDir::new().unwrap();
    let src_path = src.path().to_path_buf();
    let src_repo = git2::Repository::init(&src_path).unwrap();
    let sig = git2::Signature::now("test", "test@example.com").unwrap();
    std::fs::write(src_path.join("README.md"), "fork-source\n").unwrap();
    let mut index = src_repo.index().unwrap();
    index.add_path(std::path::Path::new("README.md")).unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = src_repo.find_tree(tree_oid).unwrap();
    let main_commit = src_repo
        .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
        .unwrap();
    // Branch off a feature branch.
    let feat_commit = src_repo
        .commit(
            Some("HEAD"),
            &sig,
            &sig,
            "feat: fork-only commit",
            &tree,
            &[&src_repo.find_commit(main_commit).unwrap()],
        )
        .unwrap();
    let _ = tree;
    // `main_commit` is a `git2::Oid` (Copy) — no need to `drop` it; the
    // explicit `drop()` was a no-op flagged by clippy.
    let feat_commit = src_repo.find_commit(feat_commit).unwrap();
    src_repo
        .branch("feat/443-fork", &feat_commit, true)
        .unwrap();
    // Bare clone target (so the fork has no working tree, like a real
    // remote on GitHub — `git fetch` reads its objects directly).
    // Use a unique, path-safe name — avoid `{:?}` on the source path
    // (it produces `C:\...` with backslashes and quotes that don't
    // round-trip as a directory name on Windows).
    let bare_dir = std::env::temp_dir().join(format!(
        "buildmesh_fork_bare_{}_{}",
        std::process::id(),
        NEXT_FORK_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
    ));
    let _ = std::fs::remove_dir_all(&bare_dir);
    let clone = git2::Repository::init_bare(&bare_dir).unwrap();
    let mut remote = clone.remote("origin", src_path.to_str().unwrap()).unwrap();
    remote
        .fetch(&["refs/heads/*:refs/heads/*"], None, None)
        .unwrap();
    // Local: a fresh repo with no remotes — this is what
    // `fetch_fork_head` will register the fork on.
    let local = TempDir::new().unwrap();
    git2::Repository::init(local.path()).unwrap();
    (local, bare_dir, src_path)
}

/// Atomic counter for unique bare-repo paths (one per test run).
static NEXT_FORK_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// First-time registration: the fork is added as `fork-alice` and the
/// head ref is materialised. `fetch_fork_head` returns `true` and
/// the resulting `git ls-remote` shows the ref under the alias.
/// This is the end-to-end "fork spawn" path that issue #443 opens up.
#[test]
fn fetch_fork_head_registers_remote_and_fetches_ref() {
    let (local, bare_dir, _src) = init_fork_fixture();
    let bare_dir_str = bare_dir.to_str().unwrap().to_string();

    let ok = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &bare_dir_str,
        "feat/443-fork",
    );
    assert!(ok, "fetch_fork_head must succeed on a real bare repo");

    // Verify the alias + URL are registered.
    let local_repo = git2::Repository::open(local.path()).unwrap();
    let remote = local_repo
        .find_remote("fork-alice")
        .expect("fork-alice remote must be registered");
    let url = remote.url().expect("remote URL must be set");
    assert_eq!(
        url, bare_dir_str,
        "remote URL must match the fork's clone URL"
    );

    // Verify the ref was fetched — it should be visible as
    // `fork-alice/feat/443-fork`.
    let reference = local_repo
        .find_reference("refs/remotes/fork-alice/feat/443-fork")
        .expect("fetched ref must be present under fork-alice/");
    assert!(
        reference.target().is_some(),
        "ref target must be a real OID"
    );
}

/// Idempotent: a second call on a repo that already has the remote
/// registered AND the right URL is a no-op. The user can spawn a
/// second agent on the same fork PR (e.g. after closing the first)
/// without `git remote add` failing. The function still returns
/// `true` because the fetch succeeds.
#[test]
fn fetch_fork_head_is_idempotent_on_repeat_call() {
    let (local, bare_dir, _src) = init_fork_fixture();
    let bare_dir_str = bare_dir.to_str().unwrap().to_string();

    let first = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &bare_dir_str,
        "feat/443-fork",
    );
    assert!(first, "first call must succeed");

    // Second call with the SAME URL — must not error (the `remote add`
    // path is the failure-prone one without the existence check; the
    // `get-url` probe should return the right URL and skip the add).
    let second = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &bare_dir_str,
        "feat/443-fork",
    );
    assert!(second, "second call must still succeed (idempotent)");

    // Remote is still there, single entry.
    let local_repo = git2::Repository::open(local.path()).unwrap();
    let remote = local_repo
        .find_remote("fork-alice")
        .expect("fork-alice remote must still be registered after repeat call");
    assert_eq!(remote.url().unwrap(), bare_dir_str);
}

/// URL drift: if the fork's clone URL changes between spawns (the
/// user renamed the repo, or — more likely — the first call stored a
/// stale URL), the second call should update the existing remote's
/// URL via `git remote set-url` rather than fail or keep the stale
/// URL. Pin this so a future refactor that skips the set-url branch
/// surfaces as a test failure (the second call would silently fetch
/// the wrong ref).
#[test]
fn fetch_fork_head_updates_url_on_drift() {
    let (local, bare_dir, _src) = init_fork_fixture();
    let stale_url = bare_dir.to_str().unwrap().to_string();
    // Reuse the SAME bare dir (so the second call still finds a real
    // repo) but pretend the URL "drifted" by passing a different
    // string that ALSO resolves to the same on-disk repo. We achieve
    // that with a file:// URL on Windows (path with backslashes
    // round-trip cleanly through git remote add).
    let drifted_url = format!("file://{}", stale_url.replace('\\', "/"));

    // First call: register the stale URL.
    let first = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &stale_url,
        "feat/443-fork",
    );
    assert!(first, "first call must succeed");

    // Second call: same alias, drifted URL — the function should run
    // `git remote set-url` and re-fetch.
    let second = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &drifted_url,
        "feat/443-fork",
    );
    assert!(second, "second call must still succeed after URL drift");

    // The stored URL must be the drifted one, not the original.
    let local_repo = git2::Repository::open(local.path()).unwrap();
    let remote = local_repo
        .find_remote("fork-alice")
        .expect("remote must still be registered");
    let stored = remote.url().unwrap();
    // git normalises file:// URLs slightly on Windows — assert it's
    // the drifted one rather than the original.
    assert_ne!(
        stored, stale_url,
        "URL must have been updated, not left at the stale value"
    );
}

/// Failure path: a non-existent clone URL must return `false` rather
/// than panic. The caller (`spawn_agent_inner`) falls back to the
/// mesh's `base_ref` and emits a `mesh-sync-warning` toast with
/// `outcome: "pr_fork_unfetchable"`. Without the failure-as-false
/// contract, a typo'd clone URL would either spawn on the wrong
/// commits silently or surface as a hard error every offline session.
#[test]
fn fetch_fork_head_returns_false_on_bad_clone_url() {
    let (local, _bare_dir, _src) = init_fork_fixture();
    let bad_url = "/nonexistent/path/to/fork/that/does/not/exist".to_string();

    let ok = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &bad_url,
        "feat/443-fork",
    );
    assert!(!ok, "fetch_fork_head must return false on a bad clone URL");
}

// ----- fetch_single_ref (issue #420) ---------------------------------
//
// Same-repo PR spawn (#420) — the worktree adoption path calls
// `fetch_single_ref` to materialise `origin/<head_ref>` so the worktree
// can be cut from it. As of issue #446 the function is a thin wrapper
// over `git::sync::do_fetch_only` (the fetch-only half of `do_sync` —
// open + dirty-check + has-remote + `git fetch`, NO `git pull` tail);
// the `-`-adversarial-ref hardening is preserved at the wrapper
// boundary because `do_fetch_only` passes the branch as a plain argv
// entry without a `--` separator (it doesn't know about the spawn
// context).
//
// These tests pin the cases the issue calls out:
//   1. success — ref exists on origin
//   2. ref-not-found — ref missing on origin (caller falls back to base_ref)
//   3. non-git path — caller passed a directory that isn't a repo
//   4. adversarial ref — `-`-prefixed input is rejected by the wrapper
//      before `do_fetch_only` sees it (the hardening migrated from the
//      shell-out's `--` separator to an upfront string check, since
//      `do_fetch_only` doesn't pass a `--` separator to `git fetch`)
//   5. dirty-skip (issue #446 acceptance #2) — a parent repo with
//      uncommitted changes must return `false` (mirrors
//      `fetch_origin_skips_dirty_parent` in `git/fetch_origin_tests.rs`)
//
// The fixture mirrors `init_fork_fixture` but for the same-repo path:
// a bare repo holds a single branch, the local repo has `origin`
// pointed at the bare, and the test calls `fetch_single_ref` against
// the local repo's path.

/// Build a "remote + local" pair: the bare repo has a single commit on
/// `main` plus a `feat/420-pr-spawn` branch; the local repo has `origin`
/// pointed at the bare. Returns `(local, bare_path)` — the local TempDir
/// owns its on-disk path; `bare_path` is a plain PathBuf that lives
/// inside `std::env::temp_dir()` and is reused across calls (it gets
/// re-populated with the same content each time, so the SHA is stable
/// per-test-process).
fn init_same_repo_fixture() -> (TempDir, std::path::PathBuf) {
    // Source: a working repo with a feature branch we can fetch.
    // We reuse the same on-disk source across tests in a single
    // process — `init_same_repo_fixture` is only called from the
    // same-repo tests below, and the contents are deterministic.
    static SRC_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    let src_path = SRC_DIR
        .get_or_init(|| {
            let src = TempDir::new().unwrap();
            let src_path = src.path().to_path_buf();
            let src_repo = git2::Repository::init(&src_path).unwrap();
            let sig = git2::Signature::now("test", "test@example.com").unwrap();
            std::fs::write(src_path.join("README.md"), "init\n").unwrap();
            let mut index = src_repo.index().unwrap();
            index.add_path(std::path::Path::new("README.md")).unwrap();
            index.write().unwrap();
            let tree_oid = index.write_tree().unwrap();
            let tree = src_repo.find_tree(tree_oid).unwrap();
            let main_commit = src_repo
                .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
            let main_commit_obj = src_repo.find_commit(main_commit).unwrap();
            src_repo
                .branch("feat/420-pr-spawn", &main_commit_obj, true)
                .unwrap();
            // Leak the TempDir guard — we want src_path to stay alive
            // for the whole process, and the bare-fetch step below
            // re-reads from the on-disk path on every test.
            std::mem::forget(src);
            src_path
        })
        .clone();

    // Bare remote — same pattern as `init_fork_fixture`. A unique
    // name per process so parallel `cargo test` invocations don't
    // collide on the bare dir.
    let bare_dir = std::env::temp_dir().join(format!(
        "buildmesh_same_repo_bare_{}_{}",
        std::process::id(),
        NEXT_FORK_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
    ));
    let _ = std::fs::remove_dir_all(&bare_dir);
    let clone = git2::Repository::init_bare(&bare_dir).unwrap();
    let mut remote = clone.remote("origin", src_path.to_str().unwrap()).unwrap();
    remote
        .fetch(&["refs/heads/*:refs/heads/*"], None, None)
        .unwrap();

    // Local repo with `origin` pointed at the bare. `fetch_single_ref`
    // will use this `origin` remote to materialise the ref.
    let local = TempDir::new().unwrap();
    let local_repo = git2::Repository::init(local.path()).unwrap();
    local_repo
        .remote("origin", bare_dir.to_str().unwrap())
        .unwrap();
    (local, bare_dir)
}

/// Success path: a ref that exists on `origin` is fetched into
/// `refs/remotes/origin/<head_ref>` and the function returns `true`.
/// This is the happy path the spawn-time worktree adoption relies on.
#[test]
fn fetch_single_ref_returns_true_when_ref_exists() {
    let (local, _bare) = init_same_repo_fixture();
    let ok = fetch_single_ref(local.path().to_str().unwrap(), "feat/420-pr-spawn");
    assert!(
        ok,
        "fetch_single_ref must return true when the ref exists on origin"
    );
    // Verify the ref actually got materialised — a true return with no
    // visible ref would mean a silent no-op, which is a worse failure
    // mode than a hard error.
    let local_repo = git2::Repository::open(local.path()).unwrap();
    let reference = local_repo
        .find_reference("refs/remotes/origin/feat/420-pr-spawn")
        .expect("origin/feat/420-pr-spawn must be materialised after success");
    assert!(
        reference.target().is_some(),
        "fetched ref must point at a real OID, not be unborn"
    );
}

/// Ref-not-found path: a ref that does NOT exist on `origin` causes
/// `git fetch` to exit non-zero. The function returns `false` (not
/// an error) so the spawn path can fall back to the mesh's
/// `base_ref` — this is the ADR 0001 offline pattern, surface as
/// `pr_head_unfetchable` rather than failing the spawn.
#[test]
fn fetch_single_ref_returns_false_when_ref_missing() {
    let (local, _bare) = init_same_repo_fixture();
    let ok = fetch_single_ref(local.path().to_str().unwrap(), "does-not-exist");
    assert!(
        !ok,
        "fetch_single_ref must return false when the ref is missing on origin \
             (caller falls back to base_ref per the offline-fallback contract)"
    );
}

/// Non-git path: a directory that isn't a git repo at all. `git fetch`
/// errors immediately; the function swallows that and returns `false`.
/// This is the "user has a partial / broken clone" edge case — the
/// spawn must not panic.
#[test]
fn fetch_single_ref_returns_false_for_non_git_directory() {
    let tmp = TempDir::new().unwrap();
    let ok = fetch_single_ref(tmp.path().to_str().unwrap(), "feat/420-pr-spawn");
    assert!(
        !ok,
        "fetch_single_ref must return false (not panic) for a non-git path"
    );
}

/// Adversarial-ref pin (issue #420 hardening): a ref starting with `-`
/// (e.g. `--upload-pack=evil`) is rejected by `git` itself because of
/// the `--` separator before `head_ref`. Without the separator, `git`
/// would parse `--upload-pack=evil` as a flag and use it for the
/// fetch — a vector for arbitrary command execution on a malicious
/// server (CVE-2017-1000117 / CVE-2018-17456 class). The hardening
/// lives in `fetch_single_ref`; this test pins the contract so a
/// future refactor that drops the `--` separator fails the test
/// rather than silently re-introducing the vulnerability.
///
/// We pass a ref that, WITHOUT the separator, `git` would parse as a
/// flag (`--upload-pack=evil`) — `git fetch` will then error out on
/// "fatal: bad config name", proving the separator did its job. With
/// the separator, the value reaches the ref-spec parser as a
/// literal ref name (which still doesn't exist on origin, so the
/// call returns `false` either way — the contract is "the function
/// returns false rather than letting `--upload-pack` reach git").
#[test]
fn fetch_single_ref_rejects_adversarial_dash_ref() {
    let (local, _bare) = init_same_repo_fixture();
    let ok = fetch_single_ref(local.path().to_str().unwrap(), "--upload-pack=evil");
    assert!(
        !ok,
        "fetch_single_ref must return false for a ref starting with '-' \
             (the wrapper rejects it before do_sync sees it)"
    );
}

/// Dirty-parent pin (issue #446 acceptance #2, inverted 2026-07-17): a
/// parent repo with uncommitted changes must STILL fetch the PR head.
/// A `git fetch` never touches the working tree — the pre-2026-07-17
/// dirty-skip meant a mesh whose root checkout stayed dirty silently
/// fell back to `base_ref` on every PR spawn, cutting the worktree
/// from the wrong commits. Pin the new contract so a future refactor
/// that re-introduces a pre-fetch dirty gate fails this test.
///
/// `is_dirty` includes untracked files, so writing one to the freshly-
/// init'd local repo is enough to dirty it — no need to seed a tracked
/// file first.
#[test]
fn fetch_single_ref_fetches_despite_dirty_parent() {
    let (local, _bare) = init_same_repo_fixture();
    // Precondition: the fixture's local repo must start clean, then we
    // make it dirty with an untracked file.
    assert!(
        !crate::env::test_helpers::repo_is_dirty(local.path()),
        "precondition: freshly-init'd local repo must start clean"
    );
    std::fs::write(local.path().join("dirty-marker.txt"), "uncommitted\n").unwrap();
    assert!(
        crate::env::test_helpers::repo_is_dirty(local.path()),
        "precondition: writing an untracked file must dirty the repo"
    );

    let ok = fetch_single_ref(local.path().to_str().unwrap(), "feat/420-pr-spawn");
    assert!(
        ok,
        "fetch_single_ref must fetch on a dirty parent — a fetch never \
             touches the working tree, and skipping cut PR worktrees from \
             stale refs"
    );
    // The head ref must be materialised so the worktree can be cut
    // from it — the whole point of the fetch.
    let repo = git2::Repository::open(local.path()).unwrap();
    assert!(
        repo.find_reference("refs/remotes/origin/feat/420-pr-spawn")
            .is_ok(),
        "the fetch must materialise refs/remotes/origin/<head_ref>"
    );
    // And the dirty marker must be untouched.
    assert_eq!(
        std::fs::read_to_string(local.path().join("dirty-marker.txt")).unwrap(),
        "uncommitted\n"
    );
}

// -----------------------------------------------------------------------
// locked_fetch_pr_head — per-Mesh sync_lock wrap (issue #698)
//
// `locked_fetch_pr_head` must run inside `services::sync_lock::with_mesh_
// sync_lock` so two concurrent PR-spawns (or a PR-spawn racing the manual
// `git_sync` from #680 / the spawn-time `fetch_origin` from #652) can't
// collide on `.git/FETCH_HEAD` / `.git/refs/remotes/<remote>/<ref>.lock`.
// Without the wrap the losing fetch fails with "another git process" and
// the spawn silently lands on `base_ref` (the wrong commits).
//
// We test the wrap with a wall-clock bound (mirroring the #680
// `git_sync_serializes_via_per_mesh_sync_lock_gh680` shape in
// `commands/git_tests.rs`). The `with_mesh_sync_lock` unit tests in
// `services::sync_lock` prove the primitive itself serialises; this test
// proves THIS specific call site uses the SAME key the spawn path uses,
// which is the bug class #698 closes.
//
// Holder enters the per-mesh lock and announces entry via an AtomicUsize
// flag before sleeping. Main thread spin-waits on the flag (deterministic
// — no `thread::sleep` race), then times `locked_fetch_pr_head`. With the
// wrap, `locked_fetch_pr_head` blocks ~450 ms waiting for the holder;
// without, it runs concurrently with the holder and finishes in tens of ms.
// -----------------------------------------------------------------------

/// Regression test for issue #698 — `locked_fetch_pr_head` must acquire
/// the per-Mesh `with_mesh_sync_lock` keyed on the spawn's `node.path`,
/// matching what `spawn_agent_inner` calls `fetch_origin` with two steps
/// earlier. Without this wrap, concurrent PR-spawns on the same Mesh
/// (and a PR-spawn racing the manual `git_sync` button) race on
/// `.git/FETCH_HEAD` / `refs/remotes/<remote>/<ref>.lock` and the loser
/// silently falls back to `base_ref`.
///
/// Strategy: holder thread enters `with_mesh_sync_lock(&path_key, ...)`
/// and announces via an AtomicUsize flag, then sleeps. Main thread
/// spin-waits on the flag (deterministic — no `thread::sleep` race), then
/// times `locked_fetch_pr_head`. With the wrap, `locked_fetch_pr_head`
/// blocks waiting for the holder; without, it returns immediately while
/// the holder is still inside its critical section.
///
/// Why wall-clock (not `fetch_add`): the per-Mesh lock is correctly
/// implemented (issue #652 + `services::sync_lock` unit tests prove it),
/// so it *prevents* simultaneous critical-section entries — `max_concurrent
/// == 1` even on a working lock. The only signal that `locked_fetch_pr_head`
/// shares the same key is that it waits for the holder to release the lock.
///
/// The test uses the same-repo branch (passes `None, None` for fork
/// fields). The fork branch shares the same wrapper so the regression
/// coverage is sufficient with one call site — a #698 regression that
/// branched out of the wrapper entirely would fail this test and the
/// #443 fork tests would still pass on the unwrapped helper, surfacing
/// the gap.
#[test]
fn locked_fetch_pr_head_serializes_via_per_mesh_sync_lock_gh698() {
    use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
    use std::time::{Duration, Instant};

    let (local, _bare) = init_same_repo_fixture();
    let path_key = local.path().to_string_lossy().into_owned();

    // Holder enters the per-mesh lock and announces entry via
    // `entered_flag` before sleeping. Spinning on the flag avoids the
    // `thread::sleep` race — CI jitter can't make `locked_fetch_pr_head`
    // sneak in first.
    let entered_flag = std::sync::Arc::new(AtomicUsize::new(0));
    let holder_path = path_key.clone();
    let entered_holder = std::sync::Arc::clone(&entered_flag);
    let holder = std::thread::spawn(move || {
        crate::services::sync_lock::with_mesh_sync_lock(&holder_path, || {
            entered_holder.store(1, AOrdering::SeqCst);
            std::thread::sleep(Duration::from_millis(500));
        });
    });

    // Spin-wait (bounded) for the holder to actually be inside the
    // critical section. Cap at 2 s so a hung holder surfaces as a
    // test panic, not a forever-wait.
    let deadline = Instant::now() + Duration::from_secs(2);
    while entered_flag.load(AOrdering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "holder thread never entered the per-mesh lock"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let start = Instant::now();
    let _ = locked_fetch_pr_head(&path_key, "feat/420-pr-spawn", None, None);
    let elapsed = start.elapsed();

    holder.join().unwrap();

    // With wrap: elapsed >= ~450 ms (`locked_fetch_pr_head` waited for
    // the holder). Without wrap: elapsed = tens of ms (the fetch ran
    // concurrently with the holder's sleep). Bound is 400 ms — leaves
    // 100 ms of slack for setup overhead and CI jitter on a busy box.
    assert!(
        elapsed >= Duration::from_millis(400),
        "locked_fetch_pr_head did not block on the per-mesh lock \
             (elapsed = {:?}); issue #698 wrap is missing — concurrent PR-spawn \
             and spawn-time fetch_origin (or manual git_sync from #680) would \
             race on .git/FETCH_HEAD and refs/remotes/<remote>/<ref>.lock",
        elapsed,
    );
}

/// Companion to `locked_fetch_pr_head_serializes_via_per_mesh_sync_lock_gh698`
/// — exercises the FORK branch (`Some/Some` → `fetch_fork_head`) of the
/// wrapper. The same-repo test alone leaves a CI blind spot: a #698
/// regression that bypassed the wrapper for fork PRs (e.g. an inlined
/// `fetch_fork_head` call in `spawn_agent_inner` to skip the remote-
/// config lock acquisition) would still pass the same-repo test and
/// every existing #443 fork unit test (those hit the bare helper
/// directly, no lock). This test closes the gap by hitting the fork
/// arm of the wrapper with the same wall-clock shape; its `git remote
/// add` then `git fetch` sequence MUST hold the lock for the holder's
/// 500 ms sleep.
#[test]
fn locked_fetch_pr_head_serializes_fork_branch_via_per_mesh_sync_lock_gh698() {
    use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
    use std::time::{Duration, Instant};

    let (local, bare_dir, _src) = init_fork_fixture();
    let bare_dir_str = bare_dir.to_str().unwrap().to_string();
    let path_key = local.path().to_string_lossy().into_owned();

    // Holder enters the per-mesh lock (same key as the wrapper) and
    // announces via `entered_flag` before sleeping.
    let entered_flag = std::sync::Arc::new(AtomicUsize::new(0));
    let holder_path = path_key.clone();
    let entered_holder = std::sync::Arc::clone(&entered_flag);
    let holder = std::thread::spawn(move || {
        crate::services::sync_lock::with_mesh_sync_lock(&holder_path, || {
            entered_holder.store(1, AOrdering::SeqCst);
            std::thread::sleep(Duration::from_millis(500));
        });
    });

    let deadline = Instant::now() + Duration::from_secs(2);
    while entered_flag.load(AOrdering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "holder thread never entered the per-mesh lock"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let start = Instant::now();
    let _ = locked_fetch_pr_head(
        &path_key,
        "feat/443-fork",
        Some("alice"),
        Some(&bare_dir_str),
    );
    let elapsed = start.elapsed();

    holder.join().unwrap();

    assert!(
        elapsed >= Duration::from_millis(400),
        "locked_fetch_pr_head (fork branch) did not block on the per-mesh \
             lock (elapsed = {:?}); issue #698 wrap is missing for the fork path \
             — concurrent fork-PR spawns would race on .git/FETCH_HEAD, \
             refs/remotes/fork-<login>/<ref>.lock, AND the git remote add/config \
             files that fetch_fork_head writes before its fetch",
        elapsed,
    );
}

// -----------------------------------------------------------------------
// Reader-thread session-id capture gate (issue #651)
//
// The orchestrator's pre-write at spawn_agent_inner (Assign mode) and the
// PTY reader thread's capture-from-output path both target the same
// `agent_nodes.cli_session_id` column. They are unsynchronised, so a
// last-writer-wins race left the row holding a UUID the agent never
// claimed — and auto-resume later invoked `claude --resume <wrong-uuid>`
// → "Conversation not found". The fix pins the gate to a single function
// of `session_id_mode` (the source of truth) so the two writers can never
// both target the same column. Each test pins one row of the truth table;
// the regression test is the `Assign(_)` row.
// -----------------------------------------------------------------------

/// Regression for issue #651. Even if a future adapter returns
/// `self_assigns_session_id() = true`, the reader thread MUST NOT capture
/// when the orchestrator is in Assign mode — the orchestrator already
/// wrote a UUID at `spawn_agent_inner` step 4, and the reader would
/// overwrite it with whatever UUID matched the regex on PTY output
/// (possibly a different log line, possibly never echoed back).
#[test]
fn reader_should_not_capture_in_assign_mode_even_if_provider_self_assigns() {
    assert!(
        !reader_should_capture_session_id(&SessionIdMode::Assign("orchestrator-uuid".into()), true,),
        "Assign mode is authoritative — reader MUST NOT overwrite the \
             orchestrator's pre-written UUID with a regex match from PTY output \
             (issue #651: 'a UUID the agent never claimed')"
    );
}

/// Resume already has the authoritative ID stored in `cli_session_id`
/// (or, for fresh `--resume` calls, the resume arg passed to the CLI).
/// Capture would race the in-flight `claude --resume <id>` with a
/// possibly-different UUID from the regex, so the reader must stay quiet.
#[test]
fn reader_should_not_capture_in_resume_mode() {
    assert!(
        !reader_should_capture_session_id(&SessionIdMode::Resume("resume-uuid".into()), true,),
        "Resume mode carries the authoritative ID; reader MUST NOT capture"
    );
}

/// `None` mode is the only mode where reader capture is allowed — and only
/// for providers that print a labeled UUID on the PTY (Codex, Agy).
/// OpenCode self-assigns `ses_…` IDs but captures them in
/// `after_fresh_spawn` (SQLite), so its PTY-capture flag is false.
#[test]
fn reader_should_capture_when_provider_self_assigns_and_mode_is_none() {
    assert!(
        reader_should_capture_session_id(&SessionIdMode::None, true),
        "Codex / Agy fresh spawns rely on the reader capturing the UUID \
             from PTY output (orchestrator has no pre-write in None mode)"
    );
}

/// Self-assigning capability is necessary but not sufficient — if the
/// provider accepts `--session-id` (Anthropic) or captures in
/// `after_fresh_spawn` (OpenCode), the PTY regex is not the source of
/// truth even when the orchestrator didn't pre-write.
#[test]
fn reader_should_not_capture_when_provider_does_not_self_assign() {
    assert!(
        !reader_should_capture_session_id(&SessionIdMode::None, false),
        "reader MUST NOT capture when provider does not self-assign; \
             any UUID match would overwrite the existing cli_session_id"
    );
}

/// Issue #1180 — `SpawnIntent::initial_prompt` is the single source
/// of truth for the GitHub-issue prefill. The spawn seam (`spawn_with_intent`)
/// routes through it; so does the desktop draft response and the
/// Autopilot watcher. Pin the wording here so any future drift would
/// surface as a unit-test failure before the agent gets the wrong
/// prompt.
#[test]
fn issue_intent_builds_its_prefill_at_the_spawn_seam() {
    let intent = SpawnIntent::Issue(GitHubWorkContext {
        owner: "alondero".into(),
        repo: "buildmesh".into(),
        number: 247,
        title: "Deepen spawn pipeline".into(),
    });

    assert_eq!(
        intent
            .initial_prompt()
            .as_ref()
            .map(intent::InitialPrompt::as_str),
        Some(
            "Please work on GitHub issue #247 — Deepen spawn pipeline\n\
https://github.com/alondero/buildmesh/issues/247"
        )
    );
}

// -----------------------------------------------------------------------
// Resume-skip decision surface (issue #949 regression).
//
// Pins the PR #1121 fix: when a Startup resume is not viable, the
// caller must NOT write `Idle` to `agent_nodes.status` — the node
// stays `Suspended` so the user's Resume / Regenerate affordances
// remain reachable. `decide_startup_resume` is the single source of
// truth for that contract; `spawn_with_intent`'s Skip arms call no
// sink. A future refactor that re-introduces an `on_idle` write here
// fails review by virtue of the decision being a single enum variant.
// -----------------------------------------------------------------------

#[test]
fn decide_startup_resume_no_session_id_is_skipped() {
    let d = decide_startup_resume(None, ResumeCause::Startup, true);
    assert_eq!(d, ResumeSkipDecision::SkipSuspended);
}

#[test]
fn decide_startup_resume_empty_session_id_is_skipped() {
    // Empty-string defense — `db::list_suspended_nodes`'s SQL filter
    // only catches NULL; legacy writes could leave an empty string
    // behind, so the empty case must be filtered here.
    let d = decide_startup_resume(Some(""), ResumeCause::Startup, true);
    assert_eq!(d, ResumeSkipDecision::SkipSuspended);
}

#[test]
fn decide_startup_resume_when_adapter_declines_is_skipped() {
    let d = decide_startup_resume(
        Some("uuid"),
        ResumeCause::Startup,
        false, // OpenCode, Terminal — no --resume flag, no auto-resume
    );
    assert_eq!(
        d,
        ResumeSkipDecision::SkipAdapterDeclines,
        "OpenCode/Terminal Startup resume must skip without writing Idle"
    );
}

#[test]
fn decide_startup_resume_explicit_no_session_id_is_an_error() {
    // User clicked Resume on a node that never captured a session id.
    // This is a hard error — surfacing it is the user-driven recovery
    // path; the orchestrator-side Startup path silently skips.
    let d = decide_startup_resume(None, ResumeCause::Explicit, true);
    assert_eq!(d, ResumeSkipDecision::NoSessionId);
}

#[test]
fn decide_startup_resume_explicit_with_session_id_proceeds() {
    let d = decide_startup_resume(
        Some("uuid-7"),
        ResumeCause::Explicit,
        false, // explicit cause is unaffected by auto_resume_on_startup
    );
    assert_eq!(d, ResumeSkipDecision::Proceed("uuid-7".to_string()));
}

#[test]
fn decide_startup_resume_startup_with_session_id_and_adapter_accepts_proceeds() {
    let d = decide_startup_resume(Some("uuid-7"), ResumeCause::Startup, true);
    assert_eq!(d, ResumeSkipDecision::Proceed("uuid-7".to_string()));
}

// -----------------------------------------------------------------
// Issue #1179: capability / recipe coherence table.
//
// For every adapter × every session mode × every value the resolver
// might forward, the prepared recipe must contain exactly the flags
// the capability descriptor advertises. The single test below drives
// the full matrix; per-adapter adapter-level tests continue to pin
// the arg shapes directly via `*_args` helpers.
// -----------------------------------------------------------------

fn make_input<'a>(
    platform: Platform,
    session: SessionIdModeRef<'a>,
    config: &'a ResolvedAgentConfig,
    prefill: Option<&'a str>,
) -> HarnessLaunchInput<'a> {
    HarnessLaunchInput {
        platform,
        runtime: EnvType::Windows,
        session,
        config,
        prefill,
        sandbox: false,
    }
}

/// Coherence pin (issue #1179): for every adapter, the
/// `HarnessCapabilities` descriptor and the recipe produced by
/// `default_prepare` agree.
///
/// 1. The recipe's model-flag presence (the flag name from
///    `adapter.model_args(m).first()`) matches
///    `caps.supports_model_override`. Kimi uses `-m`, anthropic /
///    codex / grok / agy / cursor use `--model`, mcode uses nothing.
/// 2. The recipe's effort-flag presence (matched by
///    `caps.effort_control` shape: `Closed => "--effort"`,
///    `InlineConfig => key prefix`, `None => neither`) matches
///    `caps.effort_control != None`.
/// 3. The recipe's prefill marker (trailing positional, `--prefill`,
///    or `--prompt-interactive`) matches `caps.supports_prefill`.
#[test]
fn capability_recipe_coherence() {
    let mut any_adapters = 0;
    for provider in crate::models::Provider::all() {
        let adapter = provider.adapter();
        let caps = adapter.capabilities();
        any_adapters += 1;

        // Build a config where every layer is populated, then verify
        // the recipe only carries what caps allow. Ask the adapter
        // itself for its model-flag shape — some harnesses use
        // short forms (Kimi `-m`) or vendor-specific names; the
        // adapter owns its flag vocabulary.
        let model_value = match adapter.id() {
            // mcode's `model` slot is no longer advertised; pick a
            // plausible value to attempt smuggling it past the mask.
            "mcode" => "minimax/MiniMax-Text-01",
            "codex" => "gpt-4o",
            "kimi" => "kimi-k2",
            "grok" => "grok-3",
            "agy" => "claude-sonnet",
            "cursor" => "claude-3-7-sonnet",
            "opencode" => "anthropic/claude-sonnet-4-5",
            "anthropic" => "claude-sonnet-4-5",
            "terminal" => "irrelevant",
            _ => "model",
        };
        let effort_value = match adapter.id() {
            "anthropic" => "high",
            "codex" => "xhigh",
            _ => "high", // other harnesses don't accept effort
        };
        let config = ResolvedAgentConfig {
            model: Some(model_value.to_string()),
            effort: Some(effort_value.to_string()),
            extra_args: None,
        };
        let prefill_text = "fix the auth bug in handler.rs";
        let input = make_input(
            Platform::Linux,
            SessionIdModeRef::None,
            &config,
            Some(prefill_text),
        );
        let prepared = crate::agent::launch::default_prepare(adapter, input);
        let args = &prepared.recipe.base_args;

        // 1. Model-flag coherence. Ask the adapter what its model-flag
        //    shape is; the recipe must contain it iff caps advertises
        //    the control. mcode (which used to advertise) now does
        //    not, so the recipe must not carry `--model` even when
        //    a value is in the resolved config.
        let model_flag = adapter
            .model_args(model_value)
            .first()
            .cloned()
            .unwrap_or_default();
        let has_model_flag = !model_flag.is_empty() && args.iter().any(|a| a == &model_flag);
        assert_eq!(
            has_model_flag,
            caps.supports_model_override,
            "model-flag / supports_model_override mismatch for {}: \
                 recipe has {} = {}, caps.supports_model_override = {}; args = {:?}",
            adapter.id(),
            model_flag,
            has_model_flag,
            caps.supports_model_override,
            args
        );

        // 2. Effort-flag coherence. Codex uses -c model_reasoning_effort=...;
        //    anthropic uses --effort; everything else must not carry either.
        //    Pin by `caps.effort_control` shape: Closed => "--effort";
        //    InlineConfig => the configured key prefix; None => neither.
        let has_effort_flag = match &caps.effort_control {
            crate::agent::capabilities::EffortControlKind::Closed { .. } => {
                args.iter().any(|a| a == "--effort")
            }
            crate::agent::capabilities::EffortControlKind::InlineConfig { key, .. } => {
                args.iter().any(|a| a.starts_with(key))
            }
            crate::agent::capabilities::EffortControlKind::None => false,
        };
        let has_effort_vocab = !matches!(
            caps.effort_control,
            crate::agent::capabilities::EffortControlKind::None
        );
        assert_eq!(
            has_effort_flag,
            has_effort_vocab,
            "effort-flag / effort_control mismatch for {}: \
                 recipe has effort flag = {}, caps.effort_control != None = {}; args = {:?}",
            adapter.id(),
            has_effort_flag,
            has_effort_vocab,
            args
        );

        // 3. Prefill coherence.
        let has_prefill_text = args.last().map(|a| a.as_str()) == Some(prefill_text);
        let has_prefill_flag = args.iter().any(|a| a == "--prefill");
        let has_prefill_marker = has_prefill_text
            || has_prefill_flag
            || args.iter().any(|a| a == "--prompt-interactive")
            || args.iter().any(|a| a == "--prompt");
        assert_eq!(
            has_prefill_marker,
            caps.supports_prefill,
            "prefill-marker / supports_prefill mismatch for {}: \
                 recipe has prefill marker = {}, caps.supports_prefill = {}; args = {:?}",
            adapter.id(),
            has_prefill_marker,
            caps.supports_prefill,
            args
        );

        // 4. Sandbox-flag coherence (issue #1287). The orchestrator's
        //    outer containment (macOS Seatbelt / Windows restricted-
        //    token) applies uniformly regardless of adapter; the
        //    adapter-level flag only applies when the adapter itself
        //    declared a `sandbox_args()` contribution. A second pass
        //    with `sandbox: true` must therefore add the flag iff
        //    `adapter.sandbox_args()` is non-empty. Any adapter that
        //    silently starts emitting `--sandbox` (or fails to emit
        //    it after overriding `sandbox_args`) trips this pin.
        let sandbox_input = make_input(
            Platform::Linux,
            SessionIdModeRef::None,
            &config,
            Some(prefill_text),
        );
        let sandbox_input = HarnessLaunchInput {
            sandbox: true,
            ..sandbox_input
        };
        let sandbox_prepared = crate::agent::launch::default_prepare(adapter, sandbox_input);
        let sandbox_args = sandbox_prepared
            .recipe
            .base_args
            .iter()
            .filter(|a| adapter.sandbox_args().contains(a))
            .count();
        let sandbox_vocab = adapter.sandbox_args().len();
        assert_eq!(
            sandbox_args,
            sandbox_vocab,
            "sandbox-flag / sandbox_args mismatch for {}: \
                 recipe should carry all {} declared sandbox args when sandbox=true, \
                 got {} matches; args = {:?}",
            adapter.id(),
            sandbox_vocab,
            sandbox_args,
            sandbox_prepared.recipe.base_args
        );
    }
    assert!(
        any_adapters >= 9,
        "expected at least 9 adapters in the matrix"
    );
}

/// Codex's subcommand-style resume is the one recipe shape that
/// diverges from the default. Pin the recipe contains the
/// `resume <id>` shape AND not the model's regular flags when the
/// resume is in play.
#[test]
fn codex_resume_recipe_uses_subcommand_shape() {
    let adapter =
        &crate::agent::provider::adapters::CODEX as &dyn crate::agent::provider::AgentProvider;
    let config = ResolvedAgentConfig::default();
    let input = make_input(
        Platform::Macos,
        SessionIdModeRef::Resume("sess-xyz"),
        &config,
        None,
    );
    let prepared = crate::agent::launch::default_prepare(adapter, input);
    let args = &prepared.recipe.base_args;
    assert!(args.contains(&"resume".to_string()));
    assert!(args.contains(&"sess-xyz".to_string()));
    // Codex resume recipe is the subcommand form; no `--resume <id>`
    // flag is appended.
    assert!(!args.contains(&"--resume".to_string()));
}

/// Issue #1179 follow-up pin: `mcode` no longer advertises
/// `supports_model_override`. Even with a value in the resolver
/// config, the recipe must not contain `--model`.
#[test]
fn mcode_recipe_never_carries_model_arg_under_coherence_matrix() {
    let adapter =
        &crate::agent::provider::adapters::MCODE as &dyn crate::agent::provider::AgentProvider;
    let config = ResolvedAgentConfig {
        model: Some("minimax/MiniMax-Text-01".to_string()),
        effort: None,
        extra_args: None,
    };
    let input = make_input(
        Platform::Macos,
        SessionIdModeRef::None,
        &config,
        Some("check the auth handler"),
    );
    let prepared = crate::agent::launch::default_prepare(adapter, input);
    let args = &prepared.recipe.base_args;
    assert!(
        !args.contains(&"--model".to_string()),
        "mcode recipe must never carry --model; got {:?}",
        args
    );
    assert!(
        args.last().map(|a| a.as_str()) == Some("check the auth handler"),
        "mcode prefill should be the trailing positional, got {:?}",
        args
    );
}
