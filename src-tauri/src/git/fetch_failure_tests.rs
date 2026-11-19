//! Tests for the fetch-failure classification and the permanent-failure
//! backoff (issue #2832).
//!
//! The fixture-remote tests build a local bare "remote" that has `master`
//! and no `main` — the exact shape the issue reports (a Mesh whose
//! `base_ref` names a branch the remote does not have) — and drive the
//! real fetch path against it. No network access is involved.

use super::*;
use crate::env::test_helpers::{init_repo_with_commit, TestDir};
use std::process::Command;

/// The Mesh's current permanent-failure record, if any.
///
/// The production path asks "is this Mesh paused?" through
/// [`allows_attempt`], which answers it and drops a stale record in the same
/// step. This accessor is the tests' view of the registry itself.
fn permanent_failure(mesh_path: &str) -> Option<PermanentFailure> {
    let key = mesh_key(mesh_path);
    with_records(|map| map.get(&key).cloned())
}

// ── classification ─────────────────────────────────────────────────────────

/// The exact failure the issue reports: the remote has no `main`. A retry is
/// expected to keep failing until the Mesh's `base_ref` changes, so it must
/// classify as permanent.
#[test]
fn a_missing_remote_ref_is_a_permanent_failure() {
    let reason = "fatal: couldn't find remote ref refs/heads/main";
    assert_eq!(
        classify_fetch_failure(reason),
        FetchFailureClass::Permanent,
        "a remote that does not have the branch we asked for is not fixed by retrying"
    );
}

/// The issue is explicit that the timeouts must stay transient: they look
/// like the network or the load, and the same request may well succeed
/// next time.
#[test]
fn a_fetch_timeout_is_a_transient_failure() {
    for reason in [
        "git fetch timed out after 30s",
        "fatal: unable to access 'https://example.com/x.git/': Failed to connect to example.com port 443: Timed out",
        "fatal: unable to access 'https://example.com/x.git/': Could not resolve host: example.com",
        "fatal: unable to access 'https://example.com/x.git/': Recv failure: Connection reset by peer",
        "fatal: unable to access 'https://example.com/x.git/': The remote end hung up unexpectedly",
    ] {
        assert_eq!(
            classify_fetch_failure(reason),
            FetchFailureClass::Transient,
            "a network/timeout failure must keep the normal retry schedule: {reason}"
        );
    }
}

/// Refused credentials are permanent too — the same token is still refused
/// on the next attempt.
#[test]
fn refused_credentials_are_a_permanent_failure() {
    for reason in [
        "fatal: Authentication failed for 'https://github.com/example/repo.git/'",
        "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
        "fatal: Permission denied (publickey).",
        "fatal: unable to access 'https://github.com/example/repo.git/': The requested URL returned error: 403",
        "fatal: repository 'https://github.com/example/missing.git/' not found",
    ] {
        assert_eq!(
            classify_fetch_failure(reason),
            FetchFailureClass::Permanent,
            "a refused repository cannot be fixed by retrying: {reason}"
        );
    }
}

/// The same refusal in the wording of other hosting services. GitHub says
/// "Authentication failed", GitLab and Gitea say "HTTP Basic: Access denied"
/// and Bitbucket says "repository access denied" — a Mesh on one of those
/// would otherwise spend its retry budget on a token the server has already
/// refused.
///
/// Each case below carries only its own marker, so the test fails if one of
/// them stops being recognised.
#[test]
fn a_refusal_in_another_services_wording_is_permanent() {
    for reason in [
        "remote: HTTP Basic: Access denied",
        "conq: repository access denied.",
        "remote: Permission denied",
    ] {
        assert_eq!(
            classify_fetch_failure(reason),
            FetchFailureClass::Permanent,
            "a refused token cannot be fixed by retrying: {reason}"
        );
    }
}

/// A message this function does not understand keeps the pre-#2832
/// behaviour (retry on the normal schedule) rather than silently stopping a
/// Mesh's sync.
#[test]
fn an_unrecognised_failure_stays_transient() {
    assert_eq!(
        classify_fetch_failure("fatal: something nobody has seen before"),
        FetchFailureClass::Transient
    );
}

