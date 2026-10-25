//! Tests for `git::worktree`.
//!
//! Split out of `src-tauri/src/git/worktree/mod.rs` by issue #2451. The conventions this
//! layout depends on — why the module keeps the name `tests`, and why every
//! `#[path]` module needs an entry in `RUST_PATH_INCLUDES` — are
//! documented once in `docs/development/rust-conventions.md`, under *Rust unit
//! tests live in sibling `*_tests.rs` files*.
//!
//! Run with: cd src-tauri && cargo test git::worktree::tests

#[test]
fn scoped_cold_create_has_bounded_wait_behind_sibling_sync() {
    let td = crate::env::test_helpers::TestDir::new("scoped-create-lock");
    crate::env::test_helpers::init_repo_with_commit(td.path(), &[("scope/f", "scoped")]);
    let root = td.path().to_string_lossy().to_string();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        crate::git::sync_lock::with_mesh_sync_lock(&root, || {
            held_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        })
    });
    held_rx.recv().unwrap();
    let target = td.path().join("owned");
    let outcome = super::create_git_worktree_with_timeout(
        td.path().join("scope").to_str().unwrap(),
        target.to_str().unwrap(),
        "bounded",
        "detached",
        "HEAD",
        std::time::Duration::from_millis(10),
    );
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    assert!(outcome.unwrap_err().contains("Timed out"));
    assert!(!target.exists());
}
use super::*;
use crate::env::test_helpers::{
    commit_file, init_repo_with_commit, repo_is_dirty, repo_with_drifted_head, TestDir,
};
use std::fs;
use std::path::{Path, PathBuf};

// ── spawn-path subprocess budgets (issue #1229) ─────────────────────────

/// A command that hangs for ~N seconds before exiting, built the same way
/// `process_util`'s own timeout tests build one: `ping` on Windows (its
/// `-n N` sends one echo per second), `sleep` elsewhere. `ping` is the
/// Windows-safe choice because `timeout /T` exits immediately when stdin
/// isn't a console.
fn hang_cmd(secs: u64) -> std::process::Command {
    #[cfg(target_os = "windows")]
    {
        let mut c = crate::process_util::command_no_window("ping");
        c.args(["-n", &secs.to_string(), "127.0.0.1"]);
        c
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut c = crate::process_util::command_no_window("sleep");
        c.arg(secs.to_string());
        c
    }
}

/// The failure the issue is about, at the seam the spawn path now uses: a
/// wedged git (a hung git-LFS smudge filter during `worktree add`, a
/// contended `index.lock` during checkout/reset) must become a named
/// `Err` at the deadline instead of parking the spawn task forever.
///
/// Passes the real op name a production call site uses, because the
/// message is what the user sees in the spawn-failure toast: the
/// assertion that it names the command is half the point.
#[test]
fn run_bounded_git_surfaces_a_hung_command_as_a_named_err() {
    // Four thresholds, per issue #2049's rule about waits on loaded
    // machines: the child outlives the deadline, the deadline must fire,
    // and the kill (100ms poll plus `taskkill /F /T` walking the tree)
    // must clear well inside the child's own lifetime. Measured kill
    // latency reached ~3.8s on a loaded 24-core host, so 8s is ~2x that
    // while staying far below the 12s natural exit.
    const CHILD_LIFETIME: Duration = Duration::from_secs(12);
    const KILL_BOUND: Duration = Duration::from_secs(8);

    let started = std::time::Instant::now();
    let result = run_bounded_git(hang_cmd(13), "git worktree add", Duration::from_secs(1));
    let elapsed = started.elapsed();

    // Mechanism: only the deadline path produces this Err, and the child
    // could not have exited naturally within the elapsed time.
    let msg = result.expect_err("a hung git must not report success");
    assert!(
        msg.contains("git worktree add") && msg.contains("timed out"),
        "the spawn toast must name the command that gave up, got: {msg}"
    );
    assert!(
        elapsed < KILL_BOUND,
        "the deadline must kill the child well before its {CHILD_LIFETIME:?} \
             natural exit, took {elapsed:?}"
    );
}

/// Regression guard for the wiring, not just the helper: every git CLI
/// call in this module and in `provision` goes through
/// [`run_bounded_git`], because they all run on or directly feed the
/// click-to-terminal path.
///
/// `Command::output()` and `Command::status()` are the unbounded forms —
/// they wait forever, which is precisely the #1229 defect. Scanning the
/// sources is what pins the *routing*: the helper's own behaviour is
/// covered by `process_util`'s tests, but those exercise the helper
/// directly and so would not notice a new call site opting out of it.
///
/// Scope note: `mod.rs` is production code in full since its tests moved to
/// this sibling file (#2451), so a test-only helper declared in it must not
/// shell out with `.output()` either. Comment text is stripped so this guard
/// does not trip on prose that merely names the pattern.
#[test]
fn spawn_path_git_shellouts_are_all_budgeted() {
    const UNBOUNDED: [&str; 2] = [".output()", ".status()"];

    for file in [
        "git/worktree/mod.rs",
        "git/worktree/provision.rs",
        "git/worktree/create.rs",
        "git/worktree/include.rs",
        "git/worktree/maintenance.rs",
        "git/worktree/remove.rs",
        "git/worktree/safety.rs",
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file);
        let source =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let production = source
            .split_once("\n#[cfg(test)]\nmod tests {")
            .map_or(source.as_str(), |(head, _)| head);

        for (n, line) in production.lines().enumerate() {
            // Drop line comments so doc prose naming the pattern is fine.
            let code = line.split("//").next().unwrap_or(line);
            for token in UNBOUNDED {
                assert!(
                        !code.contains(token),
                        "{file}:{} shells out with an unbounded `{token}` — route it \
                         through `run_bounded_git` so the spawn path keeps a deadline (issue #1229):\n{line}",
                        n + 1,
                    );
            }
        }
    }

    // Not vacuous: the guard must be looking at code that really does
    // shell out through the bounded seam.
    let mod_source = ["mod.rs", "create.rs", "maintenance.rs"]
        .map(|name| {
            fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("src/git/worktree")
                    .join(name),
            )
            .unwrap()
        })
        .join("\n");
    assert!(
        mod_source.matches("run_bounded_git(").count() >= 4,
        "expected the mutators to route through `run_bounded_git`"
    );
}

/// Create a branched worktree under `<root>/.claude/worktrees/<name>` via
/// the production `create_git_worktree`, returning its path.
fn make_branched_worktree(root: &Path, name: &str) -> PathBuf {
    let wt = root.join(".claude").join("worktrees").join(name);
    create_git_worktree(
        root.to_str().unwrap(),
        wt.to_str().unwrap(),
        name,
        "branched",
        "HEAD",
    )
    .expect("worktree creation must succeed");
    wt
}

// ── container exclusion (issue #1519) ───────────────────────────────────

