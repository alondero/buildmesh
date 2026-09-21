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
//! **Attention (issue #1709).** Muse exposes no interactive attention-hook
//! registration: `muse --help` has no hook/event flag, there is no workspace
//! or global hook config file, and the MSP lifecycle events (`turn/*`,
//! `approval/*`, `userInput/*` in `muse schema generate-json-schema`) are
//! served only on the separate `muse serve` stdio plane — the headless
//! architecture Buildmesh's PTY spawn does not use.
//!
//! The interactive TUI does, however, append run boundaries to its durable
//! session log (`~/.local/share/muse/sessions/YYYY/MM/DD/<uuid>/session.jsonl`).
//! Muse therefore supplies its turn signal through the passive watcher
//! (`services::muse_watcher`), mirroring Command Code: `requires_attention_hook`
//! stays `false` and `attention_capability` stays `None`.
//!
//! **Launch mode is `SkipPermissions`.** With `--disable-approval` the harness
//! never raises a tool-approval prompt — every observed `approval_disabled`
//! session carries zero `approval/requested` records — so a
//! `PermissionRequested` lifecycle signal is impossible by construction and is
//! deliberately not classified. A `run/terminal` record yields the node back
//! to the user; `terminal` is `completed | failed | cancelled`.
use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct MuseAdapter;
pub static MUSE: MuseAdapter = MuseAdapter;

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
        let native = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(std::path::PathBuf::from)?
            .join(".local/share/muse");
        let home = crate::env::cli_dir_for_spawn(native, ".local/share/muse", spawn_path)?;
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

fn find_session(
    database: &std::path::Path,
    workspace: &str,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let connection =
        rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    connection
        .busy_timeout(std::time::Duration::from_millis(200))
        .ok()?;
    // Muse's index can leave workspace/timestamp columns NULL. Read only
    // the session metadata frame from the indexed log, never transcript text.
    let mut statement = connection.prepare("SELECT session_id, session_log_path FROM sessions ORDER BY session_log_path DESC LIMIT 128").ok()?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .ok()?;
    let indexed: Vec<_> = rows.filter_map(Result::ok).collect();
    drop(statement);
    drop(connection);
    let candidates = indexed.into_iter().filter_map(|(id, path)| {
        let path = crate::env::to_host_path(&path);
        let (recorded_id, cwd, timestamp) = session_metadata(std::path::Path::new(&path))?;
        (id == recorded_id && crate::env::directories_match(&cwd, workspace))
            .then_some((id, timestamp))
    });
    crate::services::session_recovery::select_recovery_identity(
        candidates,
        anchor_ms,
        recorded_start,
    )
}

fn session_metadata(path: &std::path::Path) -> Option<(String, String, i64)> {
    use std::io::{BufRead, Read};
    let file = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(file.take(262_144)).lines().take(64) {
        let line = line.ok()?;
        let frame: serde_json::Value = serde_json::from_str(&line).ok()?;
        if let Some(metadata) = metadata_record(&frame) {
            return Some(metadata);
        }
        let Some(children) = frame.get("children").and_then(|c| c.as_array()) else {
            continue;
        };
        for child in children {
            let Some(json) = child.get("record_json").and_then(|r| r.as_str()) else {
                continue;
            };
            let Ok(record) = serde_json::from_str::<serde_json::Value>(json) else {
                continue;
            };
            if let Some(metadata) = metadata_record(&record) {
                return Some(metadata);
            }
        }
    }
    None
}

fn metadata_record(record: &serde_json::Value) -> Option<(String, String, i64)> {
    if record.get("payload_type").and_then(|p| p.as_str()) != Some("runtime.session.metadata")
        || record.pointer("/stream/kind").and_then(|s| s.as_str()) != Some("session")
    {
        return None;
    }
    let id = record.pointer("/stream/id")?.as_str()?;
    uuid::Uuid::parse_str(id).ok()?;
    let cwd = record.pointer("/payload/record/workspace_root")?.as_str()?;
    let timestamp = record.get("recorded_at")?.as_i64()?;
    Some((id.into(), cwd.into(), timestamp / 1000))
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
}
