//! Tests for `commands::pr`.
//!
//! Split out of `src-tauri/src/commands/pr.rs` by issue #2451. The conventions this
//! layout depends on — why the module keeps the name `tests`, and why every
//! `#[path]` module needs an entry in `RUST_PATH_INCLUDES` — are
//! documented once in `docs/development/rust-conventions.md`, under *Rust unit
//! tests live in sibling `*_tests.rs` files*.
//!
//! Run with: cd src-tauri && cargo test commands::pr::tests

use super::*;
use crate::env::test_helpers::init_repo_with_commit as init_repo_for_test;
use crate::git::worktree::create_git_worktree;
use crate::models::AgentNode;
use crate::services::github::tests::{fake_server, Scripted};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[test]
fn safe_directory_commands_preserve_shell_paths() {
    for (apostrophe, quoted) in [
        ("'", "''"),
        ("\u{2018}", "\u{2018}\u{2018}"),
        ("\u{2019}", "\u{2019}\u{2019}"),
        ("\u{201a}", "\u{201a}\u{201a}"),
        ("\u{201b}", "\u{201b}\u{201b}"),
    ] {
        assert_eq!(
            safe_directory_command(&format!("C:\\don{apostrophe}t\\repo"), true),
            format!("git config --global --add safe.directory 'C:/don{quoted}t/repo'"),
            "apostrophe: {apostrophe:?}",
        );
    }
    assert_eq!(
        safe_directory_command(r"/tmp/alice's\repo", false),
        r"git config --global --add safe.directory '/tmp/alice'\''s\repo'",
    );
}

/// PowerShell treats U+2018..U+201B as string delimiters, so a path holding
/// one has to reach `git` as a single exact argument (#2286).
#[cfg(windows)]
#[test]
fn safe_directory_commands_hand_git_one_exact_argument() {
    use crate::process_util::command_no_window;

    let apostrophes = ["'", "\u{2018}", "\u{2019}", "\u{201a}", "\u{201b}"];
    let paths: Vec<String> = apostrophes
        .iter()
        .map(|apostrophe| format!("C:/don{apostrophe}t\\repo"))
        .collect();
    // A `git` function in the session takes precedence over `git.exe` and prints
    // the last argument it receives, so the guidance command's own output
    // reports the path and the run touches no Git configuration.
    let script = format!(
        "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)\n\
             function git {{ [Console]::Out.WriteLine($args[-1]) }}\n{}",
        paths
            .iter()
            .map(|path| safe_directory_command(path, true))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let output = command_no_window("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "PowerShell rejected the guidance commands: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reported: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    let expected: Vec<String> = paths.iter().map(|path| path.replace('\\', "/")).collect();
    assert_eq!(reported, expected);
}

#[test]
fn github_feeds_report_unreadable_repository() {
    let _db = ensure_pr_blocking_db();
    let tmp = TempGitRepo::new();
    let mesh = db::create_mesh("unreadable-github-feed", tmp.path().to_str().unwrap()).unwrap();
    let issues = get_repo_issues_blocking(mesh.id);
    let pulls = get_repo_pulls_blocking(mesh.id, "open".to_string());
    db::delete_mesh(mesh.id).unwrap();
    assert!(issues.unwrap_err().contains("git error:"));
    assert!(pulls.unwrap_err().contains("git error:"));
}

#[test]
fn github_feeds_without_github_origin_remain_empty() {
    let _db = ensure_pr_blocking_db();
    for origin in [None, Some("https://gitlab.com/example/repo.git")] {
        let (tmp, path) = init_repo_with_commit();
        if let Some(url) = origin {
            Repository::open(&path)
                .unwrap()
                .remote("origin", url)
                .unwrap();
        }
        let mesh = db::create_mesh("non-github-feed", &path).unwrap();
        let issues = get_repo_issues_blocking(mesh.id).unwrap();
        let pulls = get_repo_pulls_blocking(mesh.id, "open".to_string()).unwrap();
        assert!(issues.items.is_empty());
        assert!(pulls.items.is_empty());
        // A read that never happened has nothing to be incomplete about,
        // and must not render as a truncation warning.
        assert!(issues.completeness.complete);
        assert!(pulls.completeness.complete);
        assert_eq!(issues.completeness.incomplete_reason, None);
        db::delete_mesh(mesh.id).unwrap();
        drop(tmp);
    }
}

#[cfg(windows)]
#[test]
#[ignore = "requires default WSL; changes process-global libgit2 config search path, run serially"]
fn live_wsl_github_repository_trust() {
    let guest_home = env::wsl_home().expect("WSL home");
    let fixture = tempfile::Builder::new()
        .prefix("buildmesh-github-")
        .tempdir_in(env::to_host_path(&guest_home.to_string_lossy()))
        .unwrap();
    let host_path = fixture.path().to_str().unwrap();
    let guest_path = env::normalize_unc_to_wsl(host_path).into_owned();
    let mut command = crate::process_util::command_no_window("wsl.exe");
    command.args([
        "-d",
        &env::get_default_wsl_distro().unwrap(),
        "--exec",
        "git",
        "init",
        &guest_path,
    ]);
    let output = crate::process_util::run_command_with_timeout(
        command,
        "WSL fixture init",
        std::time::Duration::from_secs(15),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::write(fixture.path().join(".git/config"), "[core]\nrepositoryformatversion = 0\nbare = false\n[remote \"origin\"]\nurl = https://github.com/example/wsl-fixture.git\n").unwrap();

    let config_dir = tempfile::tempdir().unwrap();
    let mut config = git2::Config::open(&config_dir.path().join(".gitconfig")).unwrap();
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
    assert_eq!(
        Repository::open(host_path).err().unwrap().code(),
        git2::ErrorCode::Owner
    );
    let error = resolve_owner_repo(host_path).unwrap_err();
    assert!(error.contains("safe.directory"), "{error}");
    assert!(error.contains("Windows"), "{error}");
    config
        .set_str("safe.directory", &host_path.replace('\\', "/"))
        .unwrap();
    let expected = Some(("example".to_string(), "wsl-fixture".to_string()));
    assert_eq!(resolve_owner_repo(host_path).unwrap(), expected);
    assert_eq!(resolve_owner_repo(&guest_path).unwrap(), expected);
    assert_eq!(repo_info(&guest_path).unwrap().owner_repo, expected);
    assert_eq!(
        github_url_for_path(&guest_path).unwrap(),
        Some("https://github.com/example/wsl-fixture".to_string())
    );
}

// ----- GitHubIssue wire shape (issue #481 follow-up: blocked_by) -----

/// Full wire shape including the `blocked_by` field. Pins the field's
/// JSON name and element type so the regenerated `src/types/generated/
/// GitHubIssue.ts` keeps `blocked_by: Array<number>` and the frontend's
/// strict typing continues to compile. The CI gate
/// `git diff --exit-code src/types/generated/` (per CLAUDE.md) catches
/// accidental drift here.
#[test]
fn github_issue_wire_shape_with_blocked_by() {
    let json = r#"{
            "number": 482,
            "url": "https://github.com/alondero/buildmesh/issues/482",
            "title": "Issue blocked by another",
            "body": "see Blocked by section",
            "state": "open",
            "labels": [],
            "blocked_by": [481, 999]
        }"#;
    let issue: GitHubIssue = serde_json::from_str(json).expect("full wire shape parses");
    assert_eq!(issue.number, 482);
    assert_eq!(issue.blocked_by, vec![481, 999]);
}

/// Missing `blocked_by` key (older frontend, partial payload, or a
/// future test fixture that forgets it) must default to `vec![]`
/// rather than failing to parse. The `#[serde(default)]` on the
/// field is the safety net; this test pins the behaviour so a future
/// refactor that flips the field to required surfaces as a test
/// failure rather than a silent rollback.
#[test]
fn github_issue_wire_shape_with_missing_blocked_by_defaults() {
    let json = r#"{
            "number": 481,
            "url": "https://github.com/alondero/buildmesh/issues/481",
            "title": "No blockers",
            "body": "None - can start immediately.",
            "state": "open",
            "labels": []
        }"#;
    let issue: GitHubIssue = serde_json::from_str(json).expect("partial wire shape parses");
    assert!(
        issue.blocked_by.is_empty(),
        "missing blocked_by defaults to empty vec"
    );
}

/// Round-trip: serialise a populated `blocked_by` and re-parse it
/// unchanged. Catches a class of bugs where serde rename / flatten
/// rules accidentally drop the field on serialisation (which would
/// only show up at the IPC seam, not at the parse-from-test-fixture
/// site).
#[test]
fn github_issue_wire_shape_blocked_by_round_trips() {
    let original = GitHubIssue {
        number: 482,
        title: "Blocked issue".into(),
        body: "body".into(),
        url: "https://github.com/x/y/issues/482".into(),
        state: "open".into(),
        labels: vec!["bug".into()],
        blocked_by: vec![481, 482, 483],
        author: "contributor-jane".into(),
    };
    let json = serde_json::to_string(&original).expect("serialise");
    let parsed: GitHubIssue = serde_json::from_str(&json).expect("re-parse");
    assert_eq!(parsed.blocked_by, vec![481, 482, 483]);
    assert_eq!(parsed.number, 482);
    assert_eq!(parsed.labels, vec!["bug".to_string()]);
    assert_eq!(parsed.author, "contributor-jane");
}

/// The contributor pill on the Issues probe row reads `author`; an
/// older cached payload without the key must still parse (additive
/// field) and default to `\"\"` — the pill then doesn't render.
#[test]
fn github_issue_wire_shape_author_defaults_empty() {
    let json = r#"{
            "number": 7,
            "url": "https://github.com/x/y/issues/7",
            "title": "legacy",
            "body": "",
            "state": "open",
            "labels": [],
            "blocked_by": []
        }"#;
    let issue: GitHubIssue = serde_json::from_str(json).expect("partial wire shape parses");
    assert_eq!(issue.author, "", "missing author defaults to empty");

    let json_author = r#"{
            "number": 8,
            "url": "https://github.com/x/y/issues/8",
            "title": "with author",
            "body": "",
            "state": "open",
            "labels": [],
            "blocked_by": [],
            "author": "octocat"
        }"#;
    let issue: GitHubIssue = serde_json::from_str(json_author).expect("author wire shape parses");
    assert_eq!(issue.author, "octocat");
}

