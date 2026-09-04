//! Low-level git2 helpers shared across commands and services.
//!
//! Each helper takes an already-open `&Repository` (or an OID pair) and returns
//! a `Result`/`Option` — deliberately **not** a bare `bool`. The previous
//! per-module copies disagreed on their error fallback: the sync and close
//! *gates* failed closed (treat an unreadable repo as dirty/risky, don't lose
//! work), while the *display* paths failed open (treat it as clean). Returning
//! the raw computation keeps that choice at the call site — gates use
//! `.unwrap_or(true)`, display uses `.unwrap_or(false)` — instead of baking one
//! direction into a shared helper and silently breaking the other.

use std::collections::HashMap;

use git2::{Oid, Repository, StatusOptions};

use crate::env::to_host_path;

/// Open the repository at a session/internal path, converting it to a
/// host-readable path first (WSL → UNC, etc.). Callers that also need the
/// host-path string for a later subprocess `current_dir` should call
/// [`to_host_path`] themselves and `Repository::open` directly; this helper is
/// for the common open-and-use-the-repo case.
pub fn open_from_host_path(path: &str) -> Result<Repository, git2::Error> {
    Repository::open(to_host_path(path))
}

/// Whether two session paths resolve to the same Git repository.
///
/// Worktree repositories share their main repository's `commondir`, which
/// makes this suitable for associating transcripts stored below a shared
/// Worktree Node parent without exposing git2 to service modules. `None`
/// means one of the paths is missing or unreadable, which lets callers retain
/// stale transcript history without falsely associating it with a Mesh.
pub fn workspaces_belong_to_same_repository(first: &str, second: &str) -> Option<bool> {
    let first_repo = Repository::open(to_host_path(first)).ok();
    let second_repo = Repository::open(to_host_path(second)).ok();
    match (first_repo, second_repo) {
        (Some(first_repo), Some(second_repo)) => Some(crate::env::directories_match(
            &first_repo.commondir().to_string_lossy(),
            &second_repo.commondir().to_string_lossy(),
        )),
        _ => None,
    }
}

/// Whether the working tree has any non-ignored change (modified, staged,
/// untracked, deleted, renamed). The canonical dirty check.
///
/// We filter only on `!is_ignored()`. A previous copy also excluded
/// `Status::CURRENT`, but `StatusOptions` here never sets `include_unmodified`,
/// so libgit2 never emits a `CURRENT` entry — the extra clause was a no-op.
///
/// **Two semantically distinct callers, two helpers:**
/// - [`is_dirty`] is the *strict* gate: any non-ignored change is dirty. Use
///   it for data-safety decisions (worktree close, base restore) where
///   hiding any real work risks silent data loss.
/// - [`is_dirty_mirrors_git_status`] is the *pull gate*: it additionally
///   honours `submodule.<path>.ignore = {dirty,all,untracked}` from
///   `.git/config`, mirroring what `git status` reports. Use it only when the
///   user's contract is "the repo looks dirty iff `git status` says so" —
///   which is exactly the sync-time ff-pull gate (`do_sync` Step 5), where a
///   submodule's WT edits being hidden matches the user's mental model.
///   Note: `commands/prune.rs` reads this kind of dirty signal too, but as a
///   UI badge (with `.unwrap_or(false)`), not a gate; that call site is fine
///   either way and intentionally stays on the strict helper today.
pub fn is_dirty(repo: &Repository) -> Result<bool, git2::Error> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    Ok(repo
        .statuses(Some(&mut opts))?
        .iter()
        .any(|e| !e.status().is_ignored()))
}