/// The permanent reading wins when a message carries both a transport
/// prefix and a refusal: git prefixes transport failures with
/// `unable to access`, including the ones that are not transient.
#[test]
fn a_refusal_behind_a_transport_prefix_is_still_permanent() {
    let reason =
        "fatal: unable to access 'https://github.com/example/repo.git/': The requested URL returned error: 403";
    assert_eq!(
        classify_fetch_failure(reason),
        FetchFailureClass::Permanent,
        "the transport prefix must not mask the refusal underneath it"
    );
}

/// A server error whose body happens to mention "not found" says little
/// about the remote's fate, so it must stay transient. Reading it as
/// permanent would pause the Mesh for hours over a blip — the exact failure
/// mode issue #2832 is about, inverted.
#[test]
fn an_error_body_that_mentions_not_found_stays_transient() {
    let reason = "remote: upstream host not found\nfatal: unable to access 'https://example.com/x.git/': The requested URL returned error: 500";
    assert_eq!(
        classify_fetch_failure(reason),
        FetchFailureClass::Transient,
        "an HTTP 500 whose body mentions 'not found' is not a missing repository: {reason}"
    );
}

/// A remote whose target is not a repository *right now* is usually a local
/// path that is not there — an unplugged drive — so it stays transient. A
/// gone URL arrives as "repository '…' not found", which is permanent.
#[test]
fn a_remote_target_that_is_not_a_repository_stays_transient() {
    let reason = "fatal: 'C:\\nonexistent-drive\\repo' does not appear to be a git repository";
    assert_eq!(
        classify_fetch_failure(reason),
        FetchFailureClass::Transient,
        "a local path that is not there now may be there on the next attempt: {reason}"
    );
}

// ── the branch the remote does not have ───────────────────────────────────

#[test]
fn the_missing_branch_is_read_out_of_gits_own_message() {
    assert_eq!(
        missing_remote_branch("fatal: couldn't find remote ref refs/heads/main"),
        Some("main".to_string())
    );
    assert_eq!(
        missing_remote_branch("fatal: could not find remote ref refs/heads/feature/auth"),
        Some("feature/auth".to_string()),
        "a nested branch name keeps its slashes"
    );
    assert_eq!(
        missing_remote_branch("fatal: unrelated failure"),
        None,
        "a failure that says nothing about a missing ref must not invent one"
    );
}

// ── the remote's default branch ────────────────────────────────────────────

#[test]
fn the_ls_remote_symref_output_names_the_default_branch() {
    let stdout = "ref: refs/heads/master\tHEAD\n0123456789abcdef0123456789abcdef01234567\tHEAD\n";
    assert_eq!(parse_ls_remote_symref(stdout), Some("master".to_string()));
    assert_eq!(
        parse_ls_remote_symref("0123456789abcdef0123456789abcdef01234567\tHEAD\n"),
        None,
        "output without the symref line names no branch"
    );
}

// ── the user-facing sentence ───────────────────────────────────────────────

/// The issue's wording: "origin has no `main`; its default branch is
/// `master`", plus the offer of the branch to use instead.
#[test]
fn the_message_names_the_missing_branch_and_offers_the_default_branch() {
    let message = describe_permanent_failure(
        "/repos/demo",
        Some("origin"),
        Some("main"),
        Some("master"),
        "fatal: couldn't find remote ref refs/heads/main",
    );
    assert!(
        message.contains("origin has no branch 'main'"),
        "the message must name the branch the remote lacks: {message}"
    );
    assert!(
        message.contains("its default branch is 'master'"),
        "the message must name the remote's default branch: {message}"
    );
    assert!(
        message.contains("origin/master"),
        "the message must offer the detected default branch as the base ref: {message}"
    );
    assert!(
        message.contains("/repos/demo"),
        "the toast is app-global, so the message must name the mesh: {message}"
    );
}

/// Without a detectable default branch the message must not claim one.
#[test]
fn the_message_does_not_invent_a_default_branch() {
    let message = describe_permanent_failure(
        "/repos/demo",
        Some("origin"),
        Some("main"),
        None,
        "fatal: couldn't find remote ref refs/heads/main",
    );
    assert!(message.contains("origin has no branch 'main'"), "{message}");
    assert!(
        !message.contains("default branch"),
        "with no detected default branch the message must not name one: {message}"
    );
}