// ----- GitHubPullRequest wire shape (issue #420) ---------------------

/// `head_ref` is optional with `#[serde(default)]` so a partial response
/// (older / cached) still parses — the spawn path surfaces "missing head
/// ref" as a user-facing error. Pin the wire shape so a future refactor
/// that flips it to required surfaces as a test failure rather than a
/// runtime panic. The Tauri-side wire struct uses the field name `url`
/// (mapped from GitHub's `html_url` by the command-side mapper), not
/// `html_url`.
#[test]
fn github_pull_request_wire_shape_with_head_ref() {
    let json = r#"{
            "number": 420,
            "url": "https://github.com/alondero/buildmesh/pull/420",
            "title": "spawn on PR",
            "body": "spawn an agent on a PR",
            "state": "open",
            "draft": false,
            "head_ref": "feat/420-pr-spawn",
            "head_sha": "0123456789abcdef0123456789abcdef01234567"
        }"#;
    let pr: GitHubPullRequest = serde_json::from_str(json).expect("full wire shape parses");
    assert_eq!(pr.head_ref, "feat/420-pr-spawn");
    // Issue #444 — the SHA must round-trip through the Tauri-side wire
    // shape unchanged so the spawn path can persist it for exact-pinning.
    assert_eq!(pr.head_sha, "0123456789abcdef0123456789abcdef01234567");
}

#[test]
fn github_pull_request_wire_shape_with_missing_head_ref_defaults() {
    let json = r#"{
            "number": 7,
            "url": "https://github.com/x/y/pull/7",
            "title": "legacy",
            "body": "",
            "state": "open",
            "draft": false
        }"#;
    let pr: GitHubPullRequest = serde_json::from_str(json).expect("partial wire shape parses");
    assert_eq!(pr.head_ref, "", "missing head_ref defaults to empty");
    assert_eq!(pr.head_sha, "", "missing head_sha defaults to empty");
    assert_eq!(
        pr.mergeable, None,
        "missing mergeable defaults to None (unknown)"
    );
    assert_eq!(
        pr.mergeable_state, "",
        "missing mergeable_state defaults to empty"
    );
    assert_eq!(pr.author, "", "missing author defaults to empty");
}

/// The contributor pill on the PRs probe row reads `author` — pin the
/// wire name so the regenerated `GitHubPullRequest.ts` keeps
/// `author: string` and the frontend's strict typing keeps compiling.
#[test]
fn github_pull_request_wire_shape_with_author() {
    let json = r#"{
            "number": 9,
            "url": "https://github.com/x/y/pull/9",
            "title": "authored PR",
            "body": "",
            "state": "open",
            "draft": false,
            "head_ref": "feat/9",
            "author": "contributor-jane"
        }"#;
    let pr: GitHubPullRequest = serde_json::from_str(json).expect("author wire shape parses");
    assert_eq!(pr.author, "contributor-jane");
}

/// Issue #1529: mergeability rides inline on the list wire shape.
/// `mergeable: true/false/null` round-trips with `mergeable_state`, so
/// the panel renders mergeable/conflict/checking without a second call.
/// `null` (UNKNOWN) must stay `None`, not coerce to `Some(false)`.
#[test]
fn github_pull_request_wire_shape_with_inline_mergeability() {
    let json = r#"{
            "number": 1529,
            "url": "https://github.com/alondero/buildmesh/pull/1529",
            "title": "perf summaries",
            "body": "",
            "state": "open",
            "draft": false,
            "head_ref": "perf/1529-summaries",
            "head_sha": "0123456789abcdef0123456789abcdef01234567",
            "mergeable": true,
            "mergeable_state": "clean"
        }"#;
    let pr: GitHubPullRequest = serde_json::from_str(json).expect("inline mergeability parses");
    assert_eq!(pr.mergeable, Some(true));
    assert_eq!(pr.mergeable_state, "clean");

    let json_null = r#"{
            "number": 1530,
            "url": "https://github.com/x/y/pull/1530",
            "title": "fresh",
            "body": "",
            "state": "open",
            "draft": false,
            "mergeable": null,
            "mergeable_state": "unknown"
        }"#;
    let pr_null: GitHubPullRequest =
        serde_json::from_str(json_null).expect("null mergeable parses");
    assert_eq!(
        pr_null.mergeable, None,
        "null must stay None, not coerce to Some(false)"
    );
    assert_eq!(pr_null.mergeable_state, "unknown");
}

/// Issue #443: a fork PR carries `head_repo_owner` (the fork's owner
/// login) + `head_repo_clone_url` (the fork's clone URL) on the wire
/// so the spawn path can register the fork as a remote. Both fields
/// are `#[serde(default)]` so a partial response (older frontend, or
/// an API hiccup that drops `head.repo`) still parses — the spawn
/// path then treats empty values as "no fork info, use the #420
/// same-repo path" and a head_ref-empty head as the refusal case.
#[test]
fn github_pull_request_wire_shape_with_fork_metadata() {
    let json = r#"{
            "number": 443,
            "url": "https://github.com/alondero/buildmesh/pull/443",
            "title": "fork PR",
            "body": "spawn on a fork",
            "state": "open",
            "draft": false,
            "head_ref": "feat/443-fork",
            "head_repo_owner": "alice",
            "head_repo_clone_url": "https://github.com/alice/buildmesh.git"
        }"#;
    let pr: GitHubPullRequest = serde_json::from_str(json).expect("fork wire shape parses");
    assert_eq!(pr.head_ref, "feat/443-fork");
    assert_eq!(pr.head_repo_owner, "alice");
    assert_eq!(
        pr.head_repo_clone_url,
        "https://github.com/alice/buildmesh.git"
    );
}

/// Partial response with no fork metadata: both fields default to "".
/// This is the #420 same-repo case — the spawn path takes the
/// `git fetch origin <head_ref>` branch.
#[test]
fn github_pull_request_wire_shape_with_missing_fork_metadata_defaults() {
    let json = r#"{
            "number": 8,
            "url": "https://github.com/x/y/pull/8",
            "title": "legacy",
            "body": "",
            "state": "open",
            "draft": false,
            "head_ref": "feat/legacy"
        }"#;
    let pr: GitHubPullRequest = serde_json::from_str(json).expect("partial wire shape parses");
    assert_eq!(pr.head_ref, "feat/legacy");
    assert_eq!(
        pr.head_repo_owner, "",
        "missing head_repo_owner defaults to empty"
    );
    assert_eq!(
        pr.head_repo_clone_url, "",
        "missing head_repo_clone_url defaults to empty"
    );
}

// ----- PrMergeabilityEntry wire shape (issue #418) ---------------------
//
// The batched endpoint returns one entry per requested PR number. Pins:
//   - `number` is present and is the `i32` JS-number shape (matches the
//     `#[ts(as = "i32")]` convention on the sibling wire types)
//   - `mergeable: null` survives the round-trip (still computing or a
//     per-PR error fallback)
//   - `mergeable_state: ""` is the safe default for missing fields

/// Full wire shape with a real merge result. Pins the JSON field names
/// and types so the regenerated `src/types/generated/PrMergeabilityEntry.ts`
/// stays in lock-step with the Rust struct. The CI drift gate
/// `git diff --exit-code src/types/generated/` catches accidental drift
/// here, but pinning it locally documents the intent.
#[test]
fn pr_mergeability_entry_wire_shape_with_clean_result() {
    let json = r#"{
            "number": 418,
            "mergeable": true,
            "mergeable_state": "clean"
        }"#;
    let entry: PrMergeabilityEntry = serde_json::from_str(json).expect("full shape parses");
    assert_eq!(entry.number, 418);
    assert_eq!(entry.mergeable, Some(true));
    assert_eq!(entry.mergeable_state, "clean");
}

/// `mergeable: null` (GitHub still computing) must round-trip as `None`,
/// not coerce to `Some(false)` — the panel renders the row in "Checking…"
/// for `None` and would falsely claim conflicts on `Some(false)`. Same
/// invariant as the existing `PrMergeability` parse test, mirrored here
/// so the batched endpoint can't drift from the per-PR contract.
#[test]
fn pr_mergeability_entry_wire_shape_with_null_mergeable() {
    let json = r#"{
            "number": 7,
            "mergeable": null,
            "mergeable_state": "unknown"
        }"#;
    let entry: PrMergeabilityEntry = serde_json::from_str(json).expect("null mergeable parses");
    assert_eq!(
        entry.mergeable, None,
        "null must stay None, not coerce to Some(false)"
    );
    assert_eq!(entry.mergeable_state, "unknown");
}

/// Per-PR error fallback — when the batched command catches an
/// individual PR's HTTP error, it returns
/// `{ mergeable: null, mergeable_state: "error: <reason>" }`. The
/// frontend renders "Checking…" for this state (matches the existing
/// per-PR failure semantics). Pin the error-state shape so a future
/// refactor that drops `mergeable_state`'s "error: " prefix is caught
/// here rather than at the UI fallback site.
#[test]
fn pr_mergeability_entry_wire_shape_with_error_state() {
    let json = r#"{
            "number": 42,
            "mergeable": null,
            "mergeable_state": "error: GitHub API error (404): Not Found"
        }"#;
    let entry: PrMergeabilityEntry = serde_json::from_str(json).expect("error state parses");
    assert_eq!(entry.mergeable, None);
    assert!(
        entry.mergeable_state.starts_with("error: "),
        "per-PR failures must carry the 'error: ' prefix so the panel can keep them in 'Checking…'"
    );
}

