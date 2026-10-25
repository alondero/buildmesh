//! Maintain worktree paths, checkout state, exclusions, and cross-runtime Git links.

use super::*;
/// Keep the parent repository's `git status` clean when the worktree
/// container lives inside it (issue #1519).
///
/// Linked worktrees are NOT auto-ignored by git: a relative container like
/// `custom-wt/` shows up as an untracked directory, `primitives::is_dirty`
/// flips true, spawn-time auto-sync aborts, and health reports permanently
/// dirty. Appending `<rel>/` to the
/// repo-local `.git/info/exclude` — never the committed `.gitignore` — keeps
/// the user's committed ignore rules untouched.
///
/// `container_host_path` is the HOST-form container dir itself (not a worktree
/// path — callers pass the parent). No-op when the container is the root
/// itself, outside the root (absolute custom dirs), or when `.git` is
/// missing. Linked worktrees and submodules use the repository's common dir.
/// Idempotent: an existing matching line is never duplicated. Best-effort
/// like `apply_worktree_include` — filesystem errors log and return so a
/// strict exclude write can never fail a spawn.
pub(crate) fn ensure_container_excluded(project_root: &str, container_host_path: &str) {
    fn squash(p: &str) -> String {
        let mut n = p.replace('\\', "/");
        while n.len() > 1 && n.ends_with('/') {
            n.pop();
        }
        n
    }
    let Ok(project) = crate::git::project::resolve(project_root) else {
        return;
    };
    let root = squash(&project.working_root);
    let container = squash(container_host_path);
    // Prefix check is case-insensitive on Windows (case-insensitive FS);
    // the written pattern keeps its original case (git honors
    // core.ignorecase there, and exact case is required elsewhere).
    let under_root = container.len() > root.len()
        && container.as_bytes().get(root.len()) == Some(&b'/')
        && if cfg!(target_os = "windows") {
            container[..root.len()].eq_ignore_ascii_case(&root)
        } else {
            container.starts_with(root.as_str())
        };
    if !under_root {
        return;
    }
    let rel = container[root.len() + 1..].to_string();
    if rel.is_empty() {
        return;
    }
    let Ok(repo) = crate::git::project::open(project_root) else {
        return;
    };
    let exclude = repo.commondir().join("info").join("exclude");
    let pattern = format!("{rel}/");
    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    if existing.lines().any(|line| line.trim() == pattern) {
        return;
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("# buildmesh: worktree container (issue #1519)\n");
    out.push_str(&pattern);
    out.push('\n');
    if let Some(parent) = exclude.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if let Err(e) = std::fs::write(&exclude, out) {
        tracing::warn!(
            "ensure_worktree_container_excluded: failed to update {}: {}",
            exclude.to_string_lossy(),
            e
        );
    }
}

/// Move (and rename) an existing worktree to a new directory via
/// `git worktree move`. This relocates the working tree on disk AND rewrites
/// git's internal references — the per-worktree admin `gitdir` pointer and the
/// worktree's own `.git` file — so every git command keeps working at the new
/// path. The warm pool (issue #612) uses it to adopt a pre-warmed plain-slug
/// worktree under an Issue/PR node's `gh{N}-`/`pr{N}-` target name without
/// paying the cold `git worktree add` checkout cost.
///
/// Shells out to the git CLI for the same reason [`add_worktree_impl`] does —
/// libgit2 exposes no worktree-move API. `project_root` is the mesh repo; the
/// two paths are the worktree's current and target host directories. Git
/// refuses to overwrite an existing target and needs the target's parent to
/// exist; both hold for the pool's sibling layout under `.claude/worktrees/`.
pub fn move_git_worktree(
    project_root: &str,
    old_host_path: &str,
    new_host_path: &str,
) -> Result<(), String> {
    // Normalise all three to host format before handing them to the Windows
    // git CLI — a WSL-style path would otherwise reach git.exe unconverted.
    // `to_host_path` is idempotent on an already-host path, so today's callers
    // (which pass host paths) are unaffected.
    let host_root = to_host_path(project_root);
    let old = to_host_path(old_host_path);
    let new = to_host_path(new_host_path);

    // Guard against `git worktree move`'s `mv`-style nesting: when the target
    // directory already exists, git moves the source *inside* it
    // (`<target>/<src-basename>`) and exits 0 rather than refusing. The spawn
    // path only calls this when the target is free, but refusing explicitly
    // keeps a stray leftover from silently producing a nested worktree.
    if std::path::Path::new(&new).exists() {
        return Err(format!("git worktree move target already exists: {}", new));
    }

    let repo = crate::git::project::open(&host_root).map_err(|e| e.to_string())?;
    crate::git::trust::ensure_linked_worktree(&repo, std::path::Path::new(&old))
        .map_err(|e| e.to_string())?;
    let old_trust_path = std::fs::canonicalize(&old).map_err(|e| e.to_string())?;

    let mut cmd = crate::process_util::git_command();
    cmd.arg("-C")
        .arg(
            repo.workdir()
                .ok_or("Cannot move worktree from a bare repository")?,
        )
        .arg("worktree")
        .arg("move")
        .arg(&old)
        .arg(&new);

    let output = run_bounded_git(cmd, "git worktree move", GIT_LOCAL_TIMEOUT)?;
    if !output.status.success() {
        return Err(format!(
            "git worktree move failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    // The source no longer exists after a successful move, including when
    // destination trust fails and the caller retains it for recovery.
    if let Err(error) = crate::git::trust::forget_worktree(&old_trust_path) {
        tracing::warn!(%error, "Could not revoke moved worktree's Buildmesh Git trust");
    }
    crate::git::trust::ensure_linked_worktree(&repo, std::path::Path::new(&new))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Hard-reset a warm pool worktree onto `sha` so it tracks a freshly-fetched
/// base ref (issue #613 ref-freshness). Runs `git -C <worktree> reset --hard
/// <sha>` — a detached-HEAD warm worktree owns no branch and has no local work
/// to preserve, so a hard reset is exactly right: it moves HEAD and the working
/// tree to the new commit, discarding the (identical-up-to-the-old-base) tree.
///
/// Shells out to the git CLI rather than libgit2 for parity with the other
/// worktree mutators here (`move_git_worktree`) and because the CLI's
/// `reset --hard` is the canonical, well-understood operation. `worktree_path`
/// is normalised to host format first so a WSL-style path never reaches
/// git.exe unconverted.
pub fn reset_warm_worktree(worktree_path: &str, sha: &str) -> Result<(), String> {
    let host = to_host_path(worktree_path);

    let mut cmd = crate::process_util::git_command();
    cmd.arg("-C").arg(&host).arg("reset").arg("--hard").arg(sha);

    let output = run_bounded_git(cmd, "git reset --hard", GIT_LOCAL_TIMEOUT)?;
    if !output.status.success() {
        return Err(format!(
            "git reset --hard {} failed in {}: {}",
            sha,
            host,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Convert a worktree link-file path to the target environment's format
/// (Windows drive path vs WSL `/mnt/...`). Shared by both halves of
/// [`sanitize_git_worktree`] so the gitlink and the back-pointer can't drift
/// onto different conversion rules.
///
/// Both arms delegate to the single canonical converter in `env::host_path`:
/// - WSL → `windows_to_wsl`, which knows about generic drives (`F:\...`,
///   `D:\...`) and Git-Bash drive style (`/f/...`) and preserves case.
/// - Windows → `to_host_path`, which handles `/mnt/`, `/home/`, and
///   `/<letter>/` shapes.
///
/// This is a thin dispatcher; building WSL `/mnt/` or UNC strings here
/// would violate the CLAUDE.md hard rule that `env::host_path` is the
/// SOLE module allowed to construct them (issue #1226).
pub(super) fn convert_link_path_for_env(path: &str, env_type: EnvType) -> String {
    match env_type {
        EnvType::Wsl => windows_to_wsl(path),
        EnvType::Windows | EnvType::WindowsInterop => to_host_path(path),
    }
}

/// Whether the worktree named `worktree_name` is registered in the
/// repository at `host_path`.
///
/// The registration lives in git metadata, not on disk — a directory that
/// looks like a worktree but was never registered is a broken worktree, and
/// the build runner must not treat it as usable.
pub fn worktree_registered_at(host_path: &str, worktree_name: &str) -> Result<bool, String> {
    let repo = git2::Repository::open(host_path)
        .or_else(|_| git2::Repository::discover(host_path))
        .map_err(|e| format!("Failed to open repository: {}", e))?;

    let worktrees = repo
        .worktrees()
        .map_err(|e| format!("Failed to list worktrees: {}", e))?;

    for index in 0..worktrees.len() {
        if let Ok(Some(name)) = worktrees.get(index) {
            if name == worktree_name {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Sanitize a worktree's git link files to the correct path format for the
/// target environment (Windows vs WSL). Repairs BOTH sides of the link:
///
/// - the worktree's `.git` gitlink (`gitdir: <admin dir>`), and
/// - the repo-side back-pointer (`<admin dir>/gitdir` → `<worktree>/.git`).
///
/// The back-pointer matters because libgit2 resolves it when opening a
/// worktree: an MSYS-flavoured git writes Git-Bash-style `/f/...` paths that
/// the Git CLI tolerates but `git2::Repository::open` reports as NotFound.
/// In the 2026-07-17 gh252 incident that mismatch made the autopilot wrap-up
/// verifier see a healthy, already-PR'd worktree as unopenable; fixing only
/// the gitlink (this function's pre-incident behaviour) left libgit2 locked
/// out through the admin side.
pub fn sanitize_git_worktree(worktree_host_path: &str, env_type: EnvType) -> Result<(), String> {
    let git_file_path = std::path::Path::new(worktree_host_path).join(".git");

    if !git_file_path.is_file() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&git_file_path)
        .map_err(|e| format!("Failed to read .git file: {}", e))?;

    if !content.starts_with("gitdir: ") {
        return Ok(());
    }

    let git_dir_path = content.trim_start_matches("gitdir: ").trim();
    if git_dir_path.is_empty() {
        return Err("invalid .git file: empty gitdir path".to_string());
    }

    let new_path = convert_link_path_for_env(git_dir_path, env_type);

    if new_path != git_dir_path {
        tracing::info!("Sanitizing .git file: {} -> {}", git_dir_path, new_path);
        // Ensure we use Unix line endings for the .git file as Git expects
        std::fs::write(&git_file_path, format!("gitdir: {}\n", new_path))
            .map_err(|e| format!("Failed to write sanitized .git file: {}", e))?;
    }

    // Repo-side back-pointer. The admin dir itself is a host filesystem
    // access, so resolve it host-side regardless of the target env format.
    let backpointer_path = std::path::Path::new(&to_host_path(&new_path)).join("gitdir");
    if backpointer_path.is_file() {
        let existing = std::fs::read_to_string(&backpointer_path)
            .map_err(|e| format!("Failed to read worktree gitdir back-pointer: {}", e))?;
        let existing = existing.trim();
        let converted = convert_link_path_for_env(existing, env_type);
        if converted != existing {
            tracing::info!(
                "Sanitizing worktree gitdir back-pointer: {} -> {}",
                existing,
                converted
            );
            std::fs::write(&backpointer_path, format!("{}\n", converted))
                .map_err(|e| format!("Failed to write sanitized gitdir back-pointer: {}", e))?;
        }
    }

    Ok(())
}

/// A cross-runtime worktree keeps administration on the host. Older Git
/// accepts relative forward links, but treats relative backpointers as missing
/// worktrees. Lock the administrative entry so guest pruning cannot delete it.
/// Buildmesh's removal path already explicitly prunes locked entries.
pub(crate) fn prepare_cross_runtime_worktree(worktree_host_path: &str) -> Result<(), String> {
    if !std::path::Path::new(worktree_host_path)
        .join(".git")
        .is_file()
    {
        return Ok(());
    }
    sanitize_git_worktree(worktree_host_path, EnvType::Windows)?;
    let repo = Repository::open(worktree_host_path).map_err(|e| e.to_string())?;
    if !repo.is_worktree() {
        return Ok(());
    }
    let root = std::fs::canonicalize(worktree_host_path).map_err(|e| e.to_string())?;
    let admin = std::fs::canonicalize(repo.path()).map_err(|e| e.to_string())?;
    let root_parts = root.components().collect::<Vec<_>>();
    let admin_parts = admin.components().collect::<Vec<_>>();
    let shared = root_parts
        .iter()
        .zip(&admin_parts)
        .take_while(|(a, b)| a == b)
        .count();
    if shared == 0 {
        return Err("Cross-runtime worktrees must share a filesystem with their repository".into());
    }
    let mut relative = std::path::PathBuf::new();
    for _ in shared..root_parts.len() {
        relative.push("..");
    }
    for part in &admin_parts[shared..] {
        relative.push(part.as_os_str());
    }
    let worktree = git2::Worktree::open_from_repository(&repo).map_err(|e| e.to_string())?;
    if matches!(worktree.is_locked(), Ok(git2::WorktreeLockStatus::Unlocked)) {
        worktree
            .lock(Some(
                "Buildmesh cross-runtime worktree; manage through Buildmesh",
            ))
            .map_err(|e| e.to_string())?;
    }
    std::fs::write(
        root.join(".git"),
        format!(
            "gitdir: {}\n",
            relative.to_string_lossy().replace('\\', "/")
        ),
    )
    .map_err(|e| e.to_string())
}