/// A failure that is not about a missing branch must not be reported as one.
/// The branch comes from the failure's OWN message, not from the base ref —
/// otherwise an authentication failure would tell the user the remote has no
/// such branch, which the failure did not say.
#[test]
fn the_message_does_not_claim_a_missing_branch_for_other_failures() {
    let message = describe_permanent_failure(
        "/repos/demo",
        Some("origin"),
        None,
        Some("master"),
        "fatal: Authentication failed for 'https://github.com/example/repo.git/'",
    );
    assert!(
        !message.contains("has no branch"),
        "an auth failure must not be reported as a missing branch: {message}"
    );
    assert!(
        message.contains("Authentication failed"),
        "the message must still say what failed: {message}"
    );
}

// ── the backoff ────────────────────────────────────────────────────────────

#[test]
fn the_permanent_backoff_starts_at_one_hour_and_doubles_to_a_cap() {
    assert_eq!(
        permanent_interval_for(1),
        Duration::from_secs(60 * 60),
        "the first permanent failure must wait an hour, not the three-minute sync interval"
    );
    assert_eq!(permanent_interval_for(2), Duration::from_secs(2 * 60 * 60));
    assert_eq!(permanent_interval_for(3), Duration::from_secs(4 * 60 * 60));
    assert_eq!(
        permanent_interval_for(20),
        PERMANENT_MAX_INTERVAL,
        "the backoff must be capped"
    );
    assert!(
        PERMANENT_BASE_INTERVAL >= Duration::from_secs(60 * 60),
        "issue #2832 requires a backoff of at least one hour"
    );
}

// The registry is process-global, so each test files its record under its
// own synthetic mesh path.
#[test]
fn a_permanent_failure_pauses_the_mesh_until_the_backoff_elapses() {
    let mesh = "/probe/mesh/permanent-pause";
    let base = Instant::now();
    let report = record_permanent_failure(mesh, "origin/main", base);
    assert!(
        report.first,
        "the first permanent failure is the one to report"
    );
    assert_eq!(report.failures, 1);
    assert_eq!(report.interval, PERMANENT_BASE_INTERVAL);

    assert!(
        !allows_attempt_at(mesh, "origin/main", base + Duration::from_secs(10)),
        "the failing fetch must not run again straight away"
    );
    assert!(
        !allows_attempt_at(
            mesh,
            "origin/main",
            base + PERMANENT_BASE_INTERVAL - Duration::from_secs(1)
        ),
        "the attempt must still be blocked right up to the backoff boundary"
    );
    assert!(
        allows_attempt_at(mesh, "origin/main", base + PERMANENT_BASE_INTERVAL),
        "the attempt resumes once the backoff has elapsed"
    );
}

/// The acceptance criterion's other half: the failing fetch runs "at most
/// once per configuration change". A changed `base_ref` drops the record, so
/// the sync resumes at once.
#[test]
fn changing_the_meshs_base_ref_resumes_the_sync_immediately() {
    let mesh = "/probe/mesh/config-change";
    let base = Instant::now();
    record_permanent_failure(mesh, "origin/main", base);
    assert!(
        !allows_attempt_at(mesh, "origin/main", base + Duration::from_secs(1)),
        "the same configuration stays paused"
    );
    assert!(
        allows_attempt_at(mesh, "origin/master", base + Duration::from_secs(1)),
        "a changed base ref describes a different situation — the sync must resume at once"
    );
    assert!(
        permanent_failure(mesh).is_none(),
        "the stale record must be dropped when the configuration changes"
    );
}

/// A successful fetch proves the permanent condition is gone.
#[test]
fn a_successful_fetch_clears_the_record() {
    let mesh = "/probe/mesh/recovered";
    let base = Instant::now();
    record_permanent_failure(mesh, "origin/main", base);
    assert!(record_success(mesh), "there was a record to clear");
    assert!(
        allows_attempt_at(mesh, "origin/main", base),
        "a recovered mesh is not paused"
    );
}

// ── the fixture remote that has no `main` ──────────────────────────────────