/// Round-trip: serialise a populated `PrMergeabilityEntry` and re-parse
/// it unchanged. Catches a class of bugs where serde rename / flatten
/// rules accidentally drop the `number` field on serialisation (which
/// would only show up at the IPC seam, not at the parse-from-fixture
/// site). The regenerated TS type derives the field name from the Rust
/// struct name, so a drop would also fail the drift gate — this test
/// makes the intent explicit.
#[test]
fn pr_mergeability_entry_round_trips_with_all_fields() {
    let original = PrMergeabilityEntry {
        number: 999,
        mergeable: Some(false),
        mergeable_state: "dirty".into(),
    };
    let json = serde_json::to_string(&original).expect("serialise");
    let parsed: PrMergeabilityEntry = serde_json::from_str(&json).expect("re-parse");
    assert_eq!(parsed.number, 999);
    assert_eq!(parsed.mergeable, Some(false));
    assert_eq!(parsed.mergeable_state, "dirty");
}

// ----- mergeability_entries helper (issue #418) -----------------------
//
// The helper is the per-PR mapping loop the batched command reuses for
// every PR. Tested with a closure probe so we can pin the mapping
// without a trait seam on GitHubClient or a wiremock dependency —
// the closure accepts the probe function as data, and the helper
// stays a pure iterator over (pr_number, probe_result).
//
// The "one token resolution" guarantee is enforced by construction
// (the command constructs `GitHubClient::new()` exactly once); these
// tests pin the per-PR mapping the helper applies on top of that
// guarantee.

/// Empty input → empty output, without ever invoking the probe. The
/// empty-pr_numbers short-circuit in `get_prs_mergeability` builds on
/// this: the public command returns `Ok(vec![])` before client
/// construction, the helper itself iterates zero times here. Pin
/// both pieces so a future refactor that drops the early return (or
/// flips the helper to call the probe once for "initialisation")
/// surfaces as a test failure rather than a slow no-op.
#[test]
fn mergeability_entries_empty_input_does_not_call_probe() {
    let calls: std::cell::RefCell<Vec<i64>> = std::cell::RefCell::new(Vec::new());
    let entries = mergeability_entries(Vec::new(), |n| {
        calls.borrow_mut().push(n);
        Ok((Some(true), "clean".into()))
    });
    assert!(entries.is_empty(), "empty input → empty output");
    assert!(
        calls.borrow().is_empty(),
        "probe must not fire on empty input"
    );
}

/// All-success path: each probe succeeds, the entry carries the
/// probed `mergeable` + `mergeable_state` AND the request's PR
/// number. The `number` round-trip is the load-bearing piece — a
/// future refactor that drops the closure arg or hard-codes the
/// number would silently desync the batched response from the
/// frontend's PR list.
#[test]
fn mergeability_entries_preserves_pr_number_on_success() {
    let entries = mergeability_entries(vec![201, 202, 204], |n| match n {
        201 => Ok((Some(true), "clean".into())),
        202 => Ok((Some(false), "dirty".into())),
        204 => Ok((None, "unknown".into())),
        _ => unreachable!(),
    });
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].number, 201);
    assert_eq!(entries[0].mergeable, Some(true));
    assert_eq!(entries[0].mergeable_state, "clean");
    assert_eq!(entries[1].number, 202);
    assert_eq!(entries[1].mergeable, Some(false));
    assert_eq!(entries[1].mergeable_state, "dirty");
    assert_eq!(entries[2].number, 204);
    assert_eq!(entries[2].mergeable, None, "still-computing stays None");
    assert_eq!(entries[2].mergeable_state, "unknown");
}

/// Per-PR HTTP failure does NOT fail the whole batch. A failing PR
/// becomes `{ mergeable: None, mergeable_state: "error: <reason>" }`
/// and the surrounding PRs still surface their real results. The
/// batched endpoint's value proposition is "transient failure on
/// one PR doesn't kill the rest"; this test pins the fallback so a
/// future refactor that re-raises `Err(_)` from the helper breaks
/// loudly here rather than regressing to the old per-PR fan-out's
/// "first failure drops the whole list" behaviour.
#[test]
fn mergeability_entries_per_pr_failure_does_not_fail_batch() {
    let entries = mergeability_entries(vec![1, 2, 3], |n| match n {
        1 => Ok((Some(true), "clean".into())),
        2 => Err(GitHubError::Api(404, "Not Found".into())),
        3 => Ok((Some(false), "dirty".into())),
        _ => unreachable!(),
    });
    assert_eq!(
        entries.len(),
        3,
        "batch must carry one entry per PR even when one fails"
    );
    // The success entries round-trip unchanged.
    assert_eq!(entries[0].number, 1);
    assert_eq!(entries[0].mergeable, Some(true));
    // The failed entry becomes the "checking" sentinel.
    assert_eq!(
        entries[1].number, 2,
        "failed entry must still carry the PR number"
    );
    assert_eq!(
        entries[1].mergeable, None,
        "failed entry must report mergeable: None"
    );
    assert!(
        entries[1].mergeable_state.starts_with("error: "),
        "failed entry must carry the 'error: ' prefix; got: {}",
        entries[1].mergeable_state
    );
    assert!(entries[1].mergeable_state.contains("404"));
    // The follow-up PR after the failure is unaffected.
    assert_eq!(entries[2].number, 3);
    assert_eq!(entries[2].mergeable, Some(false));
}

/// `GitHubError::NoToken` (auth missing) maps the same way as any
/// other per-PR error — a single probe failing for auth reasons does
/// NOT fail the whole batch. (The whole batch would already have
/// failed earlier in the public command via `GitHubClient::new()` —
/// this test exercises the helper in isolation so the per-PR
/// mapping is pinned across all error variants.)
#[test]
fn mergeability_entries_no_token_error_becomes_checking_sentinel() {
    let entries = mergeability_entries(vec![42], |_| Err(GitHubError::NoToken));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].number, 42);
    assert_eq!(entries[0].mergeable, None);
    assert!(
        entries[0].mergeable_state.starts_with("error: "),
        "NoToken must surface as 'error: ...' so the panel keeps the row in 'Checking…'"
    );
}

/// `mergeability_from_summaries` serves hits from the summary pages and
/// falls back to one direct detail request per number the pages miss
/// (older than the 100-row cap). Script: open page carries #1 only,
/// closed page is empty, then one REST detail answers #2. Cost is 3
/// requests for 2 PRs — O(pages) for the hits, one extra each only for
/// genuine misses — and request order is preserved.
#[test]
fn mergeability_from_summaries_falls_back_to_detail_past_the_cap() {
    use crate::services::github::tests::{fake_node, fake_server, Scripted};
    use std::sync::atomic::Ordering;

    let open = serde_json::Value::Array(vec![fake_node(1)]);
    let closed = serde_json::Value::Array(vec![]);
    let (base, count, handle) = fake_server(vec![
        Scripted::Page(open, false, None),
        Scripted::Page(closed, false, None),
        Scripted::Detail,
    ]);
    let client = GitHubClient::for_test(&base, "fake-token").expect("client");
    let entries = mergeability_from_summaries(&client, "acme", "demo", vec![1, 2])
        .expect("batch must not fail on a miss");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].number, 1);
    assert_eq!(entries[0].mergeable, Some(true), "summary hit");
    assert_eq!(entries[1].number, 2);
    assert_eq!(entries[1].mergeable, Some(true), "detail fallback hit");
    assert_eq!(entries[1].mergeable_state, "clean");
    handle.join().expect("server");
    assert_eq!(count.load(Ordering::SeqCst), 3);
}