/// `git status` parity variant of [`is_dirty`]. Honours
/// `submodule.<path>.ignore = {dirty,all,untracked}` from `.git/config` —
/// libgit2's `repo.statuses()` does not honour this config and always
/// reports the gitlink's WT state as `WT_MODIFIED` when the submodule's WT
/// differs from its HEAD. Without filtering, a mesh whose submodules have
/// uncommitted internal edits gets its fast-forward pull falsely skipped
/// with "fast-forward skipped: working tree has uncommitted changes" (the
/// user-visible message is in `commands/git.rs`'s
/// `sync_outcome_to_git_sync_result`; the gate that produces the variant is
/// `do_sync` Step 5).
///
/// **Scope of parity.** This helper matches `git status --ignore-submodules=
/// dirty` *for submodule working-tree changes*. It does NOT distinguish
/// staged gitlink advances (`+`-prefix lines in `git status`, surfacing as
/// `INDEX_MODIFIED` on the parent's gitlink) from WT-dirt: the parent's view
/// via `repo.statuses()` reports both as `WT_MODIFIED` on the gitlink path.
/// That divergence is benign for the pull gate (a `git pull --ff-only` on a
/// tree whose only dirt is staged changes either applies cleanly or aborts
/// without data loss) but the parity claim should NOT be relied on by any
/// future destructive-operation caller.
///
/// **Do not use this for data-safety decisions** (worktree close, base
/// restore, prune-badging): hiding submodule dirt in those callers risks
/// silently dropping user work. Keep the use site narrow — only `do_sync`'s
/// pre-pull gate (shared by the spawn auto-sync and the manual `git_sync`
/// Tauri command) routes through this helper, because the user's contract
/// there is "should the fast-forward pull happen?", not "is anything dirty?".
///
/// **`ignore = all`:** hides everything for the submodule, including HEAD
/// advance. **`ignore = untracked`:** only hides untracked files inside the
/// submodule, which libgit2 doesn't surface via the parent gitlink entry,
/// so this helper has nothing to filter for that rule.
///
/// **Config read:** `Repository::submodule().ignore_rule()` reads from
/// `.gitmodules`, NOT from `.git/config` — but the `ignore` setting lives in
/// `.git/config`, so this reads `cfg.entries("submodule\\..*\\.ignore")`
/// directly. Failure modes (no config, unreadable file) silently yield an
/// empty map, so we err on the side of reporting dirty.
pub fn is_dirty_mirrors_git_status(repo: &Repository) -> Result<bool, git2::Error> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = repo.statuses(Some(&mut opts))?;
    let ignore_rules = read_submodule_ignore_rules(repo);
    Ok(statuses.iter().any(|e| {
        let status = e.status();
        if status.is_ignored() {
            return false;
        }
        let path = match e.path() {
            Some(p) => p,
            None => return true,
        };
        if let Some(rule) = ignore_rules.get(path) {
            match rule.as_str() {
                "dirty" => return false,
                "all" => return false,
                _ => {}
            }
        }
        true
    }))
}

/// Read `submodule.<path>.ignore = {dirty,all,untracked,none}` entries from
/// `.git/config` and return a path→rule map. The lookup is intentionally
/// permissive — `Repository::submodule().ignore_rule()` reads from
/// `.gitmodules` (and returns the *default* `None` when nothing is set
/// there), but the `ignore` setting is recorded in `.git/config` by
/// `git submodule update --init`. Failure modes (missing config, unreadable
/// file) silently yield an empty map; the callers gate fetches so "no rule
/// found" ⇒ "report dirty" is the safe default.
fn read_submodule_ignore_rules(repo: &Repository) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let cfg = match repo.config() {
        Ok(c) => c,
        Err(_) => return map,
    };
    let mut entries = match cfg.entries(Some("submodule\\..*\\.ignore")) {
        Ok(e) => e,
        Err(_) => return map,
    };
    while let Some(entry) = entries.next() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = match entry.name() {
            Some(n) => n,
            None => continue,
        };
        let value = match entry.value() {
            Some(v) => v.to_string(),
            None => continue,
        };
        // Key is e.g. "submodule.lib/foo.ignore"; strip prefix + suffix to
        // recover the gitlink path. The pattern above guarantees both halves
        // exist. Trim any trailing `/` so submodule paths that end with one
        // (rare, but possible in path-as-stored cases) match the
        // `repo.statuses()` path output, which never has a trailing slash.
        if let Some(stripped) = name
            .strip_prefix("submodule.")
            .and_then(|s| s.strip_suffix(".ignore"))
        {
            map.insert(stripped.trim_end_matches('/').to_string(), value);
        }
    }
    map
}

