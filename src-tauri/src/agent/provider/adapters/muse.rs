//! Muse Code 1.3.0 interactive CLI contract, checked against the installed
//! Linux CLI (`/home/alond/.local/bin/muse` 1.3.0-R3401.1) and the Windows
//! binary (`%LOCALAPPDATA%\Programs\muse\muse.exe`, same version). See
//! docs/learning/windows-wsl-harness-interop.md.
//!
//! **Windows support (Muse 1.3.0, 2026-09).** The native Windows binary is
//! a real PE that accepts the same `--disable-approval` flag as the
//! Linux/macOS builds. The spawn recipe branches to `muse.exe` on
//! `Platform::Windows` (mirrors `claude_direct_recipe`'s `claude.exe`
//! branch at `provider/mod.rs:145-156`); `available_on()` includes
//! `Platform::Windows` so the menu filter at
//! `provider_menu.rs:42` lets the row through. The Spawn Menu rank logic
//! at `detection.rs:351-357` keeps the Windows-native profile (rank 0)
//! ahead of the WSL fallback (rank 2) when both installs are present.
//!
//! **Approval policy (issue #1705).** Every interactive harness Buildmesh
//! spawns runs unattended in a PTY, so each adapter bakes its harness's
//! "don't block on approval prompts" policy into `spawn_recipe`. Muse offers
//! three CLI knobs for that policy:
//!
//! - `--approval-mode <untrusted|on-request|never>` — explicit mode
//!   (default `on-request`).
//! - `--disable-approval` — disables tool approval only. Sibling-harness
//!   precedent: matches OpenCode `--auto` and
//!   AGY / Claude `--dangerously-skip-permissions` in spirit (one flag, one
//!   policy, adapter-owned). Confirmed supported on the Windows build
//!   via `muse.exe --help` ("Disable tool approval prompts for this
//!   workspace run").
//! - `--yolo` — disables approval **AND** sandboxing **AND** trusts the
//!   workspace. Three policies in one. Explicitly rejected by issue #1705 as
//!   too wide for the quiet default.
//!
//! The chosen policy is **`--disable-approval`** (maintainer decision,
//! issue #1705). `--yolo` is never baked in.
//!
//! **Inner OS sandbox (issue #1788).** Muse is the only harness Buildmesh
//! spawns that ships its own always-on OS sandbox: its help states
//! "Safety (approval and the sandbox are ON by default)", so with only
//! `--disable-approval` baked the agent's shell runs OS-constrained while
//! every other harness runs unconstrained. That confinement denies the
//! agent shell access to the OS credential store (Windows Credential
//! Manager / keychain / secret service), which is where gh and git
//! credential helpers resolve github.com auth on default installs —
//! Muse sessions saw an empty credential store and every gh/git network
//! operation returned 401 while sibling harnesses on the same host and
//! user worked. `--disable-sandbox` is Muse's own narrow knob for this
//! ("Disable shell filesystem/network sandboxing for this run"; verified
//! accepted by both the installed Windows 1.3.0 and WSL builds). It is
//! baked on every platform: macOS and Linux Muse also default credentials
//! to the OS keyring, so the same 401 awaits there. This changes only the
//! sandbox half of Muse's "Safety ON" pair — the approval policy above is
//! untouched and workspace trust is not forced, so the issue #1705
//! rejection of `--yolo` still stands.
//!
//! **Workspace trust (issue #1706).** Muse gates a workspace's skills and
//! rules behind an explicit trust decision. `--trust-workspace` ("trust this
//! workspace for this run (load its skills and rules); does not save trust")
//! and `--yolo` are **per-invocation** flags; the persistent store is Muse's
//! config root, `$XDG_CONFIG_HOME/muse/trust.json` when that variable is set
//! and `<home>/.config/muse/trust.json` otherwise — observed on the installed
//! 1.3.0 as
//!
//! ```json
//! { "schema_version": 1,
//!   "projects": { "\\?\F:\src\buildmesh": { "decision": "trusted" } } }
//! ```
//!
//! Keys are the **canonicalized** workspace root, so a Windows-hosted binary
//! writes a verbatim `\\?\…` path while a POSIX binary writes the plain
//! absolute path — both shapes are present in the store of a machine that runs
//! Muse on both sides.
//!
//! Buildmesh **pre-provisions** the entry (`ensure_workspace_trusted`) rather
//! than baking a flag into `spawn_recipe`, because the flag is not accepted on
//! the subcommands Buildmesh drives — Muse rejects `--trust-workspace` on
//! `muse plugins install` — and it does not persist across processes, so a
//! per-run flag could not unblock the project-scoped attention-hook install
//! (#1709) either. The merge is additive (sibling projects and unknown keys
//! round-trip), idempotent, and refuses a malformed user file instead of
//! overwriting it, mirroring the mcode / Cursor provisioning precedent.
//!
//! Buildmesh only ever *adds* a `trusted` decision. Muse's decision type is
//! `ProjectTrustDecision { trusted, untrusted }`, and a workspace Muse has
//! merely *seen* is absent from the map rather than recorded as `untrusted` —
//! verified on 1.3.0, where an untrusted `muse exec` wrote no entry at all
//! while reporting `project-skills-untrusted`. So a present `untrusted` row is
//! a decision someone made, and flipping it on every spawn would silently open
//! a consent gate Buildmesh was never granted: that case is reported as a
//! provisioning failure and the entry is left exactly as written.
//!
//! Only the **PTY spawn path** takes this step. The MSP `muse serve` plane
//! (#1681) has no in-repo launcher — only its telemetry slice landed under
//! `agent::provider::muse` — so there is no second call site to keep in sync;
//! a future serve launcher must provision trust the same way.
//!
//! Two limits are deliberate. The store path is resolved from the **host**
//! process env for a native launch and from the guest's *default* XDG root for
//! a cross-runtime one, so a guest `XDG_CONFIG_HOME` (or a non-default WSL
//! distro, #1697) is not honoured. And the write lock is **in-process**: two
//! Buildmesh instances can still lose each other's entry, because the atomic
//! write is atomic, not additive.
//!
//! **Attention (issue #1709).** The interactive TUI exposes no hook/event flag,
//! so Muse's turn signal comes from the passive watcher
//! (`services::muse_watcher`), mirroring Command Code: `requires_attention_hook`
//! stays `false` and `attention_capability` stays `None`. The watcher consumes
//! the durable session log's run boundaries
//! (`~/.local/share/muse/sessions/YYYY/MM/DD/<uuid>/session.jsonl`).
//!
//! Muse 1.3.0 *does* ship a claude-compatible plugin hook surface (a
//! `.claude-plugin/plugin.json` bundle with `Stop`/`Notification`/… handlers; a
//! live `Stop` posts the payload `http::routes::attention` already classifies
//! as a clean turn completion). It is deliberately **not** provisioned:
//! third-party hooks sit at `review_needed` until an explicit
//! `muse plugins approve`, the install lands in the user's *global* plugin
//! cache, and the node-local `--scope project` path is refused until the
//! workspace is trusted (issue #1706). Wiring it is a separate follow-up — see
//! `docs/research/muse-attention-signals.md`.
//!
//! **Launch mode is `SkipPermissions`.** With `--disable-approval` the harness
//! never raises a tool-approval prompt — every observed `approval_disabled`
//! session carries zero `approval/requested` records — so a
//! `PermissionRequested` lifecycle signal is impossible by construction and is
//! deliberately not classified. A `run/terminal` record yields the node back
//! to the user; `terminal` is `completed | failed | cancelled`.
use crate::agent::provider::{
    AgentProvider, LaunchRuntime, Platform, ResolvedPath, SpawnRecipe, UiMeta, WindowsShell,
};
use crate::models::EnvType;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub struct MuseAdapter;
pub static MUSE: MuseAdapter = MuseAdapter;

/// Muse's configuration directory, relative to the runtime home: the XDG
/// default, `.config/muse`. Verified as `%USERPROFILE%\.config\muse` for the
/// native Windows binary (1.3.0); [`resolve_config_dir`] owns the
/// `XDG_CONFIG_HOME` override and the cross-runtime branches.
const MUSE_CONFIG_DIR: &str = ".config/muse";