/// A caller passing a duplicate number (`[1, 1]`) must resolve it twice
/// from the map: entries are read with `.get`, never drained, so the
/// second occurrence cannot trip the missing-key sentinel.
#[test]
fn mergeability_from_summaries_resolves_duplicate_numbers_twice() {
    use crate::services::github::tests::{fake_node, fake_server, Scripted};
    use std::sync::atomic::Ordering;

    let open = serde_json::Value::Array(vec![fake_node(1)]);
    let (base, count, handle) = fake_server(vec![Scripted::Page(open, false, None)]);
    let client = GitHubClient::for_test(&base, "fake-token").expect("client");
    let entries = mergeability_from_summaries(&client, "acme", "demo", vec![1, 1])
        .expect("batch must not fail on duplicates");
    assert_eq!(entries.len(), 2);
    for entry in &entries {
        assert_eq!(entry.number, 1);
        assert_eq!(entry.mergeable, Some(true));
        assert!(
            !entry.mergeable_state.starts_with("error: "),
            "duplicate must not become a sentinel; got: {}",
            entry.mergeable_state
        );
    }
    handle.join().expect("server");
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

/// Build a `TempGitRepo` and an `AgentNode` that uses a branched worktree
/// under `<root>/.claude/worktrees/<name>`. The mesh root stays on
/// `main`; the worktree is on `<name>` (matching the production
/// `add_worktree_impl` branched-mode behaviour). Caller MUST hold
/// `TempGitRepo` for the node's lifetime, otherwise Drop wipes the dir.
fn make_worktree_node() -> (TempGitRepo, String, AgentNode) {
    let tmp = TempGitRepo::new();
    let root = tmp.path().to_path_buf();
    init_repo_for_test(&root, &[("README.md", "init\n")]);

    // The .claude/worktrees/ layout is what production uses — see
    // `env::resolve_agent_path`. Keep tests faithful so the path
    // resolution helper finds the worktree the same way production does.
    let wt_dir = root.join(".claude").join("worktrees").join("agent-1");
    create_git_worktree(
        root.to_str().unwrap(),
        wt_dir.to_str().unwrap(),
        "agent-1",
        "branched",
        "HEAD",
    )
    .expect("worktree creation must succeed");

    let node = AgentNode {
        id: 1,
        path: root.to_string_lossy().into_owned(),
        worktree_name: Some("agent-1".into()),
        use_worktree: true,
        ..Default::default()
    };
    (tmp, wt_dir.to_string_lossy().into_owned(), node)
}

/// Build a non-worktree `AgentNode` — agent runs in the mesh root.
/// `path` is what the agent works in; no `.claude/worktrees/`.
fn make_root_node() -> (TempGitRepo, String, AgentNode) {
    let tmp = TempGitRepo::new();
    let root = tmp.path().to_path_buf();
    init_repo_for_test(&root, &[("README.md", "init\n")]);

    let node = AgentNode {
        id: 2,
        path: root.to_string_lossy().into_owned(),
        use_worktree: false,
        ..Default::default()
    };
    (tmp, root.to_string_lossy().into_owned(), node)
}

/// RAII guard — deletes the temp dir on drop so tests don't leave state behind.
/// The guard must be held by the TEST (not the helper) for as long as the
/// path is in use; otherwise Drop fires early and the dir is gone.
struct TempGitRepo(std::path::PathBuf);

impl TempGitRepo {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        // PID-prefixed: `NEXT_ID` is only unique inside one test process,
        // and the shard runner executes several binaries against the same
        // temp directory at once.
        let tmp =
            std::env::temp_dir().join(format!("buildmesh_pr_test_{}_{}", std::process::id(), id));
        Self(tmp)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempGitRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Init a fresh repo with a single commit on `main`. Returns `(guard, path)` —
/// caller MUST hold `guard` for as long as `path` is in use.
fn init_repo_with_commit() -> (TempGitRepo, String) {
    let tmp = TempGitRepo::new();
    fs::create_dir_all(tmp.path()).unwrap();
    let mut options = git2::RepositoryInitOptions::new();
    options.initial_head("main");
    let repo = git2::Repository::init_opts(tmp.path(), &options).unwrap();
    let sig = git2::Signature::now("test", "test@example.com").unwrap();
    fs::write(tmp.path().join("file.txt"), "content").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("file.txt")).unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
        .unwrap();
    let path = tmp.path().to_string_lossy().into_owned();
    (tmp, path)
}

/// Init a fresh repo with NO commits. `repo.head()` will error, so
/// `repo_info` should return an empty branch.
fn init_repo_unborn() -> (TempGitRepo, String) {
    let tmp = TempGitRepo::new();
    fs::create_dir_all(tmp.path()).unwrap();
    let mut options = git2::RepositoryInitOptions::new();
    options.initial_head("main");
    git2::Repository::init_opts(tmp.path(), &options).unwrap();
    let path = tmp.path().to_string_lossy().into_owned();
    (tmp, path)
}

/// Init a fresh repo + commit + set the `origin` remote to `url`.
fn init_repo_with_origin(url: &str) -> (TempGitRepo, String) {
    let (guard, path) = init_repo_with_commit();
    let repo = git2::Repository::open(&path).unwrap();
    repo.remote_set_url("origin", url).unwrap();
    (guard, path)
}

#[test]
fn repo_info_returns_empty_branch_on_unborn_head() {
    let (_guard, path) = init_repo_unborn();
    let info = repo_info(&path).expect("repo_info should succeed even with no commits");
    assert_eq!(info.branch, "", "unborn head should produce empty branch");
    assert!(
        info.remote_url.is_none(),
        "no origin configured in this test"
    );
    assert!(info.owner_repo.is_none(), "no origin → no owner_repo");
}

#[test]
fn repo_info_parses_github_origin_https() {
    let (_guard, path) = init_repo_with_origin("https://github.com/alondero/buildmesh.git");
    let info = repo_info(&path).expect("repo_info");
    assert_eq!(info.branch, "main");
    assert_eq!(
        info.owner_repo,
        Some(("alondero".to_string(), "buildmesh".to_string()))
    );
}

#[test]
fn repo_info_parses_github_origin_ssh() {
    let (_guard, path) = init_repo_with_origin("git@github.com:alondero/buildmesh.git");
    let info = repo_info(&path).expect("repo_info");
    assert_eq!(
        info.owner_repo,
        Some(("alondero".to_string(), "buildmesh".to_string()))
    );
}

#[test]
fn repo_info_returns_none_owner_repo_for_non_github_origin() {
    let (_guard, path) = init_repo_with_origin("https://gitlab.com/alondero/buildmesh.git");
    let info = repo_info(&path).expect("repo_info");
    // remote_url is still set so the call site can render a useful error
    assert!(info.remote_url.is_some());
    // but owner_repo is None — `parse_owner_repo` is GitHub-specific
    assert!(info.owner_repo.is_none());
    // and the public accessor surfaces the original error wording
    let err = info.owner_repo().unwrap_err();
    assert!(err.contains("unrecognized remote URL"), "got: {}", err);
}

// ----- github_url_for_path (Issue: View on GitHub context-menu item) -----
//
// The helper is pure and takes a path string, so the tests don't need
// a DB or Tauri runtime — they reuse the `init_repo_with_origin` /
// `init_repo_unborn` helpers above. Three cases pin the wire
// contract the frontend relies on:
//   - GitHub origin (HTTPS) → `Some("https://github.com/owner/repo")`
//   - Non-GitHub origin → `None` (the menu item is hidden)
//   - No origin at all → `None` (the menu item is hidden)
// Plus a sanity check on the `.git` suffix and SSH form, mirroring
// the existing `parse_owner_repo` tests at lines 1092–1112.

#[test]
fn github_url_for_path_https_origin() {
    let (_guard, path) = init_repo_with_origin("https://github.com/alondero/buildmesh.git");
    let url = github_url_for_path(&path).expect("github_url_for_path should succeed");
    assert_eq!(
        url,
        Some("https://github.com/alondero/buildmesh".to_string())
    );
}

#[test]
fn github_url_for_path_ssh_origin() {
    let (_guard, path) = init_repo_with_origin("git@github.com:alondero/buildmesh.git");
    let url = github_url_for_path(&path).expect("github_url_for_path should succeed");
    assert_eq!(
        url,
        Some("https://github.com/alondero/buildmesh".to_string())
    );
}

#[test]
fn github_url_for_path_no_dot_git_suffix() {
    let (_guard, path) = init_repo_with_origin("https://github.com/foo/bar");
    let url = github_url_for_path(&path).expect("github_url_for_path should succeed");
    // The .git trim lives inside `parse_owner_repo`; pin the wire
    // shape so a future refactor that re-adds the suffix is caught.
    assert_eq!(url, Some("https://github.com/foo/bar".to_string()));
}

#[test]
fn github_url_for_path_non_github_origin_is_none() {
    let (_guard, path) = init_repo_with_origin("https://gitlab.com/alondero/buildmesh.git");
    let url = github_url_for_path(&path).expect("non-github origin should not error");
    assert_eq!(
        url, None,
        "GitLab URLs must collapse to None so the menu hides the item"
    );
}

#[test]
fn github_url_for_path_no_origin_is_none() {
    let (_guard, path) = init_repo_with_commit();
    let url = github_url_for_path(&path).expect("no origin should not error");
    assert_eq!(
        url, None,
        "repos with no origin remote must collapse to None"
    );
}

/// The agent's HEAD is on the worktree's branch (NOT the mesh root's branch).
/// `repo_info` previously opened `node.path` (= mesh root) and got `main`,
/// so the PR chip was looking for `head=alondero:main` instead of the
/// agent's actual branch. The `node_git_path` helper routes to the
/// worktree directory; the chip then resolves the right branch.
#[test]
fn node_git_path_resolves_to_worktree_dir_for_worktree_nodes() {
    let (_guard, _wt_path, node) = make_worktree_node();
    let resolved = env::node_git_path(&node).host_path;
    // `resolve_agent_path` builds the path with `/` separators, so on
    // Windows we may see mixed slashes — git2 handles that fine, so we
    // assert on the canonical path components rather than the raw string.
    let canonical = std::fs::canonicalize(&resolved).expect("worktree path must exist");
    let canonical_str = canonical.to_string_lossy();
    assert!(
        canonical_str.contains(".claude")
            && canonical_str.contains("worktrees")
            && canonical_str.contains("agent-1"),
        "expected the canonical worktree path under .claude/worktrees/agent-1, got: {}",
        canonical_str,
    );
}

/// Non-worktree nodes have no worktree subdirectory — the agent works in
/// the mesh root itself, so the helper should return `node.path` unchanged.
#[test]
fn node_git_path_resolves_to_mesh_path_for_root_nodes() {
    let (_guard, _root_path, node) = make_root_node();
    let resolved = env::node_git_path(&node).host_path;
    assert_eq!(
        resolved, node.path,
        "non-worktree node must resolve to its own path"
    );
    assert!(
        !resolved.contains("worktrees"),
        "must NOT add a worktree subdir for root nodes"
    );
}

/// End-to-end: with a real worktree on branch `agent-1` and the mesh root
/// on `main`, reading `repo_info(working_path)` should give `agent-1`.
/// This is the exact bug the PR chip had: previously it read `main`.
#[test]
fn repo_info_via_working_path_returns_worktree_branch_not_root_branch() {
    let (_guard, _wt_path, node) = make_worktree_node();
    let resolved = env::node_git_path(&node).host_path;
    let info = repo_info(&resolved).expect("worktree repo must open");
    assert_eq!(
            info.branch, "agent-1",
            "PR chip looks up head=<branch>; the agent's working branch is the worktree name, not the mesh's Base Ref"
        );
    // And — for the bug regression — opening the MESH ROOT itself gives
    // its base branch, not `agent-1`. The base branch name comes from
    // libgit2's host defaults (`master` or `main`).
    let root_info = repo_info(&node.path).expect("mesh root must open");
    assert_ne!(
        root_info.branch, "agent-1",
        "sanity: mesh root is on the Base Ref; this is what the bug used to read"
    );
}

/// `get_current_branch` is the mobile REST route's backing command; for a
/// worktree node it must report the worktree's branch, not the mesh's
/// Base Ref. Same bug class as the PR chip.
#[test]
fn get_current_branch_via_working_path_returns_worktree_branch() {
    let (_guard, _wt_path, node) = make_worktree_node();
    let branch = repo_info(&env::node_git_path(&node).host_path)
        .expect("worktree repo must open")
        .branch;
    assert_eq!(
        branch, "agent-1",
        "must read the worktree's HEAD, not the mesh root's"
    );
}

/// Opt-in live test — runs only with `cargo test -- --ignored` and a valid
/// `GITHUB_TOKEN` / `gh auth login` setup. Sanity-checks that the URL
/// format parses and the JSON shape matches the (extended) PullRequest struct.
#[test]
#[ignore]
fn integration_find_open_pr_for_branch_live() {
    let client = GitHubClient::new().expect("GITHUB_TOKEN must be set");
    let pr = client
        .find_open_pr_for_branch("alondero", "buildmesh", "main")
        .expect("API call failed");
    // `main` may or may not have an open PR — both outcomes are valid signals.
    // What we're really testing is that the call shape works end-to-end.
    if let Some(p) = pr {
        assert!(p.number > 0);
        assert!(p.html_url.starts_with("https://github.com/"));
    }
}

// ----- create_pr_idempotency (issue #771) -----------------------------
//
// The optimistic-with-422-recovery pattern that closes the duplicate-PR
// gap #762 opened (180s write timeout → slow POST succeeds server-side,
// client times out, retry duplicates the PR). Tests drive the PRODUCTION
// boundary (`create_pr_for_mesh_blocking_with_client` and
// `create_pr_blocking_with_client`) with a real temp repo + a `for_test`
// `GitHubClient` pointed at a fake server, so wire-level expectations
// (request method, URL encoding, status code, body shape) are pinned
// against the same code path the desktop/mobile frontends exercise.
//
// Covered cases:
//   - duplicate-create recovery       → POST 422 → GET existing → return URL
//   - happy path (no duplicate)       → POST 201 → return new URL
//   - non-422 error (403, 404, 5xx)   → propagate verbatim, no recovery GET
//   - URL encoding on the recovery GET → `:` and `/` in `owner:branch`
//                                         must percent-encode
//   - structured 422 detection        → "already exists" only via parsed
//                                       `errors[].message`, not raw body
//   - fork head (Finding 1)           → qualified `fork_user:branch` is
//                                       passed through verbatim, not
//                                       double-prefixed
//   - session_id path (Finding 6)     → `create_pr_blocking_with_client`
//                                       drives the worktree/DB branch

/// PR JSON for the existing-PR recovery fixture. Only `number` and
/// `html_url` are required; `PullRequest` carries `#[serde(default)]`
/// on every other field.
fn existing_pr_json(number: i64, html_url: &str) -> serde_json::Value {
    serde_json::json!([{
        "number": number,
        "html_url": html_url,
        "title": format!("fix: dup-PR guard (#{})", number),
        "state": "open",
        "draft": false,
    }])
}

/// PR JSON for the happy-path create fixture (POST 201 body).
fn created_pr_json(number: i64, html_url: &str) -> serde_json::Value {
    serde_json::json!({
        "number": number,
        "html_url": html_url,
        "title": "fix: dup-PR guard",
        "body": "body",
        "state": "open",
        "draft": false,
    })
}

/// Init a temp repo with one commit, `origin` set to the given GitHub
/// URL, and HEAD checked out to `branch`. Returns `(guard, path)` —
/// caller MUST hold `guard` for the temp dir's lifetime.
fn init_repo_on_branch(url: &str, branch: &str) -> (TempGitRepo, String) {
    let (guard, path) = init_repo_with_origin(url);
    let repo = git2::Repository::open(&path).expect("open fresh repo");
    let head_commit = repo
        .head()
        .expect("head exists after init_repo_with_origin")
        .peel_to_commit()
        .expect("head is a commit");
    repo.branch(branch, &head_commit, true)
        .expect("create branch");
    let refname = format!("refs/heads/{branch}");
    repo.set_head(&refname).expect("set HEAD to branch");
    repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
        .expect("checkout branch");
    (guard, path)
}

/// Pin the issue's exact failure mode: an open PR already exists, the
/// user retries, and the production boundary must return the existing
/// URL via the 422-recovery path. End-to-end (real temp repo →
/// `repo_info` → `info.owner_repo()` → `client.create_pull_request_idempotent`).
/// Two requests fire (POST 422 + GET recovery); the load-bearing
/// assertion is `count == 2` — a regression that took the optimistic
/// path but never recovered would hit `count == 1` and short-circuit
/// to a confusing error.
#[test]
fn create_pr_for_mesh_recovers_from_duplicate_create_422() {
    use std::sync::atomic::Ordering;

    let (_guard, mesh_path) =
        init_repo_on_branch("https://github.com/test-owner/test-repo.git", "feat/771");
    let existing = existing_pr_json(771, "https://github.com/test-owner/test-repo/pull/771");
    // Optimistic path: POST first, GitHub answers 422 ("already exists"),
    // we recover by GET'ing the existing PR.
    let (base, count, handle) = fake_server(vec![
            Scripted::CreatePrConflict(
                r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists for test-owner:feat/771."}]}"#.to_string(),
            ),
            // `expected_head` is the percent-encoded form the client must
            // produce: `test-owner:feat/771` → `test-owner%3Afeat%2F771`.
            Scripted::ListPulls {
                body: existing,
                expected_head: "test-owner%3Afeat%2F771".to_string(),
            },
        ]);
    let client = GitHubClient::for_test(&base, "fake-token").expect("client");

    let url = create_pr_for_mesh_blocking_with_client(
        &client,
        &mesh_path,
        "new title",
        "new body",
        "main",
    )
    .expect("must return the existing PR's URL on 422 recovery");

    assert_eq!(
        url, "https://github.com/test-owner/test-repo/pull/771",
        "must return the EXISTING PR's html_url, not the freshly-attempted one"
    );
    handle.join().expect("server");
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "POST (422) + GET (recovery) — count == 1 means the recovery GET was skipped"
    );
}