/// Build a working repo whose `origin` is a local bare repository that has
/// `master` and no `main` — the shape issue #2832 reports.
fn remote_without_main(td: &TestDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let parent = td.path().to_path_buf();
    let origin_dir = td.sibling("origin");
    let status = Command::new("git")
        .args(["init", "--bare", "--initial-branch=master"])
        .arg(&origin_dir)
        .status()
        .expect("git init --bare failed");
    assert!(status.success());
    let _repo = init_repo_with_commit(&parent, &[("README.md", "v1\n")]);
    let status = Command::new("git")
        .args(["remote", "add", "origin", &origin_dir.to_string_lossy()])
        .current_dir(&parent)
        .status()
        .expect("git remote add failed");
    assert!(status.success());
    let status = Command::new("git")
        .args(["push", "origin", "HEAD:master"])
        .current_dir(&parent)
        .status()
        .expect("git push failed");
    assert!(status.success());
    (parent, origin_dir)
}

/// The end-to-end shape of the issue: a Mesh whose `base_ref` names a
/// branch the remote does not have produces a failure the policy must
/// classify as permanent, and the branch it names is read out of git's own
/// message.
#[test]
fn fetching_a_branch_the_remote_lacks_is_classified_permanent() {
    let td = TestDir::new("fetch_failure_no_main");
    let (parent, _origin_dir) = remote_without_main(&td);

    let outcome = crate::git::sync::fetch_origin(parent.to_str().unwrap(), "origin/main");
    let reason = match outcome {
        Err(crate::git::sync::FetchError::FetchFailed(reason)) => reason,
        other => panic!(
            "a remote with no `main` must fail the fetch, got {:?}",
            other.map(|o| format!("{o:?}"))
        ),
    };
    assert_eq!(
        classify_fetch_failure(&reason),
        FetchFailureClass::Permanent,
        "the fixture remote has no `main`, so the failure is permanent: {reason}"
    );
    assert_eq!(
        missing_remote_branch(&reason),
        Some("main".to_string()),
        "the branch the remote lacks must be read out of git's message: {reason}"
    );
}

/// The message the user gets names the remote's real default branch — the
/// branch they probably meant — which the fixture remote answers as
/// `master`.
#[test]
fn the_detected_default_branch_comes_from_the_remote() {
    let td = TestDir::new("fetch_failure_default_branch");
    let (parent, _origin_dir) = remote_without_main(&td);

    let detected = detect_remote_default_branch(parent.to_str().unwrap(), "origin");
    assert_eq!(
        detected,
        Some("master".to_string()),
        "the remote's HEAD points at master — that is the branch the user probably meant"
    );
}

/// The whole policy, end to end: the failure is permanent, so the Mesh is
/// paused for at least an hour rather than retried on the three-minute sync
/// cadence, and the message offers the branch to switch to.
#[test]
fn the_permanent_failure_pauses_the_mesh_and_offers_the_default_branch() {
    let td = TestDir::new("fetch_failure_policy");
    let (parent, _origin_dir) = remote_without_main(&td);
    let mesh = parent.to_str().unwrap();

    let outcome = crate::git::sync::fetch_origin(mesh, "origin/main");
    let reason = match outcome {
        Err(crate::git::sync::FetchError::FetchFailed(reason)) => reason,
        other => panic!("expected the fetch to fail, got {other:?}"),
    };
    assert_eq!(
        classify_fetch_failure(&reason),
        FetchFailureClass::Permanent
    );

    let report = record_permanent_failure(mesh, "origin/main", Instant::now());
    assert!(report.first);
    assert!(
        report.interval >= Duration::from_secs(60 * 60),
        "the retry must wait at least an hour, got {:?}",
        report.interval
    );
    assert!(
        !allows_attempt(mesh, "origin/main"),
        "the failing fetch must not be retried on the normal schedule"
    );

    let default_branch = detect_remote_default_branch(mesh, "origin");
    let message = describe_permanent_failure(
        mesh,
        Some("origin"),
        missing_remote_branch(&reason).as_deref(),
        default_branch.as_deref(),
        &reason,
    );
    assert!(message.contains("origin has no branch 'main'"), "{message}");
    assert!(
        message.contains("origin/master"),
        "the message must offer the remote's default branch: {message}"
    );

    // The fixture's own repository is untouched by the failed fetch.
    assert!(
        parent.join("README.md").exists(),
        "the failed fetch must not touch the working tree"
    );
}
