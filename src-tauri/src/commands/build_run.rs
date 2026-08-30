//! Build/Run feature — spawns a shell in a worktree and runs build/run commands

use crate::db;
use crate::env;
use crate::models::MeshRow;
use base64::Engine;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use ts_rs::TS;

// ---------------------------------------------------------------------------
// Wire types — Tauri event payloads (issue #161)
// ---------------------------------------------------------------------------

/// Payload of the per-session `build-run-output-{sessionId}` Tauri event.
/// Emitted by the PTY reader thread on every read with a base64-encoded
/// chunk of stdout bytes (matches the production `agent-output` shape, so
/// the same `decodeBase64Bytes` helper handles both).
///
/// Generated to `src/types/generated/BuildRunOutputPayload.ts`; the TS half
/// is imported by `src/components/Terminal/BuildRunTerminalRegistry.ts`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "BuildRunOutputPayload.ts")]
pub struct BuildRunOutputPayload {
    pub data: String,
}

/// Payload of the per-session `build-run-exited-{sessionId}` Tauri event.
/// Emitted when the PTY reader sees EOF on the build/run shell. Empty
/// payload — the exit is a sentinel, not a state carrier. The event name
/// encodes the sessionId (one event family per spawned shell) so the
/// listener can match it without a payload field.
///
/// Generated to `src/types/generated/BuildRunExitedPayload.ts`; the TS half
/// is imported by `src/components/Terminal/BuildRunTerminalRegistry.ts`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "BuildRunExitedPayload.ts")]
pub struct BuildRunExitedPayload {}

// ---------------------------------------------------------------------------
// Worktree path resolution
// ---------------------------------------------------------------------------

/// Validate that the worktree directory exists on disk.
/// Returns an error if the node has no worktree_name or the directory hasn't been created yet.
fn validate_worktree_exists(resolved: &env::ResolvedPath, worktree_name: Option<&str>) -> Result<(), String> {
    let wt_name = worktree_name.ok_or_else(|| {
        "No worktree name set for this agent node. Spawn the agent first to create a worktree.".to_string()
    })?;

    // Use git2 to verify the worktree is registered in git metadata,
    // not just the directory exists on disk. This catches broken/corrupted worktrees.
    let repo = git2::Repository::open(&resolved.host_path)
        .or_else(|_| git2::Repository::discover(&resolved.host_path))
        .map_err(|e| format!("Failed to open repository: {}", e))?;

    let worktrees = repo.worktrees()
        .map_err(|e| format!("Failed to list worktrees: {}", e))?;

    // Check if our worktree name is in the list
    for i in 0..worktrees.len() {
        if let Some(name) = worktrees.get(i) {
            if name == wt_name {
                return Ok(());
            }
        }
    }

    // Also check if the path itself is a valid git worktree (it could be the main worktree)
    if std::path::Path::new(&resolved.host_path).join(".git").exists() {
        return Ok(());
    }

    Err(format!(
        "Worktree '{}' not found in git worktree list. Spawn the agent first to create the worktree.",
        wt_name
    ))
}

// ---------------------------------------------------------------------------
// Process management
// ---------------------------------------------------------------------------

/// A build/run process tracked separately from agents
struct BuildRunProcess {
    master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    /// Writer for sending user input to the PTY. Always populated (even for
    /// one-shot build/run) so `write_to_build_run` can be called for any
    /// entry in the registry. Build/run never receives user input today,
    /// but storing the writer is harmless and keeps the surface uniform.
    writer: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
}

/// Registry for build/run processes
struct BuildRunRegistry {
    processes: HashMap<i64, Arc<BuildRunProcess>>,
}

impl BuildRunRegistry {
    fn new() -> Self {
        Self {
            processes: HashMap::new(),
        }
    }

    fn insert(&mut self, node_id: i64, process: BuildRunProcess) {
        self.processes.insert(node_id, Arc::new(process));
    }

    fn remove(&mut self, node_id: &i64) -> Option<Arc<BuildRunProcess>> {
        self.processes.remove(node_id)
    }