fn assert_clean_checkout(repo: &git2::Repository) {
    assert!(!crate::git::primitives::is_dirty(repo).unwrap());
    let mut command = crate::process_util::git_command();
    command.args([
        "-C",
        repo.workdir().unwrap().to_str().unwrap(),
        "status",
        "--porcelain",
        "--untracked-files=all",
    ]);
    let output = run_bounded_git(command, "git status", GIT_LOCAL_TIMEOUT).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn container_exclude_appends_relative_container_idempotently() {
    let td = TestDir::new("exclude_relative");
    init_repo_with_commit(td.path(), &[("f.txt", "v1\n")]);
    let root = td.path().to_string_lossy().to_string();

    ensure_container_excluded(&root, &format!("{root}/custom-wt"));
    let exclude = td.path().join(".git").join("info").join("exclude");
    let body = fs::read_to_string(&exclude).expect("exclude must exist");
    assert!(
        body.lines().any(|l| l.trim() == "custom-wt/"),
        "container pattern must be appended, got:\n{body}"
    );
    // Idempotent: no duplicate line on repeat.
    ensure_container_excluded(&root, &format!("{root}/custom-wt"));
    let body2 = fs::read_to_string(&exclude).unwrap();
    assert_eq!(
        body2.lines().filter(|l| l.trim() == "custom-wt/").count(),
        1,
        "pattern must not duplicate"
    );
    // A nested container relativizes against the root.
    ensure_container_excluded(&root, &format!("{root}/custom-wt/nested"));
    let body3 = fs::read_to_string(&exclude).unwrap();
    assert!(
        body3.lines().any(|l| l.trim() == "custom-wt/nested/"),
        "nested container must relativize, got:\n{body3}"
    );
}

#[test]
fn container_exclude_ignores_outside_root_and_missing_git() {
    let td = TestDir::new("exclude_outside");
    init_repo_with_commit(td.path(), &[("f.txt", "v1\n")]);
    let root = td.path().to_string_lossy().to_string();

    // Absolute container elsewhere: nothing to ignore in this repo.
    ensure_container_excluded(&root, "/tmp/elsewhere-wt");
    assert!(
        !td.path().join(".git").join("info").join("exclude").exists()
            || !fs::read_to_string(td.path().join(".git/info/exclude"))
                .unwrap_or_default()
                .contains("elsewhere"),
        "outside-root container must not touch this repo's exclude"
    );

    // Non-repo mesh root: must not fabricate a `.git` directory.
    let bare = TestDir::new("exclude_bare");
    let bare_root = bare.path().to_string_lossy().to_string();
    ensure_container_excluded(&bare_root, &format!("{bare_root}/custom-wt"));
    assert!(
        !bare.path().join(".git").exists(),
        "must not create .git for a non-repo root"
    );
}

#[test]
fn default_container_keeps_checkout_clean_without_editing_gitignore() {
    let td = TestDir::new("exclude_default");
    init_repo_with_commit(td.path(), &[("f.txt", "v1\n"), (".gitignore", "dist/\n")]);
    let root = td.path().to_str().unwrap();
    let container = crate::env::effective_worktree_dir_raw(root, None, None);
    let wt = crate::env::resolve_worktree_node_raw(&container, "default-node");
    create_git_worktree(root, &wt, "default-node", "detached", "HEAD").unwrap();
    let repo = git2::Repository::open(td.path()).unwrap();
    assert!(repo
        .is_path_ignored(Path::new(".worktrees/default-node"))
        .unwrap());
    assert_clean_checkout(&repo);
    assert_eq!(fs::read(td.path().join(".gitignore")).unwrap(), b"dist/\n");
    assert!(Path::new(&wt).join("f.txt").is_file());
}

#[test]
fn default_container_in_linked_mesh_uses_common_exclude() {
    let td = TestDir::new("exclude_linked_default");
    let main = td.path().join("main");
    init_repo_with_commit(&main, &[("f.txt", "v1\n")]);
    let linked = td.path().join("linked");
    create_git_worktree(
        main.to_str().unwrap(),
        linked.to_str().unwrap(),
        "linked-mesh",
        "detached",
        "HEAD",
    )
    .unwrap();
    assert!(linked.join(".git").is_file());
    let container = crate::env::effective_worktree_dir_raw(linked.to_str().unwrap(), None, None);
    let wt = crate::env::resolve_worktree_node_raw(&container, "nested-node");
    create_git_worktree(
        linked.to_str().unwrap(),
        &wt,
        "nested-node",
        "detached",
        "HEAD",
    )
    .unwrap();
    let repo = git2::Repository::open(&linked).unwrap();
    assert!(repo
        .is_path_ignored(Path::new(".worktrees/nested-node"))
        .unwrap());
    assert_clean_checkout(&repo);
    let excluded = fs::read_to_string(main.join(".git/info/exclude")).unwrap();
    assert_eq!(
        excluded
            .lines()
            .filter(|line| *line == ".worktrees/")
            .count(),
        1
    );
    assert!(!linked.join(".gitignore").exists());
}

#[test]
fn create_cuts_excluded_container_for_custom_relative_dir() {
    // End-to-end: a cold create under a custom relative container leaves
    // the container ignored, so the parent repo does not report dirty
    // purely from the worktree's presence (issue #1519 §3).
    let td = TestDir::new("exclude_e2e");
    init_repo_with_commit(td.path(), &[("f.txt", "v1\n")]);
    let root = td.path().to_string_lossy().to_string();
    let wt = format!("{root}/custom-wt/e2e-node");
    create_git_worktree(&root, &wt, "e2e-node", "detached", "HEAD").expect("create must succeed");
    let exclude = td.path().join(".git").join("info").join("exclude");
    let body = fs::read_to_string(&exclude).expect("exclude must exist");
    assert!(
        body.lines().any(|l| l.trim() == "custom-wt/"),
        "cold create must exclude its container, got:\n{body}"
    );
}

// End-to-end: time the *production* `create_git_worktree` (now a git-CLI
// shell-out) against a real repo. Confirms the fix through the actual code
// path, not a hand-inlined libgit2 reproduction.
//   cargo test -p buildmesh --lib timing_create_git_worktree_e2e -- --ignored --nocapture
#[test]
#[ignore = "timing harness; set BUILDMESH_WT_TIMING_REPO=<repo> to run"]
fn timing_create_git_worktree_e2e() {
    use std::time::Instant;

    let repo_root = match std::env::var("BUILDMESH_WT_TIMING_REPO") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("SKIP: set BUILDMESH_WT_TIMING_REPO to a real repo to run");
            return;
        }
    };
    let wt_name = "wt-e2e-probe";
    let wt_path = Path::new(&repo_root)
        .join(".claude")
        .join("worktrees")
        .join(wt_name);
    let wt_path_str = wt_path.to_str().unwrap().to_string();

    // Clean residue, then drop any stale branch/metadata of the same name.
    let _ = remove_worktree_dir_with_retry(&wt_path_str);
    let repo = git2::Repository::open(&repo_root).expect("open repo");
    prune_stale_worktrees(&repo);
    if let Ok(mut b) = repo.find_branch(wt_name, git2::BranchType::Local) {
        let _ = b.delete();
    }

    // Loop a few times to separate a systematic cost from disk/Defender
    // noise (the libgit2 numbers swung 3.7↔12.4s, so a single sample lies).
    for i in 0..3 {
        let t = Instant::now();
        create_git_worktree(&repo_root, &wt_path_str, wt_name, "branched", "HEAD")
            .expect("create_git_worktree must succeed");
        eprintln!(
            "[e2e] run {} create_git_worktree (git CLI): {:?}",
            i,
            t.elapsed()
        );

        // Cleanup between runs.
        let _ = remove_worktree_dir_with_retry(&wt_path_str);
        prune_stale_worktrees(&repo);
        if let Ok(wt) = repo.find_worktree(wt_name) {
            let mut po = git2::WorktreePruneOptions::new();
            po.valid(true).working_tree(true);
            let _ = wt.prune(Some(&mut po));
        }
        let branch_to_delete = repo.find_branch(wt_name, git2::BranchType::Local);
        if let Ok(mut b) = branch_to_delete {
            let _ = b.delete();
        }
    }
}

// ── create (#210: dirty parent allowed) ──────────────────────────────────

#[test]
fn create_branched_worktree_succeeds_with_dirty_parent() {
    let td = TestDir::new("dirty_branched");
    let parent = td.path();
    init_repo_with_commit(parent, &[("README.md", "# project\n")]);

    // Make the working tree dirty: modify a tracked file + add an untracked one.
    fs::write(parent.join("README.md"), "# project (in-progress edit)\n").unwrap();
    fs::write(parent.join("scratch.txt"), "untracked local note").unwrap();
    assert!(
        repo_is_dirty(parent),
        "precondition: parent repo must be dirty"
    );

    let wt_path = parent.join(".claude").join("worktrees").join("wt-1");
    create_git_worktree(
        parent.to_str().unwrap(),
        wt_path.to_str().unwrap(),
        "wt-1",
        "branched",
        "HEAD",
    )
    .expect("branched worktree creation must succeed with a dirty parent");

    assert!(wt_path.exists(), "worktree directory should exist");
    let wt_repo = git2::Repository::open(&wt_path).expect("worktree should be a valid repo");
    let head = wt_repo.head().unwrap();
    assert_eq!(head.shorthand().unwrap_or(""), "wt-1");

    // The parent's uncommitted changes must NOT leak into the new worktree.
    let wt_readme = fs::read_to_string(wt_path.join("README.md")).unwrap();
    assert_eq!(wt_readme, "# project\n");
    assert!(!wt_path.join("scratch.txt").exists());
    assert!(
        repo_is_dirty(parent),
        "creating a worktree must not modify the parent"
    );
}

