//! Host-path conversion between the session-internal path forms and the
//! host-native form.
//!
//! This module is the **only** place in the Buildmesh tree that builds
//! `\\wsl$\...` UNC strings — the CLAUDE.md hard rule is structurally
//! enforced here, not by convention.
//!
//! It also owns the [`ResolvedPath`] machinery: the trio of `host_path` /
//! `spawn_path` / `raw_path` plus the [`Environment`] classification that
//! callers need to drive spawn commands, file operations, and filesystem
//! subscriptions on Windows / WSL hybrid hosts.
//!
//! ## Why a separate module from environment
//!
//! [`crate::env::Environment`] (in `environment.rs`) answers "what
//! environment are we in?". This module answers "given that environment, how
//! do I rewrite a path so the host tools can open it?". Splitting them keeps
//! detection silent on path conversion and conversion silent on detection —
//! a future change to either can't silently drag the other along.
//!
//! ## Adding a new path conversion
//!
//! Every `\\wsl$\`-shaped string and every `/mnt/c/` rewrite lands here, no
//! exceptions. The `.claude/hooks/guard-antipatterns.mjs` lint enforces this
//! for Windows-style paths via `// allow-wsl-path` per-line escape — that's
//! the side of the rule, this module is the open side.

use std::path::{Path, PathBuf};

use crate::models::{AgentNode, EnvType, SessionStatus};

use super::{current_env, Environment};

// ── Path-conversion primitives ─────────────────────────────────────────────

/// Convert a session path to the correct form for spawning commands
/// WSL paths are stored as Unix paths internally, Windows paths as Windows paths.
/// Stored paths are always the RAW form (see `resolve_raw_path`), so a host
/// UNC string must never reach this function — normalize back to POSIX at
/// the storage boundary instead (issue #1519).
pub fn to_spawn_path(path: &Path) -> PathBuf {
    match current_env() {
        Environment::Wsl => {
            if path.to_string_lossy().starts_with("/mnt/") {
                path.to_path_buf()
            } else if path.to_string_lossy().starts_with("C:\\") || path.to_string_lossy().starts_with("c:\\") {
                let path_str = path.to_string_lossy().to_lowercase();
                let drive = path_str.chars().next().unwrap_or('c');
                let rest = &path_str[2..].replace('\\', "/");
                PathBuf::from(format!("/mnt/{}{}", drive, rest))
            } else {
                path.to_path_buf()
            }
        }
        Environment::Windows => {
            path.to_path_buf()
        }
    }
}

/// Determine the environment for a given session path
pub fn env_for_path(path: &Path) -> Environment {
    // WSL only exists on a Windows host. On macOS and native Linux every path is
    // host-native, so a `/home/...` or `/mnt/...` path must NOT be classified as
    // WSL — doing so on Linux would route spawns through `wsl.exe` and rewrite
    // paths to bogus `\\wsl$\...` UNC form. (`Environment::Windows` is the
    // "native host, no translation" variant here, despite the name.)
    if !cfg!(target_os = "windows") {
        return Environment::Windows;
    }

    let path_str = path.to_string_lossy().to_lowercase();

    // WSL detection: paths starting with /mnt/, /home/, or \\wsl$
    if path_str.starts_with("/mnt/")
        || path_str.starts_with("/home/")
        || path_str.starts_with("\\\\wsl$")
    {
        Environment::Wsl
    } else {
        Environment::Windows
    }
}

/// Convert a path from session internal form to host-readable form
/// (e.g., /home/user -> \\wsl$\Ubuntu\home\user, /mnt/c/Users -> C:\Users, /c/Users -> C:\Users)
pub fn to_host_path(path: &str) -> String {
    // Only a Windows host has a WSL filesystem to translate into (`\\wsl$\...`
    // UNC paths, drive letters). On macOS and native Linux the path is already
    // host-native, so return it unchanged — otherwise a normal Linux
    // `/home/...` path would be rewritten to a `\\wsl$\...` UNC path that no
    // git2/file op on that host can open.
    if !cfg!(target_os = "windows") {
        return path.to_string();
    }

    if path.starts_with('/') {
        if path.starts_with("/mnt/") {
            // /mnt/c/Users -> C:\Users
            let drive = path.chars().nth(5).unwrap_or('c').to_uppercase().next().unwrap();
            format!("{}:{}", drive, path[6..].replace('/', "\\"))
        } else if path.starts_with("/home/") {
            // /home/user -> \\wsl$\Ubuntu\home\user
            let distro = super::environment::get_default_wsl_distro()
                .unwrap_or_else(|| "Ubuntu".to_string());
            format!("\\\\wsl$\\{}{}", distro, path.replace('/', "\\"))
        } else if path.len() >= 2 && path.chars().nth(1).unwrap().is_alphabetic() && (path.len() == 2 || path.chars().nth(2) == Some('/')) {
            // Handle Git Bash style /c/Users/ or /c
            let drive = path.chars().nth(1).unwrap().to_uppercase().next().unwrap();
            let rest = if path.len() > 2 { &path[2..] } else { "" };
            format!("{}:{}", drive, rest.replace('/', "\\"))
        } else {
            // Other Unix-style absolute path on Windows (e.g. /Users/...)
            // Return as-is, caller will handle if needed.
            path.to_string()
        }
    } else {
        path.to_string()
    }
}

