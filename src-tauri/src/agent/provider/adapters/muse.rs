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
//! **Attention (issue #1709).** Muse's turn signal comes from the passive
//! watcher (`services::muse_watcher`), mirroring Command Code:
//! `requires_attention_hook` stays `false` and `attention_capability` stays
//! `None`. The watcher consumes the durable session log
//! (`~/.local/share/muse/sessions/YYYY/MM/DD/<uuid>/session.jsonl`).
//!
//! Muse 1.3.0 *does* ship a claude-compatible plugin hook surface (a
//! `.claude-plugin/plugin.json` bundle with `Stop`/`Notification`/… handlers; a
//! live `Stop` posts the payload `http::routes::attention` already classifies
//! as a clean turn completion) even though `muse --help` exposes no hook/event
//! flag. It is deliberately **not** provisioned: third-party hooks sit at
//! `review_needed` until an explicit `muse plugins approve`, and the install
//! lands in the user's *global* plugin cache — a consent step Buildmesh must not
//! take unattended. Workspace trust (#1706) has since landed and removes the
//! other blocker, but the consent cost stands. Wiring it is a separate follow-up
//! — see `docs/research/muse-attention-signals.md`.
//!
//! **Launch mode is `SkipPermissions`.** With `--disable-approval` the harness
//! never raises a tool-approval prompt — every observed `approval_disabled`
//! session carries zero `approval/requested` records — so a
//! `PermissionRequested` lifecycle signal is impossible by construction and is
//! deliberately not classified. A `run/terminal` record yields the node back
//! to the user; `terminal` is `completed | failed | cancelled`.
//!
//! **A question is not an approval.** The same log folds the MSP `userInput/*`
//! notifications to disk as run-scoped events, paired by `prompt_id`. Because
//! `request_user_input` asks the user a question rather than gating a tool, it
//! still fires under `--disable-approval`, and
//! `user_input_prompt_requested` is therefore classified as `QuestionRequested`
//! (node `AwaitingInput`). `user_input_prompt_settled` publishes `WorkResumed`:
//! the wait must be closed as well as opened, or a node whose answer does not
//! clear the flag by another path stays stranded in `AwaitingInput`.
//!
//! **Reasoning effort (issue #1704).** `muse --help` advertises
//! `--reasoning-effort <EFFORT>` — "Meta reasoning effort:
//! none|minimal|low|medium|high|xhigh|max|ultra (default: high)" — on both the
//! interactive root command and `muse exec`, so the PTY path Buildmesh spawns
//! accepts it. The adapter advertises that closed vocabulary through
//! `effort_control()`, which is what lets `resolve_agent_config` keep a mesh
//! override, an application default, or an explicit spawn argument instead of
//! dropping it.
//!
//! Both halves of the wiring are load-bearing. The flag has **no `--effort`
//! alias**, so `effort_args` is overridden as well: the trait default emits
//! `["--effort", level]`, which Muse rejects as an unknown option and which
//! would turn a previously ignored effort setting into a launch it refuses.
//! The vocabulary lives in [`MUSE_EFFORT_ALLOWED`], shared with the resolver's
//! mask, so anything outside the eight levels is dropped rather than forwarded.
//!
//! **Flags Buildmesh never passes (issue #1710).** `supports_extra_args` is
//! `true`, so `extra_args` reaches any flag Muse accepts — unvalidated, which
//! is correct for an escape hatch and wrong for a default. Every flag this
//! adapter does not bake therefore carries an explicit verdict recorded in
//! `docs/learning/muse-harness-capabilities.md` ("Launch flags: explicit
//! verdicts"), verified against the installed 1.3.0 `muse --help` /
//! `muse exec --help`. The never-pass set, and why:
//!
//! - `-w` / `--worktree` (+ `--worktree-base`, `--worktree-existing`) — ADR-0003.
//!   The worktree provisioner (`git/worktree/provision.rs`) has already created
//!   the node's checkout before this process starts; a harness-created one would
//!   leave the node running in a directory Buildmesh never provisioned, which
//!   also desyncs the `trust.json` key above and the watcher's session-log path.
//!   Resume is a no-op on the worktree, so nothing re-passes it later either.
//!   Same verdict as Grok's `-w`.
//! - `--workspace <PATH>` — the node's world is its spawn cwd, which is exactly
//!   what `ensure_workspace_trusted` keys on. A second policy-gated tools root
//!   splits the agent from its node directory, and nothing would know which
//!   root won.
//! - `--provider echo` — a deterministic test double (its `--echo-delay-ms`
//!   companion exists only to keep it deterministic) that answers canned text,
//!   never touches the repo, and ignores the model override (`--model` is scoped
//!   to "non-echo providers"). Never baked, never inherited from a default.
//!   `--echo-delay-ms` follows it to never-pass: it is scoped to that provider,
//!   so there is no configuration in which Buildmesh would want it.
//! - `--base-url` — endpoint routing is a credential decision belonging to the
//!   proxied-provider story (`preferences::compatibility::resolve_provider_env`,
//!   the Codex-proxy precedent), not a freeform launch flag.
//! - `--subagent-worktree-isolation` — a documented compatibility no-op
//!   ("capability defaults on"), so passing it asserts nothing.
//! - `--no-session-log` — the passive watcher arms on that very durable log, so
//!   suppressing it makes the node unobservable rather than quiet.
//!
//! The `exec`-only flags (`--max-model-steps`, `--max-tool-output-bytes`,
//! `--context-compaction-*`, `--session-id`, `--json`, `--prompt-file`,
//! `--output-schema`, …) are absent from the interactive TUI's grammar: this
//! adapter spawns the TUI, never `muse exec`, so forwarding one would make the
//! CLI reject the launch outright. `--session-id` is the sharp one, because
//! `AgentProvider`'s default `session_assign_args` emits exactly that — hence
//! the explicit empty override below. `--max-model-steps` and
//! `--max-tool-output-bytes` are run-budget knobs for a headless mode Buildmesh
//! does not drive; the compaction thresholds additionally take a fraction, which
//! no current `HarnessConfigValue` slot can carry.
//!
//! No flag in this pass graduates to modeled — `ResolvedAgentConfig` and the
//! capability table are unchanged. OpenCode's `--agent <name>` remains the
//! template if one ever does (`HarnessConfigValue` + `ResolvedAgentConfig` +
//! capability gate + an adapter `*_args` helper, never a provider-name check at
//! a call site).
use crate::agent::capabilities::{
    EffortControlKind, PermissionModeOption, MUSE_EFFORT_ALLOWED, PERMISSION_MODE_UNATTENDED,
};
use crate::agent::managed_config;
use crate::agent::provider::{
    AgentProvider, LaunchRuntime, Platform, ResolvedPath, SpawnRecipe, UiMeta, WindowsShell,
};
use crate::models::EnvType;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct MuseAdapter;