#[test]
fn create_detached_worktree_succeeds_with_dirty_parent() {
    let td = TestDir::new("dirty_detached");
    let parent = td.path();
    init_repo_with_commit(parent, &[("README.md", "# project\n")]);
    fs::write(parent.join("README.md"), "# project (in-progress edit)\n").unwrap();
    assert!(
        repo_is_dirty(parent),
        "precondition: parent repo must be dirty"
    );

    let wt_path = parent.join(".claude").join("worktrees").join("wt-det");
    create_git_worktree(
        parent.to_str().unwrap(),
        wt_path.to_str().unwrap(),
        "wt-det",
        "detached",
        "HEAD",
    )
    .expect("detached worktree creation must succeed with a dirty parent");

    assert!(wt_path.exists());
    let wt_repo = git2::Repository::open(&wt_path).expect("worktree should be a valid repo");
    assert!(
        wt_repo.head_detached().unwrap_or(false),
        "HEAD should be detached"
    );
    let wt_readme = fs::read_to_string(wt_path.join("README.md")).unwrap();
    assert_eq!(wt_readme, "# project\n");
    assert!(repo_is_dirty(parent));
}

// ── create (#1230: pre-existing dir must be a valid linked worktree) ─────
//
// The old `if host_path.exists() { return Ok(()) }` accepted ANY directory
// as a "resume" — an empty husk left by an interrupted `git worktree add`,
// a stray user-created folder, or even a totally unrelated repo. The
// provisioner would then report `Reused` and spawn the agent on a
// non-worktree directory. The contract the cold primitive now enforces:
//   - valid linked worktree of `project_root`  → resume (early Ok)
//   - empty directory at the target            → rename aside, proceed
//   - non-empty non-worktree directory         → descriptive Err, untouched
//
// The two tests below pin (b) and (c). The resume case (a) is exercised
// by every existing create-then-create test pair (e.g. move_git_worktree_*).

/// An empty husk at the worktree path is treated as recoverable wreckage
/// from an interrupted `git worktree add`. It is renamed aside so the
/// genuine worktree create can land there, and the husk survives on disk
/// (rename, not delete — same recovery philosophy as
/// `remove_worktree_dir_with_retry`).
#[test]
fn create_recovers_from_empty_pre_existing_directory() {
    let td = TestDir::new("wt_empty_husk");
    init_repo_with_commit(td.path(), &[("f.txt", "v1\n")]);
    let root = td.path().to_string_lossy().to_string();
    let wt = td.path().join(".claude").join("worktrees").join("wt-empty");
    fs::create_dir_all(&wt).unwrap();
    assert!(wt.exists(), "precondition: stray empty dir exists");
    let husk_only = fs::read_dir(td.path().join(".claude").join("worktrees"))
        .unwrap()
        .filter_map(|e| e.ok())
        .count();
    assert_eq!(
        husk_only, 1,
        "precondition: only the husk exists in the container"
    );

    create_git_worktree(&root, wt.to_str().unwrap(), "wt-empty", "branched", "HEAD")
        .expect("empty husk must be renamed aside, not block create");

    // The path is now a real, openable, registered worktree.
    let wt_repo =
        git2::Repository::open(&wt).expect("after recovery the path must open as a Repository");
    assert_eq!(
        wt_repo.head().unwrap().shorthand().unwrap_or(""),
        "wt-empty",
        "HEAD must be on the requested branch"
    );
    let root_repo = git2::Repository::open(td.path()).unwrap();
    let registered = root_repo
        .worktrees()
        .unwrap()
        .iter()
        .filter_map(|name| name.ok().flatten())
        .filter_map(|n| root_repo.find_worktree(n).ok())
        .any(|w| w.path() == wt.as_path());
    assert!(
        registered,
        "recovered worktree must appear in the parent's linked-worktrees list"
    );

    // The husk survived (rename aside, not delete) — it sits beside the new
    // worktree under a husk marker name in the same container.
    let container = td.path().join(".claude").join("worktrees");
    let has_husk_marker = fs::read_dir(&container)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with("wt-empty.") && name != "wt-empty"
        });
    assert!(
        has_husk_marker,
        "husk must be preserved as a sibling under the container"
    );
}

// ── `git worktree add` failure classification (issue #2195) ─────────────

/// Every wording this repo's git can produce for the one repairable
/// conflict — the target path already registered as a worktree. Pinned with
/// the strings git 2.30.0.windows.2 actually printed, because the whole
/// point of the split is that the caller stops reading English: if a wording
/// is missing here, the repair silently stops firing for it.
#[test]
fn classify_worktree_add_failure_recognises_the_registered_worktree_conflict() {
    for stderr in [
            // git >= 2.30: neither "already exists" nor "already checked out" —
            // this is the message that the pre-#2195 caller never matched, so the
            // repair never ran and the user got raw git output.
            "fatal: 'C:/repo/.claude/worktrees/gh1' is a missing but already registered worktree;\nuse 'add -f' to override, or 'prune' or 'remove' to clear",
            // Older git, same conflict.
            "fatal: 'C:/repo/.claude/worktrees/gh1' already exists",
            "fatal: 'C:/repo/.claude/worktrees/gh1' already used by worktree at 'C:/repo/.claude/worktrees/gh0'",
            // The two strings the old caller matched.
            "fatal: 'gh1' is already checked out at 'C:/repo/.claude/worktrees/gh0'",
        ] {
            assert_eq!(
                classify_worktree_add_failure(stderr),
                AddWorktreeFailureKind::RegisteredWorktree,
                "must be treated as a prunable conflict: {stderr}"
            );
        }
}

/// The failure this repo can produce from git that says "already exists"
/// about something else entirely: a *branch name* clash. Pruning removes
/// worktree admin entries, never branches, so the old caller's prune-and-
/// retry fired here and then failed with the identical message.
#[test]
fn classify_worktree_add_failure_excludes_a_branch_name_clash() {
    let stderr = "fatal: A branch named 'gh1' already exists.";
    assert!(
        stderr.contains("already exists"),
        "precondition: the old substring match really did fire on this message"
    );
    assert_eq!(
        classify_worktree_add_failure(stderr),
        AddWorktreeFailureKind::Other,
        "a branch clash is not a stale worktree registration"
    );
}

/// A failure containing neither phrase must never reach the retry.
#[test]
fn classify_worktree_add_failure_leaves_unrelated_failures_alone() {
    for stderr in [
        "fatal: Not a valid object name: 'nosuchref'.",
        "fatal: could not create work tree dir 'C:/repo/wt': Permission denied",
        // Non-English locale: the words the old caller matched are absent.
        "fatal: 'C:/repo/wt' est déjà un worktree enregistré mais manquant",
        // Degenerate: git produced no stderr at all.
        "",
    ] {
        assert_eq!(
            classify_worktree_add_failure(stderr),
            AddWorktreeFailureKind::Other,
            "must not be retried: {stderr}"
        );
    }
}

/// Every path the target is registered under, as git still lists it —
/// including registrations whose directory is gone.
fn registered_worktree_paths(root: &Path) -> Vec<PathBuf> {
    let repo = git2::Repository::open(root).expect("repo must open");
    let names = repo.worktrees().expect("worktrees must list");
    (0..names.len())
        .flat_map(|i| {
            owned_worktree_name(names.get(i)).expect("registered worktree name must be valid UTF-8")
        })
        .filter_map(|name| repo.find_worktree(&name).ok())
        .map(|w| w.path().to_path_buf())
        .collect()
}

/// Cut a worktree and then move its directory aside, which is what leaves
/// git holding a registration for a path that no longer exists. Returns the
/// path the registration still points at.
fn leave_stale_registration(root: &Path, name: &str) -> PathBuf {
    let wt = root.join(".claude").join("worktrees").join(name);
    create_git_worktree(
        root.to_str().unwrap(),
        wt.to_str().unwrap(),
        name,
        "detached",
        "HEAD",
    )
    .expect("fixture worktree must be created");
    fs::rename(
        &wt,
        root.join(".claude")
            .join("worktrees")
            .join(format!("{name}-gone")),
    )
    .expect("fixture worktree dir must move aside");
    assert!(!wt.exists(), "precondition: the worktree directory is gone");
    assert!(
        registered_worktree_paths(root).contains(&wt),
        "precondition: git still holds a registration for the missing path"
    );
    wt
}

fn unreadable_worktree_name_error() -> git2::Error {
    git2::Error::new(
        git2::ErrorCode::GenericError,
        git2::ErrorClass::Os,
        "could not decode registered worktree name",
    )
}

