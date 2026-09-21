//! Environment detection and path handling for Windows + WSL hybrid setup.
//!
//! Three sub-modules, one concern each (issue #248):
//!
//! - [`environment`] — pure detection (Windows vs WSL, distro, login shell,
//!   agent CLI home dirs). No path conversion happens here.
//! - [`host_path`] — path conversion (`to_host_path`, `to_spawn_path`,
//!   `env_for_path`) and the [`host_path::ResolvedPath`] machinery. This is
//!   the **only** module in the Buildmesh tree that builds `\\wsl$\` UNC
//!   strings or `/mnt/` rewrite strings (CLAUDE.md hard rule, structurally
//!   enforced by module boundaries).
//! - [`mesh_row`] — mesh-row DTO read helper.
//!
//! Every public item in the sub-modules is re-exported here so the existing
//! `crate::env::{to_host_path, node_working_path, ResolvedPath, Environment,
//! claude_dir, codex_dir, wsl_login_shell, current_env, active_node_paths,
//! active_node_branches, mesh_row, test_helpers::*}` import shape stays
//! compile-stable — the split is internal, the API is unchanged.

mod environment;
mod host_path;
mod mesh_row;
mod windows_interop;

pub use environment::*;
pub(crate) use windows_interop::*;
pub use host_path::*;
pub use mesh_row::mesh_row;

/// Shared test fixtures used by both `mod tests` (worktree / base_ref
/// regression suites) and `fetch_origin_tests` (issue #213). Lifted
/// out of `mod tests` so the sibling fetch_origin module can reach
/// them — `mod tests` items are private to that scope. Kept inside
/// env/mod.rs rather than a standalone file so the helpers stay
/// co-located with the production code they exercise.
#[cfg(test)]
pub(crate) mod test_helpers {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEST_DIR: AtomicUsize = AtomicUsize::new(0);