    /// Write bytes (user keystrokes) to the PTY master of a live process.
    /// Returns `Err("Build run not running")` if the node has no live process
    /// — matches the agent registry's error string shape so the frontend can
    /// safely ignore the "not running" case.
    pub fn write_bytes(&self, node_id: i64, data: &[u8]) -> Result<(), String> {
        let process = self
            .processes
            .get(&node_id)
            .ok_or_else(|| "Build run not running".to_string())?;
        let mut writer = process.writer.lock().unwrap();
        writer.write_all(data).map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())
    }

    /// Resize the PTY to `cols` x `rows`. Same "not running" semantics.
    pub fn resize_pty(&self, node_id: i64, cols: u16, rows: u16) -> Result<(), String> {
        let process = self
            .processes
            .get(&node_id)
            .ok_or_else(|| "Build run not running".to_string())?;
        let master = process.master.lock().unwrap();
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())
    }
}

static BUILD_RUN_REGISTRY: once_cell::sync::Lazy<Arc<Mutex<BuildRunRegistry>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(BuildRunRegistry::new())));

// ---------------------------------------------------------------------------
// Shell selection
// ---------------------------------------------------------------------------

/// Build the `CommandBuilder` for either a one-shot build/run command or an
/// interactive shell. Terminal mode drops the `-c` / `-e` / `/c` flag and
/// the command argument — we want a long-running shell, not a one-shot.
///
/// Shell choice per platform (terminal mode):
/// - Wsl → `wsl.exe` (default user's login shell, cwd is the WSL path)
/// - macOS / native Linux → `sh` (any non-Windows host: no WSL, no PowerShell)
/// - Windows → `powershell.exe` (per user preference; ANSI renders in PTY)
fn build_shell_command(
    mode: BuildRunMode,
    command: &str,
    env_type: crate::models::EnvType,
) -> CommandBuilder {
    if mode == BuildRunMode::Terminal {
        if env_type == crate::models::EnvType::Wsl {
            CommandBuilder::new("wsl.exe")
        } else if !cfg!(target_os = "windows") {
            // macOS and native Linux: POSIX `sh` (no WSL, no PowerShell).
            CommandBuilder::new("sh")
        } else {
            CommandBuilder::new("powershell.exe")
        }
    } else if env_type == crate::models::EnvType::Wsl {
        let mut c = CommandBuilder::new("wsl.exe");
        c.arg("-e");
        c.arg(command);
        c
    } else if !cfg!(target_os = "windows") {
        // macOS and native Linux: POSIX `sh -c <command>`.
        let mut c = CommandBuilder::new("sh");
        c.arg("-c");
        c.arg(command);
        c
    } else {
        let mut c = CommandBuilder::new("cmd.exe");
        c.arg("/c");
        c.arg(command);
        c
    }
}

// ---------------------------------------------------------------------------
// Tauri command
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildRunMode {
    Build,
    Run,
    /// Interactive shell spawned in the worktree directory. The user types
    /// into it via `write_to_build_run`; output is streamed on the same
    /// `build-run-output-{node_id}` event channel as build/run.
    Terminal,
}

#[tauri::command]
pub async fn build_run(
    node_id: i64,
    mode: BuildRunMode,
    app: AppHandle,
) -> Result<(), String> {
    // Offload: the body opens the repo with git2 (worktree-registration
    // check — a stat-heavy walk on large repos / WSL UNC paths) and then
    // opens a ConPTY + spawns a shell. Both are blocking calls that must
    // not park a Tauri async worker (Command Threading convention).
    crate::commands::run_blocking("build_run", move || build_run_blocking(node_id, mode, app))
        .await
}