/// The host-accessible Codex rollout directory for an agent environment.
/// CLI-home selection belongs to `environment`; this module owns the WSL path
/// conversion needed before the host reads that directory.
pub(crate) fn codex_sessions_dir(env_type: EnvType, spawn_path: &str) -> Option<PathBuf> {
    let home = super::environment::codex_dir_for_env(env_type, spawn_path)?;
    match env_type {
        EnvType::Windows => Some(home.join("sessions")),
        EnvType::Wsl => Some(
            PathBuf::from(to_host_path(&home.to_string_lossy())).join("sessions"),
        ),
    }
}

/// The host-accessible Command Code projects root for an agent environment
/// (`<home>/.commandcode/projects`). WSL homes are converted to host-readable
/// paths by the shared environment path module. Per-project slug formatting
/// lives with the transcript format (`transcript_reader::commandcode_project_slug`);
/// this module only owns the environment boundary (home lookup + WSL translation).
pub(crate) fn commandcode_projects_dir(env_type: EnvType, spawn_path: &str) -> Option<PathBuf> {
    let home = super::environment::commandcode_dir_for_env(env_type, spawn_path)?;
    match env_type {
        EnvType::Windows => Some(home.join("projects")),
        EnvType::Wsl => Some(
            PathBuf::from(to_host_path(&home.to_string_lossy())).join("projects"),
        ),
    }
}

/// The host-accessible Antigravity brain directory for an agent environment
/// (`<agy-home>/brain`, one subdirectory per conversation). CLI-home
/// selection belongs to `environment`; this module owns the WSL path
/// conversion needed before the host reads that directory (issue #1499).
pub(crate) fn agy_brain_dir_for_env(env_type: EnvType, spawn_path: &str) -> Option<PathBuf> {
    let home = super::environment::agy_dir_for_env(env_type, spawn_path)?;
    match env_type {
        EnvType::Windows => Some(home.join("brain")),
        EnvType::Wsl => Some(
            PathBuf::from(to_host_path(&home.to_string_lossy())).join("brain"),
        ),
    }
}

/// Normalize a raw path that may be a WSL UNC path (`\\wsl$\<distro>\...` or
/// `//wsl$/<distro>/...`) into WSL spawn form (`/...`). Non-UNC paths are
/// returned unchanged (borrowed — no allocation on the hot path).
///
/// Command Code runs inside WSL with cwd `/home/user/repo` and slugs that
/// form, but Buildmesh may hold the same mesh as a Windows host UNC path.
/// Slugging the UNC form would yield `wsl-ubuntu-home-user-repo` and silently
/// match nothing; normalize before slugging (issue #1500).
pub(crate) fn normalize_unc_to_wsl(path: &str) -> std::borrow::Cow<'_, str> {
    let bytes = path.as_bytes();
    if bytes.len() < 8 {
        return std::borrow::Cow::Borrowed(path);
    }
    let sep = |b: u8| b == b'\\' || b == b'/';
    if !sep(bytes[0]) || !sep(bytes[1]) {
        return std::borrow::Cow::Borrowed(path);
    }
    if !path.get(2..6).is_some_and(|s| s.eq_ignore_ascii_case("wsl$")) {
        return std::borrow::Cow::Borrowed(path);
    }
    if !sep(bytes[6]) {
        return std::borrow::Cow::Borrowed(path);
    }
    let rest = &path[7..];
    let distro_end = rest.find(['\\', '/']).map(|i| i + 7).unwrap_or(path.len());
    let after = if distro_end < path.len() {
        &path[distro_end..]
    } else {
        ""
    };
    if after.is_empty() {
        return std::borrow::Cow::Owned("/".to_string());
    }
    let mut out = String::with_capacity(after.len() + 1);
    out.push('/');
    let mut last_was_slash = true;
    for c in after.chars() {
        if c == '\\' || c == '/' {
            if !last_was_slash {
                out.push('/');
                last_was_slash = true;
            }
        } else {
            out.push(c);
            last_was_slash = false;
        }
    }
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    std::borrow::Cow::Owned(out)
}

/// Compare CLI-recorded working directories across Windows, WSL, and native
/// Unix path syntax. APFS/HFS+ is usually case-insensitive too.
pub fn directories_match(recorded: &str, spawn: &str) -> bool {
    let recorded = normalize_directory(recorded);
    let spawn = normalize_directory(spawn);
    if case_insensitive_fs(&recorded) || case_insensitive_fs(&spawn) {
        recorded.eq_ignore_ascii_case(&spawn)
    } else {
        recorded == spawn
    }
}

fn normalize_directory(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    normalized
}

fn case_insensitive_fs(path: &str) -> bool {
    cfg!(target_os = "macos") || looks_windows_volume(path)
}

fn looks_windows_volume(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 2 && bytes[1] == b':')
        || path.starts_with("//")
        || (path.len() >= 7
            && path
                .get(..5)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("/mnt/"))
            && path.as_bytes().get(6) == Some(&b'/'))
}

// ── ResolvedPath — high-level path resolution for agent operations ─────────