impl crate::agent::provider::PromptAcceptanceReceipt
    for crate::services::muse_watcher::PromptReceipt
{
    fn accepted(&self) -> bool {
        crate::services::muse_watcher::PromptReceipt::accepted(self)
    }
}
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
    home.map(PathBuf::from)
        .map(|home| home.join(MUSE_CONFIG_DIR))
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
    let mut document = managed_config::read_document(path).map_err(|error| error.to_string())?;
    let entry = managed_config::root_object(&mut document, "muse trust.json top-level")?;

    let key = trust_key(workspace);
    let mut changed = false;
    {
        if entry.get("schema_version").is_none() {
            entry.insert(
                "schema_version".to_string(),
                serde_json::json!(MUSE_TRUST_SCHEMA_VERSION),
            );
            changed = true;
        }

        let projects =
            managed_config::object_entry(entry, "projects", "muse trust.json `projects`")?;
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
                    managed_config::json_kind(other)
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
    let content = document
        .to_pretty_json()
        .map_err(|error| format!("serialize muse trust.json failed: {error}"))?;
    managed_config::atomic_write(path, &content)
        .map_err(|error| format!("failed to write muse trust.json: {error}"))?;
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
    fn descriptor(&self) -> &'static crate::agent::capabilities::HarnessDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<crate::agent::capabilities::HarnessDescriptor> =
            std::sync::LazyLock::new(|| crate::agent::capabilities::HarnessDescriptor {
                supports_resume: true,
                auto_resume_on_startup: true,
                circuit_observation: {
                    crate::circuit::strategy::ObservationStrategy {
                        passive_watcher: Some(crate::circuit::strategy::PassiveWatcher::Muse),
                        ..crate::circuit::strategy::ObservationStrategy::UNWIRED
                    }
                },
                produces_readable_transcript: {
                    // Issue #1708: the muse reader
                    // (`services::transcript_reader::adapters::muse::MuseAdapter`) is
                    // wired, so muse nodes now hydrate the Coordinator Node Digest's
                    // rich layer AND surface in the archived-node resume picker
                    // (the `resumable = supports_resume && produces_readable_transcript`
                    // conjunction in `provider_menu.rs:53`).
                    true
                },
                supports_model_override: true,
                supports_extra_args: true,
                supports_prefill: true,
                available_on: {
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
                },
                effort_control: {
                    EffortControlKind::Closed {
                        allowed: MUSE_EFFORT_ALLOWED.iter().map(|s| s.to_string()).collect(),
                    }
                },
                permission_modes: {
                    vec![
            PermissionModeOption::unattended(
                "--disable-approval --disable-sandbox",
                "Prompts off + Muse's own OS sandbox off (today's behavior; required for unattended runs).",
            ),
            PermissionModeOption::prompt(
                "Prompts on (no flags)",
                "Muse asks for approval like a human-launched session (its own OS sandbox stays on).",
            ),
        ]
                },
                ..Default::default()
            });
        &DESCRIPTOR
    }

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
        // Issue #2151: bare — no approval flags. The effective permission
        // mode contributes `--disable-approval --disable-sandbox` via
        // `permission_args` in `default_prepare`.
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
            base_args: Vec::new(),
            trailing_args: vec![],
            windows_shell: WindowsShell::Direct,
        }
    }

    fn permission_args(&self, mode_id: &str) -> Vec<String> {
        if mode_id == PERMISSION_MODE_UNATTENDED {
            vec!["--disable-approval".into(), "--disable-sandbox".into()]
        } else {
            Vec::new()
        }
    }

    /// Issue #2060/#2061: Muse draws a mid-size paste in full in its input
    /// box and collapses only the largest into a `[Pasted Content N chars]`
    /// marker, so the paste gate also confirms a long draft by its tail.
    fn paste_gate_policy(&self) -> crate::agent::provider::PasteGatePolicy {
        crate::agent::provider::PasteGatePolicy::RenderedWithTailAnchor
    }
    fn prompt_acceptance_receipt(
        &self,
        node_id: i64,
        prompt: &str,
    ) -> Result<Option<Box<dyn crate::agent::provider::PromptAcceptanceReceipt>>, String> {
        let node = crate::db::get_agent_node_by_id(node_id).map_err(|error| error.to_string())?;
        crate::services::muse_watcher::PromptReceipt::capture(&node, prompt).map(|receipt| {
            receipt.map(|receipt| {
                Box::new(receipt) as Box<dyn crate::agent::provider::PromptAcceptanceReceipt>
            })
        })
    }
    fn self_assigns_session_id(&self) -> bool {
        true
    }
    fn captures_session_id_from_pty(&self) -> bool {
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
        provision_trust_at(
            resolve_config_dir(resolved).as_deref(),
            workspace_for(resolved),
        )
    }

    // Issue #1709: no native hook exists, so Muse's turn signal comes from the
    // backend-owned session-log watcher instead.

    fn on_spawn_activated(&self, node_id: i64) {
        crate::services::muse_watcher::activate(node_id);
    }
    fn on_process_terminated(&self, node_id: i64) {
        crate::services::muse_watcher::stop(node_id);
    }

    // Resume accepts a session UUID but does not document a positional prompt.
    fn prefill_requires_pty(&self, _text: &str) -> bool {
        true
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["resume".into(), id.into()]
    }

    /// Muse self-assigns its interactive session ids, so `Assign` mode must
    /// forward nothing. The trait default would emit `--session-id <uuid>`,
    /// and that flag exists **only on `muse exec`** — the interactive TUI
    /// would reject the launch outright (verified against the installed 1.3.0
    /// `muse --help` / `muse exec --help`, issue #1710).
    ///
    /// Today `prepare_context` never routes a self-assigning adapter into
    /// `Assign` (`agent::spawn::prepare.rs` checks `self_assigns_session_id()`
    /// first), so this is defence in depth rather than a live crash — but
    /// `prepare_launch` is a public seam, and every other self-assigning
    /// adapter (agy, cline, codex, commandcode, cursor, kimi, mcode,
    /// opencode) already overrides this to `vec![]`. Being the lone exception
    /// is exactly how an `exec`-only flag would reach the TUI later.
    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }
    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec![text.into()]
    }

    /// Issue #1704 — Muse's reasoning-effort flag is `--reasoning-effort`,
    /// with no `--effort` alias. The trait default's `["--effort", value]`
    /// would make Muse reject the launch, so the recipe shape is overridden
    /// alongside `effort_control` below.
    fn effort_args(&self, effort: &str) -> Vec<String> {
        vec!["--reasoning-effort".into(), effort.into()]
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
#[path = "muse_tests.rs"]
mod tests;
