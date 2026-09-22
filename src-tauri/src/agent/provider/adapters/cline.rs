//! Cline CLI adapter — the Cline terminal coding agent (`cline`, Cline 3.0.x).
//!
//! Cline is a **Native Provider**: it owns its own authentication (`cline auth`
//! writes `~/.cline/data/settings/providers.json`), so Buildmesh manages no
//! Cline credentials and never writes Cline's config. The only Buildmesh-owned
//! seam is the spawn-time env-var injection
//! ([`crate::preferences::resolve_provider_env`]), which layers the user's
//! Buildmesh Model Provider credentials onto the child process — see
//! `preferences::compatibility::resolve_pairing`'s surface-level fallback and
//! `docs/learning/cline-harness-capabilities.md`.
//!
//! **Verified against `cline` 3.0.62** (`--help` / `--version`, 2026-09):
//!
//! - **Fresh**: `cline -i` opens the interactive TUI (bare `cline` is
//!   interactive too; `-i`/`--tui` is the explicit form). `cline -i "<prompt>"`
//!   seeds the first turn from the positional prompt argument.
//! - **Resume**: `cline -i --id <id>` reuses an existing session. Cline
//!   *self-assigns* ids shaped `<epochms>_<5 chars from [0-9a-z]>`
//!   (e.g. `1789757012702_7of3e`); there is no mint flag, so
//!   [`AgentProvider::session_assign_args`] stays empty and live capture is a
//!   follow-up (issue #1774).
//! - **Model / effort**: `-m`/`--model <id>` (free-form and provider-scoped —
//!   never validated against a static list) and `--thinking
//!   none|low|medium|high|xhigh` (bare `--thinking` means `medium`).
//! - **Spawn**: on Windows the npm install is a `cline.cmd` shim, so the recipe
//!   wraps with `WindowsShell::Cmd` → `cmd.exe /c cline …`; `CreateProcess`
//!   cannot execute a `.cmd` directly. macOS/Linux spawn the real executable
//!   directly (`WindowsShell::Direct`).
//!
//! **Never passed** ([`CLINE_NEVER_PASS`]): Cline's own orchestration surfaces
//! duplicate Buildmesh's, or are inert/unwanted:
//!
//! - `--worktree`, `--kanban`, `-z`/`--zen`, `--team-name` — Cline's own
//!   worktree / background-hub / board orchestration.
//! - `--yolo` — named in some Cline docs but absent from 3.0.62's `--help`;
//!   never depend on it.
//! - `--hooks-dir` / `CLINE_HOOKS_DIR` — documented but inert in 3.0.62.
//! - `--data-dir` — auto-enables Cline's sandbox mode, rejected as the default
//!   by the state-isolation decision (#1779). `CLINE_DATA_DIR` stays available
//!   as a per-Mesh escape hatch outside the recipe.
//!
//! **State**: a single shared `~/.cline`, so the capture/resume invariant
//! collapses to "same `--cwd`" (the `--data-dir` half is deliberately absent).
//! WSL is not a supported or tested target in this slice — the guest-side
//! cross-runtime probes skip Cline (`detection::WSL_EXCLUDED`).
//!
//! **Attention** (issue #1775): Cline's CLI resolves *file hooks* — executable
//! files named exactly after an event — from four fixed directories
//! (`~/Documents/Cline/Hooks`, `~/.cline/hooks`, `<ws>/.clinerules/hooks`,
//! `<ws>/.cline/hooks`). Buildmesh provisions `<cline home>/hooks/<Event>.<ext>`
//! for the two events it can honestly normalise: `TaskComplete` → `agent_end`
//! (a completed turn) and `SessionShutdown` → `session_shutdown`. Each file
//! POSTs its stdin payload to the local attention route, expanding
//! `$BUILDMESH_PORT` / `$BUILDMESH_SESSION_ID` at hook-run time (Cline runs file
//! hooks with the inherited `process.env`, so one node-agnostic file set serves
//! every node and never bakes a node id). `--hooks-dir` / `CLINE_HOOKS_DIR` are
//! inert in 3.0.62, so the fixed paths are the only lever. Cline has no
//! permission/question/background primitive under its default auto-approve
//! launch, so those kinds stay unadvertised.
//!
//! **Transcript**: not wired yet (`#1776`), so `produces_readable_transcript`
//! stays `false` and the digest degrades to a spine-only read.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode, EffortControlKind};
use crate::agent::provider::{
    AgentProvider, LaunchRuntime, Platform, ResolvedPath, SpawnRecipe, UiMeta, WindowsShell,
};
use crate::agent::session_lifecycle::LifecycleKind;
use crate::models::EnvType;

pub struct ClineAdapter;
pub static CLINE: ClineAdapter = ClineAdapter;

/// Cline's own orchestration / inert flags. Never emitted on any platform —
/// regression-pinned by `spawn_recipe_never_carries_cline_orchestration_flags`.
pub const CLINE_NEVER_PASS: &[&str] = &[
    "--worktree",
    "--kanban",
    "--zen",
    "-z",
    "--team-name",
    "--yolo",
    "--hooks-dir",
    "--data-dir",
];

/// Per-platform shell selection. Mirrors the OpenCode pattern
/// (`adapters/opencode.rs`): Windows resolves the npm `.cmd` shim through
/// `cmd.exe`; macOS/Linux spawn the real binary directly.
fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::Cmd,
    }
}

// ── Attention hook provisioning (issue #1775) ───────────────────────────────

/// Minimum Cline release the Buildmesh attention hook has been validated
/// against (issue #1775). Verified against `cline` 3.0.62's file-hook layer
/// (`sdk/packages/core/src/hooks/hook-file-config.ts`): `TaskComplete` →
/// `agent_end`, `SessionShutdown` → `session_shutdown`.
pub const CLINE_MIN_HOOK_VERSION: &str = "3.0.62";

/// Marker identifying a Buildmesh-owned Cline hook file. Cline resolves hook
/// files by *exact base name* (`toHookConfigFileName` strips only the
/// extension), so the file name is not ours to namespace — this content marker
/// is what distinguishes our hook from a user-authored file at the same path.
pub const CLINE_HOOK_MARKER: &str = "BUILDMESH_CLINE_ATTENTION_HOOK";

/// The Cline file hooks Buildmesh provisions, written verbatim as the file's
/// base name. `TaskComplete` maps to `agent_end` (turn finished) and
/// `SessionShutdown` to `session_shutdown` (session going away). No other file
/// is written: Cline has no permission/question/background primitive under its
/// default auto-approve launch, so those events are never claimed.
const CLINE_PROVISIONED_HOOKS: &[&str] = &["TaskComplete", "SessionShutdown"];