/// Happy path: no duplicate → POST returns 201 → return new URL. ONE
/// round trip total (no recovery GET). A regression that always
/// pre-checks would inflate this to 2 requests, paying latency for
/// nothing on the 99% path.
#[test]
fn create_pr_for_mesh_happy_path_no_duplicate() {
    use std::sync::atomic::Ordering;

    let (_guard, mesh_path) =
        init_repo_on_branch("https://github.com/test-owner/test-repo.git", "feat/771");
    let created = created_pr_json(772, "https://github.com/test-owner/test-repo/pull/772");
    let (base, count, handle) = fake_server(vec![Scripted::CreatePullRequest(created)]);
    let client = GitHubClient::for_test(&base, "fake-token").expect("client");

    let url = create_pr_for_mesh_blocking_with_client(&client, &mesh_path, "title", "body", "main")
        .expect("creates new PR");

    assert_eq!(
        url, "https://github.com/test-owner/test-repo/pull/772",
        "must return the CREATED PR's html_url"
    );
    handle.join().expect("server");
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "exactly one POST — the optimistic path must not pre-check on the happy case"
    );
}

/// Negative control: a non-422 error (e.g. 403 forbidden, 422 with a
/// different shape, 500 server error) must NOT trigger the recovery
/// path. If it did, a permission error would silently mask as a
/// successful create, and a 422 for a missing-branch would silently
/// mask as a successful create.
#[test]
fn create_pr_for_mesh_propagates_non_422_errors() {
    use std::sync::atomic::Ordering;

    let (_guard, mesh_path) =
        init_repo_on_branch("https://github.com/test-owner/test-repo.git", "feat/771");
    let (base, count, handle) = fake_server(vec![Scripted::CreatePrError(
        403,
        r#"{"message":"Must have admin rights"}"#.to_string(),
    )]);
    let client = GitHubClient::for_test(&base, "fake-token").expect("client");

    let err = create_pr_for_mesh_blocking_with_client(&client, &mesh_path, "title", "body", "main")
        .expect_err("403 must propagate, not silently recover");

    assert!(
        err.contains("403") || err.contains("admin"),
        "error must surface the real GitHub diagnostic, got: {err}"
    );
    handle.join().expect("server");
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "exactly one POST — non-422 errors must not trigger a recovery GET"
    );
}

/// `same_branch` guard: production refuses to POST when the agent's
/// current branch equals the base branch (would be a "main → main"
/// PR). The optimistic helper doesn't need to fire — the guard fires
/// first. Without this test, a regression that dropped the
/// branch==base_branch check would only surface when a user
/// accidentally ran Create PR with no feature branch checked out.
#[test]
fn create_pr_for_mesh_refuses_same_branch_as_base() {
    // init_repo_with_origin lands HEAD on the default branch; pass
    // that as the base_branch and watch the guard reject.
    let (_guard, mesh_path) = init_repo_with_origin("https://github.com/test-owner/test-repo.git");
    let (base, _count, _handle) = fake_server(vec![]);
    let client = GitHubClient::for_test(&base, "fake-token").expect("client");

    // Detect the default branch name — git2's `Repository::head_branch_name`
    // isn't always set; fall back to inspecting the ref tree.
    let repo = git2::Repository::open(&mesh_path).expect("open");
    let head_name = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().ok().map(str::to_string))
        .unwrap_or_else(|| "main".to_string());

    let err = create_pr_for_mesh_blocking_with_client(
        &client, &mesh_path, "title", "body", &head_name, // base == head → guard rejects
    )
    .expect_err("must refuse same-branch create");

    assert!(
        err.contains("nothing to compare"),
        "error must name the same-branch violation, got: {err}"
    );
}