/// The config root Muse itself would read for a **native** launch, from the two
/// variables that decide it: `$XDG_CONFIG_HOME/muse` when that variable holds
/// an absolute path, else `<home>/.config/muse`. Split out so the precedence is
/// unit-testable without touching the process env (the
/// `env::environment::muse_auth_path_from_vars` shape).
///
/// A *relative* `$XDG_CONFIG_HOME` is ignored because the XDG spec says to —
/// and because Muse ignores it too, verified live: pointing the variable at a
/// scratch directory makes the installed Windows binary read the workspace as
/// untrusted (its store moved with the variable). Letting a relative value
/// through would resolve the store against the spawn cwd, which is the same
/// silent mismatch this function exists to remove.
fn muse_config_root_from_vars(
    home: Option<std::ffi::OsString>,
    xdg_config_home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(root) = xdg_config_home
        .map(PathBuf::from)
        .filter(|root| root.is_absolute())
    {
        return Some(root.join("muse"));
    }
    home.map(PathBuf::from).map(|home| home.join(MUSE_CONFIG_DIR))
}

/// `trust.json` schema version Buildmesh writes when it creates the file.
const MUSE_TRUST_SCHEMA_VERSION: u64 = 1;

/// The decision Muse records for a trusted workspace.
const MUSE_TRUST_DECISION: &str = "trusted";

/// Serialises the read/merge/write of the machine-global trust store. Two
/// agents can start together from different linked worktrees, so an unguarded
/// read-modify-write could atomically replace a sibling's freshly added entry
/// with our own (a lost update — atomic write only prevents torn reads).
static TRUST_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Counter backing the PID+counter `.tmp` suffix for atomic writes
/// (Cursor / AGY / mcode precedent).
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Post-spawn session-index capture window (issue #1794).
///
/// Muse self-assigns its session id and only publishes it through
/// `session-index.db`, so a fresh node has no identity until the index row
/// appears. The original schedule gave up after ~16 s
/// (`200+500+1000+2000+4000+8000` ms); a slow first boot could still be
/// publishing its index row then, leaving the node with no `cli_session_id`
/// and therefore no observable progress for the life of the run.
///
/// This window extends capture to ~4 minutes so a slow boot is still caught,
/// while staying bounded: once exhausted the node is left unobserved, which the
/// circuit watchdog (issue #1791) turns into a fast failure instead of a silent
/// full-budget stall.
pub(crate) const MUSE_CAPTURE_RETRY_MS: &[u64] = &[
    200, 500, 1_000, 2_000, 4_000, 8_000, 15_000, 30_000, 60_000, 60_000, 60_000,
];

/// Result of the post-spawn identity-capture window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureOutcome {
    /// A session identity (and its watcher) was established.
    Captured,
    /// The node's process exited before a session appeared.
    Stopped,
    /// The window was exhausted with no session identity — the node stays
    /// unobserved, so the circuit watchdog fails it fast (issue #1791).
    GaveUp,
}

/// Drive the post-spawn capture window. `capture` performs one attempt and
/// reports whether the session identity and its watcher are now in place;
/// `sleep` waits between attempts; `is_alive` ends the window early when the
/// node's process has exited. Split out so the extended window, the early stop,
/// and the give-up outcome are unit-testable without real time or a live
/// process.
async fn run_capture_window<C, CF, S, SF>(
    mut capture: C,
    mut sleep: S,
    is_alive: impl Fn() -> bool,
) -> CaptureOutcome
where
    C: FnMut() -> CF,
    CF: std::future::Future<Output = bool>,
    S: FnMut(u64) -> SF,
    SF: std::future::Future<Output = ()>,
{
    for delay in MUSE_CAPTURE_RETRY_MS {
        sleep(*delay).await;
        if !is_alive() {
            return CaptureOutcome::Stopped;
        }
        if capture().await {
            return CaptureOutcome::Captured;
        }
    }
    CaptureOutcome::GaveUp
}

/// Atomically persist `content` to `path` via a PID+counter `.tmp` file and a
/// rename, so a concurrent reader of the trust store sees either the previous
/// file or the new one — never a partial write. A `.tmp` left by an earlier
/// crash is overwritten. Mirrors `mcode::atomic_write` (the Cursor / AGY
/// shape).
fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("trust.json");
    let tmp = path.with_file_name(format!(
        "{}.{}.{}.tmp",
        file_name,
        std::process::id(),
        counter
    ));

    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }

    if let Err(error) = std::fs::rename(&tmp, path) {
        if let Err(cleanup) = std::fs::remove_file(&tmp) {
            tracing::warn!(
                "muse atomic_write: failed to clean up temp file {:?}: {}",
                tmp,
                cleanup
            );
        }
        return Err(error);
    }
    Ok(())
}

/// Human-readable JSON kind tag for malformed-file rejections, mirroring
/// `mcode::settings_kind` — the rejection names the actual shape rather than
/// `serde_json::Value`.
fn settings_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Object(_) => "Object",
        serde_json::Value::Array(_) => "Array",
        serde_json::Value::String(_) => "String",
        serde_json::Value::Number(_) => "Number",
        serde_json::Value::Bool(_) => "Boolean",
        serde_json::Value::Null => "Null",
    }
}

/// The path the spawned Muse process treats as its workspace: the CWD
/// Buildmesh hands the child, which is the value Muse canonicalizes into a
/// `trust.json` key. A WSL guest and a Windows binary reached through interop
/// both run in `spawn_path`; a native launch runs in `host_path` (equal to
/// `spawn_path` on a native host). Mirrors `codex::trust_project_path`.
fn workspace_for(resolved: &ResolvedPath) -> &str {
    match resolved.env_type {
        EnvType::Wsl | EnvType::WindowsInterop => &resolved.spawn_path,
        EnvType::Windows => &resolved.host_path,
    }
}

/// The key Muse files a workspace under in `trust.json`.
///
/// Muse canonicalizes the workspace root, so a native target yields that
/// platform's canonical form — a verbatim `\\?\F:\src\…` path on Windows,
/// the plain absolute path on POSIX. Both shapes are observable in a 1.3.0
/// store on a machine that runs Muse natively and under WSL.
///
/// Canonicalizing is attempted only when **this host** can interpret the path
/// (`is_windows_path` agrees with the host OS). The cross-runtime directions
/// cannot: a Windows host resolving a guest's `/mnt/…` would land on the
/// current drive's `\mnt\…`, and a Linux host cannot resolve `C:\…` at all.
/// Those keys are therefore built verbatim from the runtime's own path,
/// which is what the target binary's own canonicalization produces once the
/// workspace holds no symlinks.
fn trust_key(workspace: &str) -> String {
    let windows_path = crate::env::is_windows_path(workspace);
    if windows_path == cfg!(windows) {
        if let Ok(canonical) = std::fs::canonicalize(workspace) {
            return canonical.to_string_lossy().into_owned();
        }
    }
    if windows_path {
        let normalized = workspace.replace('/', "\\");
        if normalized.starts_with("\\\\?\\") {
            normalized
        } else {
            format!("\\\\?\\{normalized}")
        }
    } else {
        workspace.to_string()
    }
}

/// Resolve the config directory of the Muse runtime that will execute the
/// spawn. A native launch reads the XDG precedence from this process env; a
/// WSL guest or a Windows binary reached through interop gets the **runtime's
/// own** home from `cli_dir_for_spawn` — the same seam
/// `services::muse_sessions::data_root` uses for Muse's data root, and
/// `mcode::resolve_plugin_dir` for its plugin manifest.
///
/// The cross-runtime branches resolve the runtime's *default* XDG root, so a
/// guest `XDG_CONFIG_HOME` (or a non-default WSL distro, #1697) is not
/// honoured — both are recorded as limits in the module docs.
fn resolve_config_dir(resolved: &ResolvedPath) -> Option<PathBuf> {
    let native = muse_config_root_from_vars(
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }),
        std::env::var_os("XDG_CONFIG_HOME"),
    )?;
    crate::env::cli_dir_for_spawn(native, MUSE_CONFIG_DIR, &resolved.spawn_path)
}