/// POSIX hook body. Expands the callback URL from `BUILDMESH_PORT` /
/// `BUILDMESH_SESSION_ID` at hook-run time, so one file serves every node.
/// Prints `{}` so a blocking hook still returns valid control JSON; a
/// non-Buildmesh Cline session (env absent) is a no-op.
///
/// The URL uses the literal `127.0.0.1`, not `localhost`: the attention server
/// binds IPv4 loopback explicitly, and a numeric literal keeps the callback off
/// DNS/`::1` resolution on the hook's hot path. `--noproxy '*'` is the POSIX
/// mirror of the PowerShell path's disabled default proxy — curl has no
/// built-in loopback exemption, so without it a user's `http_proxy` would send
/// the loopback POST to the corporate proxy and the delivery would fail
/// silently (`|| true`).
const CLINE_HOOK_SCRIPT_SH: &str = r#"#!/usr/bin/env bash
# Buildmesh Cline attention hook. Marker: BUILDMESH_CLINE_ATTENTION_HOOK
# Managed by Buildmesh — edits are overwritten on the next spawn.
# 127.0.0.1 (not localhost) + --noproxy '*' keep the callback off DNS/IPv6
# resolution and off any machine-configured HTTP proxy.
payload=$(cat)
if [ -n "$BUILDMESH_PORT" ] && [ -n "$BUILDMESH_SESSION_ID" ]; then
  printf '%s' "$payload" | curl -sf --noproxy '*' --connect-timeout 1 --max-time 2 -o /dev/null \
    -X POST -H "Content-Type: application/json" --data-binary @- \
    "http://127.0.0.1:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID" || true
fi
printf '{}'
"#;

/// Windows hook body (`powershell -File`). Sends the payload as UTF-8 bytes so
/// `agent_end`'s `turn.outputText` survives non-ASCII. Three lines are
/// load-bearing: the literal `127.0.0.1` (not `localhost`) keeps the callback
/// off DNS/`::1` resolution, `DefaultWebProxy = $null` stops a machine proxy
/// intercepting a loopback POST, and `Expect100Continue = $false` stops
/// `HttpWebRequest` waiting for an interim `100 Continue` that Buildmesh's HTTP
/// server never emits (the hook would otherwise stall and drop the callback).
/// See [`CLINE_HOOK_SCRIPT_SH`].
const CLINE_HOOK_SCRIPT_PS1: &str = r#"# Buildmesh Cline attention hook. Marker: BUILDMESH_CLINE_ATTENTION_HOOK
# Managed by Buildmesh - edits are overwritten on the next spawn.
# 127.0.0.1 (not localhost) keeps the callback off DNS/IPv6 resolution.
$ErrorActionPreference = 'SilentlyContinue'
[System.Net.WebRequest]::DefaultWebProxy = $null
[System.Net.ServicePointManager]::Expect100Continue = $false
$payload = [Console]::In.ReadToEnd()
if ($env:BUILDMESH_PORT -and $env:BUILDMESH_SESSION_ID) {
  $url = "http://127.0.0.1:$($env:BUILDMESH_PORT)/api/attention/$($env:BUILDMESH_SESSION_ID)"
  try {
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($payload)
    Invoke-WebRequest -UseBasicParsing -Method Post -Uri $url -ContentType 'application/json' -Body $bytes -TimeoutSec 2 | Out-Null
  } catch { }
}
[Console]::Out.Write('{}')
"#;

/// The hook-file extension Cline executes for this spawn. Windows uses
/// PowerShell (`powershell -File`); every other runtime uses the POSIX script
/// (`bash <path>`). Both extensions are first-class in Cline's
/// `inferHookCommand`, so neither needs an exec bit and neither depends on
/// `node`/`bun` being on `PATH`. The predicate mirrors `mcode`: a macOS/Linux
/// host reports `EnvType::Windows` for a native path, so the host OS — not only
/// `env_type` — decides the shell.
fn hook_extension(env_type: EnvType) -> &'static str {
    if cfg!(target_os = "windows") && env_type == EnvType::Windows {
        "ps1"
    } else {
        "sh"
    }
}

fn hook_script(env_type: EnvType) -> &'static str {
    if hook_extension(env_type) == "ps1" {
        CLINE_HOOK_SCRIPT_PS1
    } else {
        CLINE_HOOK_SCRIPT_SH
    }
}

/// Resolve the directory Buildmesh's Cline hook files live in:
/// `<cline home>/hooks`. Cline's four search roots also include
/// `<ws>/.clinerules/hooks` and `<ws>/.cline/hooks`, but Buildmesh provisions
/// only the user-global root — the hook content is node-agnostic, so a single
/// file set serves every node and never writes into (or pollutes) a node's
/// worktree.
///
/// `runtime.harness_home` wins when set; otherwise the home is resolved through
/// `cli_dir_for_spawn` so a WSL/Interop guest resolves the *guest* home
/// converted back to a host path (never a Linux path handed to a Windows API).
/// `None` means no home was resolvable — the caller returns `Ok(())` with no
/// side effects, matching the mcode precedent.
fn hooks_dir(resolved: &ResolvedPath, runtime: &LaunchRuntime) -> Option<PathBuf> {
    if let Some(home) = runtime.harness_home.as_deref() {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(crate::env::to_host_path(trimmed)).join("hooks"));
        }
    }
    crate::env::cli_dir_for_spawn(crate::env::cline_dir(), ".cline", &resolved.spawn_path)
        .map(|dir| dir.join("hooks"))
}

/// Atomically persist `content` to `path` via a sibling temp file + rename.
/// Mirrors `grok.rs:133` / `mcode.rs` — a crash leaves the canonical file
/// untouched.
fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(content.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map(|_| ()).map_err(|error| error.error)
}