/// A fully-resolved set of paths for an agent node, ready for use by callers
/// without needing to compose env detection + host conversion + worktree logic.
#[derive(Debug, Clone)]
pub struct ResolvedPath {
    /// Host-accessible path for file system operations (e.g. Windows UNC path for WSL,
    /// or native path on macOS/Windows).
    pub host_path: String,
    /// Path to use as CWD when spawning agent/shell processes.
    pub spawn_path: String,
    /// The input `base_path`, optionally with a `/`-separated
    /// `.claude/worktrees/{trimmed_name}` appended for a Worktree Node. The
    /// "raw" form is input-pass-through — `node.path` is whatever was stored
    /// in the DB (POSIX `/home/...`, Windows native `C:\...`, or WSL UNC
    /// `\\wsl$\...`), and the worktree subdir is appended with `/` regardless
    /// of host. This is the input to `to_host_path` / `to_spawn_path` and
    /// the form the frontend's `getNodeGitPath` (in `src/lib/paths.ts`)
    /// mirrors for the GIT_CHANGED subscription. Exposed so callers like
    /// `file_watcher` can stop re-spelling the worktree rule and consume
    /// the single canonical authority (issue #409).
    pub raw_path: String,
    /// The detected environment type for this path.
    pub env_type: EnvType,
}

/// Resolve an already-computed raw path into host/spawn/env forms.
/// Single tail shared by `resolve_agent_path` (legacy layout) and the
/// configurable-directory path (issue #1519) — the only difference is
/// how `raw_path` was built; the env detection + host conversion is
/// identical so every consumer agrees on the same directory.
pub fn resolve_raw_path(raw_path: &str) -> ResolvedPath {
    // Detect environment from the effective path
    let path_buf = PathBuf::from(raw_path);
    let env_internal = env_for_path(&path_buf);
    let env_type = EnvType::from(env_internal);

    // On macOS and native Linux, paths are always host-native — no WSL
    // conversion needed. Only a Windows host translates based on the detected
    // environment (WSL agents need `\\wsl$\...` host paths + `/mnt/...` spawn
    // paths).
    let (host_path, spawn_path) = if !cfg!(target_os = "windows") {
        (raw_path.to_string(), raw_path.to_string())
    } else {
        let host = to_host_path(raw_path);
        let spawn = to_spawn_path(&path_buf).to_string_lossy().to_string();
        (host, spawn)
    };

    ResolvedPath {
        host_path,
        spawn_path,
        raw_path: raw_path.to_string(),
        env_type,
    }
}

/// Resolve the working directory for an agent node, accounting for worktree
/// layout and environment differences.
///
/// - `base_path`: The agent node's stored `path` field (project root).
/// - `worktree_name`: If set, the worktree subdirectory name under
///   `{base_path}/.claude/worktrees/{name}`.
///
/// Returns a `ResolvedPath` with host, spawn, and env fields populated.
///
/// Legacy layout preserved byte-for-byte for the no-config case (issue
/// #1519): with neither the Mesh override nor the application default
/// set, this is `{base}/.claude/worktrees/{trimmed}`. Configurable
/// directories go through `resolve_agent_path_in_dir` instead.
pub fn resolve_agent_path(base_path: &str, worktree_name: Option<&str>) -> ResolvedPath {
    // Compute the effective path (with worktree if applicable)
    // NOTE: intentionally does NOT trim `worktree_name` here — the
    // canonical trim lives in `worktree_segment` / `node_working_path`.
    // Direct callers pass already-trimmed values; keeping the raw
    // `is_empty` gate preserves the byte-for-byte legacy contract the
    // `env::mod` regression pins assert.
    let raw_path = match worktree_name {
        Some(wt_name) if !wt_name.is_empty() => {
            format!("{}/.claude/worktrees/{}", base_path, wt_name)
        }
        _ => base_path.to_string(),
    };

    resolve_raw_path(&raw_path)
}

// ── Configurable Worktree Node directories (issue #1519) ────────────────────

/// Default worktree container dir name under the Mesh root when neither
/// the Mesh override nor the application default is set.
pub const DEFAULT_WORKTREE_DIR_NAME: &str = ".claude/worktrees";