    /// Per-test scratch directory under %TEMP%, named uniquely so parallel
    /// cargo test invocations don't collide. Removed on drop.
    pub(crate) struct TestDir(PathBuf);
    impl TestDir {
        pub(crate) fn new(suffix: &str) -> Self {
            let id = NEXT_TEST_DIR.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!(
                "buildmesh_wt_test_{}_{}_{}",
                suffix,
                std::process::id(),
                id
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Init a repo with one initial commit containing the given files.
    /// `path` is expected to exist (callers pass a `TestDir`).
    pub(crate) fn init_repo_with_commit(
        path: &Path,
        files: &[(&str, &str)],
    ) -> git2::Repository {
        let repo = git2::Repository::init(path).unwrap();
        let sig = git2::Signature::now("test", "test@example.com").unwrap();

        let mut index = repo.index().unwrap();
        for (name, content) in files {
            let full = path.join(name);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(&full, content).unwrap();
            index.add_path(Path::new(name)).unwrap();
        }
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        {
            // Scope the Tree borrow so it's dropped before we return `repo`.
            let tree = repo.find_tree(tree_oid).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        repo
    }

    /// Returns true if the repo at `path` has any non-ignored working-tree
    /// changes. Used as a precondition assertion by the dirty-parent tests
    /// so a silent failure to dirty the repo doesn't make the test pass
    /// for the wrong reason.
    pub(crate) fn repo_is_dirty(path: &Path) -> bool {
        let repo = git2::Repository::open(path).unwrap();
        let mut opts = git2::StatusOptions::new();
        opts.include_untracked(true).recurse_untracked_dirs(true);
        let statuses = repo.statuses(Some(&mut opts)).unwrap();
        statuses.iter().any(|entry| !entry.status().is_ignored())
    }

    /// Add a commit on top of current HEAD with the given file content,
    /// advancing HEAD. Returns the new commit oid.
    pub(crate) fn commit_file(
        repo: &git2::Repository,
        root: &Path,
        name: &str,
        content: &str,
    ) -> git2::Oid {
        let sig = git2::Signature::now("test", "test@example.com").unwrap();
        fs::write(root.join(name), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(name)).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "more", &tree, &[&parent])
            .unwrap()
    }

    /// Set up a repo where `origin/main` (a remote-tracking ref) points at the
    /// initial commit, then drift the local HEAD forward to a second commit.
    /// Returns (repo, origin_main_oid).
    pub(crate) fn repo_with_drifted_head(root: &Path) -> (git2::Repository, git2::Oid) {
        let repo = init_repo_with_commit(root, &[("f.txt", "from-origin-main\n")]);
        let origin_oid = repo.head().unwrap().peel_to_commit().unwrap().id();
        repo.reference("refs/remotes/origin/main", origin_oid, false, "test")
            .unwrap();
        commit_file(&repo, root, "f.txt", "local-drift\n");
        assert_ne!(
            repo.head().unwrap().peel_to_commit().unwrap().id(),
            origin_oid,
            "precondition: HEAD must differ from origin/main"
        );
        (repo, origin_oid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `AgentNode` is consumed by the worktree-rule tests below. It used to
    // be in scope from env/mod.rs's `use crate::models::{...}` import; the
    // split moved that import into `host_path.rs`, so we re-import here
    // for the test scope. (A `pub use` from host_path would leak the data
    // type into the `crate::env` public surface — not desired.)
    use crate::models::AgentNode;

    #[test]
    fn parses_default_wsl_distribution_from_star_marker() {
        let listing = "  NAME              STATE           VERSION\n* Ubuntu            Running         2\n  Debian            Stopped         2\n";
        assert_eq!(
            environment::parse_wsl_distro_list(listing).as_deref(),
            Some("Ubuntu")
        );
    }

    #[test]
    fn parses_wsl_home_marker_after_login_banner() {
        let output = b"Welcome to Ubuntu\r\nlast login: today\n__BUILDMESH_WSL_HOME__/home/alond\r\n";
        assert_eq!(
            environment::parse_wsl_home_output(output).as_deref(),
            Some(std::path::Path::new("/home/alond"))
        );
    }

    #[test]
    fn rejects_unmarked_wsl_home_output() {
        assert!(environment::parse_wsl_home_output(b"/home/alond\n").is_none());
    }

    #[test]
    fn parses_guest_codex_home_override_after_probe_marker() {
        let output = b"__BUILDMESH_WSL_CODEX_HOME__/var/lib/codex\r\n";
        assert_eq!(
            environment::parse_wsl_codex_home_output(output).as_deref(),
            Some(std::path::Path::new("/var/lib/codex"))
        );
    }

    #[test]
    fn cursor_dir_uses_the_current_environment_home() {
        let expected = match current_env() {
            Environment::Wsl => std::env::var("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from("/root"))
                .join(".cursor"),
            Environment::Windows => std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| {
                    let user = std::env::var("USERNAME").unwrap_or_else(|_| "Public".to_string());
                    std::path::PathBuf::from(format!("C:\\Users\\{user}"))
                })
                .join(".cursor"),
        };
        assert_eq!(cursor_dir(), expected);
    }

    /// Issue #1283: the AGY brain directory lives under the same home
    /// directory every other CLI helper consults — `~/.gemini/antigravity-cli/
    /// brain/`. Pin the path shape so the transcript reader's locator can
    /// rely on `env::agy_brain_dir()` returning exactly
    /// `<home>/.gemini/antigravity-cli/brain` (or `GEMINI_HOME` /
    /// `ANTIGRAVITY_HOME` overrides, but those aren't tested because the
    /// bare-env expectation matches every supported platform).
    #[test]
    fn agy_brain_dir_uses_the_current_environment_home() {
        let brain = agy_brain_dir();
        let dir = agy_dir();
        assert_eq!(
            brain,
            dir.join("brain"),
            "brain dir must always sit under the AGY home ({dir:?})"
        );
        let expected_suffix = match current_env() {
            Environment::Wsl => ".gemini/antigravity-cli/brain",
            Environment::Windows => ".gemini\\antigravity-cli\\brain",
        };
        // Path-builder/separator normalization makes a literal contains
        // check the right pin — Windows separators and posix separators
        // both match via `ends_with` after the path was constructed.
        let path_str = brain.to_string_lossy().replace('\\', "/");
        assert!(
            path_str.ends_with(&expected_suffix.replace('\\', "/")),
            "agy_brain_dir should end with `{expected_suffix}`, got `{}`",
            path_str
        );
    }

    /// Pin the Grok home directory resolution (issue #1281). When `GROK_HOME`
    /// is unset, `grok_dir()` derives from the current environment's home
    /// (`$HOME` on WSL; `$USERPROFILE`/`$HOME`/`$USERNAME` on Windows). Note:
    /// `GROK_HOME` honours the override here too (covered by the cursor /
    /// codex tests in spirit) — this test pins the *base* derivation so a
    /// future refactor that breaks `.grok` suffixing catches it.
    #[test]
    fn grok_dir_uses_the_current_environment_home() {
        let expected = match current_env() {
            Environment::Wsl => std::env::var("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from("/root"))
                .join(".grok"),
            Environment::Windows => std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| {
                    let user = std::env::var("USERNAME").unwrap_or_else(|_| "Public".to_string());
                    std::path::PathBuf::from(format!("C:\\Users\\{user}"))
                })
                .join(".grok"),
        };
        assert_eq!(grok_dir(), expected);
    }

    /// Issue #1774: the Cline CLI home directory must match the current
    /// environment's home — `~/.cline` everywhere, with the same
    /// `$HOME`/`$USERPROFILE` fallback rules the other CLI helpers
    /// already pin. The capture poller reads its SQLite store under
    /// `<cline home>/data/db/sessions.db`, so the path the helper emits
    /// must land in a place that actually exists on a real install.
    #[test]
    fn cline_dir_uses_the_current_environment_home() {
        let expected = match current_env() {
            Environment::Wsl => std::env::var("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from("/root"))
                .join(".cline"),
            Environment::Windows => std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| {
                    let user = std::env::var("USERNAME").unwrap_or_else(|_| "Public".to_string());
                    std::path::PathBuf::from(format!("C:\\Users\\{user}"))
                })
                .join(".cline"),
        };
        assert_eq!(cline_dir(), expected);
    }

    /// Issue #1774: `cline_db_path_for_env` must always append the
    /// authoritative `data/db/sessions.db` suffix onto whatever home
    /// the spawn-aware resolver picked (override or `~/.cline`). The
    /// SQLite store is the canonical capture source — `~/.cline/data/
    /// sessions/<id>/` is the fallback. If the suffix drifts, the
    /// capture poller silently reads the wrong file.
    #[test]
    fn cline_db_path_for_env_appends_canonical_suffix() {
        use crate::models::EnvType;
        // Windows path: bare-home derivation, suffix must still apply.
        let windows_home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from(r"C:\Users\Public"));
        let path = cline_db_path_for_env(EnvType::Windows, "")
            .expect("windows home must resolve to a Cline DB path");
        let expected = windows_home.join(".cline").join("data").join("db").join("sessions.db");
        assert_eq!(path, expected, "windows DB path must end in data/db/sessions.db");
        assert!(path.ends_with("data/db/sessions.db") || path.ends_with("data\\db\\sessions.db"),
                "DB path must carry the data/db/sessions.db suffix regardless of separator");
    }

    /// Round 1 review: `CLINE_DATA_DIR` IS the data directory (per
    /// `cline --help`); the DB sits under it directly. The override
    /// must NOT double up the `data/` prefix or the SQLite open lands
    /// on a non-existent path. Round 2 review: drive the resolver via
    /// the injected closure rather than mutating `std::env` — the
    /// latter races with concurrent cargo test threads.
    #[test]
    fn cline_db_path_honours_cline_data_dir_override() {
        use crate::env::cline_db_path_with_resolver;
        use crate::models::EnvType;
        let data_dir: std::ffi::OsString =
            std::path::PathBuf::from(r"C:\custom\data").into_os_string();
        let injected = data_dir.clone();
        let path = cline_db_path_with_resolver(EnvType::Windows, "", move |key| {
            if key == "CLINE_DATA_DIR" {
                Some(injected.clone())
            } else {
                None
            }
        })
        .expect("override must resolve to a DB path");
        let expected = std::path::PathBuf::from(r"C:\custom\data")
            .join("db")
            .join("sessions.db");
        assert_eq!(
            path, expected,
            "override must append db/sessions.db directly (no extra data/)"
        );
    }

    /// Round 2 review: an empty `CLINE_DATA_DIR` must NOT trigger the
    /// override-suffix branch. `std::env::var_os` reports `Some("")`
    /// for an empty value, so the production code keys off the
    /// override resolver (which trims and rejects empty), not the
    /// raw env-var presence. A regression that swapped the suffix
    /// decision back to `env::var_os("CLINE_DATA_DIR").is_some()`
    /// would re-introduce the empty-string bug — pin the predicate
    /// here with the same closure-injection pattern.
    #[test]
    fn cline_db_path_treats_empty_cline_data_dir_as_unset() {
        use crate::env::cline_db_path_with_resolver;
        use crate::models::EnvType;
        let windows_home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from(r"C:\Users\Public"));
        let expected = windows_home
            .join(".cline")
            .join("data")
            .join("db")
            .join("sessions.db");
        for empty_value in ["", " ", "\t", "  \t "] {
            let injected: std::ffi::OsString = empty_value.into();
            let path = cline_db_path_with_resolver(EnvType::Windows, "", |key| {
                if key == "CLINE_DATA_DIR" {
                    Some(injected.clone())
                } else {
                    None
                }
            })
            .expect("default path must resolve when override is empty");
            assert_eq!(
                path, expected,
                "empty {empty_value:?} must fall through to the bare-home data/db/sessions.db suffix"
            );
        }
    }

    /// Round 2 review: when `CLINE_DATA_DIR` is unset, the production
    /// helper goes through the bare-home `~/.cline/data/db/sessions.db`
    /// path even if a sibling var (e.g. `OPENCODE_DATA_DIR`) is set.
    /// The injection closure returns `None` for the Cline key, and
    /// the path must reflect the default derivation.
    #[test]
    fn cline_db_path_ignores_unrelated_env_vars() {
        use crate::env::cline_db_path_with_resolver;
        use crate::models::EnvType;
        let windows_home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from(r"C:\Users\Public"));
        let expected = windows_home
            .join(".cline")
            .join("data")
            .join("db")
            .join("sessions.db");
        let path = cline_db_path_with_resolver(EnvType::Windows, "", |key| {
            if key == "SOME_OTHER_VAR" {
                Some("/totally/different/path".into())
            } else {
                None
            }
        })
        .expect("default path must resolve when CLINE_DATA_DIR is unset");
        assert_eq!(path, expected);
    }

    /// Round 1 review: a Windows Buildmesh driving a WSL Cline must
    /// convert the guest POSIX path to a `\\wsl$\—` UNC path before
    /// opening the SQLite store. `cline_db_path_for_host` is the
    /// single seam that does this; passing the raw POSIX path to a
    /// host `Connection::open` fails silently. On non-WSL env types
    /// the helper is a no-op.
    ///
    /// Windows-only: the WSL→UNC translation lives in
    /// `host_path::to_host_path`, which only branches into the UNC
    /// prefix on `cfg!(target_os = "windows")`. On macOS / native Linux
    /// the helper is a no-op, so `guest == host` and the difference
    /// assertion would fire spuriously (Linux CI is
    /// `.github/workflows/build.yml:25, :158`).
    #[cfg(target_os = "windows")]
    #[test]
    fn cline_db_path_for_host_translates_wsl_guest_path() {
        use crate::models::EnvType;
        // On non-WSL hosts the host-path helper must equal the raw
        // helper (Windows / WindowsInterop stay on the host).
        let raw_windows = cline_db_path_for_env(EnvType::Windows, "")
            .expect("windows home must resolve");
        let host_windows = cline_db_path_for_host(EnvType::Windows, "")
            .expect("windows host-path must resolve");
        assert_eq!(raw_windows, host_windows, "Windows paths must be no-ops");
        // On WSL the host-path helper must differ from the raw guest
        // path — every Windows-side reader needs the UNC translation.
        // The test only asserts the difference, not the exact UNC
        // string (which depends on the active distro).
        let guest = cline_db_path_for_env(EnvType::Wsl, "/home/alond/repo")
            .expect("WSL guest home must resolve");
        let host = cline_db_path_for_host(EnvType::Wsl, "/home/alond/repo")
            .expect("WSL host-path must resolve");
        let host_str = host.to_string_lossy();
        assert_ne!(
            guest, host,
            "WSL host-path must differ from raw guest path (UNC translation)"
        );
        assert!(
            host_str.starts_with("\\\\wsl") || host_str.starts_with("//wsl"),
            "WSL host-path must start with the UNC prefix; got: {host_str}"
        );
    }

    /// Test: when worktree_name is None, resolve_agent_path returns base_path directly
    /// (i.e., no .claude/worktrees/ subdirectory)
    #[test]
    fn resolve_agent_path_with_none_worktree_returns_base_path() {
        let base = "/home/user/my-repo";
        let resolved = resolve_agent_path(base, None);

        // Should NOT contain worktrees subdirectory
        assert!(!resolved.host_path.contains("worktrees"),
            "Expected base path without worktree subdir, got: {}", resolved.host_path);
        assert!(!resolved.spawn_path.contains("worktrees"),
            "Expected base path without worktree subdir, got: {}", resolved.spawn_path);
    }

    /// Test: when worktree_name is Some("foo"), resolve_agent_path returns
    /// {base}/.claude/worktrees/foo
    #[test]
    fn resolve_agent_path_with_some_worktree_returns_worktree_path() {
        let base = "/home/user/my-repo";
        let resolved = resolve_agent_path(base, Some("foo"));

        // Path should contain worktrees subdirectory and the specific worktree name
        assert!(resolved.host_path.contains("worktrees") && resolved.host_path.contains("foo"),
            "Expected worktree subdir, got: {}", resolved.host_path);
        assert!(resolved.spawn_path.contains("worktrees") && resolved.spawn_path.contains("foo"),
            "Expected worktree subdir, got: {}", resolved.spawn_path);
    }

    /// Test: when worktree_name is Some(""), it's treated as no worktree
    #[test]
    fn resolve_agent_path_with_empty_worktree_returns_base_path() {
        let base = "/home/user/my-repo";
        let resolved = resolve_agent_path(base, Some(""));

        assert!(!resolved.host_path.contains(".claude/worktrees"),
            "Expected base path without worktree subdir, got: {}", resolved.host_path);
    }

    /// Test: resolve_agent_path works with Windows paths too
    #[test]
    fn resolve_agent_path_with_windows_path() {
        let base = "C:\\Users\\user\\my-repo";
        let resolved = resolve_agent_path(base, None);

        // Should return a valid path without crashing
        assert!(!resolved.host_path.is_empty());
        assert!(!resolved.spawn_path.is_empty());
    }

    /// Test: resolve_agent_path works with WSL paths
    #[test]
    fn resolve_agent_path_with_wsl_path() {
        let base = "/mnt/c/Users/user/my-repo";
        let resolved = resolve_agent_path(base, None);

        // Should return a valid path without crashing
        assert!(!resolved.host_path.is_empty());
        assert!(!resolved.spawn_path.is_empty());
    }

    /// Minimal Agent Node fixture; `use_worktree`/`worktree_name` are the only
    /// fields the Node Working Directory rule reads. `..Default::default()`
    /// covers the rest so future optional columns don't reopen this fixture
    /// (issue #457).
    fn node(use_worktree: bool, worktree_name: Option<&str>) -> AgentNode {
        AgentNode {
            path: "/home/user/my-repo".to_string(),
            worktree_name: worktree_name.map(str::to_string),
            use_worktree,
            ..Default::default()
        }
    }

    /// A Worktree Node resolves into its `.claude/worktrees/<name>` dir.
    #[test]
    fn node_working_path_for_worktree_node_resolves_worktree_dir() {
        let resolved = node_working_path(&node(true, Some("gentle-fox")));
        assert!(
            resolved.host_path.contains("worktrees") && resolved.host_path.contains("gentle-fox"),
            "expected worktree dir, got: {}",
            resolved.host_path
        );
    }

    /// A Root Node resolves to the Mesh root — never a worktree subdir.
    #[test]
    fn node_working_path_for_root_node_resolves_mesh_root() {
        let resolved = node_working_path(&node(false, Some("ignored")));
        assert!(
            !resolved.host_path.contains("worktrees"),
            "root node must not resolve into a worktree, got: {}",
            resolved.host_path
        );
    }

    /// The canonical rule trims the worktree name. This is the behaviour
    /// `pr::node_working_path` lacked (it fed an untrimmed name straight to
    /// `resolve_agent_path`), so a name with stray whitespace resolved to a
    /// different directory than diff/close-safety used. One resolver, one rule.
    #[test]
    fn node_working_path_trims_worktree_name() {
        let trimmed = node_working_path(&node(true, Some("foo")));
        let padded = node_working_path(&node(true, Some("  foo  ")));
        assert_eq!(padded.host_path, trimmed.host_path);
    }

    /// A whitespace-only worktree name collapses to "no worktree".
    #[test]
    fn node_working_path_blank_worktree_name_is_root() {
        let resolved = node_working_path(&node(true, Some("   ")));
        assert!(!resolved.host_path.contains("worktrees"));
    }

    /// `node_worktree_path` is `Some` only for a Worktree Node; the `None` for a
    /// Root Node is what close-safety and removal lean on to skip root nodes.
    #[test]
    fn node_worktree_path_is_some_for_worktree_none_for_root() {
        assert!(node_worktree_path(&node(true, Some("gentle-fox"))).is_some());
        assert!(node_worktree_path(&node(false, Some("gentle-fox"))).is_none());
        assert!(node_worktree_path(&node(true, None)).is_none());
        assert!(node_worktree_path(&node(true, Some("   "))).is_none());
    }

    /// When present, the worktree path agrees with the working path (it's the
    /// same dir — `node_worktree_path` is just the `Option` view of it).
    #[test]
    fn node_worktree_path_agrees_with_working_path() {
        let n = node(true, Some("gentle-fox"));
        assert_eq!(
            node_worktree_path(&n).map(|r| r.host_path),
            Some(node_working_path(&n).host_path)
        );
    }

    // ----- active_node_paths (#607 / #621) -----
    //
    // `n.path` alone is the mesh root. A Worktree Node's work lives at
    // `<mesh>/.claude/worktrees/<name>` — that subdir must also enter the
    // active set, or `path_is_active` matches every linked worktree against
    // the mesh root alone and flags them all `is_active: false` in both
    // the Worktree Manager (#607) and Mesh Health (#621). Delegating to
    // `node_worktree_path` keeps the one-rule invariant intact.

    /// Regression for #607 / #621: a Worktree Node must contribute BOTH its
    /// mesh path AND its resolved worktree dir, so the linked worktree on
    /// disk matches against the active set instead of being flagged
    /// inactive/stale.
    #[test]
    fn active_node_paths_includes_resolved_worktree_dir_for_worktree_nodes() {
        let paths = active_node_paths(&[node(true, Some("gentle-fox"))]);

        assert!(
            paths.iter().any(|p| p == "/home/user/my-repo"),
            "mesh path must be present so the main worktree still matches: {:?}",
            paths
        );
        assert!(
            paths.iter().any(|p| p.contains("gentle-fox")),
            "Worktree Node must contribute its resolved worktree dir (#607 / #621): {:?}",
            paths
        );
    }

    /// A Root Node has no worktree dir to add — only its mesh path participates.
    #[test]
    fn active_node_paths_root_node_contributes_only_mesh_path() {
        let paths = active_node_paths(&[node(false, None)]);

        assert_eq!(
            paths.len(),
            1,
            "root node contributes exactly one path: {:?}",
            paths
        );
        assert_eq!(paths[0], "/home/user/my-repo");
    }

    /// A whitespace-only `worktree_name` collapses to "no worktree" per the
    /// canonical rule in `node_worktree_path`, so it contributes only the
    /// mesh path — same as a Root Node.
    #[test]
    fn active_node_paths_blank_worktree_name_contributes_only_mesh_path() {
        let paths = active_node_paths(&[node(true, Some("   "))]);

        assert_eq!(
            paths.len(),
            1,
            "blank worktree name is treated as no worktree: {:?}",
            paths
        );
        assert_eq!(paths[0], "/home/user/my-repo");
    }

    // ----- raw_path contract (issue #409) -----
    //
    // `raw_path` is the POSIX-style effective path (input to `to_host_path` /
    // `to_spawn_path`) and the string the GIT_CHANGED payload carries to the
    // frontend for `getNodeGitPath()` to subscribe on. These assertions pin
    // that `env::node_working_path` is now the SOLE Rust definition of the
    // worktree rule — `file_watcher::node_internal_path` was deleted and
    // consumes `raw_path` instead. If any case here drifts, the GIT_CHANGED
    // match contract breaks and changed-files go stale (issue #387).

    /// Worktree Node: raw_path is `<base>/.claude/worktrees/<trimmed_name>`.
    #[test]
    fn raw_path_for_worktree_node_is_worktree_subdir() {
        assert_eq!(
            node_working_path(&node(true, Some("gentle-fox"))).raw_path,
            "/home/user/my-repo/.claude/worktrees/gentle-fox"
        );
    }

    /// Root Node: raw_path is the Mesh root regardless of a stale
    /// `worktree_name`. Regression: without the `use_worktree` gate, a Root
    /// Node with a stale `worktree_name` emitted a worktree subdir the
    /// frontend never subscribed to (issue #383).
    #[test]
    fn raw_path_for_root_node_ignores_stale_worktree_name() {
        assert_eq!(
            node_working_path(&node(false, Some("gentle-fox"))).raw_path,
            "/home/user/my-repo"
        );
    }

    /// No worktree name → Mesh root.
    #[test]
    fn raw_path_without_worktree_name_is_mesh_root() {
        assert_eq!(
            node_working_path(&node(true, None)).raw_path,
            "/home/user/my-repo"
        );
    }

    /// Padded `worktree_name` is trimmed (parity with the frontend's
    /// `getNodeGitPath()` in `src/lib/paths.ts`, issue #387).
    #[test]
    fn raw_path_for_padded_worktree_name_is_trimmed() {
        assert_eq!(
            node_working_path(&node(true, Some("  gentle-fox  "))).raw_path,
            "/home/user/my-repo/.claude/worktrees/gentle-fox"
        );
    }

    /// Whitespace-only worktree name trims to empty → Mesh root.
    #[test]
    fn raw_path_for_whitespace_only_worktree_name_is_mesh_root() {
        assert_eq!(
            node_working_path(&node(true, Some("   "))).raw_path,
            "/home/user/my-repo"
        );
    }

    /// `raw_path` is the input to `to_host_path` / `to_spawn_path` — the
    /// "pre-transform" form. The raw/host/spawn triple is internally
    /// consistent in that `raw_path` does NOT go through `to_host_path`; a
    /// regression that routed `raw_path` through that conversion would
    /// produce a UNC-shaped string on Windows and break the GIT_CHANGED
    /// match contract (which depends on `raw_path` matching
    /// `getNodeGitPath()` in the TS layer). This single value assertion
    /// pins that contract for the standard Worktree Node fixture: if
    /// `raw_path` ever diverges from `{base}/.claude/worktrees/{trimmed}`,
    /// the regression is caught here.
    #[test]
    fn raw_path_is_effective_path_not_host_or_spawn_form() {
        let n = node(true, Some("gentle-fox"));
        let resolved = node_working_path(&n);
        assert_eq!(resolved.raw_path, "/home/user/my-repo/.claude/worktrees/gentle-fox");
    }

    // ----- wsl_login_shell helper (issue #548) -----
    //
    // `parse_login_shell_from_passwd` is the pure function behind
    // `wsl_login_shell()`. The impure wrapper runs `wsl.exe` once per session
    // (cached via `Lazy`); on a Linux CI host it always returns `None`
    // because `wsl.exe` doesn't exist. These tests pin the parsing rules so
    // a regression in the helper can't silently hand the Terminal adapter
    // `/usr/sbin/nologin` and crash the spawn.

    /// Real-world `getent passwd` line for an ohmyzsh user. Field 7 is the
    /// absolute path to the login shell; everything before it must be ignored.
    #[test]
    fn parse_login_shell_extracts_field_7_with_gecos() {
        assert_eq!(
            parse_login_shell_from_passwd("alice:x:1000:1000:Alice Smith:/home/alice:/usr/bin/zsh"),
            Some("/usr/bin/zsh".to_string())
        );
    }

    /// `getent passwd` line without a GECOS field (the 5th field is empty).
    /// Some distros / NSS backends omit GECOS for system accounts; the parser
    /// must not require it.
    #[test]
    fn parse_login_shell_extracts_field_7_without_gecos() {
        assert_eq!(
            parse_login_shell_from_passwd("user:x:1000:1000::/home/user:/bin/bash"),
            Some("/bin/bash".to_string())
        );
    }

    /// Trailing newline (and any other trailing whitespace) must be trimmed.
    #[test]
    fn parse_login_shell_trims_trailing_whitespace() {
        assert_eq!(
            parse_login_shell_from_passwd("user:x:1000:1000::/home/user:/usr/bin/fish\n"),
            Some("/usr/bin/fish".to_string())
        );
    }

    /// Service accounts whose login shell is `/usr/sbin/nologin` must
    /// collapse to `None` — spawning that would exit immediately.
    #[test]
    fn parse_login_shell_rejects_nologin() {
        assert_eq!(
            parse_login_shell_from_passwd("ftp:x:114:120:ftp daemon:/srv/ftp:/usr/sbin/nologin"),
            None
        );
    }

    /// `nobody` is conventionally `/bin/false` and must also collapse to `None`.
    #[test]
    fn parse_login_shell_rejects_false() {
        assert_eq!(
            parse_login_shell_from_passwd("nobody:x:65534:65534::/:/bin/false"),
            None
        );
    }

    /// An empty 7th field (a malformed passwd entry) must collapse to `None`,
    /// not the empty string.
    #[test]
    fn parse_login_shell_rejects_empty_shell() {
        assert_eq!(
            parse_login_shell_from_passwd("user:x:1000:1000::/home/user:"),
            None
        );
    }

    /// Lines with fewer than 7 fields are malformed — return `None` rather
    /// than panic on the missing `nth(6)`.
    #[test]
    fn parse_login_shell_rejects_too_few_fields() {
        assert_eq!(parse_login_shell_from_passwd(""), None);
        assert_eq!(parse_login_shell_from_passwd("user"), None);
        assert_eq!(parse_login_shell_from_passwd("user:x:1000:1000::/home/user"), None);
    }

    /// The cached lookup must be safe to call and must return an `Option<&'static str>`
    /// (not panic) — on a host where WSL is unavailable it is `None`, on a
    /// Windows+WSL host it is `Some("/usr/bin/zsh")`. We only assert the type
    /// and that it doesn't panic; behavioural pinning lives in the
    /// `parse_login_shell_from_passwd` tests above.
    #[test]
    fn wsl_login_shell_returns_option_without_panicking() {
        let _ = wsl_login_shell();
    }
}