/// Add or refresh one Buildmesh-owned hook file, idempotently and without ever
/// clobbering a user-authored file.
///
/// - Missing file → write it.
/// - Existing file carrying [`CLINE_HOOK_MARKER`] → rewrite only when the
///   content drifted (a re-provision with identical content is a no-op, so no
///   spurious mtime bump — issue #886).
/// - Existing file **without** the marker → `Err`. Cline names hooks by event,
///   so this path is not ours to take silently; surfacing the error lets the
///   spawn mark `SignalHealth::Unavailable` instead of destroying the user's
///   hook (the mcode / cursor malformed-file precedent).
fn ensure_hook_file(path: &Path, content: &str) -> Result<(), String> {
    match std::fs::read_to_string(path) {
        Ok(existing) => {
            if !existing.contains(CLINE_HOOK_MARKER) {
                return Err(format!(
                    "refusing to overwrite non-Buildmesh Cline hook at {path:?}; \
                     rename or remove it and respawn"
                ));
            }
            if existing == content {
                return Ok(());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("failed to read {path:?}: {error}")),
    }
    write_atomic(path, content)
        .map_err(|error| format!("failed to write Cline hook {path:?}: {error}"))?;
    tracing::info!("cline provision_attention_hooks: wrote {path:?}");
    Ok(())
}

/// Inner provisioner, split out so the "no home resolvable" branch is testable
/// without the spawn-path contract. Returns `Ok(())` with no side effects when
/// `hooks_root` is `None`.
///
/// Each event is provisioned independently: a file we refuse to clobber for
/// one event (a user-authored hook at that exact name) must not suppress the
/// other signal — the two hooks are unrelated, and losing the session-exit
/// callback because the turn-completion file is occupied would be gratuitous.
/// Every failure is logged; the first is returned so the spawn path can mark
/// the node `SignalHealth::Unavailable` (which a later successful callback
/// clears).
fn provision_at(
    hooks_root: Option<&Path>,
    env_type: EnvType,
) -> Result<(), String> {
    let Some(root) = hooks_root else {
        tracing::debug!(
            "cline provision_attention_hooks: hook config root unresolvable; \
             skipping with no side effects"
        );
        return Ok(());
    };
    std::fs::create_dir_all(root)
        .map_err(|error| format!("failed to create Cline hooks dir {root:?}: {error}"))?;
    let script = hook_script(env_type);
    let extension = hook_extension(env_type);
    let mut first_error: Option<String> = None;
    for event in CLINE_PROVISIONED_HOOKS {
        let path = root.join(format!("{event}.{extension}"));
        if let Err(error) = ensure_hook_file(&path, script) {
            tracing::warn!("cline provision_attention_hooks: {error}");
            first_error.get_or_insert(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Off-`PATH` install candidates for Cline, in the documented resolver order:
///
/// 1. `CLINE_BIN_PATH` when set (wins unconditionally).
/// 2. `%APPDATA%\npm\cline.cmd` — the npm shim `cmd.exe` resolves when the
///    npm prefix is on `PATH`.
/// 3. `%APPDATA%\npm\node_modules\@cline\cli-windows-{x64,arm64}\bin\cline.exe`
///    — the platform binary spawned directly (avoids `cmd.exe` and Node, at
///    the cost of the wrapper's CA-cert harvesting:
///    `~/.cline/cli-node-extra-ca-certs.pem` → `NODE_EXTRA_CA_CERTS`). Both
///    architectures are probed because `@cline/cli` ships separate
///    platform-specific binaries and the npm prefix doesn't symlink them.
///
/// Pure — the caller supplies `appdata` — so the order is unit-testable
/// without touching the real filesystem. `detection::detect_installed_profiles`
/// probes these in order before falling back to the generic `PATH` sweep.
pub fn install_candidates(env_bin_path: Option<&str>, appdata: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = env_bin_path.map(str::trim).filter(|p| !p.is_empty()) {
        candidates.push(PathBuf::from(path));
    }
    if let Some(appdata) = appdata {
        let npm = appdata.join("npm");
        candidates.push(npm.join("cline.cmd"));
        // Both architectures — `@cline/cli` ships separate platform packages
        // and npm installs the one matching the host CPU. The walk can't
        // pre-know which is on disk, so it probes both in deterministic order
        // (x64 first to match the documented CLI naming) and lets the
        // existence check pick whichever exists.
        for arch in ["x64", "arm64"] {
            candidates.push(
                npm.join("node_modules")
                    .join("@cline")
                    .join(format!("cli-windows-{arch}"))
                    .join("bin")
                    .join("cline.exe"),
            );
        }
    }
    candidates
}

/// The first [`install_candidates`] entry that exists — `CLINE_BIN_PATH` wins
/// when set. **The resolved absolute path is what `spawn_environment::wrap`
/// receives as `executable_override`** (issue #1773 review — previously the
/// path was discarded and `spawn_recipe.binary = "cline"` was always used,
/// which fails with `'cline' is not recognized` for off-PATH installs).
pub fn resolve_install(
    env_bin_path: Option<&str>,
    appdata: Option<&Path>,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    install_candidates(env_bin_path, appdata)
        .into_iter()
        .find(|candidate| exists(candidate))
}

impl AgentProvider for ClineAdapter {
    fn id(&self) -> &'static str {
        "cline"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Cline".into(),
            // Cline's ship-candidate mascot green (#3DDC84). Verify against the
            // current Cline brand at PR time; the frontend brand registry
            // (`src/lib/brandRegistry.tsx`) carries the same hex.
            color: "#3DDC84".into(),
            icon: "C".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "cline",
            base_args: vec!["-i".into()],
            trailing_args: Vec::new(),
            windows_shell: shell_for(platform),
        }
    }

    fn supports_resume(&self) -> bool {
        true
    }

    /// `true` — the file hooks are provisioned at spawn (issue #1775) and open
    /// the Autopilot / review-circuit gate. See [`Self::attention_capability`]
    /// for the structured contract.
    fn requires_attention_hook(&self) -> bool {
        true
    }

    /// Issue #1775 — the honest Cline contract. Cline's file-hook layer
    /// (`TaskComplete` → `agent_end`, `SessionShutdown` → `session_shutdown`)
    /// delivers a completed turn and a session-exit signal. Buildmesh launches
    /// Cline with its default auto-approve policy and passes no approval flag,
    /// so no permission prompt is raised — `permission_requested` /
    /// `question_requested` / `background_running` / `process_idle` are
    /// impossible by construction and are not advertised.
    fn attention_capability(&self) -> AttentionCapability {
        AttentionCapability::Hook {
            events: vec![LifecycleKind::TurnCompleted, LifecycleKind::SessionExited],
            launch_mode: AttentionLaunchMode::SkipPermissions,
            trust: None,
            min_version: Some(CLINE_MIN_HOOK_VERSION.into()),
        }
    }

    fn auto_resume_on_startup(&self) -> bool {
        true
    }

    fn self_assigns_session_id(&self) -> bool {
        true
    }

    /// Cline's self-assigned ids (`<epochms>_<base36>`) are not UUID-shaped, so
    /// the PTY labeled-UUID regex can never match. Capture is wired separately
    /// (issue #1774).
    fn captures_session_id_from_pty(&self) -> bool {
        false
    }

    fn supports_model_override(&self) -> bool {
        true
    }

    fn supports_extra_args(&self) -> bool {
        true
    }

    fn supports_prefill(&self) -> bool {
        true
    }

    /// Windows, macOS, Linux. WSL is deliberately absent from the supported
    /// story for this slice (see the module docstring).
    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Linux, Platform::Macos]
    }

    /// Cline always self-assigns, so a fresh spawn never carries a mint flag.
    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    /// Resume is `cline -i --id <id>`; the base recipe's `-i` survives the
    /// `default_prepare` composition.
    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--id".into(), id.into()]
    }

    /// Issue #1774: Cline's TUI never prints its self-assigned
    /// `<epochms>_<base36>` id, so the PTY labeled-UUID regex in
    /// `session_capture` can never match. Start the SQLite poller
    /// (`services::cline_session::start_capture_poller`) so the id lands
    /// in `cli_session_id` within ~1-2s of spawn — the same shape AGY
    /// and OpenCode use for their self-assigned ids.
    ///
    /// `spawn_path` is the spawn-time directory of the node (Root Node:
    /// mesh path; Worktree Node: resolved worktree directory). The
    /// poller matches it against the Cline `sessions.cwd` column so a
    /// sibling spawn in the same mesh root does not steal the row.
    fn after_fresh_spawn(
        &self,
        node_id: i64,
        spawn_path: &str,
        env_type: EnvType,
        _app: &tauri::AppHandle,
    ) {
        crate::services::cline_session::start_capture_poller(
            node_id,
            spawn_path.to_string(),
            env_type,
        );
    }

    /// Issue #1774: read Cline's `<home>/data/db/sessions.db` to find
    /// an interactive session row whose `cwd` matches the suspended
    /// node and whose embedded epoch ms is inside the recovery window.
    /// Returns the candidate id only — the startup service decides
    /// whether to persist it (via `db::recover_suspended_cli_session_id`)
    /// based on the durable process generation.
    fn recover_suspended_session_id(
        &self,
        spawn_path: &str,
        env_type: EnvType,
        anchor_ms: i64,
        recorded_start: bool,
    ) -> Option<String> {
        crate::services::cline_session::find_historic_id_for_directory(
            env_type,
            spawn_path,
            anchor_ms,
            recorded_start,
        )
    }

    /// `cline -i "<prefill>"` — the positional prompt seeds the TUI's first
    /// turn. **Returns just the prefill text**; the base `-i` is already in
    /// `spawn_recipe.base_args` and `default_prepare` extends `base_args`
    /// with this list. Emitting `-i` here too would compose to
    /// `cline -i -i "<text>"` — a flag the Cline CLI accepts (last write
    /// wins) but that we shouldn't rely on. Regression-pinned by
    /// `launch::tests::cline_prefill_composes_without_repeating_the_tui_flag`.
    ///
    /// Platform-aware line-end handling lives one level up in
    /// [`crate::agent::launch::normalize_prefill_for_platform`]: Windows
    /// flattens newlines to single spaces (the `cmd.exe /c` requirement),
    /// macOS/Linux preserves them (direct spawn).
    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec![text.to_string()]
    }

    /// Cline's closed-vocabulary reasoning-effort flag is `--thinking`
    /// (not the trait default `--effort`).
    fn effort_args(&self, effort: &str) -> Vec<String> {
        vec!["--thinking".into(), effort.into()]
    }

    fn effort_control(&self) -> EffortControlKind {
        EffortControlKind::Closed {
            allowed: crate::agent::capabilities::CLINE_EFFORT_ALLOWED
                .iter()
                .map(|value| value.to_string())
                .collect(),
        }
    }

    /// Provision Cline's attention file hooks (issue #1775). Writes
    /// `<cline home>/hooks/TaskComplete.<ext>` and `.../SessionShutdown.<ext>`
    /// with a node-agnostic script that expands the callback URL from
    /// `BUILDMESH_PORT` / `BUILDMESH_SESSION_ID` at hook-run time.
    ///
    /// The write is additive (only our two event files, and we never touch a
    /// file that lacks our marker) and idempotent (issue #886 — an unchanged
    /// file is not rewritten). A user-authored file at our exact path returns
    /// `Err` rather than being clobbered; an unresolvable home returns `Ok(())`
    /// with no side effects, so an unusual spawn still proceeds with only the
    /// attention callback lost.
    fn provision_attention_hooks(
        &self,
        resolved: &ResolvedPath,
        runtime: &LaunchRuntime,
        _node_id: i64,
    ) -> Result<(), String> {
        // `node_id` is unused: the URL expands from the per-agent environment
        // Cline inherits (`env: process.env`), so the file is node-agnostic and
        // shared across nodes.
        provision_at(hooks_dir(resolved, runtime).as_deref(), resolved.env_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows keeps `WindowsShell::Cmd` because the npm install is a `.cmd`
    /// batch shim; `CreateProcess` will not execute it directly. macOS/Linux
    /// spawn the real executable. Regression for the OpenCode #827 class of
    /// bug (a recipe that hardcodes one shell for every platform).
    #[test]
    fn spawn_recipe_uses_cmd_on_windows_and_direct_elsewhere() {
        for platform in CLINE.available_on() {
            let recipe = CLINE.spawn_recipe(*platform, EnvType::Windows);
            assert_eq!(recipe.binary, "cline", "binary must be exactly `cline` on {platform:?}");
            assert_eq!(
                recipe.base_args,
                vec!["-i".to_string()],
                "base recipe must be exactly `[\"-i\"]` on {platform:?}; got {:?}",
                recipe.base_args
            );
            assert!(recipe.trailing_args.is_empty());
            let expected = if *platform == Platform::Windows {
                WindowsShell::Cmd
            } else {
                WindowsShell::Direct
            };
            assert_eq!(
                recipe.windows_shell, expected,
                "{platform:?} must use {expected:?} — the npm shim needs cmd.exe, \
                 the macOS/Linux executables spawn directly"
            );
        }
    }

    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(CLINE.id(), "cline");
        let ui = CLINE.ui();
        assert_eq!(ui.label, "Cline");
        assert_eq!(ui.color, "#3DDC84");
        assert_eq!(ui.icon, "C");
    }

    #[test]
    fn available_on_excludes_no_supported_platform_and_has_no_wsl_handling() {
        let platforms = CLINE.available_on();
        assert_eq!(
            platforms.len(),
            3,
            "available_on must pin to exactly {{Windows, Linux, Macos}}; got {platforms:?}"
        );
        assert!(platforms.contains(&Platform::Windows));
        assert!(platforms.contains(&Platform::Linux));
        assert!(platforms.contains(&Platform::Macos));
    }

    #[test]
    fn self_assigns_session_id_and_never_mints_one() {
        assert!(CLINE.self_assigns_session_id());
        assert!(
            !CLINE.captures_session_id_from_pty(),
            "Cline's <epochms>_<base36> ids are not UUID-shaped; PTY capture must stay off"
        );
        assert!(
            CLINE.session_assign_args("anything").is_empty(),
            "Cline self-assigns; session_assign_args must stay empty (no mint flag)"
        );
    }

    #[test]
    fn resume_args_use_id_flag() {
        assert_eq!(
            CLINE.resume_args("1789757012702_7of3e"),
            vec!["--id", "1789757012702_7of3e"],
            "Cline resume is `--id <id>`, not `--resume` / `--session`"
        );
    }

    #[test]
    fn prefill_args_return_only_the_positional_prompt() {
        // `prefill_args` carries the prefill text only — the base recipe's
        // `-i` is composed with this list by `default_prepare`. Pin both:
        // the lone token (so a future edit doesn't smuggle `-i` back in) and
        // its exact text.
        assert_eq!(CLINE.prefill_args("fix the auth bug"), vec!["fix the auth bug"]);
    }

    #[test]
    fn prefill_args_pass_text_through_unchanged() {
        // Platform-aware newline handling lives in
        // `launch::normalize_prefill_for_platform`. The adapter's contract
        // is now strictly "return the prefill text verbatim" — the leading
        // space-join for `cmd.exe /c` would destroy multi-line prompts on
        // macOS/Linux (issue #1773 review).
        let args = CLINE.prefill_args("fix auth\nthen run tests");
        assert_eq!(args, vec!["fix auth\nthen run tests"]);
    }

    #[test]
    fn effort_args_use_thinking_flag() {
        assert_eq!(CLINE.effort_args("xhigh"), vec!["--thinking", "xhigh"]);
    }

    #[test]
    fn model_args_use_long_form() {
        assert_eq!(CLINE.model_args("unbiased/pareto"), vec!["--model", "unbiased/pareto"]);
    }

    #[test]
    fn capabilities_descriptor_advertises_attention_and_honest_empty_transcript() {
        let caps = CLINE.capabilities();
        assert_eq!(caps.harness_id, "cline");
        assert!(caps.supports_resume);
        assert!(caps.auto_resume_on_startup);
        // Issue #1775 — the file hooks are provisioned and open the gate.
        assert!(caps.requires_attention_hook);
        assert!(matches!(
            caps.attention_capability,
            crate::agent::capabilities::AttentionCapability::Hook { .. }
        ));
        assert!(!caps.supports_passive_turn_watcher);
        // The transcript reader is issue #1776 — still honest-empty.
        assert!(!caps.produces_readable_transcript);
        assert!(caps.supports_model_override);
        assert!(caps.supports_effort_override);
        assert!(caps.supports_extra_args);
        assert!(caps.supports_prefill);
        assert!(!caps.is_plain_terminal);
        assert_eq!(
            caps.effort_control,
            crate::agent::capabilities::EffortControlKind::Closed {
                allowed: crate::agent::capabilities::CLINE_EFFORT_ALLOWED
                    .iter()
                    .map(|value| value.to_string())
                    .collect(),
            }
        );
        assert_eq!(
            caps.available_on,
            vec!["windows".to_string(), "linux".to_string(), "macos".to_string()]
        );
    }

    /// Regression for the ticket's "what the recipe MUST NOT carry" list. The
    /// maximal composition (resume + model + effort + extras + prefill) is
    /// checked on every supported platform so a future edit that smuggles one
    /// of Cline's own orchestration flags into the base recipe, a `*_args`
    /// helper, or the prefill path trips here.
    #[test]
    fn spawn_recipe_never_carries_cline_orchestration_flags() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

        for platform in CLINE.available_on() {
            for session in [
                SessionIdModeRef::None,
                SessionIdModeRef::Resume("1789757012702_7of3e"),
            ] {
                let config = ResolvedAgentConfig {
                    model: Some("unbiased/pareto".to_string()),
                    effort: Some("xhigh".to_string()),
                    extra_args: Some("--verbose".to_string()),
                };
                let input = HarnessLaunchInput {
                    platform: *platform,
                    runtime: EnvType::Windows,
                    session,
                    config: &config,
                    prefill: Some("fix the auth bug"),
                    sandbox: false,
                };
                let prepared = default_prepare(&CLINE, input);
                let argv: Vec<&str> = prepared.recipe.argv().collect();
                for forbidden in CLINE_NEVER_PASS {
                    assert!(
                        !argv.contains(forbidden),
                        "Cline's own orchestration flag {forbidden} must never reach the \
                         argv ({platform:?}, {session:?}); got {argv:?}"
                    );
                }
            }
        }
    }

    /// Fresh vs resume argv shapes through the real composition seam:
    /// `cline -i [--model m] [--thinking e] [--verbose] [-i <prefill>]` and
    /// `cline -i --id <id> …`. Pins the resume flag and the absence of a mint
    /// flag.
    #[test]
    fn default_prepare_fresh_and_resume_argv_shapes() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig::default();
        let fresh = default_prepare(
            &CLINE,
            HarnessLaunchInput {
                platform: Platform::Windows,
                runtime: EnvType::Windows,
                session: SessionIdModeRef::None,
                config: &config,
                prefill: None,
                sandbox: false,
            },
        );
        assert_eq!(fresh.recipe.base_args, vec!["-i".to_string()]);

        let resumed = default_prepare(
            &CLINE,
            HarnessLaunchInput {
                platform: Platform::Windows,
                runtime: EnvType::Windows,
                session: SessionIdModeRef::Resume("1789757012702_7of3e"),
                config: &config,
                prefill: None,
                sandbox: false,
            },
        );
        assert_eq!(
            resumed.recipe.base_args,
            vec!["-i".to_string(), "--id".to_string(), "1789757012702_7of3e".to_string()],
            "resume argv must be `cline -i --id <id>`"
        );
        assert!(
            !resumed.recipe.base_args.iter().any(|a| a == "--session-id"),
            "Cline self-assigns; resume must not carry a mint flag"
        );
    }

    /// Detection resolver order: `CLINE_BIN_PATH` wins even when the npm shim
    /// also exists. Direct-platform binaries probe both `x64` and `arm64`
    /// because `@cline/cli` ships architecture-specific packages.
    #[test]
    fn install_candidates_prefer_env_override_then_npm_shim_then_direct_exe() {
        let appdata = Path::new("C:/Users/me/AppData/Roaming");
        let candidates = install_candidates(Some("D:/tools/cline.exe"), Some(appdata));
        assert_eq!(candidates.len(), 4);
        assert_eq!(candidates[0], PathBuf::from("D:/tools/cline.exe"));
        assert_eq!(candidates[1], appdata.join("npm").join("cline.cmd"));
        assert!(
            candidates[2]
                .to_string_lossy()
                .replace('\\', "/")
                .ends_with("npm/node_modules/@cline/cli-windows-x64/bin/cline.exe"),
            "third candidate must be the x64 direct platform binary; got {:?}",
            candidates[2]
        );
        assert!(
            candidates[3]
                .to_string_lossy()
                .replace('\\', "/")
                .ends_with("npm/node_modules/@cline/cli-windows-arm64/bin/cline.exe"),
            "fourth candidate must be the arm64 direct platform binary; got {:?}",
            candidates[3]
        );

        // Blank / whitespace-only overrides are ignored rather than producing a
        // bogus first candidate.
        let blank = install_candidates(Some("   "), Some(appdata));
        assert_eq!(blank[0], appdata.join("npm").join("cline.cmd"));

        // No appdata (macOS/Linux) leaves only the env override, if any.
        assert!(install_candidates(None, None).is_empty());
    }

    #[test]
    fn resolve_install_picks_the_first_existing_candidate() {
        let appdata = Path::new("C:/appdata");
        let shim = appdata.join("npm").join("cline.cmd");
        let direct_x64 = appdata
            .join("npm")
            .join("node_modules")
            .join("@cline")
            .join("cli-windows-x64")
            .join("bin")
            .join("cline.exe");
        let direct_arm64 = appdata
            .join("npm")
            .join("node_modules")
            .join("@cline")
            .join("cli-windows-arm64")
            .join("bin")
            .join("cline.exe");
        let override_path = PathBuf::from("D:/override/cline.exe");

        // Only the shim exists → shim wins over the absent node_modules binaries.
        let only_shim = |p: &Path| p == shim;
        assert_eq!(
            resolve_install(None, Some(appdata), &only_shim).as_deref(),
            Some(shim.as_path())
        );

        // Shim absent, x64 direct binary present → falls through to the walk
        // in deterministic order. x64 is checked first (documented CLI order).
        let only_x64 = |p: &Path| p == direct_x64;
        assert_eq!(
            resolve_install(None, Some(appdata), &only_x64).as_deref(),
            Some(direct_x64.as_path())
        );

        // x64 absent, arm64 direct binary present → falls through to arm64.
        let only_arm64 = |p: &Path| p == direct_arm64;
        assert_eq!(
            resolve_install(None, Some(appdata), &only_arm64).as_deref(),
            Some(direct_arm64.as_path())
        );

        // Everything present → the override wins (resolver order).
        let all = |p: &Path| {
            p == shim || p == direct_x64 || p == direct_arm64 || p == override_path
        };
        assert_eq!(
            resolve_install(Some("D:/override/cline.exe"), Some(appdata), &all).as_deref(),
            Some(override_path.as_path())
        );

        // Nothing present → None (no false-positive menu row).
        assert!(resolve_install(None, Some(appdata), &|_| false).is_none());
    }

    // —— Issue #1774: session-id capture wiring ——————————————————————————

    /// Cline self-assigns and disables PTY capture. The fresh-spawn
    /// hook must run a SQLite poller (`services::cline_session`) — the
    /// two capability flags below are the contract the registry reads
    /// to decide whether to wire that poller. **This test asserts only
    /// the flags**: it would NOT catch a refactor that left the flags
    /// intact but emptied the `after_fresh_spawn` body, or rewired the
    /// hook to call a different helper. The flag pins are the
    /// contract; the hook body itself (the call into
    /// `services::cline_session::start_capture_poller`) is currently
    /// unpinned — the helper's tests cover the historic-recovery path
    /// via `find_historic_id_for_db_path`, which exercises
    /// `list_sessions_in_window` and `select_recovery_identity`, but
    /// neither `try_capture_from_db_path` (the fresh-capture SQLite
    /// read) nor the retry loop in `start_capture_poller` has a
    /// dedicated test today.
    #[test]
    fn self_assigns_session_id_and_skips_pty_uuid_capture() {
        // Cline mints its own session ids; auto-resume must drive --id
        // (the registry reads `self_assigns_session_id` to know whether
        // to wire the after_fresh_spawn poller).
        assert!(
            CLINE.self_assigns_session_id(),
            "Cline mints its own session ids; auto-resume must drive --id"
        );
        // Cline ids are `<epochms>_<base36>`, not UUIDs, so the PTY
        // labeled-UUID regex can never match — capture must stay off
        // and a separate SQLite poller (issue #1774) reads the id.
        assert!(
            !CLINE.captures_session_id_from_pty(),
            "Cline ids are <epochms>_<base36>, not UUIDs — PTY capture must stay off"
        );
    }

    /// `recover_suspended_session_id` is the durable path used by the
    /// startup sweep (issue #1774 / issue #1224 family). It must surface
    /// `None` when no home is resolvable (e.g. an `$HOME`-less Linux
    /// container) — a real `None` is what lets the sweep skip the node
    /// instead of binding garbage. The positive path is exercised by
    /// `services::cline_session::tests::historic_*` against a controlled
    /// SQLite fixture; this test pins the no-home contract for the
    /// adapter seam itself.
    #[test]
    fn recover_suspended_session_id_returns_none_for_unresolvable_path() {
        // No home resolvable in a bare test env: the helper returns None.
        let no_home_result = CLINE.recover_suspended_session_id("/no/such/path", EnvType::Wsl, 0, false);
        assert!(
            no_home_result.is_none(),
            "without a resolvable Cline home, the adapter must return None, not a synthesised id"
        );
    }

    // —— Issue #1775: attention hook provisioning ———————————————————————

    /// Pin the structured attention contract. `agent_end`/`session_shutdown`
    /// are the only events Cline's file-hook layer emits for us; a permission
    /// or question signal is impossible under the default auto-approve launch
    /// and must not be claimed.
    #[test]
    fn attention_capability_advertises_turn_completed_and_session_exited_only() {
        let capability = CLINE.attention_capability();
        match &capability {
            AttentionCapability::Hook {
                events,
                launch_mode,
                trust,
                min_version,
            } => {
                assert_eq!(
                    events,
                    &vec![LifecycleKind::TurnCompleted, LifecycleKind::SessionExited],
                    "TaskComplete + SessionShutdown are the only wired events"
                );
                for impossible in [
                    LifecycleKind::PermissionRequested,
                    LifecycleKind::QuestionRequested,
                    LifecycleKind::BackgroundRunning,
                    LifecycleKind::ProcessIdle,
                ] {
                    assert!(
                        !events.contains(&impossible),
                        "{impossible:?} has no Cline primitive and must not be advertised"
                    );
                }
                assert_eq!(*launch_mode, AttentionLaunchMode::SkipPermissions);
                assert!(trust.is_none(), "Cline needs no workspace-trust step: {trust:?}");
                assert_eq!(min_version.as_deref(), Some(CLINE_MIN_HOOK_VERSION));
            }
            _ => panic!("expected Hook, got {capability:?}"),
        }
    }

    #[test]
    fn requires_attention_hook_is_enabled_after_1775() {
        assert!(
            CLINE.requires_attention_hook(),
            "issue #1775 wires the file hooks; reverting to false would re-close the Autopilot gate"
        );
    }

    /// The extension Cline will execute. Pinned so a refactor cannot silently
    /// switch Windows to a script PowerShell cannot run (or vice versa).
    ///
    /// The first assertion is not Windows-only: `runtime_for_spawn_path`
    /// reports `EnvType::Windows` for a *native* path even on a macOS/Linux
    /// host (the enum tracks Windows-ness, not the host OS), so the host OS and
    /// `env_type` must agree before we write a `.ps1`. On a Unix host this
    /// asserts `"sh"` for `EnvType::Windows` — the exact case a bare
    /// `env_type` check would get wrong.
    #[test]
    fn hook_extension_matches_host_platform() {
        let expected_win = if cfg!(target_os = "windows") { "ps1" } else { "sh" };
        assert_eq!(hook_extension(EnvType::Windows), expected_win);
        // Any WSL/Interop runtime always uses the POSIX script.
        assert_eq!(hook_extension(EnvType::Wsl), "sh");
        assert_eq!(hook_extension(EnvType::WindowsInterop), "sh");
        // The chosen script always carries the marker and the callback anchors.
        for script in [CLINE_HOOK_SCRIPT_SH, CLINE_HOOK_SCRIPT_PS1] {
            assert!(script.contains(CLINE_HOOK_MARKER));
            assert!(script.contains("BUILDMESH_PORT"));
            assert!(script.contains("BUILDMESH_SESSION_ID"));
            assert!(script.contains("/api/attention/"));
        }
        assert!(
            !CLINE_HOOK_SCRIPT_SH.contains("Invoke-WebRequest")
                && !CLINE_HOOK_SCRIPT_PS1.contains("curl"),
            "each script must use its own platform's fetch primitive"
        );
        assert!(
            CLINE_HOOK_SCRIPT_SH.contains("--noproxy"),
            "the POSIX hook must bypass any configured HTTP proxy explicitly; \
             without it a user's http_proxy would swallow the loopback callback"
        );
        assert!(
            CLINE_HOOK_SCRIPT_PS1.contains("DefaultWebProxy"),
            "the Windows hook must disable the machine proxy explicitly"
        );
    }

    fn provision_test_home(home: &Path, env_type: EnvType) -> PathBuf {
        let path = home.to_string_lossy().to_string();
        CLINE
            .provision_attention_hooks(
                &crate::env::ResolvedPath {
                    host_path: path.clone(),
                    spawn_path: path.clone(),
                    raw_path: path,
                    env_type,
                },
                &LaunchRuntime {
                    harness_home: Some(home.to_string_lossy().to_string()),
                    wsl_distro: None,
                },
                7,
            )
            .expect("provision_attention_hooks should succeed");
        home.join("hooks")
    }

    #[test]
    fn provision_writes_both_hook_files_with_env_expanded_url() {
        let home = tempfile::tempdir().unwrap();
        let hooks = provision_test_home(home.path(), EnvType::Windows);
        let extension = hook_extension(EnvType::Windows);
        for event in CLINE_PROVISIONED_HOOKS {
            let path = hooks.join(format!("{event}.{extension}"));
            let body = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{path:?} not written: {error}"));
            assert!(body.contains(CLINE_HOOK_MARKER), "{event} missing marker");
            // Node-agnostic: the URL is expanded from the run environment, never
            // baked, so one file set serves every node.
            assert!(
                body.contains("$BUILDMESH_PORT") || body.contains("$env:BUILDMESH_PORT"),
                "{event} must expand the port at hook-run time: {body}"
            );
            assert!(
                !body.contains("/api/attention/7") && !body.contains("/api/attention/7 "),
                "{event} must not bake the node id: {body}"
            );
        }
    }

    #[test]
    fn provision_is_idempotent_and_preserves_sibling_user_files() {
        let home = tempfile::tempdir().unwrap();
        let hooks = provision_test_home(home.path(), EnvType::Windows);
        let extension = hook_extension(EnvType::Windows);
        // A user-authored hook for the same event with a *different* extension
        // coexists — Cline runs every matching file — and must round-trip.
        let sibling = hooks.join("TaskComplete.mjs");
        std::fs::write(&sibling, "console.log('user hook');").unwrap();

        let ours = hooks.join(format!("TaskComplete.{extension}"));
        let first = std::fs::read_to_string(&ours).unwrap();
        let ours_before = std::fs::metadata(&ours).unwrap().modified().unwrap();
        let sibling_before = std::fs::metadata(&sibling).unwrap().modified().unwrap();

        // Exceed any coarse filesystem timestamp granularity so a rewrite is
        // observable through mtime, not merely through content equality.
        std::thread::sleep(std::time::Duration::from_millis(25));

        // Re-provisioning with identical content is a no-op: the file is not
        // rewritten (mtime unchanged — the issue #886 idempotency invariant),
        // and the sibling user hook is untouched.
        provision_test_home(home.path(), EnvType::Windows);
        assert_eq!(std::fs::read_to_string(&ours).unwrap(), first);
        assert_eq!(
            std::fs::metadata(&ours).unwrap().modified().unwrap(),
            ours_before,
            "an unchanged hook must not be rewritten (mtime must not bump)"
        );
        assert_eq!(
            std::fs::read_to_string(&sibling).unwrap(),
            "console.log('user hook');",
            "a sibling user hook must not be touched"
        );
        assert_eq!(
            std::fs::metadata(&sibling).unwrap().modified().unwrap(),
            sibling_before,
            "a sibling user hook must not be rewritten"
        );
    }

    /// A non-Buildmesh file occupying our exact event name is the user's — we
    /// fail closed instead of clobbering it (the mcode / cursor precedent).
    #[test]
    fn provision_refuses_to_clobber_a_non_buildmesh_hook() {
        let home = tempfile::tempdir().unwrap();
        let hooks = home.path().join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let extension = hook_extension(EnvType::Windows);
        let occupied = hooks.join(format!("TaskComplete.{extension}"));
        std::fs::write(&occupied, "#!/bin/sh\necho mine\n").unwrap();

        let path = home.path().to_string_lossy().to_string();
        let result = CLINE.provision_attention_hooks(
            &crate::env::ResolvedPath {
                host_path: path.clone(),
                spawn_path: path.clone(),
                raw_path: path,
                env_type: EnvType::Windows,
            },
            &LaunchRuntime {
                harness_home: Some(home.path().to_string_lossy().to_string()),
                wsl_distro: None,
            },
            7,
        );
        assert!(result.is_err(), "must refuse a user-authored hook: {result:?}");
        assert_eq!(
            std::fs::read_to_string(&occupied).unwrap(),
            "#!/bin/sh\necho mine\n",
            "the user's file must survive intact"
        );
        // The other event is provisioned regardless: one occupied file must not
        // suppress the session-exit signal.
        let other = hooks.join(format!("SessionShutdown.{extension}"));
        let other_body = std::fs::read_to_string(&other)
            .unwrap_or_else(|error| panic!("{other:?} must still be written: {error}"));
        assert!(
            other_body.contains(CLINE_HOOK_MARKER),
            "SessionShutdown must be provisioned even when TaskComplete is occupied"
        );
    }

    #[test]
    fn provision_at_with_no_root_is_a_side_effect_free_no_op() {
        let sandbox = tempfile::tempdir().unwrap();
        let before = std::fs::read_dir(sandbox.path()).unwrap().count();
        let result = provision_at(None, EnvType::Windows);
        assert!(result.is_ok(), "unresolvable home must be Ok(()): {result:?}");
        let after = std::fs::read_dir(sandbox.path()).unwrap().count();
        assert_eq!(before, after, "nothing may be created for an unresolvable root");
    }

    #[test]
    fn provision_leaves_no_tmp_residue() {
        let home = tempfile::tempdir().unwrap();
        let hooks = provision_test_home(home.path(), EnvType::Windows);
        let residue: Vec<_> = std::fs::read_dir(&hooks)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(residue.is_empty(), "atomic write left .tmp residue: {residue:?}");
    }

    /// End-to-end delivery: run the provisioned hook script against a real
    /// loopback listener with `BUILDMESH_*` in its environment and assert it
    /// POSTs the stdin payload to `/api/attention/<node>` and prints `{}`.
    /// This is the evidence that the env-expanded (non-baked) URL actually
    /// reaches the route. The child env carries a dead proxy and no `NO_PROXY`,
    /// so the test also proves the hook bypasses a configured proxy on its own
    /// (the POSIX `--noproxy '*'` / PowerShell `DefaultWebProxy = null`).
    #[test]
    fn provisioned_hook_posts_stdin_to_the_attention_route() {
        use std::io::{Read, Write};
        use std::time::{Duration, Instant};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        // Returns the request, or `None` when the hook never connected — the
        // caller reports that alongside the hook's own stderr rather than
        // panicking inside this thread.
        let server = std::thread::spawn(move || -> Option<(String, Vec<u8>)> {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => return None,
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut headers = Vec::new();
            let mut byte = [0u8; 1];
            while !headers.ends_with(b"\r\n\r\n") {
                if stream.read_exact(&mut byte).is_err() {
                    return None;
                }
                headers.push(byte[0]);
            }
            let headers = String::from_utf8(headers).ok()?;
            // A compliant server answers the interim `Expect: 100-continue`
            // before the body arrives.
            if headers.to_ascii_lowercase().contains("expect: 100-continue") {
                stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").ok()?;
            }
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse().unwrap())
                })?;
            let mut body = vec![0u8; length];
            stream.read_exact(&mut body).ok()?;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
                .ok()?;
            Some((headers, body))
        });

        let home = tempfile::tempdir().unwrap();
        let hooks = provision_test_home(home.path(), EnvType::Windows);
        let extension = hook_extension(EnvType::Windows);
        let script = hooks.join(format!("TaskComplete.{extension}"));
        assert!(script.is_file(), "hook script not written: {script:?}");

        let payload = br#"{"hookName":"agent_end","taskId":"session_1790003303940_9ouga","turn":{"status":"completed","outputText":"caf\u00e9"}}"#;
        let mut input = tempfile::tempfile().unwrap();
        input.write_all(payload).unwrap();
        use std::io::Seek;
        input.rewind().unwrap();

        let mut invocation = if cfg!(target_os = "windows") {
            let mut command = crate::process_util::command_no_window("powershell.exe");
            command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"]);
            command.arg(&script);
            command
        } else {
            let mut command = crate::process_util::command_no_window("bash");
            command.arg(&script);
            command
        };
        invocation
            .env("BUILDMESH_PORT", port.to_string())
            .env("BUILDMESH_SESSION_ID", "741")
            // Point every proxy variable at a dead loopback port and clear any
            // inherited NO_PROXY: the hook must bypass the proxy itself
            // (`--noproxy '*'` / `DefaultWebProxy = null`) rather than relying
            // on the environment to exempt loopback. A hook that honoured the
            // proxy would fail here instead of silently dropping the callback.
            .env("http_proxy", "http://127.0.0.1:1")
            .env("HTTP_PROXY", "http://127.0.0.1:1")
            .env("https_proxy", "http://127.0.0.1:1")
            .env("HTTPS_PROXY", "http://127.0.0.1:1")
            .env("ALL_PROXY", "http://127.0.0.1:1")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .stdin(std::process::Stdio::from(input));
        let output = crate::process_util::run_command_with_timeout(
            invocation,
            "cline attention hook",
            Duration::from_secs(20),
        )
        .unwrap();
        let request = server.join().unwrap();
        assert!(
            output.status.success(),
            "hook exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "{}",
            "hook stdout must be valid control JSON; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let (headers, body) = request.unwrap_or_else(|| {
            panic!(
                "hook never reached the listener; stdout: {:?}, stderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        assert!(
            headers.starts_with("POST /api/attention/741 HTTP/1.1\r\n"),
            "{headers}"
        );
        assert_eq!(body, payload, "the stdin payload must be forwarded verbatim");
    }
}