/// Add `workspace` to Muse's `trust.json` as `trusted`, preserving every other
/// project entry and every sibling top-level key the user's file carries.
///
/// Muse resolves a workspace's skills, rules and project-scoped plugins
/// through this store; `--trust-workspace` is per-run and is not accepted on
/// the subcommands Buildmesh drives (verified: `muse plugins install` takes no
/// trust flag), so a persisted entry is the only lever that unblocks both the
/// spawned session (#1706) and the project-scoped attention-hook install
/// (#1709).
///
/// Idempotent: an entry that already reads `{"decision":"trusted"}` leaves the
/// file untouched (no spurious mtime bump). A malformed file — bad JSON, a
/// non-object `projects`, or a non-object entry for this workspace — is
/// refused with `Err` rather than overwritten, so the user's content reaches
/// the spawn path as an actionable provisioning failure (the mcode / Cursor
/// precedent). Any *other* fields Muse may carry inside this workspace's entry
/// survive the decision update.
///
/// Any decision Muse already carries that is **not** `trusted` is also an
/// `Err`, and the entry is left exactly as written — as is a `decision` that
/// isn't even a string. Buildmesh only ever adds a decision: `untrusted` is a
/// value someone recorded (see the module docs for why it is a decision rather
/// than a "seen" marker), and overwriting one on every spawn would open a
/// consent gate nobody granted it. Muse's own `--trust-workspace` trusts for a
/// single run only, and does not write.
///
/// A write re-serialises the whole document through serde_json's sorted map,
/// so top-level key order can differ from Muse's own field order (Muse writes
/// `schema_version` first). Values — including every entry Buildmesh does not
/// own — are untouched; only the idempotent no-op path leaves the bytes alone.
fn ensure_trust_file(path: &Path, workspace: &str) -> Result<(), String> {
    let (mut trust, trailing_newline): (serde_json::Value, bool) =
        match std::fs::read_to_string(path) {
            Ok(content) => (
                serde_json::from_str(&content).map_err(|error| {
                    format!(
                        "refusing to overwrite malformed {path:?}: {error}. \
                         Repair or remove the file and retry"
                    )
                })?,
                content.ends_with('\n'),
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (serde_json::json!({}), false)
            }
            Err(error) => return Err(format!("failed to read {path:?}: {error}")),
        };
    if !trust.is_object() {
        return Err(format!(
            "muse trust.json top-level must be a JSON object; got {}",
            settings_kind(&trust)
        ));
    }

    let key = trust_key(workspace);
    let mut changed = false;
    {
        let entry = trust
            .as_object_mut()
            .expect("trust.json verified object above");
        if entry.get("schema_version").is_none() {
            entry.insert(
                "schema_version".to_string(),
                serde_json::json!(MUSE_TRUST_SCHEMA_VERSION),
            );
            changed = true;
        }

        let projects = entry
            .entry("projects")
            .or_insert_with(|| serde_json::json!({}));
        if !projects.is_object() {
            return Err(format!(
                "muse trust.json `projects` must be a JSON object; got {}",
                settings_kind(projects)
            ));
        }
        let projects = projects
            .as_object_mut()
            .expect("projects verified object above");
        match projects.get_mut(&key) {
            Some(serde_json::Value::Object(record)) => match record.get("decision") {
                // Already what we would write — leave the bytes alone.
                Some(serde_json::Value::String(decision)) if decision == MUSE_TRUST_DECISION => {}
                // A decision Muse recorded that Buildmesh did not make. The
                // store's serde surface on 1.3.0 is `ProjectTrustDecision`
                // { trusted, untrusted }, and a workspace Muse has merely seen
                // is **absent** from the map rather than recorded (verified: an
                // untrusted `muse exec` writes no entry at all). So a present
                // non-`trusted` value is a real decision, and reversing it on
                // every spawn would open a consent gate Buildmesh was never
                // granted. A non-string value is the same refusal — this is no
                // more ours to rewrite than a denial is.
                Some(decision) => {
                    return Err(format!(
                        "muse records workspace {key:?} as {decision}, not \
                         {MUSE_TRUST_DECISION:?}; refusing to overwrite an explicit trust \
                         decision. Trust the workspace in Muse (or remove its entry) to let \
                         this node load the workspace's skills and rules"
                    ));
                }
                // No decision recorded yet: fill it in, keeping siblings.
                None => {
                    record.insert(
                        "decision".to_string(),
                        serde_json::json!(MUSE_TRUST_DECISION),
                    );
                    changed = true;
                }
            },
            Some(other) => {
                return Err(format!(
                    "muse trust.json entry for {key:?} must be a JSON object; got {}",
                    settings_kind(other)
                ));
            }
            None => {
                projects.insert(
                    key.clone(),
                    serde_json::json!({ "decision": MUSE_TRUST_DECISION }),
                );
                changed = true;
            }
        }
    }

    if !changed {
        return Ok(());
    }
    let mut content = serde_json::to_string_pretty(&trust)
        .map_err(|error| format!("serialize muse trust.json failed: {error}"))?;
    if trailing_newline {
        content.push('\n');
    }
    atomic_write(path, &content).map_err(|error| format!("failed to write muse trust.json: {error}"))?;
    tracing::info!("muse ensure_workspace_trusted: trusted {:?}", key);
    Ok(())
}

/// Inner provisioner used by [`MuseAdapter::ensure_workspace_trusted`]. Split
/// out so the unresolvable-config-root branch is unit-testable without
/// touching the process env or the real user store (mirrors
/// `mcode::provision_at`).
///
/// Unlike `mcode::provision_at`'s unresolvable case — a silent `Ok(())` — this
/// returns `Err`, because a failed trust write *is* #1706 failing: the node
/// still runs, but without the workspace's skills and rules, so the failure has
/// to be distinguishable from success. `Err` is the only channel that carries
/// it: the spawn path (which never aborts on a provisioning failure) logs a
/// warning and emits a `provider-error` event with the message below.
/// `signal_health` is deliberately **not** touched — it describes whether the
/// node can report its own turn completion, and Muse's passive watcher is
/// unaffected by a trust failure.
fn provision_trust_at(config_dir: Option<&Path>, workspace: &str) -> Result<(), String> {
    let Some(root) = config_dir else {
        return Err(
            "muse config directory is unresolvable; the workspace trust decision cannot be recorded"
                .to_string(),
        );
    };
    let path = root.join("trust.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create muse config dir {parent:?}: {error}"))?;
    }
    ensure_trust_file(&path, workspace)
}