/// Trim raw user input for a `worktree_directory` setting and collapse
/// blank to `None` (inherit/default). No shell-variable or `~` expansion —
/// values are stored verbatim and joined literally at resolution time.
pub fn normalize_worktree_directory(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// True for a Windows drive-relative path: a single leading backslash with no
/// second (`\foo\bar` resolves against the current drive's root — it is
/// neither a usable absolute nor a mesh-relative path, so validation
/// rejects it outright instead of joining it onto the Mesh root and
/// landing on the drive root).
pub fn is_drive_relative_worktree_path(p: &str) -> bool {
    let bytes = p.trim().as_bytes();
    bytes.len() >= 2 && bytes[0] == b'\\' && bytes[1] != b'\\'
}

/// Characters no path segment may contain. The list is the Windows
/// forbidden set (`/` and `\` are the separators, handled separately);
/// POSIX forbids only `/`+NUL, so satisfying Windows keeps both hosts
/// safe. `~` and `$` are deliberately allowed — shell variables and
/// `~` are stored literally, never expanded.
const FORBIDDEN_WORKTREE_SEGMENT_CHARS: &[char] = &[':', '*', '?', '"', '<', '>', '|'];

/// Validate + normalize a RELATIVE `worktree_directory` value (shared by the
/// per-Mesh and application-default write paths). Leading/trailing
/// separators are stripped; every remaining segment must be non-empty (no
/// `a//b`) and must not be `.` / `..` (no escaping the Mesh root — rejects
/// `..`, `../..`, `../../etc`, `.`, and `sub/../..` alike) nor contain
/// forbidden characters. Returns the canonical stored form.
pub fn normalize_relative_worktree_dir(value: &str) -> Result<String, String> {
    let stripped = value.trim().trim_matches(['/', '\\']);
    if stripped.is_empty() {
        return Err(format!(
            "worktree directory '{value}' resolves to nothing — use a relative path like 'worktrees' or clear it to inherit"
        ));
    }
    let mut cleaned: Vec<&str> = Vec::new();
    for seg in stripped.split(['/', '\\']) {
        if seg.is_empty() {
            return Err(format!(
                "worktree directory '{value}' contains an empty segment (consecutive separators) — use a path like 'worktrees/sub'"
            ));
        }
        if seg == "." || seg == ".." {
            return Err(format!(
                "worktree directory '{value}' must stay inside the mesh — '.' and '..' segments are not allowed (got '{seg}')"
            ));
        }
        if let Some(bad) = seg.chars().find(|c| FORBIDDEN_WORKTREE_SEGMENT_CHARS.contains(c)) {
            return Err(format!(
                "worktree directory '{value}' contains forbidden character '{bad}' — use letters, numbers, '-', '_', or '.'"
            ));
        }
        cleaned.push(seg);
    }
    Ok(cleaned.join("/"))
}

/// True when `p` is an absolute path in either host environment:
/// POSIX `/...`, Windows drive `C:\` / `C:/`, or UNC `\\...`.
/// Relative values (including `~/...` and `$HOME/...`, which are NOT
/// expanded per the issue) return false and resolve from the Mesh root.
/// Drive-relative `\foo` (see [`is_drive_relative_worktree_path`]) is NOT
/// absolute — validation rejects it before it can reach resolution.
pub fn is_absolute_worktree_path(p: &str) -> bool {
    let t = p.trim();
    if t.is_empty() {
        return false;
    }
    if t.starts_with('/') {
        return true;
    }
    if t.starts_with("\\\\") || t.starts_with("//") {
        return true;
    }
    let bytes = t.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/') {
        return true;
    }
    // Bare `C:` (no slash) is a drive-relative path on Windows, not an
    // absolute one — treat as relative so it joins from the Mesh root
    // rather than silently resolving to the drive's CWD.
    false
}

/// Effective worktree container directory (raw form) for a Mesh.
///
/// Precedence: Mesh override → application default → `.claude/worktrees`
/// under the Mesh root. Relative values join from `mesh_path` with `/`
/// (matching the legacy `format!` spelling); absolute values are used with
/// trailing separators trimmed so `<dir>/<name>` never doubles up — the
/// same normalization [`validate_worktree_directory`] stores. Inputs are
/// trimmed; blank collapses to inherit/default. No shell/`~` expansion.
pub fn effective_worktree_dir_raw(
    mesh_path: &str,
    mesh_setting: Option<&str>,
    app_setting: Option<&str>,
) -> String {
    let chosen = normalize_worktree_directory(mesh_setting)
        .or_else(|| normalize_worktree_directory(app_setting));
    // A trailing separator on the stored root (e.g. from a CLI import)
    // must not produce `root//dir` — strip once here and in the
    // frontend mirror (`getEffectiveWorktreeDir`) so both spellings agree.
    let root = mesh_path.trim_end_matches(['/', '\\']);
    let root = if root.is_empty() { mesh_path } else { root };
    match chosen {
        None => format!("{}/{}", root, DEFAULT_WORKTREE_DIR_NAME),
        Some(dir) => {
            if is_absolute_worktree_path(&dir) {
                let trimmed = dir.trim_end_matches(['/', '\\']);
                if trimmed.is_empty() {
                    format!("{}/{}", root, DEFAULT_WORKTREE_DIR_NAME)
                } else {
                    trimmed.to_string()
                }
            } else {
                // Values reaching here passed `normalize_relative_worktree_dir`
                // at the write boundary (no leading/trailing separators, no
                // `.`/`..`/empty segments); trim defensively so a legacy row
                // still joins to exactly one separator.
                let trimmed_dir = dir.trim_matches(['/', '\\']);
                if trimmed_dir.is_empty() {
                    format!("{}/{}", root, DEFAULT_WORKTREE_DIR_NAME)
                } else {
                    format!("{}/{}", root, trimmed_dir)
                }
            }
        }
    }
}

/// Effective raw path for one Worktree Node: `<effective_dir>/<trimmed_name>`.
/// `effective_dir_raw` comes from `effective_worktree_dir_raw`; `node_name`
/// is trimmed and must be non-empty (callers gate on `worktree_segment`).
pub fn resolve_worktree_node_raw(
    effective_dir_raw: &str,
    node_name: &str,
) -> String {
    let trimmed = node_name.trim();
    let dir = effective_dir_raw.trim_end_matches(['/', '\\']);
    format!("{}/{}", dir, trimmed)
}