/// Sync core for [`build_run`] — see the `*_blocking` + `run_blocking`
/// convention in `commands/mod.rs`.
fn build_run_blocking(
    node_id: i64,
    mode: BuildRunMode,
    app: AppHandle,
) -> Result<(), String> {
    // 1. Get agent node (node.path == mesh.path for all nodes)
    let node = db::get_agent_node_by_id(node_id)
        .map_err(|e| format!("failed to get agent node {}: {}", node_id, e))?;

    // 2. Read canonical mesh row from DB (for build/run command strings).
    // The worktree-vs-root decision below does NOT consult `row.use_worktree`
    // — the per-node override (`SpawnButtonCluster`'s alt-click path
    // bypasses the mesh setting and persists `use_worktree=false` on the
    // node row) means the mesh-level flag is the wrong authority here.
    let mesh = db::get_mesh_by_path(&node.path)
        .map_err(|e| format!("failed to get mesh for path {}: {}", node.path, e))?;
    let row = MeshRow::from(&mesh);

    // 3. Resolve cwd via the canonical Node Working Directory rule
    //    (`resolve_build_run_cwd` → `env::node_working_path`). Gates on
    //    `node.use_worktree` + non-empty trimmed `worktree_name`, so a Root
    //    Node spawned in a worktree-enabled mesh resolves to the mesh root
    //    — the bug the user reported surfaced here as "No worktree name set
    //    for this agent node." for any Build/Run/Terminal click on a Root
    //    Node.
    let resolved = resolve_build_run_cwd(&node);

    // 4. Validate + sanitize the worktree only for Worktree Nodes. Root
    //    Nodes have no worktree to inspect and no `.git` worktree-link to
    //    sanitize. Pulling `wt_name` from `env::worktree_segment` (not from
    //    `node.worktree_name` directly) keeps the trim invariant in one
    //    place — a DB row with stray whitespace around the name would
    //    otherwise bypass the trim and fail the git2 worktree-list compare.
    if let Some(wt_name) = env::worktree_segment(&node) {
        validate_worktree_exists(&resolved, Some(wt_name))?;

        // Sanitize .git file to ensure proper worktree isolation across environments
        if let Err(e) = crate::git::worktree::sanitize_git_worktree(&resolved.host_path, resolved.env_type) {
            tracing::warn!("build_run: failed to sanitize worktree .git file: {}", e);
        }
    }

    // 5. Resolve the command for this (mode, context) tuple (issue #802).
    //    Root context prefers the per-context `root_build_command` /
    //    `root_run_command` and falls back to `build_command` /
    //    `run_command`; worktree context always uses the latter. Terminal
    //    mode spawns an interactive shell directly, so its command is empty.
    //    `is_root` comes from `env::worktree_segment` — the SAME signal
    //    `resolve_build_run_cwd` uses to choose the cwd — so the command
    //    always matches the directory the shell actually spawns in.
    let is_root = env::worktree_segment(&node).is_none();
    let command = resolve_build_run_command(mode, is_root, &row).ok_or_else(|| {
        match mode {
            BuildRunMode::Run => "run command not configured".to_string(),
            // Terminal always resolves to Some(""), so only Build reaches here.
            _ => "build command not configured".to_string(),
        }
    })?;

    // 6. Get shell working directory from resolved path
    let shell_cwd = &resolved.spawn_path;

    // 7. Spawn PTY with shell
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }).map_err(|e| format!("failed to open PTY: {}", e))?;

    let mut cmd = build_shell_command(mode, command, resolved.env_type);
    cmd.cwd(shell_cwd);
    crate::pty::strip_git_env_vars(&mut cmd);

    let child = pair.slave.spawn_command(cmd)
        .map_err(|e| format!("failed to spawn shell: {}", e))?;

    let reader = pair.master
        .try_clone_reader()
        .map_err(|e| format!("failed to clone PTY reader: {}", e))?;

    // Take writer up-front so the registry can serve `write_to_build_run`
    // for terminal mode. `take_writer()` is a one-shot — must be called
    // before the master is moved into the registry.
    let writer = pair.master
        .take_writer()
        .map_err(|e| format!("failed to take PTY writer: {}", e))?;

    // 8. Store process in registry
    {
        let mut registry = BUILD_RUN_REGISTRY.lock().unwrap();
        registry.insert(node_id, BuildRunProcess {
            master: Arc::new(Mutex::new(pair.master)),
            writer: Arc::new(Mutex::new(writer)),
        });
    }

    // 9. Spawn reader thread to stream output to frontend
    let node_id_clone = node_id;
    let app_handle = app.clone();
    std::thread::spawn(move || {
        let mut r = reader;
        let mut buf = [0u8; 1024];
        loop {
            match r.read(&mut buf) {
                Ok(0) => {
                    // EOF — process exited. Notify the frontend so the
                    // BuildRunTerminalRegistry can flip `ptyAlive=false`
                    // and surface a visible banner if the terminal is
                    // currently attached — without this, a shell that
                    // exits while the user is on another mesh would
                    // leave a zombie PTY that silently swallows keystrokes.
                    let _ = app_handle.emit(
                        &format!("build-run-exited-{}", node_id_clone),
                        BuildRunExitedPayload {},
                    );
                    break;
                }
                Ok(n) => {
                    let data = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
                    let _ = app_handle.emit(
                        &format!("build-run-output-{}", node_id_clone),
                        BuildRunOutputPayload { data },
                    );
                }
                Err(e) => {
                    tracing::error!("build_run PTY read error: {}", e);
                    break;
                }
            }
        }
        // Drop the BUILD_RUN_REGISTRY entry on EOF so subsequent
        // `write_to_build_run` calls correctly hit the "Build run not
        // running" path. Without this, the registry still holds the
        // writer after the child has exited, so `write_bytes` succeeds
        // at the syscall level — keystrokes vanish silently into a dead
        // PTY instead of producing the visible rejection. See code-review
        // Finding Alt-2.
        BUILD_RUN_REGISTRY.lock().unwrap().remove(&node_id_clone);
    });

    // 10. Drop child guard — the process continues in the reader thread
    drop(child);

    Ok(())
}