#[test]
fn linked_worktree_scan_finds_match_after_unreadable_name() {
    let td = TestDir::new("wt_match_after_unreadable_name");
    init_repo_with_commit(td.path(), &[("f.txt", "v1\n")]);
    let root = td.path().to_string_lossy().to_string();
    let worktree_path = td.path().join(".claude").join("worktrees").join("linked");
    create_git_worktree(
        &root,
        worktree_path.to_str().unwrap(),
        "linked",
        "detached",
        "HEAD",
    )
    .expect("fixture worktree must be created");
    let repo = git2::Repository::open(td.path()).unwrap();
    let names = repo.worktrees().unwrap();
    let registered_name = names
        .iter()
        .filter_map(Result::ok)
        .flatten()
        .find(|name| {
            repo.find_worktree(name)
                .is_ok_and(|worktree| worktree.path() == worktree_path)
        })
        .expect("fixture worktree must be registered");

    assert!(super::has_registered_worktree_path(
        &repo,
        &worktree_path,
        [
            Err(unreadable_worktree_name_error()),
            Ok(Some(registered_name))
        ],
    ));
}

#[test]
fn stale_worktree_scan_prunes_registration_after_unreadable_name() {
    let td = TestDir::new("wt_prune_after_unreadable_name");
    init_repo_with_commit(td.path(), &[("f.txt", "v1\n")]);
    let stale_path = leave_stale_registration(td.path(), "stale-after-error");
    let repo = git2::Repository::open(td.path()).unwrap();

    super::prune_stale_worktree_names(
        &repo,
        [
            Err(unreadable_worktree_name_error()),
            Ok(Some("stale-after-error")),
        ],
    );

    assert!(
        repo.find_worktree("stale-after-error").is_err(),
        "the later stale registration must be pruned despite the unreadable entry"
    );
    assert!(
        !registered_worktree_paths(td.path()).contains(&stale_path),
        "the pruned registration must disappear from git's worktree list"
    );
}

/// End-to-end through the production `create_git_worktree`: a stale
/// registration must still be recognised and repaired by prune-and-retry.
/// On git 2.30 the failing `git worktree add` says "is a missing but already
/// registered worktree", which contains neither phrase the pre-#2195 caller
/// matched — so before the typed split this create returned git's raw error
/// and the worktree was never cut.
#[test]
fn create_repairs_a_stale_worktree_registration() {
    let td = TestDir::new("wt_repair_stale");
    init_repo_with_commit(td.path(), &[("f.txt", "v1\n")]);
    let root = td.path().to_string_lossy().to_string();
    let stale = leave_stale_registration(td.path(), "wt-repair");

    create_git_worktree(
        &root,
        stale.to_str().unwrap(),
        "wt-repair",
        "detached",
        "HEAD",
    )
    .expect("a stale registration must be pruned and the create retried");

    assert!(
        git2::Repository::open(&stale).is_ok(),
        "the repaired create must leave a real worktree at the target"
    );
    assert!(
        registered_worktree_paths(td.path()).contains(&stale),
        "the retry must have re-registered the path"
    );
}

/// The other half of the contract: a failure that pruning cannot fix is
/// returned as-is and prunes nothing. A stale registration is left in the
/// repo purely as the witness — it is exactly what a prune would have
/// removed, so its survival is the proof that no prune-and-retry ran.
#[test]
fn create_does_not_prune_or_retry_a_branch_name_clash() {
    let td = TestDir::new("wt_branch_clash");
    init_repo_with_commit(td.path(), &[("f.txt", "v1\n")]);
    let root = td.path().to_string_lossy().to_string();
    let stale = leave_stale_registration(td.path(), "wt-stale");

    // A branch name that is already taken, checked out nowhere.
    let repo = git2::Repository::open(td.path()).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("clashed", &head, false).unwrap();

    let target = td.path().join(".claude").join("worktrees").join("wt-new");
    let err = create_git_worktree(
        &root,
        target.to_str().unwrap(),
        "clashed",
        "branched",
        "HEAD",
    )
    .expect_err("a branch-name clash must fail the create");
    assert!(
        err.contains("already exists"),
        "git's own wording must still reach the user, got: {err}"
    );

    assert!(
        registered_worktree_paths(td.path()).contains(&stale),
        "a non-conflict failure must not prune stale worktrees"
    );
    assert!(
        !target.exists(),
        "a non-conflict failure must not leave a half-created worktree"
    );
}

/// A non-empty, non-repo directory at the worktree path contains user
/// data we must not touch. The function must refuse with a descriptive
/// Err that names the offending path, and the directory must be
/// byte-for-byte untouched (no rename, no delete, no files removed).
#[test]
fn create_refuses_non_empty_non_repo_directory_with_descriptive_error() {
    let td = TestDir::new("wt_user_dir");
    init_repo_with_commit(td.path(), &[("f.txt", "v1\n")]);
    let root = td.path().to_string_lossy().to_string();
    let wt = td
        .path()
        .join(".claude")
        .join("worktrees")
        .join("wt-userdata");
    fs::create_dir_all(&wt).unwrap();
    let sentinel = wt.join("user-notes.md");
    let sentinel_bytes = b"important user data - must survive a refused create\n";
    fs::write(&sentinel, sentinel_bytes).unwrap();
    let nested_dir = wt.join("nested");
    fs::create_dir_all(&nested_dir).unwrap();
    let nested_file = nested_dir.join("deep.txt");
    fs::write(&nested_file, "deeper user data\n").unwrap();

    let err = create_git_worktree(
        &root,
        wt.to_str().unwrap(),
        "wt-userdata",
        "branched",
        "HEAD",
    )
    .expect_err("non-empty non-repo dir at worktree path must be refused");

    let err_lower = err.to_lowercase();
    let path_str = wt.display().to_string();
    assert!(
        err.contains(&path_str) || err_lower.contains("non-worktree"),
        "error must name the offending path or the failure class, got: {}",
        err
    );
    assert!(
        !err_lower.contains("permission") && !err_lower.contains("delete"),
        "error must not promise to delete user data, got: {}",
        err
    );

    // User data is untouched — the refused-create contract.
    assert!(
        sentinel.exists(),
        "user file at top level must not be removed"
    );
    assert_eq!(
        fs::read(&sentinel).unwrap(),
        sentinel_bytes,
        "top-level user file content must be byte-for-byte identical"
    );
    assert!(nested_dir.exists(), "nested user dir must not be removed");
    assert!(nested_file.exists(), "nested user file must not be removed");
    assert_eq!(
        fs::read_to_string(&nested_file).unwrap(),
        "deeper user data\n",
        "nested user file content must be byte-for-byte identical"
    );
}

// ── move (#612: warm-pool adoption rename) ───────────────────────────────

/// Create a DETACHED worktree under `<root>/.claude/worktrees/<name>`,
/// matching the on-disk shape the warm pool actually cuts (it pins
/// `detached` mode so a claim can `git checkout -b` to upgrade). Move tests
/// use this rather than `make_branched_worktree` for production fidelity.
fn make_detached_worktree(root: &Path, name: &str) -> PathBuf {
    let wt = root.join(".claude").join("worktrees").join(name);
    create_git_worktree(
        root.to_str().unwrap(),
        wt.to_str().unwrap(),
        name,
        "detached",
        "HEAD",
    )
    .expect("detached worktree creation must succeed");
    wt
}

#[test]
fn cross_runtime_root_without_git_remains_usable() {
    let td = TestDir::new("cross_runtime_root");
    prepare_cross_runtime_worktree(td.path().to_str().unwrap()).unwrap();
    assert!(!td.path().join(".git").exists());
}