/// Resolve with an explicit effective directory (issue #1519).
/// `effective_dir_raw` is the container dir from
/// `effective_worktree_dir_raw`; `worktree_name` is the trimmed node slug.
/// `None`/empty resolves to the Mesh root (Root Node).
pub fn resolve_agent_path_in_dir(
    mesh_path: &str,
    effective_dir_raw: &str,
    worktree_name: Option<&str>,
) -> ResolvedPath {
    let raw_path = match worktree_name.map(str::trim).filter(|s| !s.is_empty()) {
        Some(name) => resolve_worktree_node_raw(effective_dir_raw, name),
        None => mesh_path.to_string(),
    };
    resolve_raw_path(&raw_path)
}

/// Validate a raw `worktree_directory` input for a Mesh.
///
/// - Trim; blank → `Ok(None)` (inherit/default — clearing restores
///   inheritance).
/// - Drive-relative `\foo` → `Err` (ambiguous drive root — neither usable
///   absolute nor mesh-relative).
/// - Relative → normalized via [`normalize_relative_worktree_dir`] (no
///   `.`/`..` escape, no forbidden characters) and resolved from the root.
/// - Absolute → trailing separators trimmed, filesystem roots rejected,
///   then must resolve to the same host environment (native/Windows versus
///   WSL) as `mesh_path`, else `Err` with an actionable message naming
///   both sides and suggesting a relative path.
/// - Never expands shell variables or `~` (treated literally).
pub fn validate_worktree_directory(
    mesh_path: &str,
    value: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(trimmed) = normalize_worktree_directory(value) else {
        return Ok(None);
    };
    if is_drive_relative_worktree_path(&trimmed) {
        return Err(format!(
            "worktree directory '{trimmed}' starts with a single backslash (drive-relative) — \
             use a relative path like 'worktrees' resolved from the mesh root, or a full absolute path"
        ));
    }
    if !is_absolute_worktree_path(&trimmed) {
        return normalize_relative_worktree_dir(&trimmed).map(Some);
    }
    let normalized = trimmed.trim_end_matches(['/', '\\']).to_string();
    if normalized.is_empty() || normalized.len() == 2 && normalized.as_bytes()[1] == b':' {
        return Err(format!(
            "worktree directory '{trimmed}' must not be the filesystem root — \
             use a relative path like 'worktrees' or a dedicated folder"
        ));
    }
    let mesh_env = env_for_path(&PathBuf::from(mesh_path));
    let dir_env = env_for_path(&PathBuf::from(&normalized));
    if mesh_env != dir_env {
        let (dir_kind, mesh_kind) = match dir_env {
            super::Environment::Wsl => ("WSL", "Windows (native)"),
            super::Environment::Windows => ("Windows (native)", "WSL"),
        };
        return Err(format!(
            "worktree directory '{normalized}' is a {} path but mesh '{}' is on {} — \
             choose a path in the same environment (native/Windows versus WSL) \
             or use a relative path like 'worktrees' resolved from the mesh root",
            dir_kind, mesh_path, mesh_kind
        ));
    }
    Ok(Some(normalized))
}

/// The trimmed, non-empty worktree name iff the node runs in a worktree — the
/// single definition of "does this Agent Node have a Worktree Node dir".
///
/// `pub(crate)` so command handlers that need to validate a worktree (e.g.
/// `commands::build_run` feeding the name into `validate_worktree_exists`'s
/// git2 worktree-list compare) read the SAME trimmed value the canonical
/// resolver consumed. Re-reading `node.worktree_name` directly bypasses the
/// trim invariant; reaching for `worktree_segment` keeps it in one place.
pub(crate) fn worktree_segment(node: &AgentNode) -> Option<&str> {
    if !node.use_worktree {
        return None;
    }
    node.worktree_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
}

/// The [Node Working Directory](../../CONTEXT.md): the directory an Agent Node's
/// work physically lives in — its Worktree Node dir for a worktree node, or the
/// Mesh root for a Root Node. This is the one canonical home of the
/// `use_worktree` + (trimmed, non-empty) `worktree_name` rule; diff, PR,
/// close-safety, removal, and the coordinator transcript reader all resolve
/// through here so every layer agrees on a node's directory. Callers pick the
/// `host_path` (Windows git2 / file ops), the `spawn_path` (the path as the
/// agent saw it — the form Claude Code encodes for its on-disk transcript
/// dir), or the `raw_path` (the POSIX-style effective path; the string the
/// GIT_CHANGED payload carries to the frontend's `getNodeGitPath()` for
/// subscription matching, issue #409).
///
/// Mirrors the frontend's `getNodeGitPath` in `src/lib/paths.ts` — keep the two
/// in sync if the worktree layout ever changes (paired cross-language defaults,
/// not a single source; see [[buildmesh-use-worktree-derivation]] and
/// [[feedback_cross-language-default-coupling]]).
///
/// Issue #1519: Worktree Nodes created after the configurable directory
/// landed carry the exact resolved dir in `node.worktree_path` (immutable —
/// changing a setting affects future nodes without moving live worktrees).
/// When present (trimmed, non-empty) it wins over recomputation; `None`
/// (Root Nodes + pre-#1519 rows) falls back to the legacy
/// `<mesh>/.claude/worktrees/<name>` layout byte-for-byte.
pub fn node_working_path(node: &AgentNode) -> ResolvedPath {
    if worktree_segment(node).is_some() {
        if let Some(stored) = node
            .worktree_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return resolve_raw_path(stored);
        }
        return resolve_agent_path(&node.path, worktree_segment(node));
    }
    resolve_agent_path(&node.path, None)
}