/// The abbreviated OID (respects the repo's `core.abbreviate`, default 7), as
/// `git rev-parse --short` would print it. Returns an empty string if the OID
/// can't be resolved (e.g. unborn HEAD passed a sentinel) rather than failing —
/// callers use it for display.
pub fn short_sha(repo: &Repository, oid: Oid) -> String {
    repo.find_object(oid, None)
        .ok()
        .and_then(|obj| obj.short_id().ok())
        .map(|buf| String::from_utf8_lossy(&buf).into_owned())
        .unwrap_or_default()
}

/// The branch a repo's HEAD points at, read from HEAD's symbolic target so it
/// survives the branch ref being deleted. Returns `None` for a detached HEAD
/// (no symbolic target).
pub fn head_branch_name(repo: &Repository) -> Option<String> {
    let head = repo.find_reference("HEAD").ok()?;
    let target = head.symbolic_target()?;
    target.strip_prefix("refs/heads/").map(|s| s.to_string())
}

/// The upstream remote-tracking OID configured for a local branch, if there is
/// one and it resolves. `None` covers "no upstream configured" and "upstream
/// ref missing/unreadable" alike — callers that must distinguish those treat
/// `None` as "nothing to compare against".
///
/// Resolution is two-tier. `branch_upstream_name` maps `refs/heads/<b>`
/// through the remote's configured fetch refspec — but a remote wired
/// URL-only (a hand-written `remote.<r>.url` with no `remote.<r>.fetch`
/// line; seen in the wild 2026-07-17 on pixelcache) has no refspec to map
/// through, so git2 errors even though `branch.<b>.remote`/`.merge` are set
/// and `git pull` resolves the upstream fine. The fallback mirrors git's own
/// config lookup directly: `refs/remotes/<branch.<b>.remote>/<branch.<b>.merge
/// minus refs/heads/>`. Without it, every behind-count in the app reads 0 for
/// such a repo — the sidebar ↓N badge never shows and the sync machinery
/// reports "Already up to date" against a frozen ref.
pub fn upstream_oid_for_branch(repo: &Repository, branch: &str) -> Option<Oid> {
    let refname = format!("refs/heads/{}", branch);
    if let Ok(buf) = repo.branch_upstream_name(&refname) {
        if let Some(name) = buf.as_str() {
            if let Some(oid) = repo.find_reference(name).ok().and_then(|r| r.target()) {
                return Some(oid);
            }
        }
    }
    // `branch_upstream_name` maps through the remote's configured fetch
    // refspec, so a URL-only remote (no `remote.<r>.fetch` — 2026-07-17
    // pixelcache incident) makes it error even though `branch.<b>.remote` is
    // set. Mirror `git pull`'s lookup from the branch config directly.
    let cfg = repo.config().ok()?;
    let remote = cfg.get_string(&format!("branch.{}.remote", branch)).ok()?;
    let merge = cfg.get_string(&format!("branch.{}.merge", branch)).ok()?;
    let merge_branch = merge.strip_prefix("refs/heads/")?;
    repo.find_reference(&format!("refs/remotes/{}/{}", remote, merge_branch))
        .ok()?
        .target()
}

/// `(ahead, behind)` of `local` relative to `upstream`, saturating to `u32`.
///
/// Uses git2's `graph_ahead_behind` rather than `git rev-list ...@{u}`: the
/// `@{u}` brace syntax is silently mangled by `Command::args` on Windows.
pub fn ahead_behind(repo: &Repository, local: Oid, upstream: Oid) -> Result<(u32, u32), git2::Error> {
    let (ahead, behind) = repo.graph_ahead_behind(local, upstream)?;
    Ok((
        ahead.try_into().unwrap_or(u32::MAX),
        behind.try_into().unwrap_or(u32::MAX),
    ))
}