#[tauri::command]
pub async fn get_mesh_row(mesh_id: i64) -> Result<MeshRow, String> {
    crate::commands::run_blocking("get_mesh_row", move || {
        let mesh = db::get_mesh_by_id(mesh_id)
            .map_err(|e| format!("failed to get mesh {}: {}", mesh_id, e))?;
        Ok(MeshRow::from(&mesh))
    })
    .await
}

/// Close a build/run terminal for a node
#[tauri::command]
pub async fn close_build_run(node_id: i64) -> Result<(), String> {
    let mut registry = BUILD_RUN_REGISTRY.lock().unwrap();
    if let Some(process) = registry.remove(&node_id) {
        let master = process.master.lock().unwrap();
        drop(master);
    }
    Ok(())
}

/// Forward user keystrokes to the live build/run PTY. Currently meaningful
/// only for `BuildRunMode::Terminal`; build/run ignores input.
/// Mirrors `agent::write_to_agent` (`src-tauri/src/commands/agent.rs:291`).
#[tauri::command]
pub fn write_to_build_run(node_id: i64, data: String) -> Result<(), String> {
    let registry = BUILD_RUN_REGISTRY.lock().unwrap();
    registry.write_bytes(node_id, data.as_bytes())
}

/// Resize the live build/run PTY to the given terminal grid size.
/// Mirrors `agent::resize_agent` (`src-tauri/src/commands/agent.rs:286`).
#[tauri::command]
pub fn resize_build_run(node_id: i64, rows: u16, cols: u16) -> Result<(), String> {
    let registry = BUILD_RUN_REGISTRY.lock().unwrap();
    registry.resize_pty(node_id, cols, rows)
}

// ---------------------------------------------------------------------------
// Path decision
// ---------------------------------------------------------------------------

/// Where the build/run shell should be spawned for `node`.
///
/// Delegates to the canonical Node Working Directory rule
/// (`env::node_working_path`), which gates on `node.use_worktree` +
/// non-empty trimmed `worktree_name` — NOT on `mesh.use_worktree`. The
/// previous inline implementation used `row.use_worktree`, which broke for
/// Root Nodes spawned in a worktree-enabled mesh (`SpawnButtonCluster`'s
/// alt-click path bypasses the mesh setting and persists `use_worktree=false`
/// on the node row). The resulting `spawn_worktree_name = None` then hit
/// `validate_worktree_exists` and surfaced as "No worktree name set for this
/// agent node. Spawn the agent first to create a worktree." — exactly the
/// regression the user reported for Build/Run/Terminal on a Root Node.
fn resolve_build_run_cwd(node: &crate::models::AgentNode) -> env::ResolvedPath {
    env::node_working_path(node)
}

