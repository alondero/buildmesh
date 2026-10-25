//! Copy explicitly included files/directories into an owned worktree.

use super::*;
/// Copy the files listed in `<project_root>/.worktreeinclude` into a freshly
/// materialised worktree. Best-effort: a missing manifest is a no-op and a
/// failed copy is logged, never fatal — the worktree is already usable without
/// the extras.
///
/// Shared by the cold `create_git_worktree` path AND the warm-pool adoption
/// path (issue #612). The warm pool pre-copies these at prewarm time, but the
/// referenced files (local config, env files, build artifacts) can change
/// before an Issue/PR spawn adopts the entry, so re-applying on adoption keeps
/// an adopted worktree byte-for-byte equivalent to a cold-spawned one.
pub(crate) fn apply_worktree_include(project_root: &str, host_path: &std::path::Path) {
    let include_root = crate::git::project::resolve(project_root)
        .map(|project| project.working_root)
        .unwrap_or_else(|_| to_host_path(project_root));
    let project_root = include_root.as_str();
    let include_file_path = std::path::Path::new(project_root).join(".worktreeinclude");
    if !include_file_path.exists() {
        return;
    }
    tracing::info!("Found .worktreeinclude, copying included files...");
    let Ok(content) = std::fs::read_to_string(&include_file_path) else {
        return;
    };
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let src = std::path::Path::new(project_root).join(trimmed);
        let dest = host_path.join(trimmed);

        if src.exists() {
            if let Some(parent) = dest.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            if src.is_file() {
                if let Err(e) = std::fs::copy(&src, &dest) {
                    tracing::warn!(
                        "Failed to copy included file {} -> {}: {}",
                        src.display(),
                        dest.display(),
                        e
                    );
                } else {
                    tracing::info!("Copied included file: {}", trimmed);
                }
            } else if src.is_dir() {
                // #248: directory entries in `.worktreeinclude` are now
                // recursively copied rather than log-and-skipped. The previous
                // "not yet implemented" branch silently dropped `config/`,
                // `build/`, `.env/` and similar untracked subtrees the agent
                // needed, forcing users into per-file `.worktreeinclude`
                // entries. `copy_dir_all` is std::fs-only (no `walkdir`/`fs_extra`
                // dep — the tree doesn't carry either). Pinned by
                // `apply_worktree_include_copies_directory_recursively` and
                // `apply_worktree_include_directory_missing_source_is_noop`.
                if let Err(e) = copy_dir_all(&src, &dest) {
                    tracing::warn!(
                        ".worktreeinclude: dir copy {} -> {} failed: {}",
                        trimmed,
                        dest.display(),
                        e
                    );
                } else {
                    tracing::info!("Copied included dir: {}", trimmed);
                }
            }
        }
    }
}

/// Recursive `cp -r` for the `.worktreeinclude` directory-copy case (#248).
///
/// The `env::apply_worktree_include` walker hits `src.is_dir()` for entries
/// like `config/`, `build/`, or `.env/` — previously these were log-and-skipped
/// (mod.rs:215), forcing users into per-file entries. `copy_dir_all` walks the
/// source subtree and mirrors it under `dest`, copying regular files and
/// recursing into subdirectories. Symlinks, FIFOs, and sockets are
/// intentionally skipped (carrying those into a freshly-checked-out worktree
/// would be a surprise — a symlink to `../../secrets.env` is a leakage vector,
/// not a feature).
///
/// **No `walkdir` dep:** the tree doesn't depend on it (`Cargo.toml`), and the
/// handful of entries in any real `.worktreeinclude` (typically 1-3 dirs, 10-50
/// files total) don't justify a new crate. `std::fs::read_dir` + a small
/// recursive helper is the right shape.
///
/// Errors propagate up: a single failed entry bubbles; the caller
/// (`apply_worktree_include`) logs and continues rather than failing the worktree
/// materialisation (the agent can still operate on the parts that did copy).
pub(crate) fn copy_dir_all(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    if !dest.exists() {
        std::fs::create_dir_all(dest)?;
    }
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let src_child = entry.path();
        let dest_child = dest.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_all(&src_child, &dest_child)?;
        } else if ft.is_file() {
            std::fs::copy(&src_child, &dest_child)?;
        }
        // symlinks / fifos / sockets: silently skipped (see fn doc).
    }
    Ok(())
}