/// URL-encoding pin for the recovery GET. The fake server's
/// `Scripted::ListPulls { expected_head, .. }` asserts the request
// line contains the EXACT percent-encoded head substring. A
/// regression that drops encoding (or re-orders the encoding rules)
/// would corrupt branches like `feat/771` → `feat/771` (unescaped
/// `/` in a query value is technically tolerated by some HTTP
/// parsers but not by `reqwest`'s strict builder) and fail this
/// assertion. The branch name uses both `:` (owner separator) and
/// `/` (nested ref) to exercise the two most common special chars.
#[test]
fn create_pr_for_mesh_url_encodes_recovery_head_param() {
    let (_guard, mesh_path) = init_repo_on_branch(
        "https://github.com/test-owner/test-repo.git",
        "feat/with/slashes",
    );
    let existing = existing_pr_json(773, "https://github.com/test-owner/test-repo/pull/773");
    // Both `:` (after owner) and `/` (in branch) must percent-encode:
    // `test-owner:feat/with/slashes` → `test-owner%3Afeat%2Fwith%2Fslashes`.
    let (base, _count, handle) = fake_server(vec![
            Scripted::CreatePrConflict(
                r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists for test-owner:feat/with/slashes."}]}"#.to_string(),
            ),
            Scripted::ListPulls {
                body: existing,
                expected_head: "test-owner%3Afeat%2Fwith%2Fslashes".to_string(),
            },
        ]);
    let client = GitHubClient::for_test(&base, "fake-token").expect("client");

    // The fake server's path-must-contain assertion fires inside the
    // request handler — if the client didn't produce the exact encoded
    // head, `handle.join()` would never return (or would panic). So
    // the success of this test IS the URL-encoding pin.
    let _url =
        create_pr_for_mesh_blocking_with_client(&client, &mesh_path, "title", "body", "main")
            .expect("recovery GET must use percent-encoded head");
    handle.join().expect("server");
}

// ----- session_id path (Finding 6) ----------------------------------
//
// `create_pr_blocking_with_client` is the documented test seam for the
// production `create_pr` command. The PR only exercised the
// `create_pr_for_mesh_blocking_with_client` boundary; this drives the
// full DB → worktree → GitHub path so a regression in the session
// translation is caught here rather than at e2e time.

/// Install this test's private database and hand back the guard the test
/// body holds for its remaining statements.
fn ensure_pr_blocking_db() -> crate::db::test_support::IsolatedDbGuard {
    crate::db::test_support::isolated()
}

/// Insert a mesh row pointing at `path` and an agent_node row in
/// `use_worktree` mode, then create the worktree the production path
/// expects (`<root>/.claude/worktrees/<name>`). Returns
/// `(tmp, session_id, branch)` — caller MUST hold `tmp` for the
/// node's lifetime.
fn make_session_node(mesh_name: &str, origin_url: &str, branch: &str) -> (TempGitRepo, i64) {
    let tmp = TempGitRepo::new();
    let root = tmp.path().to_path_buf();
    // Init a real git repo + commit + ensure HEAD sits on a named
    // `main` branch. `init_repo_for_test` commits via `Some("HEAD")`
    // on an unborn HEAD; the resulting ref shape depends on git2
    // version (sometimes symbolic to `refs/heads/<default>`, sometimes
    // a direct SHA ref). Belt-and-suspenders: read whatever `head()`
    // returns, then explicitly create+checkout a `main` branch
    // anchored at the commit so the worktree below has a real
    // branch ref to fork from.
    let repo = init_repo_for_test(&root, &[("README.md", "init\n")]);
    let head = repo.head().expect("head exists after commit");
    let head_commit = head.peel_to_commit().expect("head is a commit");
    // If HEAD is already on `main`, leave it. Otherwise create
    // `main` and re-anchor HEAD to it.
    let head_is_main = head.shorthand().map(|s| s == "main").unwrap_or(false);
    if !head_is_main {
        repo.set_head_detached(head_commit.id()).expect("detach");
        repo.branch("main", &head_commit, true)
            .expect("create main");
    }
    repo.set_head("refs/heads/main").expect("set HEAD to main");
    repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
        .expect("checkout main");
    // Set the origin remote — required so `repo_info` can resolve
    // owner/repo for the GitHub call. Set on the main repo; the
    // worktree shares the same `.git` and inherits the remote.
    repo.remote_set_url("origin", origin_url)
        .expect("set origin");
    // Create the branched worktree at the production path.
    let wt_dir = root.join(".claude").join("worktrees").join("agent-1");
    create_git_worktree(
        root.to_str().unwrap(),
        wt_dir.to_str().unwrap(),
        branch,
        "branched", // worktree_mode
        "main",
    )
    .expect("worktree creation must succeed");
    // Insert a mesh + node row so `db::get_agent_node_by_id` resolves.
    let mesh = crate::db::create_mesh(mesh_name, root.to_str().unwrap()).expect("create_mesh");
    let node = crate::db::create_agent_node(
        mesh.id,
        "agent-1",
        root.to_str().unwrap(),
        "main", // base_branch
        crate::models::EnvType::Windows,
        "claude",
        Some("agent-1"),
        None,
        None,
        None,
        true,
        None,
        None,
        None,
    )
    .expect("create_agent_node");
    (tmp, node.id)
}

/// Pin the full session_id path end-to-end. A duplicate-create 422
/// from GitHub must be recovered via `find_open_pr_for_branch` and
/// return the existing PR's URL. The test proves:
///   - the DB row is found
///   - the worktree branch is read (`feat/771` is the agent's branch,
///     NOT `main`)
///   - the recovery GET uses the right `head=` parameter
///
/// Without a test here, a regression that always used `node.branch`
/// (the base, "main") would only surface in the production app.
#[test]
fn create_pr_blocking_recovers_from_duplicate_create_422() {
    use std::sync::atomic::Ordering;

    let _db = ensure_pr_blocking_db();

    let (_tmp, session_id) = make_session_node(
        "pr-blocking-mesh",
        "https://github.com/test-owner/test-repo.git",
        "feat/771",
    );
    let existing = existing_pr_json(771, "https://github.com/test-owner/test-repo/pull/771");
    let (base, count, handle) = fake_server(vec![
            Scripted::CreatePrConflict(
                r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists for test-owner:feat/771."}]}"#.to_string(),
            ),
            Scripted::ListPulls {
                body: existing,
                expected_head: "test-owner%3Afeat%2F771".to_string(),
            },
        ]);
    let client = GitHubClient::for_test(&base, "fake-token").expect("client");

    let url = create_pr_blocking_with_client(&client, session_id, "new title", "new body")
        .expect("must return the existing PR's URL on 422 recovery");

    assert_eq!(
        url, "https://github.com/test-owner/test-repo/pull/771",
        "must return the EXISTING PR's html_url, not the freshly-attempted one"
    );
    handle.join().expect("server");
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "POST (422) + GET (recovery) — count == 1 means the recovery GET was skipped"
    );
}

// ----- structured 422 detection (Finding 2) -------------------------

/// A 422 whose body contains the literal phrase "already exists" in a
/// non-error field (e.g. an echoed title) must NOT trigger the
/// recovery path. The previous substring-on-raw-body check would
/// falsely fire here; the structured `errors[].message` parse must
/// distinguish real duplicate-create errors from validation echoes.
#[test]
fn create_pr_for_mesh_does_not_recover_on_unrelated_422() {
    use std::sync::atomic::Ordering;

    let (_guard, mesh_path) =
        init_repo_on_branch("https://github.com/test-owner/test-repo.git", "feat/echo");
    // 422 with a body that contains "already exists" only as a
    // echoed input field — GitHub's "Validation Failed" envelope
    // names a different field ("No commits between main and feat/echo")
    // and has no "already exists" in `errors[].message`.
    let (base, count, handle) = fake_server(vec![Scripted::CreatePrError(
            422,
            r#"{"message":"Validation Failed","errors":[{"message":"No commits between main and feat/echo."}]}"#.to_string(),
        )]);
    let client = GitHubClient::for_test(&base, "fake-token").expect("client");

    let err = create_pr_for_mesh_blocking_with_client(
        &client,
        &mesh_path,
        "title with 'already exists' in it",
        "body",
        "main",
    )
    .expect_err("422 without duplicate-create message must propagate");

    assert!(
        err.contains("422") || err.contains("No commits"),
        "error must surface the real GitHub diagnostic, got: {err}"
    );
    handle.join().expect("server");
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "exactly one POST — non-duplicate 422s must not trigger a recovery GET"
    );
}

// ----- fork head handling (Finding 1) -------------------------------