/// The node's Worktree Node dir — `Some` only for a Worktree Node, `None` for a
/// Root Node. The `None` is load-bearing: close-safety and worktree removal skip
/// Root Nodes through it (a Root Node has no worktree to inspect or delete).
pub fn node_worktree_path(node: &AgentNode) -> Option<ResolvedPath> {
    worktree_segment(node).map(|_| node_working_path(node))
}

/// The paths a node "owns" — its mesh root plus the resolved Worktree Node
/// dir, if any. Single canonical reader of the `use_worktree` +
/// `worktree_name` rule via `node_worktree_path`; the worktree-manager
/// (`get_git_prune_info`, issue #607) and mesh-health
/// (`find_base_branch_holder`, issue #621) both consume this so they
/// can't drift apart on the worktree-dir entry.
pub fn active_node_paths(nodes: &[AgentNode]) -> Vec<String> {
    let mut paths = Vec::new();
    for node in nodes {
        paths.push(node.path.clone());
        if let Some(resolved) = node_worktree_path(node) {
            paths.push(resolved.host_path);
        }
    }
    paths
}

/// The branches a node currently has checked out — the `branch` field of
/// every non-archived node. Sibling reader to [`active_node_paths`]: just
/// as a worktree is "active" when a node's path resolves to it, a branch
/// is "active" when a live node has it checked out.
///
/// Used by the Worktree Manager tab to (a) flag local branches with an
/// `is_active` badge in the prune list and (b) gate the delete checkbox
/// so the user can't accidentally drop a branch a live agent is using.
/// The corresponding backend guard in `commands::prune::delete_branches`
/// is defence-in-depth — a stale UI (or a direct API call) must not be
/// able to delete a branch a live node is on.
///
/// **Two filter axes the caller must handle**:
/// 1. **Mesh scope** — branch names are not filesystem-unique like paths,
///    so a `feature-a` in `/repo1` would collide with a `feature-a` in
///    `/repo2`. Callers that care about repo-scoped active-ness should
///    pre-filter `nodes` by mesh (`db::list_agent_nodes_by_mesh`) before
///    calling this. Both `delete_branches` and `get_git_prune_info`
///    mesh-scope via `db::list_agent_nodes_by_mesh(mesh_id)` — the
///    path asymmetry that previously existed (`get_git_prune_info` used
///    the global `db::list_agent_nodes`) was closed in PR #660, and
///    worktree path active-checks are now also mesh-scoped (defensive
///    symmetry even though filesystem paths are host-unique on disk).
/// 2. **Archived status** — enforced here. The function drops any node
///    whose `status == SessionStatus::Archived` so a closed agent node
///    doesn't keep its branch locked. This matches the contract
///    `db::list_agent_nodes()` provides on its own (`WHERE status !=
///    'archived'`); `db::list_agent_nodes_by_mesh` does NOT filter
///    archived, which is why the filter lives here.
pub fn active_node_branches(nodes: &[AgentNode]) -> Vec<String> {
    nodes
        .iter()
        .filter(|n| n.status != SessionStatus::Archived)
        .map(|n| n.branch.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commandcode_projects_dir_resolves_base_without_slug() {
        // The base carries no slug; slug formatting lives in transcript_reader
        // (issue #1500 review). The composer test lives there too.
        let dir = commandcode_projects_dir(
            EnvType::Windows,
            r"F:\src\buildmesh\.claude\worktrees\saucy-thunderous-cove",
        )
        .expect("windows projects dir should resolve");
        let dir_str = dir.to_string_lossy().replace('\\', "/");
        assert!(
            dir_str.ends_with(".commandcode/projects"),
            "projects base should end with .commandcode/projects, got {dir_str}"
        );
        assert!(
            !dir_str.contains("saucy"),
            "base must not contain a slug, got {dir_str}"
        );
    }

    #[test]
    fn normalize_unc_to_wsl_maps_host_unc_into_spawn_form() {
        // Issue #1500 review: discovery may hold a Windows host UNC path for a
        // WSL mesh; the CLI slugged the in-WSL cwd, so normalize first.
        assert_eq!(
            normalize_unc_to_wsl(r"\\wsl$\Ubuntu\home\user\repo"),
            "/home/user/repo"
        );
        assert_eq!(
            normalize_unc_to_wsl("//wsl$/Ubuntu/home/user/repo"),
            "/home/user/repo"
        );
        assert_eq!(
            normalize_unc_to_wsl(r"\\WSL$\ubuntu\HOME\user\repo\"),
            "/HOME/user/repo"
        );
        assert_eq!(normalize_unc_to_wsl(r"\\wsl$\Ubuntu"), "/");
        // Non-UNC paths pass through borrowed (no allocation).
        assert!(matches!(
            normalize_unc_to_wsl("/home/user/repo"),
            std::borrow::Cow::Borrowed(_)
        ));
        assert!(matches!(
            normalize_unc_to_wsl(r"F:\src\buildmesh"),
            std::borrow::Cow::Borrowed(_)
        ));
        assert_eq!(normalize_unc_to_wsl(""), "");
    }

    // ── Configurable Worktree Node directories (issue #1519) ────────────────

    #[test]
    fn effective_dir_never_doubles_separators_on_trailing_slash_root() {
        // A stored root with a trailing separator (e.g. from a CLI import)
        // must join to exactly one separator on both sides of the contract
        // (see `getEffectiveWorktreeDir`).
        assert_eq!(
            effective_worktree_dir_raw("/repo/mesh/", None, None),
            "/repo/mesh/.claude/worktrees"
        );
        assert_eq!(
            effective_worktree_dir_raw("/repo/mesh/", None, Some("custom-wt")),
            "/repo/mesh/custom-wt"
        );
    }

    #[test]
    fn effective_dir_defaults_to_claude_worktrees_when_unconfigured() {
        assert_eq!(
            effective_worktree_dir_raw("/repo/mesh", None, None),
            "/repo/mesh/.claude/worktrees"
        );
        // Blank collapses to inherit/default.
        assert_eq!(
            effective_worktree_dir_raw("/repo/mesh", Some("   "), Some("")),
            "/repo/mesh/.claude/worktrees"
        );
    }

    #[test]
    fn effective_dir_relative_app_setting_applies_to_inheriting_meshes() {
        assert_eq!(
            effective_worktree_dir_raw("/repo/mesh", None, Some("custom-wt")),
            "/repo/mesh/custom-wt"
        );
        // Trimmed; trailing separators collapsed so `<dir>/<name>` never doubles.
        assert_eq!(
            effective_worktree_dir_raw("/repo/mesh", None, Some("  custom-wt/ ")),
            "/repo/mesh/custom-wt"
        );
    }

    #[test]
    fn effective_dir_mesh_override_wins_over_app_default() {
        assert_eq!(
            effective_worktree_dir_raw(
                "/repo/mesh",
                Some("mesh-wt"),
                Some("app-wt")
            ),
            "/repo/mesh/mesh-wt"
        );
        // Clearing the override restores inheritance.
        assert_eq!(
            effective_worktree_dir_raw("/repo/mesh", Some("  "), Some("app-wt")),
            "/repo/mesh/app-wt"
        );
    }

    #[test]
    fn effective_dir_absolute_used_verbatim() {
        assert_eq!(
            effective_worktree_dir_raw("/repo/mesh", Some("/tmp/wt"), None),
            "/tmp/wt"
        );
        // No shell/`~` expansion — treated literally (relative → joins).
        assert_eq!(
            effective_worktree_dir_raw("/repo/mesh", Some("~/wt"), None),
            "/repo/mesh/~/wt"
        );
        assert_eq!(
            effective_worktree_dir_raw("/repo/mesh", Some("$HOME/wt"), None),
            "/repo/mesh/$HOME/wt"
        );
    }

    #[test]
    fn is_absolute_detects_posix_windows_and_unc() {
        assert!(is_absolute_worktree_path("/tmp/wt"));
        assert!(is_absolute_worktree_path("C:\\wt"));
        assert!(is_absolute_worktree_path("C:/wt"));
        assert!(is_absolute_worktree_path("\\\\wsl$\\Ubuntu\\home\\u"));
        assert!(!is_absolute_worktree_path("custom-wt"));
        assert!(!is_absolute_worktree_path("a/b"));
        assert!(!is_absolute_worktree_path("~/wt"));
        assert!(!is_absolute_worktree_path("$HOME/wt"));
        assert!(!is_absolute_worktree_path(""));
        assert!(!is_absolute_worktree_path("C:"));
    }

    #[test]
    fn resolve_node_raw_joins_effective_dir_and_trimmed_name() {
        assert_eq!(
            resolve_worktree_node_raw("/repo/mesh/custom", "  my-node  "),
            "/repo/mesh/custom/my-node"
        );
    }

    #[test]
    fn resolve_in_dir_matches_legacy_for_default() {
        let legacy = resolve_agent_path("/repo/mesh", Some("my-node"));
        let effective = effective_worktree_dir_raw("/repo/mesh", None, None);
        let via_dir = resolve_agent_path_in_dir("/repo/mesh", &effective, Some("my-node"));
        assert_eq!(via_dir.raw_path, legacy.raw_path);
        assert_eq!(via_dir.raw_path, "/repo/mesh/.claude/worktrees/my-node");
    }

    #[test]
    fn node_working_path_prefers_persisted_worktree_path() {
        let mut node = AgentNode {
            path: "/repo/mesh".to_string(),
            worktree_name: Some("my-node".to_string()),
            use_worktree: true,
            worktree_path: Some("/repo/mesh/custom/my-node".to_string()),
            ..Default::default()
        };
        assert_eq!(
            node_working_path(&node).raw_path,
            "/repo/mesh/custom/my-node"
        );
        // Blank stored path falls back to legacy (hand-edited blank).
        node.worktree_path = Some("   ".to_string());
        assert_eq!(
            node_working_path(&node).raw_path,
            "/repo/mesh/.claude/worktrees/my-node"
        );
        // Legacy rows without stored path retain the legacy fallback.
        node.worktree_path = None;
        assert_eq!(
            node_working_path(&node).raw_path,
            "/repo/mesh/.claude/worktrees/my-node"
        );
        // Root nodes ignore a stale stored path.
        node.use_worktree = false;
        node.worktree_path = Some("/repo/mesh/custom/my-node".to_string());
        assert_eq!(node_working_path(&node).raw_path, "/repo/mesh");
    }

    #[test]
    fn manual_warm_claim_resolves_through_normalized_node_path() {
        // Issue #1519 review: warm-pool rows store HOST paths (UNC on WSL)
        // while `worktree_path` stores RAW form. The spawn must resolve the
        // node's normalized raw path — feeding the host UNC form into
        // `resolve_raw_path` yields a UNC `spawn_path` that `wsl.exe --cd`
        // rejects on Windows. This pins the storage contract both halves
        // rely on (`prepare_context` normalizes at claim time).
        let raw = normalize_unc_to_wsl("\\\\wsl$\\Ubuntu\\home\\u\\wt\\slug").into_owned();
        assert_eq!(raw, "/home/u/wt/slug");
        let node = AgentNode {
            path: "/home/u/repo".to_string(),
            worktree_name: Some("slug".to_string()),
            use_worktree: true,
            worktree_path: Some(raw.clone()),
            ..Default::default()
        };
        let resolved = node_working_path(&node);
        assert_eq!(resolved.raw_path, raw);
        assert!(
            !resolved.raw_path.starts_with("\\\\"),
            "raw_path must never be UNC — downstream derivation assumes raw form, got: {}",
            resolved.raw_path
        );
    }

    #[test]
    fn normalize_worktree_directory_trims_and_collapses_blank() {
        assert_eq!(normalize_worktree_directory(None), None);
        assert_eq!(normalize_worktree_directory(Some("   ")), None);
        assert_eq!(
            normalize_worktree_directory(Some("  custom  ")),
            Some("custom".to_string())
        );
    }

    #[test]
    fn validate_relative_and_blank_collapse() {
        assert_eq!(validate_worktree_directory("/repo/mesh", None).unwrap(), None);
        assert_eq!(
            validate_worktree_directory("/repo/mesh", Some("   ")).unwrap(),
            None
        );
        assert_eq!(
            validate_worktree_directory("/repo/mesh", Some("  custom  ")).unwrap(),
            Some("custom".to_string())
        );
        // Absolute in the same env as the mesh passes (on non-Windows every
        // path is native so this always passes; on Windows the mesh/path
        // pair below shares the native env).
        assert!(validate_worktree_directory("/tmp/mesh", Some("/tmp/wt"))
            .unwrap()
            .is_some());
    }

    #[test]
    fn validate_rejects_directory_traversal_and_dot_segments() {
        for bad in [
            "..",
            ".",
            "../..",
            "../../etc",
            "sub/../../etc",
            "wt/../..",
            "./wt",
            "wt/.",
        ] {
            let err = validate_worktree_directory("/repo/mesh", Some(bad))
                .expect_err(&format!("{bad:?} must be rejected"));
            assert!(
                err.contains("must stay inside the mesh") || err.contains("resolves to nothing"),
                "traversal input {bad:?} needs an actionable error, got: {err}"
            );
        }
        // Near-misses that are legitimate names stay valid.
        assert_eq!(
            validate_worktree_directory("/repo/mesh", Some("..wt")).unwrap(),
            Some("..wt".to_string())
        );
        assert_eq!(
            validate_worktree_directory("/repo/mesh", Some("my..wt")).unwrap(),
            Some("my..wt".to_string())
        );
    }

    #[test]
    fn validate_rejects_drive_relative_and_forbidden_characters() {
        let err = validate_worktree_directory("/repo/mesh", Some("\\foo\\bar"))
            .expect_err("single-leading-backslash must be rejected");
        assert!(err.contains("drive-relative"), "got: {err}");
        for bad in ["a:b", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b", "C:foo"] {
            validate_worktree_directory("/repo/mesh", Some(bad))
                .expect_err(&format!("{bad:?} must be rejected"));
        }
        // Consecutive separators are rejected rather than silently collapsed.
        validate_worktree_directory("/repo/mesh", Some("a//b"))
            .expect_err("empty segment must be rejected");
    }

    #[test]
    fn validate_normalizes_relative_and_absolute_forms() {
        // Leading/trailing separators stripped on relative values.
        assert_eq!(
            validate_worktree_directory("/repo/mesh", Some("  custom-wt/ ")).unwrap(),
            Some("custom-wt".to_string())
        );
        assert_eq!(
            effective_worktree_dir_raw("/repo/mesh", Some("custom-wt/"), None),
            "/repo/mesh/custom-wt"
        );
        // Trailing separators trimmed on absolute values for consistency.
        assert_eq!(
            effective_worktree_dir_raw("/repo/mesh", Some("/tmp/wt/"), None),
            "/tmp/wt"
        );
        // Filesystem roots are rejected, not joined.
        for root in ["/", "///", "C:", "C:\\", "C:/"] {
            validate_worktree_directory("C:\\repo\\mesh", Some(root))
                .expect_err(&format!("{root:?} must be rejected"));
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn validate_absolute_mismatch_errors_with_actionable_message() {
        let err = validate_worktree_directory(r"C:\repo\mesh", Some("/home/user/wt"))
            .expect_err("WSL absolute for a Windows mesh must be rejected");
        assert!(
            err.contains("same environment"),
            "mismatch error must be actionable, got: {err}"
        );
        let err = validate_worktree_directory("/home/user/mesh", Some(r"C:\wt"))
            .expect_err("Windows absolute for a WSL mesh must be rejected");
        assert!(
            err.contains("same environment"),
            "mismatch error must be actionable, got: {err}"
        );
    }
}