/// Normalise a path for equality comparison: convert to host form, trim a
/// trailing separator, and canonicalise (best-effort — on Windows a held-handle
/// error falls back to a slash-normalised string compare). Used to match
/// worktree paths against the live agent-node path list.
pub fn normalize_for_compare(path: &str) -> String {
    let host = to_host_path(path);
    let trimmed = host.trim_end_matches(['/', '\\']);
    std::fs::canonicalize(trimmed)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| trimmed.replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::test_helpers::{commit_file, init_repo_with_commit, TestDir};
    use std::fs;

    #[test]
    fn is_dirty_false_on_clean_repo() {
        let td = TestDir::new("prim_clean");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        assert!(!is_dirty(&repo).unwrap());
    }

    #[test]
    fn is_dirty_true_with_untracked_file() {
        let td = TestDir::new("prim_untracked");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        fs::write(td.path().join("scratch.txt"), "x").unwrap();
        assert!(is_dirty(&repo).unwrap());
    }

    #[test]
    fn is_dirty_true_with_modified_tracked_file() {
        let td = TestDir::new("prim_modified");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        fs::write(td.path().join("f.txt"), "changed\n").unwrap();
        assert!(is_dirty(&repo).unwrap());
    }

    /// Build a parent repo at `td` with one tracked file, then register
    /// a real submodule at `<root>/sub` whose working tree is dirty after
    /// the parent commit. The parent's `.git/config` carries
    /// `submodule.sub.ignore = <rule>`; the test sets `rule` per-case.
    ///
    /// Returns the open `Repository` for the parent so callers can assert
    /// against `is_dirty`. Mirrors the regression seen on 2026-07-04 against
    /// lambo-recomp's `lib/N64ModernRuntime` + `lib/rt64` — submodules with
    /// uncommitted internal edits whose parent carries `submodule.X.ignore =
    /// dirty` so `git status` itself reports clean.
    fn make_submodule_setup(td: &TestDir, ignore_rule: Option<&str>) -> git2::Repository {
        use std::path::Path;
        // 1. Submodule as its own repo (one commit, one file).
        let sub_root = td.path().join("sub");
        fs::create_dir_all(&sub_root).unwrap();
        let _sub = init_repo_with_commit(&sub_root, &[("sub.txt", "x\n")]);

        // 2. Parent repo.
        let parent = init_repo_with_commit(td.path(), &[("root.txt", "r\n")]);
        let url = format!(
            "file://{}",
            sub_root.to_str().unwrap().replace('\\', "/")
        );

        // 3. Register + init + add gitlink to index.
        let mut sm = parent.submodule(&url, Path::new("sub"), true).unwrap();
        sm.init(true).unwrap();
        sm.add_to_index(true).unwrap();

        // 4. `.gitmodules` is written to disk by `submodule()` but NOT added
        //    to the index — `git submodule add` would do that for us. Stage
        //    it manually so the parent can commit the registration.
        parent
            .index()
            .unwrap()
            .add_path(Path::new(".gitmodules"))
            .unwrap();
        parent.index().unwrap().write().unwrap();

        // 5. Apply the per-test ignore rule. `None` means "leave the config
        // untouched" — each test uses a fresh `TestDir` so no rule exists to
        // begin with; we don't explicitly clear because the fixture starts
        // blank by construction.
        let mut cfg = parent.config().unwrap();
        if let Some(rule) = ignore_rule {
            cfg.set_str("submodule.sub.ignore", rule).unwrap();
        }

        // 6. Commit .gitmodules + gitlink.
        let sig = git2::Signature::now("test", "test@example.com").unwrap();
        let mut index = parent.index().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let parent_commit = parent.head().unwrap().peel_to_commit().unwrap();
        let tree = parent.find_tree(tree_oid).unwrap();
        parent
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "add submodule",
                &tree,
                &[&parent_commit],
            )
            .unwrap();

        // 7. Make the submodule's working tree dirty post-commit, matching
        //    the production scenario (submodule HEAD matches the parent's
        //    gitlink; submodule WT has uncommitted edits).
        fs::write(sub_root.join("sub.txt"), "y\n").unwrap();

        // Submodule + commit borrows of `parent` end here; drop them so we
        // can move `parent` out at return time.
        drop((sm, parent_commit, tree));
        parent
    }

    #[test]
    fn is_dirty_mirrors_git_status_submodule_wt_dirty_with_ignore_dirty_says_clean() {
        // Mirrors lambo-recomp's 2026-07-04 regression:
        //   - submodule WT has uncommitted edits
        //   - submodule HEAD matches the parent's recorded gitlink SHA
        //   - parent's .git/config sets `submodule.sub.ignore = dirty`
        // `git status` honours the ignore rule and reports clean.
        // `is_dirty_mirrors_git_status` must do the same or the sync's
        // ff-pull is falsely skipped with "fast-forward skipped: working
        // tree has uncommitted changes" (the user-visible message lives in
        // commands/git.rs's sync_outcome_to_git_sync_result; the gate that
        // emits SyncOutcome::FetchedButDirty is do_sync Step 5).
        // Note: `is_dirty` (strict) would still report this as dirty — that's
        // the correct behaviour for data-safety gates, just not for the fetch.
        let td = TestDir::new("prim_submod_ignore_dirty");
        let parent = make_submodule_setup(&td, Some("dirty"));
        assert!(
            !is_dirty_mirrors_git_status(&parent).unwrap(),
            "is_dirty_mirrors_git_status must mirror `git status`, which honours \
             submodule.<path>.ignore = dirty"
        );
    }

    #[test]
    fn is_dirty_mirrors_git_status_submodule_wt_dirty_without_ignore_dirty_says_dirty() {
        // No `submodule.sub.ignore` set: libgit2's WT_MODIFIED on the
        // gitlink is the canonical dirty signal and `git status` agrees.
        // Pairs with the case above to pin down the symmetry: ignore=dirty
        // filters, !ignore=dirty keeps.
        let td = TestDir::new("prim_submod_no_ignore");
        let parent = make_submodule_setup(&td, None);
        assert!(
            is_dirty_mirrors_git_status(&parent).unwrap(),
            "without ignore=dirty, a dirty submodule WT must still report as dirty"
        );
    }

    #[test]
    fn is_dirty_mirrors_git_status_submodule_wt_dirty_with_ignore_all_says_clean() {
        // `submodule.X.ignore = all` hides everything for the submodule,
        // including any HEAD-advance dirt. Mirrors what
        // `git status --ignore-submodules=all` does.
        let td = TestDir::new("prim_submod_ignore_all");
        let parent = make_submodule_setup(&td, Some("all"));
        assert!(
            !is_dirty_mirrors_git_status(&parent).unwrap(),
            "ignore=all must filter WT-only submodule dirt"
        );
    }

    #[test]
    fn is_dirty_strict_submodule_wt_dirty_still_says_dirty_with_ignore_dirty() {
        // The strict canonical [`is_dirty`] is consumed by data-safety gates
        // (worktree close, base restore, prune badging). It must NOT hide
        // submodule dirt regardless of `submodule.X.ignore` — otherwise those
        // callers silently lose user work. This test pins that contract:
        // same fixture as the parity test, but asserting strict-is-dirty.
        let td = TestDir::new("prim_submod_strict_keeps");
        let parent = make_submodule_setup(&td, Some("dirty"));
        assert!(
            is_dirty(&parent).unwrap(),
            "strict is_dirty must NOT honour submodule.X.ignore — \
             data-safety gates depend on it staying strict"
        );
    }

    #[test]
    fn short_sha_is_nonempty_prefix_of_full_oid() {
        let td = TestDir::new("prim_shortsha");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
        let short = short_sha(&repo, oid);
        assert!(!short.is_empty());
        assert!(oid.to_string().starts_with(&short));
        assert!(short.len() >= 7);
    }

    #[test]
    fn head_branch_name_returns_branch_then_none_when_detached() {
        let td = TestDir::new("prim_headbranch");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        // init_repo_with_commit commits on whatever the default branch is.
        let on_branch = head_branch_name(&repo);
        assert!(on_branch.is_some(), "expected a symbolic HEAD branch");

        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.set_head_detached(head.id()).unwrap();
        assert_eq!(head_branch_name(&repo), None, "detached HEAD has no branch");
    }

    #[test]
    fn ahead_behind_counts_both_directions() {
        let td = TestDir::new("prim_aheadbehind");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        let base = repo.head().unwrap().peel_to_commit().unwrap().id();
        let next = commit_file(&repo, td.path(), "f.txt", "b\n");

        assert_eq!(ahead_behind(&repo, next, base).unwrap(), (1, 0));
        assert_eq!(ahead_behind(&repo, base, next).unwrap(), (0, 1));
        assert_eq!(ahead_behind(&repo, base, base).unwrap(), (0, 0));
    }

    #[test]
    fn upstream_oid_for_branch_resolves_configured_tracking_branch() {
        let td = TestDir::new("prim_upstream");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        let branch = head_branch_name(&repo).unwrap();
        let base = repo.head().unwrap().peel_to_commit().unwrap().id();

        // A configured remote (with its default fetch refspec) is what lets
        // `branch_upstream_name` map the branch's `merge` ref to the
        // `refs/remotes/origin/*` tracking ref.
        repo.remote("origin", td.path().to_str().unwrap()).unwrap();
        // Point a remote-tracking ref at the base commit and wire the branch's
        // upstream config to it, then advance the local branch by one commit.
        repo.reference(
            &format!("refs/remotes/origin/{}", branch),
            base,
            false,
            "test",
        )
        .unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str(&format!("branch.{}.remote", branch), "origin").unwrap();
        cfg.set_str(&format!("branch.{}.merge", branch), &format!("refs/heads/{}", branch))
            .unwrap();
        let local = commit_file(&repo, td.path(), "f.txt", "b\n");

        let upstream = upstream_oid_for_branch(&repo, &branch).expect("upstream resolves");
        assert_eq!(upstream, base);
        assert_eq!(ahead_behind(&repo, local, upstream).unwrap(), (1, 0));
    }

    #[test]
    fn upstream_oid_for_branch_falls_back_to_branch_config_when_refspec_missing() {
        // 2026-07-17 pixelcache incident: `.git/config` has
        // `remote.origin.url` + `branch.<b>.remote`/`.merge`, but NO
        // `remote.origin.fetch` refspec (remote wired by hand, not via
        // `git remote add`). git2's `branch_upstream_name` maps the branch
        // through the fetch refspec, so it errors — yet git itself (and
        // `git pull`) resolves the upstream fine from the branch config.
        // The fallback must do the same, or every behind-count in the app
        // silently reads 0 against a frozen tracking ref.
        let td = TestDir::new("prim_upstream_no_refspec");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        let branch = head_branch_name(&repo).unwrap();
        let base = repo.head().unwrap().peel_to_commit().unwrap().id();

        // URL-only remote: write the config key directly — `repo.remote()`
        // would also write the fetch refspec, defeating the test.
        let mut cfg = repo.config().unwrap();
        cfg.set_str("remote.origin.url", td.path().to_str().unwrap())
            .unwrap();
        cfg.set_str(&format!("branch.{}.remote", branch), "origin")
            .unwrap();
        cfg.set_str(
            &format!("branch.{}.merge", branch),
            &format!("refs/heads/{}", branch),
        )
        .unwrap();
        repo.reference(
            &format!("refs/remotes/origin/{}", branch),
            base,
            false,
            "test",
        )
        .unwrap();

        assert_eq!(
            upstream_oid_for_branch(&repo, &branch),
            Some(base),
            "upstream must resolve via branch.<b>.remote/.merge when the \
             remote has no fetch refspec"
        );
    }

    #[test]
    fn upstream_oid_for_branch_none_without_tracking_config() {
        let td = TestDir::new("prim_no_upstream");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        let branch = head_branch_name(&repo).unwrap();
        assert_eq!(upstream_oid_for_branch(&repo, &branch), None);
    }

    #[test]
    fn normalize_for_compare_is_stable_across_trailing_slash() {
        let td = TestDir::new("prim_normalize");
        let p = td.path().to_string_lossy().to_string();
        let with_slash = format!("{}/", p.trim_end_matches(['/', '\\']));
        assert_eq!(normalize_for_compare(&p), normalize_for_compare(&with_slash));
    }
}