/// `find_open_pr_for_branch` must NOT prepend `owner:` when the
/// caller has already supplied a qualified `head` (e.g. a fork
/// PR's `fork_user:branch`). The previous implementation
/// unconditionally formatted `"{owner}:{branch}"`, which on a fork
/// produced `upstream:fork_user:branch` — never matched anything,
/// and the 422 recovery then surfaced the GitHub 422 as opaque
/// error rather than the existing PR's URL.
///
/// The fake server asserts the request line contains the EXACT
/// qualified head (`fork-user%3Afeat%2Ffork`) and not
/// `upstream-owner%3Afork-user%3Afeat%2Ffork`.
#[test]
fn find_open_pr_for_branch_does_not_double_prefix_fork_head() {
    use std::sync::atomic::Ordering;

    // We exercise the seam directly so the assertion is at the
    // exact layer the bug lived in — `find_open_pr_for_branch`
    // itself, not its `create_pull_request_idempotent` caller.
    let (base, count, handle) = fake_server(vec![Scripted::ListPulls {
        body: existing_pr_json(42, "https://github.com/upstream-owner/repo/pull/42"),
        // The exact percent-encoded form the client must produce.
        // NO `upstream-owner:` prefix — the head is pre-qualified.
        expected_head: "fork-user%3Afeat%2Ffork".to_string(),
    }]);
    let client = GitHubClient::for_test(&base, "fake-token").expect("client");

    let pr = client
        .find_open_pr_for_branch("upstream-owner", "repo", "fork-user:feat/fork")
        .expect("must succeed")
        .expect("must find the existing fork PR");

    assert_eq!(pr.number, 42);
    handle.join().expect("server");
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

// ----- merge-method vocabulary (merge-strategy dropdown) -----------

#[test]
fn normalise_merge_method_accepts_the_three_github_verbs() {
    assert_eq!(normalise_merge_method(Some("squash".into())), "squash");
    assert_eq!(normalise_merge_method(Some("merge".into())), "merge");
    assert_eq!(normalise_merge_method(Some("rebase".into())), "rebase");
}

#[test]
fn normalise_merge_method_falls_back_to_squash_for_absent_or_unknown() {
    // Older clients (PrPill menu, mobile HTTP route) don't send a
    // method at all; an unrecognised one must never reach GitHub's
    // API verbatim. Both collapse to the historical squash behaviour.
    assert_eq!(normalise_merge_method(None), "squash");
    assert_eq!(normalise_merge_method(Some("octopus".into())), "squash");
    assert_eq!(normalise_merge_method(Some(String::new())), "squash");
}
// ----- node-scoped create-PR (issue #2024 rank 4, issue #1567) ------
//
// The bug: the mobile create-PR request carried only a mesh id, and the
// route resolved `mesh.path`. For a mesh whose root sits on `main` that
// published `main -> main`; for a root sitting on an unrelated feature it
// published the wrong branch entirely — while the sheet displayed the
// NODE's branch.
//
// These tests drive the real production resolver against a real git repo
// (mesh root + linked agent worktree), so a regression to mesh-root
// resolution shows up as a wrong `head_branch`, not as a passing mock.

/// Mesh root on `root_branch`, agent worktree on `node_branch`, mesh
/// `base_ref` as given. Returns `(guard, mesh_id, node_id)`; the caller
/// MUST hold `guard` — it owns both repositories.
///
/// The worktree is cut from `main` BEFORE the root is moved onto
/// `root_branch`, which is the production order: an agent branches from
/// the base ref, not from wherever the mesh root happens to be parked.
#[allow(clippy::too_many_arguments)]
fn make_mesh_root_and_node(
    origin_url: &str,
    root_branch: &str,
    node_branch: &str,
    base_ref: &str,
) -> (TempGitRepo, i64, i64) {
    let tmp = TempGitRepo::new();
    let root = tmp.path().to_path_buf();
    let repo = init_repo_for_test(&root, &[("README.md", "init\n")]);
    let head = repo.head().expect("head exists after commit");
    let head_commit = head.peel_to_commit().expect("head is a commit");
    // Anchor a real `main` branch. `init_repo_for_test` commits via
    // `Some("HEAD")` on an unborn HEAD, and the resulting ref shape is
    // git2-version dependent, so `main` may or may not already exist.
    let head_is_main = head.shorthand().map(|s| s == "main").unwrap_or(false);
    if !head_is_main {
        repo.set_head_detached(head_commit.id()).expect("detach");
        repo.branch("main", &head_commit, true)
            .expect("create main");
    }
    repo.set_head("refs/heads/main").expect("set HEAD to main");
    repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
        .expect("checkout main");
    repo.remote_set_url("origin", origin_url)
        .expect("set origin");

    let wt_dir = root.join(".claude").join("worktrees").join("agent-1");
    create_git_worktree(
        root.to_str().unwrap(),
        wt_dir.to_str().unwrap(),
        node_branch,
        "branched",
        "main",
    )
    .expect("worktree creation must succeed");

    // Park the mesh root on its own branch. This is the state that used
    // to decide what the PR published.
    if root_branch != "main" {
        repo.set_head_detached(head_commit.id()).expect("detach");
        repo.branch(root_branch, &head_commit, true)
            .expect("create root branch");
        repo.set_head(&format!("refs/heads/{root_branch}"))
            .expect("set root HEAD");
        repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
            .expect("checkout root branch");
    }

    let mesh = crate::db::create_mesh_with_base_ref_and_worktree(
        "pr-source-mesh",
        root.to_str().unwrap(),
        base_ref,
        true,
    )
    .expect("create_mesh_with_base_ref_and_worktree");
    let node = crate::db::create_agent_node(
        mesh.id,
        "agent-1",
        root.to_str().unwrap(),
        "main",
        crate::models::EnvType::Windows,
        "claude",
        Some("agent-1"),
        None,
        None,
        None,
        true,
        None,
        None,
        None,
    )
    .expect("create_agent_node");
    (tmp, mesh.id, node.id)
}

#[test]
fn pr_source_uses_node_worktree_when_mesh_root_is_on_main() {
    let _db = ensure_pr_blocking_db();
    let (_tmp, mesh_id, node_id) = make_mesh_root_and_node(
        "https://github.com/test-owner/test-repo.git",
        "main",
        "agent/fix-x",
        "origin/main",
    );

    let resolved = resolve_pr_source_for_node(mesh_id, node_id).expect("resolution succeeds");

    assert_eq!(
        resolved.source.head_branch, "agent/fix-x",
        "the PR source is the agent worktree's branch, never the mesh root's `main`"
    );
    assert_eq!(resolved.source.base_branch, "main");
    assert_eq!(
        resolved.owner_repo,
        Some(("test-owner".to_string(), "test-repo".to_string())),
        "the node worktree inherits the root repo's origin remote"
    );
}

#[test]
fn pr_source_uses_node_worktree_when_mesh_root_is_on_another_feature() {
    let _db = ensure_pr_blocking_db();
    let (_tmp, mesh_id, node_id) = make_mesh_root_and_node(
        "https://github.com/test-owner/test-repo.git",
        "unrelated/other",
        "agent/fix-x",
        "origin/main",
    );

    let resolved = resolve_pr_source_for_node(mesh_id, node_id).expect("resolution succeeds");

    assert_eq!(
        resolved.source.head_branch, "agent/fix-x",
        "a mesh root parked on `unrelated/other` must not become the PR source"
    );
    assert_eq!(resolved.source.base_branch, "main");
}

#[test]
fn pr_source_derives_base_from_mesh_base_ref_not_a_hardcoded_main() {
    let _db = ensure_pr_blocking_db();
    let (_tmp, mesh_id, node_id) = make_mesh_root_and_node(
        "https://github.com/test-owner/test-repo.git",
        "trunk",
        "agent/fix-x",
        "origin/trunk",
    );

    let resolved = resolve_pr_source_for_node(mesh_id, node_id).expect("resolution succeeds");

    assert_eq!(
        resolved.source.base_branch, "trunk",
        "a mesh configured against origin/trunk must not be published into main"
    );
    assert_eq!(resolved.source.head_branch, "agent/fix-x");
}

#[test]
fn pr_source_rejects_node_belonging_to_another_mesh() {
    let _db = ensure_pr_blocking_db();
    let (_tmp_a, mesh_a, node_in_a) = make_mesh_root_and_node(
        "https://github.com/test-owner/test-repo.git",
        "main",
        "agent/fix-x",
        "origin/main",
    );
    let (_tmp_b, mesh_b, _node_in_b) = make_mesh_root_and_node(
        "https://github.com/test-owner/other-repo.git",
        "main",
        "agent/fix-y",
        "origin/main",
    );

    let err = resolve_pr_source_for_node(mesh_b, node_in_a).expect_err("ownership must fail");
    // The status is the contract the HTTP route depends on: ownership is
    // the one case that is genuinely a 403 (#2190 review).
    assert_eq!(err.status(), "403 Forbidden");
    assert!(matches!(err, PrSourceError::NotOwned { .. }));
    assert!(
        err.to_string().contains("does not belong to mesh"),
        "error must name the ownership violation, got: {err}"
    );
    assert!(mesh_a != mesh_b, "the two meshes must actually differ");
}

#[test]
fn pr_source_rejects_node_branch_equal_to_the_base_ref() {
    let _db = ensure_pr_blocking_db();
    // The node worktree sits on `agent/fix-x` and the mesh's base_ref
    // points at that same branch — the "nothing to compare" case a
    // misconfigured mesh produces. It must fail before GitHub is ever
    // called rather than opening a self-targeting PR. (Pointing the
    // base at `main` instead would fail earlier, in worktree creation:
    // the root already owns `main`.)
    let (_tmp, mesh_id, node_id) = make_mesh_root_and_node(
        "https://github.com/test-owner/test-repo.git",
        "main",
        "agent/fix-x",
        "origin/agent/fix-x",
    );

    let err = resolve_pr_source_for_node(mesh_id, node_id).expect_err("same-branch must fail");
    // A validation failure, NOT an auth failure. Reporting it as 403 was
    // a live bug: the mobile client treats 403 as an expired session and
    // wipes the credentials (#2190 review).
    assert_eq!(err.status(), "422 Unprocessable Entity");
    assert!(matches!(err, PrSourceError::SameBranch { .. }));
    assert!(
        err.to_string().contains("nothing to compare"),
        "error must explain the same-branch case, got: {err}"
    );
}

#[test]
fn local_base_branch_strips_a_configured_remote_but_not_a_real_slash_branch() {
    // The prefix is stripped only when the repository actually has that
    // remote. There is no syntax that tells `origin/main` from
    // `feature/x`, so character-shape guessing would rewrite the latter
    // into `x` and target a branch that does not exist.
    let remotes = vec!["origin".to_string(), "upstream".to_string()];
    // The remote qualifier must go: GitHub's `base=` wants a branch name.
    assert_eq!(local_base_branch("origin/main", &remotes), "main");
    assert_eq!(local_base_branch("origin/trunk", &remotes), "trunk");
    assert_eq!(local_base_branch("upstream/release", &remotes), "release");
    assert_eq!(local_base_branch("  origin/main  ", &remotes), "main");
    // Already-local, or a branch whose own name contains a slash.
    assert_eq!(local_base_branch("main", &remotes), "main");
    assert_eq!(local_base_branch("feature/x", &remotes), "feature/x");
    // A prefix that is not a configured remote is left alone.
    assert_eq!(local_base_branch("origin/main", &[]), "origin/main");
    assert_eq!(local_base_branch("origin/", &remotes), "origin/");
    assert_eq!(local_base_branch("", &remotes), "");
}

/// The wire-level proof: the `head=` GitHub is asked about is the NODE's
/// branch. The scripted fake asserts the exact percent-encoded head in
/// the request line, so a regression to mesh-root resolution fails here
/// instead of quietly publishing the root's branch.
#[test]
fn create_pr_for_node_source_asks_github_about_the_node_branch() {
    use std::sync::atomic::Ordering;

    let _db = ensure_pr_blocking_db();
    let (_tmp, mesh_id, node_id) = make_mesh_root_and_node(
        "https://github.com/test-owner/test-repo.git",
        "main",
        "agent/fix-x",
        "origin/main",
    );
    let existing = existing_pr_json(900, "https://github.com/test-owner/test-repo/pull/900");
    let (base, count, handle) = fake_server(vec![
            Scripted::CreatePrConflict(
                r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists for test-owner:agent/fix-x."}]}"#.to_string(),
            ),
            Scripted::ListPulls {
                body: existing,
                // `:` -> %3A, `/` -> %2F. The fake asserts this exact value
                // appears in the recovery GET's request line.
                expected_head: "test-owner%3Aagent%2Ffix-x".to_string(),
            },
        ]);
    let client = GitHubClient::for_test(&base, "fake-token").expect("client");

    let result = create_pr_for_node_source_blocking_with_client(
        &client, mesh_id, node_id, "title", "body", None, None,
    )
    .expect("duplicate-create recovery must succeed");

    assert_eq!(
        result.url,
        "https://github.com/test-owner/test-repo/pull/900"
    );
    assert_eq!(
        result.head_branch, "agent/fix-x",
        "the echoed source must be the node worktree's branch"
    );
    assert_eq!(result.base_branch, "main");
    handle.join().expect("server");
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

/// Issue #2333: creating a PR from the phone with an empty description.
/// GitHub's REST schema types the description as `string | null`, so the
/// `POST /pulls` 201 answers `"body": null` — and the deserialiser failed
/// the whole response with `HTTP error: error decoding response body`
/// even though GitHub had created the PR, leaving the sheet in its error
/// state with no URL to show. The fixture is GitHub's literal shape; the
/// load-bearing assertion is that the acknowledged URL comes back.
#[test]
fn create_pr_for_node_source_acknowledges_a_pr_created_without_a_description() {
    use std::sync::atomic::Ordering;

    let _db = ensure_pr_blocking_db();
    let (_tmp, mesh_id, node_id) = make_mesh_root_and_node(
        "https://github.com/test-owner/test-repo.git",
        "main",
        "agent/fix-x",
        "origin/main",
    );
    let created = serde_json::json!({
        "number": 901,
        "html_url": "https://github.com/test-owner/test-repo/pull/901",
        "title": "fix(github): acknowledge pull requests created with an empty description",
        "body": null,
        "state": "open",
        "draft": false,
        "merged": false,
        "head": {"ref": "agent/fix-x", "sha": "abc123", "repo": null},
        "base": {"ref": "main"}
    });
    let (base, count, handle) = fake_server(vec![Scripted::CreatePullRequest(created)]);
    let client = GitHubClient::for_test(&base, "fake-token").expect("client");

    let result = create_pr_for_node_source_blocking_with_client(
        &client,
        mesh_id,
        node_id,
        "fix(github): acknowledge pull requests created with an empty description",
        // The description the user left empty.
        "",
        None,
        None,
    )
    .expect("a PR GitHub created must be acknowledged, not reported as a decode failure");

    assert_eq!(
        result.url,
        "https://github.com/test-owner/test-repo/pull/901"
    );
    assert_eq!(result.head_branch, "agent/fix-x");
    assert_eq!(result.base_branch, "main");
    handle.join().expect("server");
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "exactly one POST — the happy path must not pre-check"
    );
}

/// The retry half of #2333: the create response was interrupted, so the
/// phone submits again, GitHub answers 422 "already exists", and the
/// recovery read returns the PR the first attempt created. That PR carries
/// the same empty description, so its `"body": null` must not fail the
/// recovery read the same way — that is what left the user unable to
/// acknowledge a PR that already existed.
#[test]
fn create_pr_for_node_source_recovers_a_duplicate_that_has_no_description() {
    use std::sync::atomic::Ordering;

    let _db = ensure_pr_blocking_db();
    let (_tmp, mesh_id, node_id) = make_mesh_root_and_node(
        "https://github.com/test-owner/test-repo.git",
        "main",
        "agent/fix-x",
        "origin/main",
    );
    let existing = serde_json::json!([{
        "number": 902,
        "html_url": "https://github.com/test-owner/test-repo/pull/902",
        "title": "fix(github): acknowledge pull requests created with an empty description",
        "body": null,
        "state": "open",
        "draft": false,
        "head": {"ref": "agent/fix-x", "sha": "abc123"}
    }]);
    let (base, count, handle) = fake_server(vec![
            Scripted::CreatePrConflict(
                r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists for test-owner:agent/fix-x."}]}"#.to_string(),
            ),
            Scripted::ListPulls {
                body: existing,
                expected_head: "test-owner%3Aagent%2Ffix-x".to_string(),
            },
        ]);
    let client = GitHubClient::for_test(&base, "fake-token").expect("client");

    let result = create_pr_for_node_source_blocking_with_client(
        &client, mesh_id, node_id, "title", "", None, None,
    )
    .expect("the existing PR must be recovered rather than failing to decode it");

    assert_eq!(
        result.url,
        "https://github.com/test-owner/test-repo/pull/902"
    );
    assert_eq!(result.head_branch, "agent/fix-x");
    handle.join().expect("server");
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "POST (422) + GET (recovery) — count == 1 means the recovery GET was skipped"
    );
}