/// Resolve the command string for a `(mode, context)` pair (issue #802).
///
/// Root context (`is_root == true`) prefers the per-context
/// `root_build_command` / `root_run_command` and falls back to
/// `build_command` / `run_command`; worktree context always uses the latter.
/// Terminal mode needs no command, so it resolves to `Some("")` — never
/// `None` — and callers spawn an interactive shell instead of running a
/// one-shot command.
///
/// The fallback (`.or(build_command)`) preserves PR #801's behaviour: a mesh
/// that never sets the `root_*` columns runs the same command in both
/// contexts, exactly as before this feature.
fn resolve_build_run_command(
    mode: BuildRunMode,
    is_root: bool,
    row: &MeshRow,
) -> Option<&str> {
    match (mode, is_root) {
        (BuildRunMode::Build, false) => row.build_command.as_deref(),
        (BuildRunMode::Build, true) => {
            row.root_build_command.as_deref().or(row.build_command.as_deref())
        }
        (BuildRunMode::Run, false) => row.run_command.as_deref(),
        (BuildRunMode::Run, true) => {
            row.root_run_command.as_deref().or(row.run_command.as_deref())
        }
        (BuildRunMode::Terminal, _) => Some(""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AgentNode;

    fn node_fixture(use_worktree: bool, worktree_name: Option<&str>) -> AgentNode {
        AgentNode {
            path: "/home/user/my-repo".to_string(),
            worktree_name: worktree_name.map(str::to_string),
            use_worktree,
            ..Default::default()
        }
    }

    /// Regression: a Root Node (use_worktree=false, worktree_name=None) must
    /// resolve to the mesh root even when the mesh has use_worktree=true. The
    /// previous inline implementation used `row.use_worktree`, then fed
    /// `spawn_worktree_name = None` into `validate_worktree_exists`, which
    /// surfaced "No worktree name set for this agent node." for any Build/Run/
    /// Terminal click on a Root Node inside a worktree-enabled mesh — the
    /// bug the user reported.
    ///
    /// Asserts on `raw_path` (the input-pass-through POSIX form, mirrored by
    /// the frontend's `getNodeGitPath`) rather than `host_path` — `host_path`
    /// goes through `to_host_path`, which converts a `/home/...` fixture to
    /// a WSL UNC path on Windows. The raw form is the contract this helper
    /// is responsible for (issue #409).
    #[test]
    fn resolve_build_run_cwd_root_node_resolves_to_mesh_root() {
        let resolved = resolve_build_run_cwd(&node_fixture(false, None));
        assert_eq!(resolved.raw_path, "/home/user/my-repo");
        assert!(
            !resolved.raw_path.contains("worktrees"),
            "Root Node raw_path must not contain a worktree subdir, got: {}",
            resolved.raw_path
        );
    }

    /// A stale `worktree_name` on a Root Node is ignored — same canonical
    /// rule `env::node_working_path` uses everywhere else (issue #383).
    #[test]
    fn resolve_build_run_cwd_root_node_ignores_stale_worktree_name() {
        let resolved = resolve_build_run_cwd(&node_fixture(false, Some("stale-name")));
        assert!(
            !resolved.raw_path.contains("worktrees"),
            "stale worktree_name on Root Node must not leak into raw_path: {}",
            resolved.raw_path
        );
    }

    /// A Worktree Node still resolves into its `.claude/worktrees/<name>`
    /// subdir — the canonical behaviour, preserved.
    #[test]
    fn resolve_build_run_cwd_worktree_node_resolves_worktree_subdir() {
        let resolved = resolve_build_run_cwd(&node_fixture(true, Some("gentle-fox")));
        assert_eq!(
            resolved.raw_path,
            "/home/user/my-repo/.claude/worktrees/gentle-fox"
        );
    }

    /// The worktree validator gate must skip Root Nodes — otherwise we'd
    /// call `git2::Repository::open` on the mesh root and look for a
    /// non-existent worktree, tripping the user-facing "Worktree '...' not
    /// found" error. This pins `env::worktree_segment`'s contract at the
    /// call site: `Some(_)` only when the node is a Worktree Node.
    #[test]
    fn worktree_segment_is_some_only_for_worktree_nodes() {
        assert!(env::worktree_segment(&node_fixture(false, None)).is_none());
        assert!(env::worktree_segment(&node_fixture(false, Some("ignored"))).is_none());
        assert_eq!(
            env::worktree_segment(&node_fixture(true, Some("gentle-fox"))),
            Some("gentle-fox"),
        );
        assert!(env::worktree_segment(&node_fixture(true, None)).is_none());
        assert!(env::worktree_segment(&node_fixture(true, Some("   "))).is_none());

        // Trim invariant (issue #383 — Root Node + stale `worktree_name`):
        // the trimmed segment is what the git2 worktree-list compare sees.
        assert_eq!(
            env::worktree_segment(&node_fixture(true, Some("  gentle-fox  "))),
            Some("gentle-fox"),
        );
    }

    #[test]
    fn build_run_mode_serializes_lowercase() {
        for (variant, expected) in [
            (BuildRunMode::Build, "\"build\""),
            (BuildRunMode::Run, "\"run\""),
            (BuildRunMode::Terminal, "\"terminal\""),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected, "serialize {:?}", variant);

            let round: BuildRunMode = serde_json::from_str(&json).unwrap();
            assert_eq!(round, variant, "round-trip {:?}", variant);
        }
    }

    #[test]
    fn build_run_registry_write_bytes_to_dead_session() {
        let registry = BuildRunRegistry::new();
        let result = registry.write_bytes(42, b"hello");
        assert!(result.is_err());
        // Frontend matches on this substring to swallow the "not running"
        // case silently — keep the contract stable.
        assert!(result.unwrap_err().contains("not running"));
    }

    #[test]
    fn build_run_registry_resize_pty_to_dead_session() {
        let registry = BuildRunRegistry::new();
        let result = registry.resize_pty(42, 80, 24);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not running"));
    }

    // --- Per-context command resolution (issue #802) --------------------

    /// Build a `MeshRow` with only the four command columns set — every other
    /// field defaults. Goes through `MeshRow::from` so the mapping is exercised
    /// alongside the resolver.
    fn row(
        build: Option<&str>,
        run: Option<&str>,
        root_build: Option<&str>,
        root_run: Option<&str>,
    ) -> MeshRow {
        MeshRow::from(&crate::models::Mesh {
            build_command: build.map(str::to_string),
            run_command: run.map(str::to_string),
            root_build_command: root_build.map(str::to_string),
            root_run_command: root_run.map(str::to_string),
            ..Default::default()
        })
    }

    /// Worktree Build context always uses `build_command`, even when a
    /// `root_build_command` is set — the root command must not leak into a
    /// Worktree Node's build.
    #[test]
    fn resolve_command_build_worktree_uses_build_command() {
        let r = row(Some("npm run build"), None, Some("cargo build --workspace"), None);
        assert_eq!(
            resolve_build_run_command(BuildRunMode::Build, false, &r),
            Some("npm run build")
        );
    }

    /// Root Build context prefers `root_build_command` when set.
    #[test]
    fn resolve_command_build_root_uses_root_build_command() {
        let r = row(Some("npm run build"), None, Some("cargo build --workspace"), None);
        assert_eq!(
            resolve_build_run_command(BuildRunMode::Build, true, &r),
            Some("cargo build --workspace")
        );
    }

    /// Root Build with no `root_build_command` falls back to `build_command`
    /// — PR #801's behaviour, unchanged for meshes without the new field.
    #[test]
    fn resolve_command_build_root_falls_back_to_build_command() {
        let r = row(Some("npm run build"), None, None, None);
        assert_eq!(
            resolve_build_run_command(BuildRunMode::Build, true, &r),
            Some("npm run build")
        );
    }

    /// Worktree Run context always uses `run_command`.
    #[test]
    fn resolve_command_run_worktree_uses_run_command() {
        let r = row(None, Some("npm run dev"), None, Some("cargo run -p app"));
        assert_eq!(
            resolve_build_run_command(BuildRunMode::Run, false, &r),
            Some("npm run dev")
        );
    }

    /// Root Run context prefers `root_run_command` when set.
    #[test]
    fn resolve_command_run_root_uses_root_run_command() {
        let r = row(None, Some("npm run dev"), None, Some("cargo run -p app"));
        assert_eq!(
            resolve_build_run_command(BuildRunMode::Run, true, &r),
            Some("cargo run -p app")
        );
    }

    /// Root Run with no `root_run_command` falls back to `run_command`.
    #[test]
    fn resolve_command_run_root_falls_back_to_run_command() {
        let r = row(None, Some("npm run dev"), None, None);
        assert_eq!(
            resolve_build_run_command(BuildRunMode::Run, true, &r),
            Some("npm run dev")
        );
    }

    /// A mesh with no command configured at all resolves to `None` so the
    /// caller surfaces the "not configured" error.
    #[test]
    fn resolve_command_unconfigured_is_none() {
        let r = row(None, None, None, None);
        assert_eq!(resolve_build_run_command(BuildRunMode::Build, true, &r), None);
        assert_eq!(resolve_build_run_command(BuildRunMode::Build, false, &r), None);
        assert_eq!(resolve_build_run_command(BuildRunMode::Run, true, &r), None);
        assert_eq!(resolve_build_run_command(BuildRunMode::Run, false, &r), None);
    }

    /// Terminal mode needs no command in either context — always `Some("")`.
    #[test]
    fn resolve_command_terminal_is_empty_in_both_contexts() {
        let r = row(Some("b"), Some("r"), Some("rb"), Some("rr"));
        assert_eq!(resolve_build_run_command(BuildRunMode::Terminal, true, &r), Some(""));
        assert_eq!(resolve_build_run_command(BuildRunMode::Terminal, false, &r), Some(""));
    }
}