#[cfg(windows)]
#[test]
#[ignore = "requires default WSL with Git and Windows Git"]
fn live_wsl_cross_runtime_git_on_both_filesystems() {
    let host_root = tempfile::tempdir().unwrap();
    let guest_home = crate::env::wsl_home().expect("WSL home");
    let guest_root = tempfile::Builder::new()
        .prefix("buildmesh-interop-")
        .tempdir_in(crate::env::to_host_path(&guest_home.to_string_lossy()))
        .unwrap();
    // Trust only this fixture in an isolated libgit2 global config.
    // WSL ownership differs from the Windows account; never alter the
    // user's Git config or disable repository ownership checks globally.
    let config_dir = tempfile::tempdir().unwrap();
    let mut config = git2::Config::open(&config_dir.path().join(".gitconfig")).unwrap();
    for path in [
        guest_root.path().to_path_buf(),
        guest_root.path().join(".claude/worktrees/guest"),
    ] {
        config
            .set_multivar(
                "safe.directory",
                "^$",
                &path.to_string_lossy().replace('\\', "/"),
            )
            .unwrap();
    }
    drop(config);
    struct RestoreConfig(std::ffi::CString);
    impl Drop for RestoreConfig {
        fn drop(&mut self) {
            unsafe {
                git2::opts::set_search_path(git2::ConfigLevel::Global, self.0.clone()).unwrap();
            }
        }
    }
    let _restore =
        RestoreConfig(unsafe { git2::opts::get_search_path(git2::ConfigLevel::Global).unwrap() });
    unsafe {
        git2::opts::set_search_path(git2::ConfigLevel::Global, config_dir.path()).unwrap();
    }
    for root in [host_root.path(), guest_root.path()] {
        init_repo_with_commit(root, &[("file.txt", "initial\n")]);
        let wt = make_detached_worktree(root, "guest");
        prepare_cross_runtime_worktree(wt.to_str().unwrap()).unwrap();
        let guest_path =
            crate::env::windows_to_wsl(&crate::env::normalize_unc_to_wsl(wt.to_str().unwrap()));
        for guest in [false, true] {
            let mut command = if guest {
                let mut command = crate::process_util::command_no_window("wsl.exe");
                command.args([
                    "-d",
                    &crate::env::get_default_wsl_distro().unwrap(),
                    "--exec",
                    "git",
                    "-C",
                    &guest_path,
                ]);
                command
            } else {
                let mut command = crate::process_util::git_command();
                command.arg("-C").arg(&wt);
                command
            };
            command.args(["status", "--porcelain"]);
            let output = crate::process_util::run_command_with_timeout(
                command,
                "cross-runtime Git",
                std::time::Duration::from_secs(10),
            )
            .unwrap();
            assert!(
                output.status.success(),
                "guest={guest}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                output.stdout.is_empty(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
        remove_one_worktree(wt.to_str().unwrap()).unwrap();
        assert!(!wt.exists());
    }
}

#[test]
fn cross_runtime_worktree_keeps_host_git_access_and_can_be_removed() {
    let td = TestDir::new("cross_runtime");
    init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
    let wt = make_detached_worktree(td.path(), "guest");
    let admin = Repository::open(&wt).unwrap().path().to_path_buf();
    let backpointer = fs::read(admin.join("gitdir")).unwrap();
    prepare_cross_runtime_worktree(wt.to_str().unwrap()).unwrap();
    assert_eq!(
        fs::read_to_string(wt.join(".git")).unwrap(),
        "gitdir: ../../../.git/worktrees/guest\n"
    );
    assert_eq!(fs::read(admin.join("gitdir")).unwrap(), backpointer);
    let repo = Repository::open(&wt).unwrap();
    assert!(repo.statuses(None).unwrap().is_empty());
    assert!(admin.join("locked").exists());
    prepare_cross_runtime_worktree(wt.to_str().unwrap()).unwrap();
    drop(repo);
    remove_one_worktree(wt.to_str().unwrap()).unwrap();
    assert!(!wt.exists());
    assert!(!admin.exists());
}

#[test]
fn move_git_worktree_relocates_dir_and_keeps_git_working() {
    let td = TestDir::new("move_wt");
    init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
    // A pre-warmed worktree under a plain slug, the pool's on-disk shape
    // (detached, as the pool cuts it).
    let src = make_detached_worktree(td.path(), "warm-amber-fox");
    let dst = td
        .path()
        .join(".claude")
        .join("worktrees")
        .join("gh123-fix-this");
    assert!(src.exists(), "precondition: warm worktree exists");
    assert!(!dst.exists(), "precondition: target name is free");

    move_git_worktree(
        td.path().to_str().unwrap(),
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    )
    .expect("git worktree move must succeed");

    assert!(
        !src.exists(),
        "source directory must be gone after the move"
    );
    assert!(dst.exists(), "target directory must exist after the move");

    // Git's internal references must have followed the directory: the moved
    // worktree opens as a repo and its HEAD resolves (a `git worktree move`
    // that didn't rewrite the gitdir pointer would break both).
    let moved =
        git2::Repository::open(&dst).expect("moved worktree must still open as a git repository");
    assert!(
        moved.head().is_ok(),
        "HEAD must resolve inside the moved worktree"
    );
    // And the root repo's admin list must point at the new path, not the old.
    let root = git2::Repository::open(td.path()).unwrap();
    let still_registered = root
        .worktrees()
        .unwrap()
        .iter()
        .filter_map(|name| name.ok().flatten())
        .filter_map(|n| root.find_worktree(n).ok())
        .any(|wt| wt.path() == dst.as_path());
    assert!(
        still_registered,
        "the moved worktree must be registered at its new path"
    );
}

#[test]
fn move_git_worktree_refuses_an_occupied_target() {
    let td = TestDir::new("move_wt_clash");
    init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
    let src = make_detached_worktree(td.path(), "warm-amber-fox");
    // An occupied target. Bare `git worktree move` would nest the source
    // INSIDE it (mv semantics) and exit 0; our existence guard must refuse
    // instead so a stray leftover never produces a nested worktree.
    let dst = make_branched_worktree(td.path(), "gh7-occupied");

    let err = move_git_worktree(
        td.path().to_str().unwrap(),
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    )
    .expect_err("moving onto an existing target must be refused");
    assert!(
        err.contains("target already exists"),
        "error must name the occupied-target guard, got: {}",
        err
    );
    assert!(
        src.exists(),
        "source must be untouched after a refused move"
    );
    // The target must NOT have gained a nested worktree.
    assert!(
        !dst.join("warm-amber-fox").exists(),
        "the source must not have been nested inside the occupied target"
    );
}

// ── sanitize (unix-style link corruption, 2026-07-17 gh252 incident) ─────

/// Turn `F:\a\b` into the Git-Bash-style `/f/a/b` an MSYS-flavoured git
/// writes into worktree link files.
#[cfg(windows)]
fn to_msys_style(p: &Path) -> String {
    let s = p.to_str().unwrap().replace('\\', "/");
    let drive = s.chars().next().unwrap().to_ascii_lowercase();
    format!("/{}{}", drive, &s[2..])
}

/// A worktree whose link files carry Git-Bash-style `/f/...` paths opens
/// fine under the Git CLI but is NotFound to libgit2 — which made the
/// autopilot wrap-up verifier report a healthy, PR'd worktree as broken.
/// `sanitize_git_worktree` must repair BOTH sides of the link (the
/// worktree's `.git` gitlink AND the repo-side `gitdir` back-pointer) so
/// libgit2 can open it again.
#[cfg(windows)]
#[test]
fn sanitize_repairs_unix_style_links_on_both_sides_for_libgit2() {
    let td = TestDir::new("sanitize_unix_links");
    let parent = td.path();
    init_repo_with_commit(parent, &[("README.md", "# p\n")]);
    let wt = make_branched_worktree(parent, "wt-unix");
    let admin_dir = parent.join(".git").join("worktrees").join("wt-unix");

    fs::write(
        wt.join(".git"),
        format!("gitdir: {}\n", to_msys_style(&admin_dir)),
    )
    .unwrap();
    fs::write(
        admin_dir.join("gitdir"),
        format!("{}/.git\n", to_msys_style(&wt)),
    )
    .unwrap();
    assert!(
        git2::Repository::open(&wt).is_err(),
        "precondition (the bug): unix-style link files must break libgit2 open"
    );

    sanitize_git_worktree(wt.to_str().unwrap(), EnvType::Windows).expect("sanitize must succeed");

    git2::Repository::open(&wt).expect("libgit2 must open the worktree after sanitize");
    let backpointer = fs::read_to_string(admin_dir.join("gitdir")).unwrap();
    assert!(
        !backpointer.trim_start().starts_with('/'),
        "repo-side gitdir back-pointer must be host-format, got: {}",
        backpointer
    );
}

/// Pure conversion table for both env targets — in particular the
/// Git-Bash `/f/...` drive style (what an MSYS git writes into link
/// files, the 2026-07-17 corruption) must repair under BOTH targets,
/// and already-correct paths must pass through unchanged.
///
/// `convert_link_path_for_env` is now a 2-line dispatcher into the
/// canonical converters in `env::host_path` (issue #1226 — the
/// duplicate converter previously here violated the CLAUDE.md hard
/// rule that `env::host_path` is the SOLE module allowed to build
/// `/mnt/` and WSL UNC strings). Assertions target the BEHAVIOR
/// (literal output strings), not the underlying call — so a
/// regression in `windows_to_wsl` or `to_host_path` fails this test,
/// not just the dispatcher wiring.
#[test]
fn convert_link_path_covers_git_bash_drive_style_for_both_envs() {
    // → WSL
    assert_eq!(
        convert_link_path_for_env("/f/src/repo/.git/worktrees/wt", EnvType::Wsl),
        "/mnt/f/src/repo/.git/worktrees/wt"
    );
    assert_eq!(
        convert_link_path_for_env("F:\\src\\repo", EnvType::Wsl),
        "/mnt/f/src/repo"
    );
    // Real WSL paths must NOT be re-mangled.
    assert_eq!(
        convert_link_path_for_env("/mnt/f/src", EnvType::Wsl),
        "/mnt/f/src"
    );
    assert_eq!(
        convert_link_path_for_env("/home/u/repo", EnvType::Wsl),
        "/home/u/repo"
    );
    // → Windows (host conversion is a Windows-host behaviour).
    #[cfg(windows)]
    {
        assert_eq!(
            convert_link_path_for_env("/f/src/repo", EnvType::Windows),
            "F:\\src\\repo"
        );
        assert_eq!(
            convert_link_path_for_env("/mnt/c/Users", EnvType::Windows),
            "C:\\Users"
        );
    }
}

/// The common case — healthy Windows-format link files — must pass through
/// sanitize byte-identical (no churn on every spawn).
#[cfg(windows)]
#[test]
fn sanitize_leaves_healthy_link_files_untouched() {
    let td = TestDir::new("sanitize_noop");
    let parent = td.path();
    init_repo_with_commit(parent, &[("README.md", "# p\n")]);
    let wt = make_branched_worktree(parent, "wt-ok");
    let admin_gitdir = parent
        .join(".git")
        .join("worktrees")
        .join("wt-ok")
        .join("gitdir");
    let gitlink_before = fs::read_to_string(wt.join(".git")).unwrap();
    let backpointer_before = fs::read_to_string(&admin_gitdir).unwrap();

    sanitize_git_worktree(wt.to_str().unwrap(), EnvType::Windows).expect("sanitize must succeed");

    assert_eq!(fs::read_to_string(wt.join(".git")).unwrap(), gitlink_before);
    assert_eq!(
        fs::read_to_string(&admin_gitdir).unwrap(),
        backpointer_before
    );
    git2::Repository::open(&wt).expect("worktree must still open");
}

#[test]
fn resolve_sha_base_ref_resolves_and_falls_back_to_head() {
    let td = TestDir::new("resolve_sha");
    let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
    let head = repo
        .head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .id()
        .to_string();

    // A resolvable ref returns its concrete SHA.
    assert_eq!(
        crate::git::sha::resolve_sha_at(
            td.path().to_str().unwrap(),
            &crate::git::sha::ShaSource::BaseRef("HEAD".to_string()),
        )
        .unwrap()
        .to_string(),
        head
    );
    // An unresolvable ref degrades to HEAD (offline resilience) rather than
    // erroring — the property the warm-pool adoption path depends on.
    assert_eq!(
        crate::git::sha::resolve_sha_at(
            td.path().to_str().unwrap(),
            &crate::git::sha::ShaSource::BaseRef("origin/does-not-exist".to_string()),
        )
        .unwrap()
        .to_string(),
        head,
        "an unfetched ref must fall back to HEAD, not hard-fail the checkout"
    );
}

// ── the one SHA resolver + base_ref (#230, #2533) ─────────────────────────

#[test]
fn resolve_sha_head_returns_mesh_root_head() {
    let td = TestDir::new("resolve_head");
    let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
    let head = repo.head().unwrap().peel_to_commit().unwrap().id();
    assert_eq!(
        crate::git::sha::resolve_sha(
            &repo,
            &crate::git::sha::ShaSource::BaseRef("HEAD".to_string())
        )
        .unwrap(),
        head
    );
    assert_eq!(
        crate::git::sha::resolve_sha(&repo, &crate::git::sha::ShaSource::BaseRef("".to_string()))
            .unwrap(),
        head
    );
}

#[test]
fn resolve_sha_resolves_remote_ref() {
    let td = TestDir::new("resolve_remote");
    let (repo, origin_oid) = repo_with_drifted_head(td.path());
    assert_eq!(
        crate::git::sha::resolve_sha(
            &repo,
            &crate::git::sha::ShaSource::BaseRef("origin/main".to_string())
        )
        .unwrap(),
        origin_oid,
        "origin/main must resolve to the remote-tracking commit, not HEAD"
    );
}

#[test]
fn resolve_sha_falls_back_to_head_when_unresolvable() {
    let td = TestDir::new("resolve_fallback");
    let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
    let head = repo.head().unwrap().peel_to_commit().unwrap().id();
    assert_eq!(
        crate::git::sha::resolve_sha(
            &repo,
            &crate::git::sha::ShaSource::BaseRef("origin/does-not-exist".to_string())
        )
        .unwrap(),
        head,
        "an unresolvable ref must fall back to HEAD rather than error"
    );
}

#[test]
fn branched_worktree_bases_off_base_ref_not_head() {
    let td = TestDir::new("base_ref_branched");
    let parent = td.path();
    let (_repo, origin_oid) = repo_with_drifted_head(parent);

    let wt_path = parent.join(".claude").join("worktrees").join("wt-br");
    create_git_worktree(
        parent.to_str().unwrap(),
        wt_path.to_str().unwrap(),
        "wt-br",
        "branched",
        "origin/main",
    )
    .expect("branched worktree creation must succeed");

    let content = fs::read_to_string(wt_path.join("f.txt")).unwrap();
    assert_eq!(content, "from-origin-main\n");
    let wt_repo = git2::Repository::open(&wt_path).unwrap();
    assert_eq!(
        wt_repo.head().unwrap().peel_to_commit().unwrap().id(),
        origin_oid
    );
}

#[test]
fn detached_worktree_bases_off_base_ref_not_head() {
    let td = TestDir::new("base_ref_detached");
    let parent = td.path();
    let (_repo, origin_oid) = repo_with_drifted_head(parent);

    let wt_path = parent.join(".claude").join("worktrees").join("wt-det");
    create_git_worktree(
        parent.to_str().unwrap(),
        wt_path.to_str().unwrap(),
        "wt-det",
        "detached",
        "origin/main",
    )
    .expect("detached worktree creation must succeed");

    let wt_repo = git2::Repository::open(&wt_path).unwrap();
    assert!(wt_repo.head_detached().unwrap_or(false));
    assert_eq!(
        wt_repo.head().unwrap().peel_to_commit().unwrap().id(),
        origin_oid
    );
    assert_eq!(
        fs::read_to_string(wt_path.join("f.txt")).unwrap(),
        "from-origin-main\n"
    );
}

#[test]
fn worktree_base_ref_head_reproduces_mesh_root_head() {
    let td = TestDir::new("base_ref_head");
    let parent = td.path();
    let (repo, origin_oid) = repo_with_drifted_head(parent);
    let head_oid = repo.head().unwrap().peel_to_commit().unwrap().id();

    let wt_path = parent.join(".claude").join("worktrees").join("wt-head");
    create_git_worktree(
        parent.to_str().unwrap(),
        wt_path.to_str().unwrap(),
        "wt-head",
        "branched",
        "HEAD",
    )
    .expect("worktree creation must succeed");

    let wt_repo = git2::Repository::open(&wt_path).unwrap();
    let wt_oid = wt_repo.head().unwrap().peel_to_commit().unwrap().id();
    assert_eq!(wt_oid, head_oid);
    assert_ne!(wt_oid, origin_oid);
    assert_eq!(
        fs::read_to_string(wt_path.join("f.txt")).unwrap(),
        "local-drift\n"
    );
}

#[test]
fn worktree_unresolvable_base_ref_falls_back_to_head() {
    let td = TestDir::new("base_ref_offline");
    let parent = td.path();
    let repo = init_repo_with_commit(parent, &[("f.txt", "only-commit\n")]);
    let head_oid = repo.head().unwrap().peel_to_commit().unwrap().id();

    let wt_path = parent.join(".claude").join("worktrees").join("wt-off");
    create_git_worktree(
        parent.to_str().unwrap(),
        wt_path.to_str().unwrap(),
        "wt-off",
        "branched",
        "origin/main",
    )
    .expect("spawn must not fail when base_ref is unresolvable");

    let wt_repo = git2::Repository::open(&wt_path).unwrap();
    assert_eq!(
        wt_repo.head().unwrap().peel_to_commit().unwrap().id(),
        head_oid,
        "unresolvable base_ref must fall back to HEAD"
    );
}

// ── apply_worktree_include (.worktreeinclude, issue #248) ────────────────
//
// `.worktreeinclude` lists project-root-relative paths that should be
// copied into every freshly materialised worktree (typical entries:
// `.env`, build caches, plugin dirs). The current implementation copies
// individual files and log-and-skips directory entries (mod.rs:214-216)
// — issue #248 fills that gap so a directory listing copies recursively.
// These tests pin both the happy path AND the silent no-op contract for
// missing sources (mirrors the missing-file / missing-manifest semantics).

/// `apply_worktree_include` recursively copies a directory entry into the
/// worktree — both top-level files AND nested files inside the listed
/// directory. The companion no-op test pins that a missing source stays
/// silent rather than erroring.
#[test]
fn apply_worktree_include_copies_directory_recursively() {
    let td = TestDir::new("wti_dir");
    let project_root = td.path();
    let wt = project_root.join(".claude").join("worktrees").join("wt");

    // Build a `.worktreeinclude` that lists one regular file AND a
    // directory with a nested file — verifies the recursive case is
    // distinct from the existing per-file copy.
    fs::create_dir_all(project_root.join("config/envs")).unwrap();
    fs::write(project_root.join("config/envs/prod.env"), "PROD=1\n").unwrap();
    fs::create_dir_all(project_root.join("config/nested")).unwrap();
    fs::write(project_root.join("config/nested/.keep"), "keep\n").unwrap();
    fs::write(project_root.join("README-top"), "top\n").unwrap();
    fs::write(
        project_root.join(".worktreeinclude"),
        "README-top\nconfig\n",
    )
    .unwrap();

    fs::create_dir_all(&wt).unwrap();
    apply_worktree_include(project_root.to_str().unwrap(), &wt);

    assert_eq!(fs::read_to_string(wt.join("README-top")).unwrap(), "top\n");
    assert_eq!(
        fs::read_to_string(wt.join("config/envs/prod.env")).unwrap(),
        "PROD=1\n"
    );
    assert_eq!(
        fs::read_to_string(wt.join("config/nested/.keep")).unwrap(),
        "keep\n"
    );
}

#[test]
fn apply_worktree_include_discovers_repository_root_from_scope() {
    let td = TestDir::new("wti_scoped");
    crate::env::test_helpers::init_repo_with_commit(
        td.path(),
        &[("family/parents/AGENTS.md", "instructions")],
    );
    fs::write(td.path().join("local.config"), "root configuration").unwrap();
    fs::write(td.path().join(".worktreeinclude"), "local.config\n").unwrap();
    let wt = td.path().join("owned");
    fs::create_dir_all(&wt).unwrap();

    apply_worktree_include(td.path().join("family/parents").to_str().unwrap(), &wt);

    assert_eq!(
        fs::read_to_string(wt.join("local.config")).unwrap(),
        "root configuration"
    );
}

/// A `.worktreeinclude` entry pointing at a non-existent path must be a
/// silent no-op — both for missing files (existing behaviour) and for
/// missing directories (the new code path). Pinned so a future cleanup
/// pass can't turn this into a hard error and break offline spawns.
#[test]
fn apply_worktree_include_directory_missing_source_is_noop() {
    let td = TestDir::new("wti_missing_dir");
    let project_root = td.path();
    let wt = project_root.join(".claude/worktrees/wt");
    fs::write(project_root.join(".worktreeinclude"), "does-not-exist\n").unwrap();
    fs::create_dir_all(&wt).unwrap();

    // Must not panic — a missing source is treated identically to a
    // missing file or a missing manifest: the worktree is still usable.
    apply_worktree_include(project_root.to_str().unwrap(), &wt);
    // The worktree exists but no spurious directory was created.
    assert!(!wt.join("does-not-exist").exists());
}

// ── close-safety ─────────────────────────────────────────────────────────

#[test]
fn close_safety_reports_clean_merged_worktree_as_safe() {
    let td = TestDir::new("safe_clean");
    init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
    let wt = make_branched_worktree(td.path(), "wt-safe");

    let safety = close_safety(&wt.to_string_lossy());

    assert!(!safety.has_uncommitted);
    assert!(!safety.has_unpushed);
    assert!(!safety.is_detached);
}

#[test]
fn close_safety_reports_uncommitted_changes() {
    let td = TestDir::new("safe_dirty");
    init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
    let wt = make_branched_worktree(td.path(), "wt-dirty");
    fs::write(wt.join("scratch.txt"), "not committed").unwrap();

    let safety = close_safety(&wt.to_string_lossy());

    assert!(safety.has_uncommitted);
    assert!(!safety.has_unpushed);
}

#[test]
fn close_safety_reports_branch_commits_not_reachable_from_another_ref() {
    let td = TestDir::new("safe_ahead");
    init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
    let wt = make_branched_worktree(td.path(), "wt-ahead");
    let wt_repo = git2::Repository::open(&wt).unwrap();
    commit_file(&wt_repo, &wt, "work.txt", "committed locally");

    let safety = close_safety(&wt.to_string_lossy());

    assert!(!safety.has_uncommitted);
    assert!(safety.has_unpushed);
}

#[test]
fn close_safety_reports_detached_head_without_extra_commits_as_safe() {
    let td = TestDir::new("safe_detached_safe");
    init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
    let wt = make_branched_worktree(td.path(), "wt-detached-safe");
    let wt_repo = git2::Repository::open(&wt).unwrap();
    let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
    wt_repo.set_head_detached(head.id()).unwrap();

    let safety = close_safety(&wt.to_string_lossy());

    assert!(safety.is_detached);
    assert!(!safety.has_unpushed);
}

#[test]
fn close_safety_reports_detached_head_with_unique_commits_as_unpushed() {
    let td = TestDir::new("safe_detached_risk");
    init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
    let wt = make_branched_worktree(td.path(), "wt-detached-risk");
    let wt_repo = git2::Repository::open(&wt).unwrap();
    let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
    wt_repo.set_head_detached(head.id()).unwrap();
    commit_file(&wt_repo, &wt, "detached.txt", "detached work");

    let safety = close_safety(&wt.to_string_lossy());

    assert!(safety.is_detached);
    assert!(safety.has_unpushed);
}

#[test]
fn close_safety_treats_missing_worktree_as_nothing_to_remove() {
    let td = TestDir::new("safe_missing");
    let missing = td.path().join("does-not-exist");

    let safety = close_safety(&missing.to_string_lossy());

    assert_eq!(safety.worktree_path, None);
    assert!(!safety.has_uncommitted);
    assert!(!safety.has_unpushed);
    assert!(!safety.is_detached);
}

#[test]
fn close_safety_treats_non_repo_directory_as_nothing_to_remove() {
    let td = TestDir::new("safe_nonrepo");
    fs::write(td.path().join("stray.txt"), "not a git repo").unwrap();

    let safety = close_safety(&td.path().to_string_lossy());

    assert_eq!(safety.worktree_path, None);
    assert!(!safety.has_uncommitted);
}

// ── close-safety: [gone] upstream (#252) ─────────────────────────────────
//
// After a squash-merge + auto-delete, git config still points the local
// branch at `refs/remotes/origin/<branch>`, but the tracking ref has been
// pruned. `find_reference` then fails and the old code wrongly fell into
// `return true`. The fix falls through to the reachability check so the
// common "work is on main, branch is just dead weight" case closes cleanly.

/// Configure `branch.<name>.remote`/`.merge` to point at `origin/<name>`
/// without ever creating the `refs/remotes/origin/<name>` tracking ref —
/// the post-prune `[gone]` state from issue #252. Mirrors `git remote add`
/// closely enough for git2 to resolve the upstream: `remote.origin.url`,
/// the standard fetch refspec, and the per-branch remote/merge pointers.
fn configure_upstream_without_tracking_ref(repo: &git2::Repository, branch: &str) {
    let mut cfg = repo.config().unwrap();
    cfg.set_str("remote.origin.url", "https://example.invalid/repo.git")
        .unwrap();
    cfg.set_str("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*")
        .unwrap();
    cfg.set_str(&format!("branch.{}.remote", branch), "origin")
        .unwrap();
    cfg.set_str(
        &format!("branch.{}.merge", branch),
        &format!("refs/heads/{}", branch),
    )
    .unwrap();
    // Sanity: the tracking ref really is absent (the bug only triggers here).
    assert!(
        repo.find_reference(&format!("refs/remotes/origin/{}", branch))
            .is_err(),
        "test precondition: tracking ref must be pruned"
    );
}

#[test]
fn close_safety_reports_gone_upstream_with_merged_work_as_safe() {
    // Squash-merge + auto-delete: HEAD on the feature branch points at the
    // same commit as main (the work is safely on main), upstream config
    // exists, but the tracking ref has been pruned. Must NOT warn — closing
    // should be allowed. The current bug returns true from the
    // `find_reference` Err path and never consults reachability.
    let td = TestDir::new("safe_gone_upstream_merged");
    init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
    let wt = make_branched_worktree(td.path(), "wt-merged");
    let wt_repo = git2::Repository::open(&wt).unwrap();

    // Sanity: HEAD on wt-merged is at the same commit as the parent's
    // default branch — that's the reachable-from-another-ref precondition
    // the fix relies on.
    let wt_head_oid = wt_repo.head().unwrap().peel_to_commit().unwrap().id();
    let parent_repo = git2::Repository::open(td.path()).unwrap();
    let parent_head_oid = parent_repo.head().unwrap().peel_to_commit().unwrap().id();
    assert_eq!(
        wt_head_oid, parent_head_oid,
        "test precondition: wt-merged HEAD must equal main HEAD (FF-merged)"
    );

    configure_upstream_without_tracking_ref(&wt_repo, "wt-merged");

    let safety = close_safety(&wt.to_string_lossy());

    assert!(!safety.is_detached);
    assert!(!safety.has_uncommitted);
    assert!(
        !safety.has_unpushed,
        "configured-but-gone upstream with work reachable from another ref must not warn \
             (issue #252: false-positive 'unpushed' on squash-merged branches)"
    );
}

#[test]
fn close_safety_reports_gone_upstream_with_unmerged_work_as_unpushed() {
    // Safety net: when the upstream tracking ref is gone AND HEAD is not
    // reachable from any other branch, we must still warn — closing would
    // otherwise lose the only copy of the work.
    let td = TestDir::new("safe_gone_upstream_orphan");
    init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
    let wt = make_branched_worktree(td.path(), "wt-orphan");
    let wt_repo = git2::Repository::open(&wt).unwrap();
    commit_file(&wt_repo, &wt, "exclusive.txt", "only on wt-orphan");

    configure_upstream_without_tracking_ref(&wt_repo, "wt-orphan");

    let safety = close_safety(&wt.to_string_lossy());

    assert!(!safety.is_detached);
    assert!(
        safety.has_unpushed,
        "orphan branch with [gone] upstream must still warn — closing would lose work"
    );
}

// ── remove ───────────────────────────────────────────────────────────────

#[test]
fn remove_one_worktree_removes_dir_and_prunes_admin_entry() {
    let td = TestDir::new("remove_wt");
    let root_repo = init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
    let wt = make_branched_worktree(td.path(), "wt-remove");
    assert!(wt.exists());

    remove_one_worktree(&wt.to_string_lossy()).expect("removal succeeds");

    assert!(!wt.exists(), "working directory must be gone");
    // The git admin entry must be pruned too.
    assert!(
        root_repo.find_worktree("wt-remove").is_err(),
        "worktree admin entry must be pruned"
    );
}

#[test]
fn remove_one_worktree_is_idempotent_for_missing_dir() {
    let td = TestDir::new("remove_missing");
    let missing = td.path().join("never-existed");
    remove_one_worktree(&missing.to_string_lossy())
        .expect("a missing worktree dir counts as already removed");
}

/// A path with no repository above it has no admin metadata to reconcile,
/// so there is genuinely nothing to prune and the removal succeeds (a
/// #1080-style derived path, or a mesh root that was deleted).
#[test]
fn prune_orphaned_worktree_entry_reports_nothing_when_no_repository_is_above_the_path() {
    let td = TestDir::new("prune_no_repo");
    let missing = td.path().join("never-existed");
    // The temp directory is not a git repository, so discovery says
    // "not found" — the only legitimate success without a prune.
    prune_orphaned_worktree_entry(&missing.to_string_lossy())
        .expect("no repository above the path means nothing to reconcile");
}

/// Any other discovery failure — an I/O error, a repository that cannot be
/// read — means the git half was *not* finished, so the caller must keep the
/// tombstone instead of retiring it (issue #2139 review round 2).
#[test]
fn prune_orphaned_worktree_entry_fails_closed_when_discovery_errors() {
    let td = TestDir::new("prune_discover_error");
    let missing = td.path().join("never-existed");
    // An OS error is not "not found": the repository may well be there and
    // unreadable, so the admin entry could still exist. `git2::Error` is
    // not `Clone`, so the closure builds a fresh one per call.
    let failure = prune_orphaned_worktree_entry_with(missing.to_str().unwrap(), |_| {
        Err(git2::Error::new(
            git2::ErrorCode::GenericError,
            git2::ErrorClass::Os,
            "could not read the repository",
        ))
    })
    .expect_err("a failed repository read must keep the tombstone");
    assert_eq!(
        failure.operation, OP_PRUNE_ADMIN_ENTRY,
        "the failure names the step that could not finish"
    );
    assert!(
        failure
            .detail
            .contains("could not open the repository above"),
        "the failure says which repository could not be read, got: {}",
        failure.detail
    );
}

// ── issue #2139 — blocked cleanup stays recoverable ─────────────────────

/// Issue #2139: a removal interrupted between the rename into `<path>
/// .removing` staging and the staged delete leaves BOTH the staging
/// directory and a live git admin entry, with the original path already
/// gone. The old early-return ("missing directory = nothing to remove")
/// treated that state as a finished removal: the tombstone was dequeued
/// while the staging directory leaked and `git worktree list` kept a dead
/// entry forever. Recovery must finish both halves — disk and git
/// bookkeeping — and report failure if it can't, so the cleanup intent
/// survives in the queue until the removal is genuinely complete.
#[test]
fn removing_staging_is_recovered_when_original_directory_is_absent() {
    let td = TestDir::new("staging_recover");
    let root_repo = init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
    let wt = make_branched_worktree(td.path(), "wt-staging");
    let staging = format!("{}.removing", wt.to_string_lossy());

    // An interrupted removal: the atomic rename landed, the staged delete
    // never ran.
    std::fs::rename(&wt, &staging).unwrap();
    assert!(!wt.exists(), "precondition: original path is absent");
    assert!(
        std::path::Path::new(&staging).exists(),
        "precondition: the staged copy survives"
    );

    remove_one_worktree(&wt.to_string_lossy())
        .expect("a leftover staging directory must be reclaimed, not ignored");

    assert!(
        !std::path::Path::new(&staging).exists(),
        "the leftover .removing staging must be deleted"
    );
    assert!(
        root_repo.find_worktree("wt-staging").is_err(),
        "git bookkeeping must finish too: the admin entry must be pruned"
    );
}

/// A blocked cleanup must record *what* failed, not just that something
/// did. A plain directory that isn't a worktree fails at the open step;
/// the returned failure names that operation so the UI can show
/// "failed operation: <step>" next to the OS error.
#[test]
fn removal_failure_names_the_operation_that_failed() {
    let td = TestDir::new("failure_operation");
    let plain = td.path().join("not-a-worktree");
    std::fs::create_dir_all(&plain).unwrap();

    let failure = remove_one_worktree_and_branch_detailed(&plain.to_string_lossy())
        .expect_err("a non-worktree directory cannot be removed");

    assert_eq!(
        failure.operation, OP_OPEN_WORKTREE_REPO,
        "the failure must identify the step that failed"
    );
    assert!(
        !failure.detail.trim().is_empty(),
        "the failure must carry the underlying error text"
    );
}