#[test]
fn create_pr_for_node_source_rejects_a_stale_expected_head() {
    let _db = ensure_pr_blocking_db();
    let (_tmp, mesh_id, node_id) = make_mesh_root_and_node(
        "https://github.com/test-owner/test-repo.git",
        "main",
        "agent/fix-x",
        "origin/main",
    );
    let client = GitHubClient::for_test("http://127.0.0.1:1", "fake-token").expect("client");

    let err = create_pr_for_node_source_blocking_with_client(
        &client,
        mesh_id,
        node_id,
        "title",
        "body",
        None,
        Some("agent/moved-on"),
    )
    .expect_err("a worktree that moved since preview must fail");

    assert!(
        err.to_string().contains("agent/fix-x") && err.to_string().contains("agent/moved-on"),
        "error must name both the actual and the previewed branch, got: {err}"
    );
    // The worktree moved after preview: a stale client view, not an
    // authorization problem (#2190 review).
    assert_eq!(err.status(), "422 Unprocessable Entity");
    assert!(matches!(err, PrSourceError::StaleHead { .. }));
}

#[test]
fn create_pr_for_node_source_rejects_a_base_equal_to_the_node_branch() {
    let _db = ensure_pr_blocking_db();
    let (_tmp, mesh_id, node_id) = make_mesh_root_and_node(
        "https://github.com/test-owner/test-repo.git",
        "main",
        "agent/fix-x",
        "origin/main",
    );
    let client = GitHubClient::for_test("http://127.0.0.1:1", "fake-token").expect("client");

    let err = create_pr_for_node_source_blocking_with_client(
        &client,
        mesh_id,
        node_id,
        "title",
        "body",
        Some("agent/fix-x"),
        None,
    )
    .expect_err("base == source must fail");

    assert!(
        err.to_string().contains("nothing to compare"),
        "error must explain the same-branch case, got: {err}"
    );
    assert_eq!(err.status(), "422 Unprocessable Entity");
}

#[test]
fn create_pr_for_node_source_rejects_cross_mesh_node_before_calling_github() {
    let _db = ensure_pr_blocking_db();
    let (_tmp_a, _mesh_a, node_in_a) = make_mesh_root_and_node(
        "https://github.com/test-owner/test-repo.git",
        "main",
        "agent/fix-x",
        "origin/main",
    );
    let (_tmp_b, mesh_b, _node_in_b) = make_mesh_root_and_node(
        "https://github.com/test-owner/other-repo.git",
        "main",
        "agent/fix-y",
        "origin/main",
    );
    // Port 1 refuses connections, so if ownership were not checked first
    // this test would fail with a connection error instead of the
    // ownership message.
    let client = GitHubClient::for_test("http://127.0.0.1:1", "fake-token").expect("client");

    let err = create_pr_for_node_source_blocking_with_client(
        &client, mesh_b, node_in_a, "title", "body", None, None,
    )
    .expect_err("cross-mesh node must be rejected");

    assert!(
        err.to_string().contains("does not belong to mesh"),
        "ownership must be checked before any GitHub call, got: {err}"
    );
    assert_eq!(err.status(), "403 Forbidden");
}

#[test]
fn pr_source_missing_node_is_a_404_not_a_server_fault() {
    // A node id with no DB row used to surface rusqlite's "Query returned
    // no rows" and answer 500; `create` already answered 404. The typed
    // error keeps the two routes consistent (#2190 review).
    let _db = ensure_pr_blocking_db();
    let err = resolve_pr_source_for_node(1, 987_654_321).expect_err("unknown node must fail");
    assert_eq!(err.status(), "404 Not Found");
    assert!(matches!(err, PrSourceError::NotFound(987_654_321)));
}