impl AgentProvider for MuseAdapter {
    fn id(&self) -> &'static str {
        "muse"
    }
    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Meta Muse".into(),
            color: "#0866ff".into(),
            icon: "M".into(),
        }
    }
    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        // Issue #1705: bake `--disable-approval`. See the module docstring
        // for the rationale (sibling-harness precedent; `--yolo` rejected
        // as too wide; outer sandbox stays on).
        //
        // Issue #1788: bake `--disable-sandbox` alongside `--disable-approval`.
        // Muse's own OS sandbox stays on with the approval flag alone and
        // blocks the agent shell from the OS credential keyring, breaking
        // gh/git auth that every other harness inherits. See the module
        // docstring "Inner OS sandbox" section.
        //
        // Binary stem branches on `Platform::Windows` to `muse.exe`,
        // mirroring `claude_direct_recipe` at `provider/mod.rs:145-156`.
        // The branch is defensive + convention-following: Windows
        // `CreateProcess` auto-appends `.exe` for PATH searches, so a
        // bare `muse` would also resolve `muse.exe` on PATH (Kimi proves
        // this with its bare `"kimi"` recipe). The explicit branch makes
        // the platform dependency visible at the type level and protects
        // against a future Muse `.cmd` shim (which `CreateProcess` does
        // NOT auto-resolve — that needs a `cmd.exe /c` wrapper like
        // OpenCode uses).
        let binary = match platform {
            Platform::Windows => "muse.exe",
            _ => "muse",
        };
        SpawnRecipe {
            binary,
            base_args: vec!["--disable-approval".into(), "--disable-sandbox".into()],
            trailing_args: vec![],
            windows_shell: WindowsShell::Direct,
        }
    }
    fn supports_resume(&self) -> bool {
        true
    }
    fn auto_resume_on_startup(&self) -> bool {
        true
    }
    fn self_assigns_session_id(&self) -> bool {
        true
    }
    fn captures_session_id_from_pty(&self) -> bool {
        false
    }
    fn requires_attention_hook(&self) -> bool {
        false
    }

    /// Issue #1706 — record the workspace in Muse's persistent trust store
    /// before the process starts, so the workspace's skills, rules and
    /// Buildmesh-authored agent config load. The whole read/merge/write runs
    /// under one process-wide lock: the store is machine-global, and two
    /// spawns starting together from different worktrees would otherwise each
    /// lose the other's entry. See the module docstring for why this is
    /// pre-provisioning rather than a `spawn_recipe` flag.
    ///
    /// `runtime` is unused: Muse's launch routing leaves `LaunchRuntime`
    /// default for a native launch (only a Codex proxy populates
    /// `harness_home`), so the store is resolved from the spawn path by
    /// [`resolve_config_dir`] — the same seam `muse_sessions::data_root` uses.
    fn ensure_workspace_trusted(
        &self,
        resolved: &ResolvedPath,
        _runtime: &LaunchRuntime,
    ) -> Result<(), String> {
        let _guard = TRUST_WRITE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        provision_trust_at(resolve_config_dir(resolved).as_deref(), workspace_for(resolved))
    }

    // Issue #1709: no native hook exists, so Muse's turn signal comes from the
    // backend-owned session-log watcher instead.
    fn supports_passive_turn_watcher(&self) -> bool {
        true
    }
    fn on_spawn_activated(&self, node_id: i64) {
        crate::services::muse_watcher::activate(node_id);
    }
    fn on_process_terminated(&self, node_id: i64) {
        crate::services::muse_watcher::stop(node_id);
    }
    fn produces_readable_transcript(&self) -> bool {
        // Issue #1708: the muse reader
        // (`services::transcript_reader::adapters::muse::MuseAdapter`) is
        // wired, so muse nodes now hydrate the Coordinator Node Digest's
        // rich layer AND surface in the archived-node resume picker
        // (the `resumable = supports_resume && produces_readable_transcript`
        // conjunction in `provider_menu.rs:53`).
        true
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
    // Resume accepts a session UUID but does not document a positional prompt.
    fn prefill_requires_pty(&self, _text: &str) -> bool {
        true
    }
    fn available_on(&self) -> &'static [Platform] {
        // Windows joined the supported set in Muse Code 1.3.0
        // (2026-09 — the binary at `%LOCALAPPDATA%\Programs\muse\muse.exe`
        // is a real PE binary that accepts `--disable-approval` like the
        // Linux/macOS builds). The detection probe at
        // `detection.rs:374-391` finds `muse.exe` on Windows PATH; with
        // Windows in this list, the menu filter at
        // `provider_menu.rs:42` lets the row through. The rank logic at
        // `detection.rs:351-357` keeps the Windows-native profile
        // (rank 0) ahead of the WSL-only one (rank 2) when both
        // installs are present.
        &[Platform::Linux, Platform::Macos, Platform::Windows]
    }
    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["resume".into(), id.into()]
    }
    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec![text.into()]
    }

    fn recover_suspended_session_id(
        &self,
        spawn_path: &str,
        _env_type: EnvType,
        anchor_ms: i64,
        recorded_start: bool,
    ) -> Option<String> {
        let home = crate::services::muse_sessions::data_root(spawn_path)?;
        find_session(
            &home.join("session-index.db"),
            spawn_path,
            anchor_ms,
            recorded_start,
        )
    }

    fn after_fresh_spawn(
        &self,
        node_id: i64,
        spawn_path: &str,
        _env_type: EnvType,
        app: &tauri::AppHandle,
    ) {
        let spawn_path = spawn_path.to_string();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let outcome = run_capture_window(
                || {
                    // Capture is the watcher's arm point: the same session
                    // index that supplies the id also resolves the log path.
                    let spawn_path = spawn_path.clone();
                    let app = app.clone();
                    async move {
                        let result =
                            crate::blocking::run_blocking("muse_session_capture", move || {
                                crate::services::session_recovery::recover_live_node(node_id)
                            })
                            .await;
                        let Ok(Some(session_id)) = result else {
                            return false;
                        };
                        let started =
                            crate::blocking::run_blocking("muse watcher start", move || {
                                crate::services::muse_watcher::start_for_session(
                                    node_id,
                                    &session_id,
                                    &spawn_path,
                                    &app,
                                )
                            })
                            .await;
                        match started {
                            Ok(()) => true,
                            // The id is durable once captured, so a transient
                            // log-path failure (e.g. a not-yet-visible WSL
                            // file) retries on the next tick instead of
                            // stranding the watcher.
                            Err(error) => {
                                tracing::warn!(
                                    "muse watcher: could not start for node {node_id}: {error}"
                                );
                                false
                            }
                        }
                    }
                },
                |ms| tokio::time::sleep(std::time::Duration::from_millis(ms)),
                || crate::agent::process::PROCESS_REGISTRY.contains(&node_id),
            )
            .await;
            // Giving up is not silent: the node stays without a session
            // identity, so the circuit watchdog (issue #1791) fails any wait on
            // it at the first-observation window instead of burning the full
            // active budget. Say so once, with the window that elapsed.
            if outcome == CaptureOutcome::GaveUp {
                let window_s = MUSE_CAPTURE_RETRY_MS.iter().sum::<u64>() / 1_000;
                tracing::warn!(
                    "muse session capture: node {node_id} produced no session identity within \
                     {window_s}s; circuit waits on it will fail fast as unobserved (#1791)"
                );
            }
        });
    }

    fn before_resume_spawn<'a>(
        &'a self,
        node_id: i64,
        session_id: &str,
        spawn_path: &str,
        _env_type: EnvType,
        app: &'a tauri::AppHandle,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        let session_id = session_id.to_string();
        let spawn_path = spawn_path.to_string();
        let app = app.clone();
        Box::pin(async move {
            if let Err(error) = crate::services::muse_watcher::start_for_resumed_session_async(
                node_id,
                &session_id,
                &spawn_path,
                app,
            )
            .await
            {
                tracing::warn!("muse watcher: could not resume watch for node {node_id}: {error}");
            }
        })
    }
}

/// Resolve a node's Muse session identity from `session-index.db` **and** the
/// on-disk session tree, then let the launch anchor pick between them. The
/// index alone is insufficient: a live session has no index row until a later
/// Muse process flushes it (run 183 — the node stayed unobserved, so its
/// circuit wait failed fast). See [`crate::services::muse_sessions`].
fn find_session(
    database: &std::path::Path,
    workspace: &str,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let root = database.parent()?;
    let candidates =
        crate::services::muse_sessions::workspace_candidates(root, workspace, anchor_ms);
    crate::services::session_recovery::select_recovery_identity(
        candidates,
        anchor_ms,
        recorded_start,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Pick the canonical `EnvType` that pairs with each `Platform` the
    /// muse adapter advertises on. Keeps the per-platform pin test readable
    /// as "this is the host that runs the binary" rather than a sloppy
    /// single `EnvType::Wsl` everywhere (issue #1705 round-1 review —
    /// `Platform::Macos, EnvType::Wsl` is a platform-impossible pairing).
    /// Mirrors the pairing OpenCode's `spawn_recipe_direct_on_macos` test
    /// uses: macOS has no WSL runtime, and `EnvType` has no `Macos`
    /// variant, so the canonical macOS pairing is `(Macos, Windows)`.
    fn env_type_for(platform: Platform) -> EnvType {
        match platform {
            // Linux runtime covers native Linux + WSL-on-Windows (the
            // Ubuntu distro at `/home/alond/.local/bin/muse`). The muse
            // adapter's `spawn_recipe` is platform-agnostic, but the
            // pairing keeps the test accurate as "the host that runs the
            // binary" rather than a meaningless one-size-fits-all value.
            Platform::Linux => EnvType::Wsl,
            // macOS has no WSL runtime and no `EnvType::Macos` variant; the
            // canonical macOS pairing is `(Macos, Windows)`, matching
            // OpenCode's `spawn_recipe_direct_on_macos` test pattern.
            Platform::Macos => EnvType::Windows,
            Platform::Windows => EnvType::Windows,
        }
    }

    #[test]
    fn muse_uses_documented_interactive_arguments() {
        assert_eq!(
            MUSE.spawn_recipe(Platform::Linux, EnvType::Wsl).binary,
            "muse"
        );
        assert_eq!(MUSE.resume_args("session-uuid"), ["resume", "session-uuid"]);
        assert_eq!(MUSE.prefill_args("fix the bug"), ["fix the bug"]);
        assert_eq!(MUSE.model_args("model-id"), ["--model", "model-id"]);
        assert!(!MUSE.captures_session_id_from_pty());
        assert!(MUSE.prefill_requires_pty("follow-up"));
        // The baked `--disable-approval` + `--disable-sandbox` policy
        // (issues #1705 + #1788) is pinned
        // exhaustively by `spawn_recipe_carries_disable_approval_on_supported_platforms`
        // below — that test iterates every supported host and adds the
        // `--yolo` negative assertion. Keeping the assertion only there
        // keeps the named domain of this test (per-adapter `*_args` shape)
        // focused.
    }

    /// Issue #1705 + #1788 — per-platform pin of the baked policy flags.
    /// Mirrors OpenCode's `spawn_recipe_carries_auto_flag_on_every_platform`:
    /// iterate over `available_on()` (not every `Platform` variant) and
    /// assert the exact base_args vector + per-platform binary name so a
    /// future flag smuggle (e.g. `--approval-mode never` or `--yolo`
    /// slipping in alongside `--disable-approval` + `--disable-sandbox`)
    /// trips here, not at runtime.
    ///
    /// Each platform variant is paired with the canonical `EnvType` for
    /// that host (see [`env_type_for`]) so the test reads as "the host
    /// that actually runs the binary" — `Platform::Macos, EnvType::Wsl`
    /// would be a platform-impossible pairing.
    ///
    /// Binary-name shape: Windows uses `muse.exe` (matches Anthropic's
    /// `claude.exe` branch at `provider/mod.rs:145-156`); macOS/Linux
    /// keep the bare stem. The Windows branch is defensive + convention
    /// (`CreateProcess` auto-appends `.exe` for PATH searches), but
    /// mirrors Anthropic exactly so a future Muse `.cmd` shim still
    /// resolves correctly.
    #[test]
    fn spawn_recipe_carries_disable_approval_on_supported_platforms() {
        for platform in MUSE.available_on() {
            let recipe = MUSE.spawn_recipe(*platform, env_type_for(*platform));
            let expected_binary = match *platform {
                Platform::Windows => "muse.exe",
                _ => "muse",
            };
            assert_eq!(
                recipe.binary, expected_binary,
                "muse binary name must be exact on {platform:?}: \
                 Windows → muse.exe (mirrors claude_direct_recipe's \
                 claude.exe branch), others → muse"
            );
            assert_eq!(
                recipe.base_args,
                vec!["--disable-approval".to_string(), "--disable-sandbox".to_string()],
                "muse base recipe must be exactly \n                `[\"--disable-approval\", \"--disable-sandbox\"]` \
                 on {platform:?} (approval policy #1705; sandbox off so the \
                 agent shell reaches the OS credential keyring, #1788); got {:?}",
                recipe.base_args
            );
            assert!(
                matches!(recipe.windows_shell, WindowsShell::Direct),
                "muse is a real PE binary on Windows / ELF on Linux / Mach-O \
                 on macOS — must use WindowsShell::Direct on {platform:?}; got {:?}",
                recipe.windows_shell
            );
            // `--yolo` is the explicit no-go for issue #1705: it disables
            // approval AND sandboxing AND trusts the workspace. A future
            // "while we're here" edit that adds it would silently widen the
            // policy beyond the maintainer-approved scope. `--disable-sandbox`
            // (#1788) is the narrow, adapter-owned replacement for the
            // sandboxing half; workspace trust stays untouched.
            assert!(
                !recipe.base_args.iter().any(|a| a == "--yolo"),
                "muse base recipe must never bake --yolo (issue #1705): \
                 it disables approval + sandboxing + workspace trust in one \
                 flag and was explicitly rejected; got {:?}",
                recipe.base_args
            );
        }
    }

    /// Pin the exact `available_on()` set. Pre-fix this failed with
    /// `len() == 2` because `Platform::Windows` was absent (Muse
    /// originally shipped Linux + macOS only — the Windows binary landed
    /// in 1.3.0 this week). Now that Windows is supported, the assertion
    /// forces any future "while we're here" addition (or removal) to
    /// surface in review, not at runtime as a missing menu row. Mirrors
    /// `kimi::available_on_all_three_platforms` at kimi.rs:425-437.
    #[test]
    fn available_on_all_three_platforms() {
        let platforms = MUSE.available_on();
        assert_eq!(
            platforms.len(),
            3,
            "available_on should pin to exactly {{Windows, Linux, Macos}} — got {:?}",
            platforms
        );
        assert!(platforms.contains(&Platform::Windows), "muse is available on Windows since 1.3.0; got {:?}", platforms);
        assert!(platforms.contains(&Platform::Linux));
        assert!(platforms.contains(&Platform::Macos));
    }

    // -- Prepared-launch evidence (issue #1705 round-1 review) ------------
    //
    // The per-platform pin above proves `spawn_recipe()` itself returns the
    // baked policy; it does not prove the policy survives `default_prepare`
    // composition. The two tests below route fresh + resume launches through
    // the real orchestration seam (`agent::launch::default_prepare`) so the
    // baked flag is proven to land in the final argv alongside the model
    // override, the prefill text, and the resume id, in the documented order.
    // Without these, a future refactor that reorders the layers (e.g.
    // prepending `--model` before `--disable-approval`) would slip past the
    // per-platform pin but break a real spawn.
    //
    // Mirrors OpenCode's `fresh_recipe_forwards_model_and_prompt_without_session_id`
    // and `resume_recipe_carries_session_flag` — the engineering contract
    // (`docs/agents/engineering.md`) requires testing fresh AND resume paths
    // for changed launch recipes.

    /// Issue #1705 + #1788 fresh launch: the baked `--disable-approval` +
    /// `--disable-sandbox` must land ahead of the model override and the
    /// prefill text in the final argv. Pin the exact `base_args` vector so a
    /// future reorder that pushes `--disable-approval` past `--model` (or
    /// drops either flag during layer composition) trips here, not in
    /// production.
    #[test]
    fn default_prepare_fresh_launch_carries_disable_approval_with_model_and_prefill() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig {
            model: Some("claude-sonnet-4-5".to_string()),
            effort: None,
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Wsl,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: Some("fix the auth bug"),
            sandbox: false,
        };
        let prepared = default_prepare(&MUSE, input);
        // Order is `base_recipe -> model -> prefill`. muse's `prefill_args`
        // returns a positional element (no `--prefill` flag — see
        // `muse_uses_documented_interactive_arguments`), so prefill lands
        // as a bare trailing argv element after the model flag+value.
        assert_eq!(
            prepared.recipe.base_args,
            vec![
                "--disable-approval".to_string(),
                "--disable-sandbox".to_string(),
                "--model".to_string(),
                "claude-sonnet-4-5".to_string(),
                "fix the auth bug".to_string(),
            ],
            "fresh launch argv must keep --disable-approval + --disable-sandbox \
             ahead of --model and the prefill text; got {:?}",
            prepared.recipe.base_args
        );
        // Negative guards: no session-assign flag (muse self-assigns), no
        // `--prefill` flag (the prefill shape is positional), no
        // approval-policy smuggle (`--yolo` was explicitly rejected).
        assert!(
            !prepared.recipe.base_args.iter().any(|a| a == "--session"
                || a == "--session-id"
                || a == "--prefill"
                || a == "--yolo"),
            "fresh launch must not emit session-assign / --prefill / --yolo; \
             got {:?}",
            prepared.recipe.base_args
        );
    }

    /// Issue #1705 + #1788 resume launch: the baked `--disable-approval` +
    /// `--disable-sandbox` must land ahead of the resume subcommand + session
    /// id, matching the order the OpenCode adapter uses for
    /// `--auto --session <id>`. Pin the exact vector so a future edit that
    /// orders the resume subcommand before the baked flags (or that drops
    /// either flag during composition) trips here.
    #[test]
    fn default_prepare_resume_launch_carries_disable_approval_then_resume_uuid() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig::default();
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Wsl,
            session: SessionIdModeRef::Resume("12345678-1234-4234-8234-123456789abc"),
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&MUSE, input);
        assert_eq!(
            prepared.recipe.base_args,
            vec![
                "--disable-approval".to_string(),
                "--disable-sandbox".to_string(),
                "resume".to_string(),
                "12345678-1234-4234-8234-123456789abc".to_string(),
            ],
            "resume launch argv must be exactly \
             `--disable-approval --disable-sandbox resume <uuid>`; got {:?}",
            prepared.recipe.base_args
        );
        assert!(
            !prepared.recipe.base_args.iter().any(|a| a == "--yolo"),
            "resume launch must never bake --yolo (issues #1705 + #1788); got {:?}",
            prepared.recipe.base_args
        );
    }

    #[test]
    fn muse_session_index_matches_workspace_and_refuses_ambiguous_identity() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("session-index.db");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch("CREATE TABLE sessions (session_id TEXT, session_log_path TEXT);")
            .unwrap();
        let insert = |id: &str, workspace: &str, timestamp: i64, wrapped: bool| {
            let log = directory.path().join(format!("{id}.jsonl"));
            let record = serde_json::json!({"payload_type": "runtime.session.metadata", "stream": {"kind":"session", "id":id}, "recorded_at":timestamp, "payload":{"record":{"workspace_root":workspace}}});
            let frame = if wrapped {
                serde_json::json!({"retained_frame":"session_permission_transaction","children":[{"record_json":record.to_string()}]})
            } else {
                record
            };
            std::fs::write(&log, format!("{{}}\n{frame}\n")).unwrap();
            connection
                .execute(
                    "INSERT INTO sessions VALUES (?1, ?2)",
                    rusqlite::params![id, log.to_str().unwrap()],
                )
                .unwrap();
        };
        insert(
            "12345678-1234-4234-8234-123456789abc",
            "/workspace",
            10000000,
            false,
        );
        insert(
            "22345678-1234-4234-8234-123456789abc",
            "/other",
            10000000,
            true,
        );
        assert_eq!(
            find_session(&database, "/workspace", 10000, true).as_deref(),
            Some("12345678-1234-4234-8234-123456789abc")
        );
        assert_eq!(find_session(&database, "/workspace", 20000, true), None);
        insert(
            "32345678-1234-4234-8234-123456789abc",
            "/workspace",
            11000000,
            true,
        );
        assert_eq!(find_session(&database, "/workspace", 10000, true), None);
    }

    /// Run 183: a live Muse session has no `session-index.db` row, so capture
    /// must still find the identity from the on-disk session log. Without the
    /// fallback the node stayed unobserved and #1791 failed its circuit wait.
    #[test]
    fn find_session_recovers_a_live_session_absent_from_the_index() {
        let root = tempfile::tempdir().unwrap();
        let id = "01a0c54b-5ed4-7a61-91d7-a7a72c42fe24";
        let workspace = "F:\\src\\buildmesh\\.claude\\worktrees\\gh1816";
        let dir = root
            .path()
            .join("sessions/1970/01/01")
            .join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let record = serde_json::json!({
            "payload_type": "runtime.session.metadata",
            "stream": {"kind": "session", "id": id},
            "recorded_at": 10_000_000i64,
            "payload": {"record": {"workspace_root": workspace}},
        });
        // The metadata frame follows a retained-frame envelope, as on disk.
        let envelope = serde_json::json!({
            "retained_frame": "session_permission_transaction",
            "children": [{"record_json": record.to_string()}],
        });
        std::fs::write(dir.join("session.jsonl"), format!("{envelope}\n{record}\n")).unwrap();

        // No `session-index.db` at all.
        let database = root.path().join("session-index.db");
        assert_eq!(
            find_session(&database, workspace, 10_000, true).as_deref(),
            Some(id),
            "the on-disk session log must supply the identity the index omitted"
        );
    }

    // -- Issue #1794: extended session-index capture window --------------

    /// The window must outlast a slow first boot (~60 s) while staying bounded,
    /// so a genuinely failed capture gives up and leaves the node unobserved
    /// rather than retrying forever.
    #[test]
    fn capture_window_extends_past_the_original_sixteen_second_budget() {
        let total: u64 = MUSE_CAPTURE_RETRY_MS.iter().sum();
        assert!(
            total > 60_000,
            "the window must still be polling after ~60 s (the slow-boot case); got {total} ms"
        );
        assert!(
            total <= 5 * 60_000,
            "the window must stay bounded so a failed capture surfaces as unobserved; got {total} ms"
        );
        assert!(
            MUSE_CAPTURE_RETRY_MS.len() > 6,
            "the window must extend the original six-attempt schedule"
        );
    }

    /// A fake index that only becomes reachable after ~60 s still produces a
    /// session identity, because the retry window now spans it. The clock is
    /// virtual so the test does not actually wait.
    #[tokio::test]
    async fn capture_window_reaches_a_session_index_that_appears_after_sixty_seconds() {
        use std::cell::Cell;
        let elapsed = Cell::new(0u64);
        let outcome = run_capture_window(
            || {
                let reachable = elapsed.get() >= 60_000;
                async move { reachable }
            },
            |ms| {
                elapsed.set(elapsed.get() + ms);
                async move {}
            },
            || true,
        )
        .await;
        assert_eq!(outcome, CaptureOutcome::Captured);
        assert!(
            elapsed.get() >= 60_000,
            "capture must have reached the ~60 s index, only polled {} ms",
            elapsed.get()
        );
    }

    /// Exhausting the window is an explicit give-up, not an infinite retry —
    /// that is what lets the #1791 fast fail surface instead of a silent stall.
    #[tokio::test]
    async fn exhausted_capture_window_gives_up() {
        let outcome = run_capture_window(|| async { false }, |_| async {}, || true).await;
        assert_eq!(outcome, CaptureOutcome::GaveUp);
    }

    /// A node whose process exited stops the window early; it must not keep
    /// polling a dead session.
    #[tokio::test]
    async fn capture_window_stops_when_the_process_exits() {
        let outcome = run_capture_window(|| async { false }, |_| async {}, || false).await;
        assert_eq!(outcome, CaptureOutcome::Stopped);
    }

    // -----------------------------------------------------------------
    // Issue #1706 — Muse workspace trust (`~/.config/muse/trust.json`).
    //
    // Contract pinned here:
    //   1. `trust_key` is a canonical absolute path of the right shape — a
    //      verbatim `\\?\` path on Windows, a plain absolute path on POSIX —
    //      and never host-resolves a cross-runtime path.
    //   2. The store is Muse's config root, including the `XDG_CONFIG_HOME`
    //      override.
    //   3. A fresh store gets `schema_version` 1 and one trusted entry.
    //   4. Additive merge preserves sibling projects and unknown keys.
    //   5. Idempotent re-provision rewrites nothing.
    //   6. Malformed files, unexpected shapes, and an explicit non-`trusted`
    //      decision are refused, not clobbered.
    //   7. An unresolvable config root is an error, not a silent pass.
    //   8. Atomic write leaves no `.tmp` residue.
    //
    // The byte-exact check that `trust_key` reproduces Muse's *own* stored keys
    // cannot be hermetic; it lives in the ignored
    // `live_trust_key_reproduces_the_installed_stores_keys` below.
    // -----------------------------------------------------------------

    fn trust_path(home: &Path) -> PathBuf {
        home.join("trust.json")
    }

    fn read_trust(home: &Path) -> serde_json::Value {
        let body = std::fs::read_to_string(trust_path(home)).expect("trust.json must exist");
        serde_json::from_str(&body).expect("trust.json must be valid JSON")
    }

    fn decision_for(value: &serde_json::Value, key: &str) -> Option<String> {
        value["projects"][key]["decision"].as_str().map(str::to_string)
    }

    /// A workspace that exists on disk, so `trust_key` canonicalizes it the
    /// way Muse does rather than taking the verbatim fallback.
    fn existing_workspace() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().to_string();
        (dir, path)
    }

    /// The key's *shape*, not a re-run of the implementation: re-canonicalizing
    /// in the assertion would only restate `trust_key`'s own body.
    #[test]
    fn trust_key_is_the_canonical_path_of_the_workspace() {
        let (_dir, workspace) = existing_workspace();
        let key = trust_key(&workspace);
        let name = Path::new(&workspace)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        assert!(
            key.ends_with(&name),
            "the canonical key keeps the workspace's own directory name: {key}"
        );
        if cfg!(windows) {
            assert!(
                key.starts_with("\\\\?\\"),
                "a native Windows store keys a verbatim `\\\\?\\` path (observed on 1.3.0): {key}"
            );
            assert!(
                !key.contains('/'),
                "a Windows key uses backslashes: {key}"
            );
        } else {
            assert!(
                key.starts_with('/'),
                "a POSIX key is an absolute path: {key}"
            );
        }
    }

    /// A guest `/mnt/…` path must be keyed verbatim: a Windows host resolving
    /// it would land on its own current drive's `\mnt\…` and trust the wrong
    /// directory.
    #[test]
    fn trust_key_never_host_resolves_a_cross_runtime_path() {
        assert_eq!(
            trust_key("/mnt/f/src/buildmesh/.claude/worktrees/gh1706"),
            "/mnt/f/src/buildmesh/.claude/worktrees/gh1706"
        );
    }

    /// A Windows runtime path keyed from a non-Windows host (interop) gets the
    /// verbatim form Muse stores; separators are normalized and an
    /// already-verbatim path is left alone.
    #[test]
    fn trust_key_builds_a_verbatim_windows_path() {
        assert_eq!(trust_key("Q:/buildmesh-trust-probe/nope"), "\\\\?\\Q:\\buildmesh-trust-probe\\nope");
        assert_eq!(trust_key("Q:\\buildmesh-trust-probe\\nope"), "\\\\?\\Q:\\buildmesh-trust-probe\\nope");
        assert_eq!(
            trust_key("\\\\?\\Q:\\buildmesh-trust-probe\\nope"),
            "\\\\?\\Q:\\buildmesh-trust-probe\\nope",
            "an already-verbatim key must not gain a second prefix"
        );
    }

    /// The harness runs in `spawn_path`; `host_path` is the host-readable form
    /// of the same directory on a cross-runtime spawn. Keying the UNC form for
    /// a WSL node would record a path the guest Muse never sees.
    #[test]
    fn workspace_for_uses_the_path_the_harness_runs_in() {
        // The host-readable form of a guest worktree, as `ResolvedPath` spells
        // it on a Windows host: the guest binary never sees this one.
        let host_form = "\\\\wsl$\\Ubuntu\\home\\u\\proj"; // allow-wsl-path
        let resolved = |env_type| ResolvedPath {
            host_path: host_form.into(),
            spawn_path: "/home/u/proj".into(),
            raw_path: "/home/u/proj".into(),
            env_type,
        };
        assert_eq!(workspace_for(&resolved(EnvType::Wsl)), "/home/u/proj");
        assert_eq!(
            workspace_for(&resolved(EnvType::WindowsInterop)),
            "/home/u/proj"
        );
        assert_eq!(workspace_for(&resolved(EnvType::Windows)), host_form);
    }

    /// Muse reads `$XDG_CONFIG_HOME/muse` when that variable holds an absolute
    /// path, and `<home>/.config/muse` otherwise. Writing to the default while a
    /// user has the override set would leave the store unread — the exact
    /// silent failure #1706 exists to remove. A *relative* value is ignored
    /// (the XDG spec, and Muse's own behaviour).
    #[test]
    fn muse_config_root_honours_the_xdg_override() {
        // Absolute for whichever host runs the test: `is_absolute` is
        // platform-specific, and a POSIX-style `/opt/config` is *not* absolute
        // on Windows.
        let (home, xdg) = if cfg!(windows) {
            ("C:\\Users\\u", "D:\\xdg")
        } else {
            ("/home/u", "/opt/config")
        };
        let user_home = Some(std::ffi::OsString::from(home));
        let default_root = PathBuf::from(home).join(MUSE_CONFIG_DIR);

        assert_eq!(
            muse_config_root_from_vars(user_home.clone(), Some(xdg.into())),
            Some(PathBuf::from(xdg).join("muse"))
        );
        assert_eq!(
            muse_config_root_from_vars(user_home.clone(), None),
            Some(default_root.clone())
        );
        // Empty and relative values are both "not set", never the cwd.
        for unset in ["", "relative/config", "../up"] {
            assert_eq!(
                muse_config_root_from_vars(user_home.clone(), Some(unset.into())),
                Some(default_root.clone()),
                "XDG_CONFIG_HOME={unset:?} must be ignored"
            );
        }
        assert_eq!(muse_config_root_from_vars(None, None), None);
    }

    /// End-to-end on the store-path seam: a native spawn resolves Muse's config
    /// root, not some adjacent directory. The cross-runtime branches are
    /// `cli_dir_for_spawn` (shared with `muse_sessions::data_root`).
    #[test]
    fn resolve_config_dir_targets_muse_config_root_for_a_native_spawn() {
        let workspace = if cfg!(windows) {
            "C:\\work\\proj"
        } else {
            "/work/proj"
        };
        let resolved = ResolvedPath {
            host_path: workspace.into(),
            spawn_path: workspace.into(),
            raw_path: workspace.into(),
            env_type: EnvType::Windows,
        };
        let root = resolve_config_dir(&resolved)
            .expect("any real session has a home directory to resolve against");
        assert!(
            root.ends_with("muse"),
            "the native store is Muse's config root: {root:?}"
        );
    }

    #[test]
    fn provision_writes_a_fresh_store_with_one_trusted_entry() {
        let home = tempfile::tempdir().unwrap();
        let (_dir, workspace) = existing_workspace();

        provision_trust_at(Some(home.path()), &workspace).expect("fresh provision must succeed");

        let value = read_trust(home.path());
        assert_eq!(
            value["schema_version"].as_u64(),
            Some(MUSE_TRUST_SCHEMA_VERSION)
        );
        assert_eq!(
            decision_for(&value, &trust_key(&workspace)).as_deref(),
            Some("trusted")
        );
        assert_eq!(
            value["projects"].as_object().unwrap().len(),
            1,
            "a fresh store carries exactly the one workspace: {value}"
        );
    }

    /// The store is shared across every Muse node on the machine — a re-run
    /// must merge, never replace. A user's unrelated project and their own
    /// top-level keys round-trip untouched.
    #[test]
    fn provision_preserves_sibling_projects_and_unknown_keys() {
        let home = tempfile::tempdir().unwrap();
        let (_dir, workspace) = existing_workspace();
        std::fs::write(
            trust_path(home.path()),
            r#"{
                "schema_version": 1,
                "unrelated": { "keep": true },
                "projects": {
                    "\\\\?\\F:\\src\\other": { "decision": "trusted" },
                    "\\\\?\\F:\\src\\denied": { "decision": "denied", "note": "user" }
                }
            }"#,
        )
        .unwrap();

        provision_trust_at(Some(home.path()), &workspace).unwrap();

        let value = read_trust(home.path());
        assert_eq!(
            value["unrelated"]["keep"],
            serde_json::json!(true),
            "unknown top-level keys must round-trip: {value}"
        );
        assert_eq!(
            decision_for(&value, &trust_key(&workspace)).as_deref(),
            Some("trusted")
        );
        assert_eq!(
            value["projects"]["\\\\?\\F:\\src\\other"]["decision"],
            serde_json::json!("trusted"),
            "a sibling project entry must be untouched: {value}"
        );
        assert_eq!(
            value["projects"]["\\\\?\\F:\\src\\denied"]["decision"],
            serde_json::json!("denied"),
            "a sibling's own decision must not be rewritten: {value}"
        );
        assert_eq!(value["projects"]["\\\\?\\F:\\src\\denied"]["note"], "user");
    }

    /// Distinct worktrees accumulate — the store is a map, not last-writer-wins.
    #[test]
    fn provision_accumulates_entries_for_distinct_workspaces() {
        let home = tempfile::tempdir().unwrap();
        let (_first, first) = existing_workspace();
        let (_second, second) = existing_workspace();

        provision_trust_at(Some(home.path()), &first).unwrap();
        provision_trust_at(Some(home.path()), &second).unwrap();

        let value = read_trust(home.path());
        assert_eq!(
            decision_for(&value, &trust_key(&first)).as_deref(),
            Some("trusted")
        );
        assert_eq!(
            decision_for(&value, &trust_key(&second)).as_deref(),
            Some("trusted")
        );
        assert_eq!(value["projects"].as_object().unwrap().len(), 2);
    }

    /// Issue #886 idempotency invariant: re-provisioning an already-trusted
    /// workspace writes nothing. The store is seeded by hand in a *compact*
    /// form that already carries the trusted decision, so any rewrite would
    /// re-serialise it and change the bytes — the byte comparison is the
    /// proof, and unlike a read-only-file check it holds on every platform
    /// (POSIX `rename(2)` replaces a read-only target happily).
    #[test]
    fn provision_is_idempotent_and_leaves_an_already_trusted_file_untouched() {
        let home = tempfile::tempdir().unwrap();
        let (_dir, workspace) = existing_workspace();
        let key = serde_json::to_string(&trust_key(&workspace)).unwrap();
        let seeded =
            format!("{{\"schema_version\":1,\"projects\":{{{key}:{{\"decision\":\"trusted\"}}}}}}");
        std::fs::write(trust_path(home.path()), &seeded).unwrap();

        provision_trust_at(Some(home.path()), &workspace)
            .expect("an already-trusted workspace is a no-op");

        assert_eq!(
            std::fs::read_to_string(trust_path(home.path())).unwrap(),
            seeded,
            "a no-op pass must not rewrite the store (a write would re-serialise it)"
        );
    }

    /// Muse's own store ends with a newline; a rewrite must not silently strip
    /// it (the `agent::workspace_trust` precedent for `settings.json`).
    #[test]
    fn provision_preserves_a_trailing_newline() {
        let home = tempfile::tempdir().unwrap();
        let (_dir, workspace) = existing_workspace();
        std::fs::write(
            trust_path(home.path()),
            "{\"schema_version\":1,\"projects\":{}}\n",
        )
        .unwrap();

        provision_trust_at(Some(home.path()), &workspace).unwrap();

        let body = std::fs::read_to_string(trust_path(home.path())).unwrap();
        assert!(
            body.ends_with('\n'),
            "the store's trailing newline must survive a rewrite: {body:?}"
        );
        assert_eq!(
            decision_for(&read_trust(home.path()), &trust_key(&workspace)).as_deref(),
            Some("trusted")
        );
    }

    /// A decision Muse recorded is not Buildmesh's to reverse. `untrusted` is
    /// the only other value `ProjectTrustDecision` has, and Muse does not write
    /// it for a workspace it has merely seen (such a workspace is absent), so
    /// this row is a real decision — flipping it on every spawn would open a
    /// consent gate Buildmesh was never granted.
    #[test]
    fn provision_refuses_to_override_an_untrusted_decision() {
        let home = tempfile::tempdir().unwrap();
        let (_dir, workspace) = existing_workspace();
        let key = serde_json::to_string(&trust_key(&workspace)).unwrap();
        let seeded = format!(
            r#"{{ "projects": {{ {key}: {{ "decision": "untrusted", "note": "keep" }} }} }}"#
        );
        std::fs::write(trust_path(home.path()), &seeded).unwrap();

        let result = provision_trust_at(Some(home.path()), &workspace);

        let message = result.expect_err("a recorded untrusted decision must not be overwritten");
        assert!(
            message.contains("untrusted"),
            "the failure must name the decision it found: {message}"
        );
        assert_eq!(
            std::fs::read_to_string(trust_path(home.path())).unwrap(),
            seeded,
            "the entry must be left byte-identical"
        );
    }

    /// A `decision` that isn't even a string is the same class of "shape we do
    /// not own" as a malformed file — refused, not quietly rewritten.
    #[test]
    fn provision_refuses_a_non_string_decision() {
        let (_dir, workspace) = existing_workspace();
        let key = serde_json::to_string(&trust_key(&workspace)).unwrap();
        for decision in ["123", "null", "true", "{}"] {
            let home = tempfile::tempdir().unwrap();
            let seeded =
                format!(r#"{{ "projects": {{ {key}: {{ "decision": {decision} }} }} }}"#);
            std::fs::write(trust_path(home.path()), &seeded).unwrap();

            let result = provision_trust_at(Some(home.path()), &workspace);
            assert!(
                result.is_err(),
                "decision {decision} must be refused; got {result:?}"
            );
            assert_eq!(
                std::fs::read_to_string(trust_path(home.path())).unwrap(),
                seeded,
                "decision {decision} must not be rewritten"
            );
        }
    }

    /// An entry with no `decision` at all is not a decision Muse recorded —
    /// Buildmesh fills it in, keeping the entry's sibling fields.
    #[test]
    fn provision_fills_in_a_missing_decision() {
        let home = tempfile::tempdir().unwrap();
        let (_dir, workspace) = existing_workspace();
        let key = serde_json::to_string(&trust_key(&workspace)).unwrap();
        std::fs::write(
            trust_path(home.path()),
            format!(r#"{{ "projects": {{ {key}: {{ "note": "keep" }} }} }}"#),
        )
        .unwrap();

        provision_trust_at(Some(home.path()), &workspace).unwrap();

        let value = read_trust(home.path());
        assert_eq!(
            decision_for(&value, &trust_key(&workspace)).as_deref(),
            Some("trusted")
        );
        assert_eq!(
            value["projects"][trust_key(&workspace)]["note"],
            serde_json::json!("keep"),
            "fields inside the entry must survive: {value}"
        );
    }

    /// A broken user file must reach the spawn path as an actionable failure,
    /// not be silently replaced (the mcode / Cursor precedent).
    #[test]
    fn provision_refuses_to_overwrite_a_malformed_file() {
        let home = tempfile::tempdir().unwrap();
        let (_dir, workspace) = existing_workspace();
        let malformed = r#"{ "projects": {},, }"#;
        std::fs::write(trust_path(home.path()), malformed).unwrap();

        let result = provision_trust_at(Some(home.path()), &workspace);
        assert!(
            result.is_err(),
            "a malformed store must be refused; got {result:?}"
        );
        assert_eq!(
            std::fs::read_to_string(trust_path(home.path())).unwrap(),
            malformed,
            "malformed content must NOT be overwritten"
        );
    }

    /// Valid JSON of the wrong shape is a misconfiguration, not an empty file:
    /// refuse rather than clobber it with `{"projects": {…}}`.
    #[test]
    fn provision_refuses_unexpected_shapes() {
        let (_dir, workspace) = existing_workspace();
        let key = serde_json::to_string(&trust_key(&workspace)).unwrap();
        for body in [
            "[1, 2, 3]".to_string(),
            r#"{ "projects": [] }"#.to_string(),
            format!(r#"{{ "projects": {{ {key}: "denied" }} }}"#),
        ] {
            let home = tempfile::tempdir().unwrap();
            std::fs::write(trust_path(home.path()), &body).unwrap();

            let result = provision_trust_at(Some(home.path()), &workspace);
            assert!(
                result.is_err(),
                "shape {body} must be refused; got {result:?}"
            );
            assert_eq!(
                std::fs::read_to_string(trust_path(home.path())).unwrap(),
                body,
                "content must NOT be overwritten"
            );
        }
    }

    /// Trust is the whole point of #1706 — an unresolvable config root must
    /// surface, not be swallowed.
    #[test]
    fn provision_returns_err_when_the_config_root_is_unresolvable() {
        let result = provision_trust_at(None, "/workspace");
        assert!(
            result.is_err(),
            "an unresolvable config root must surface: {result:?}"
        );
        assert!(result.unwrap_err().contains("config directory"));
    }

    #[test]
    fn provision_atomic_write_leaves_no_tmp_residue() {
        let home = tempfile::tempdir().unwrap();
        let (_dir, workspace) = existing_workspace();
        provision_trust_at(Some(home.path()), &workspace).unwrap();

        let residue: Vec<_> = std::fs::read_dir(home.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            residue.is_empty(),
            "atomic write must not leave .tmp residue; found {residue:?}"
        );
    }

    /// Pin the store's identity so a refactor that moves it trips here before
    /// it silently trusts nothing.
    #[test]
    fn muse_trust_constants_are_pinned() {
        assert_eq!(MUSE_CONFIG_DIR, ".config/muse");
        assert_eq!(MUSE_TRUST_SCHEMA_VERSION, 1);
        assert_eq!(MUSE_TRUST_DECISION, "trusted");
    }

    /// Live evidence for the key derivation, which no hermetic test can prove:
    /// on a machine whose store Muse populated itself (worktrees trusted
    /// interactively), `trust_key` must reproduce the stored key **byte for
    /// byte**. Muse keys a native workspace canonically — a verbatim `\\?\`
    /// path on Windows, the plain absolute path on POSIX — so the Windows
    /// prefix is stripped where present and POSIX keys are compared as they
    /// stand. Ignored by default because it reads the real user store; run
    /// with `cargo test --lib live_trust_key -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads the real user Muse trust store"]
    fn live_trust_key_reproduces_the_installed_stores_keys() {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .expect("a resolvable user home");
        let store = home.join(MUSE_CONFIG_DIR).join("trust.json");
        let Ok(body) = std::fs::read_to_string(&store) else {
            panic!("no Muse trust store at {store:?} — trust a workspace first");
        };
        let value: serde_json::Value = serde_json::from_str(&body).expect("valid store");
        let projects = value["projects"].as_object().expect("projects map");

        let mut checked = 0;
        for stored in projects.keys() {
            // Strip the verbatim prefix Muse's Windows canonicalization added;
            // a POSIX key is already the plain path. Skip entries whose
            // directory no longer exists — they cannot be re-canonicalized.
            let plain = stored.strip_prefix("\\\\?\\").unwrap_or(stored.as_str());
            if !Path::new(plain).exists() {
                continue;
            }
            assert_eq!(
                trust_key(plain),
                *stored,
                "trust_key must reproduce the key Muse stored for {plain}"
            );
            checked += 1;
        }
        assert!(checked > 0, "no existing store entry was verifiable");
        eprintln!("muse trust_key: reproduced {checked} stored key(s)");
    }
}
