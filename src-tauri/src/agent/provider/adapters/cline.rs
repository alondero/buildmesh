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
//! **Attention / transcript**: honest-empty in this slice — `#1775` provisions
//! the attention hook and `#1776` the transcript reader, so the descriptor
//! reports [`AttentionCapability::None`](crate::agent::capabilities::AttentionCapability::None),
//! `supports_passive_turn_watcher: false` and `produces_readable_transcript:
//! false`.

use std::path::{Path, PathBuf};

use crate::agent::capabilities::EffortControlKind;
use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
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

    /// Honest-empty in this slice: the attention hook lands with #1775, so
    /// Cline stays outside the Autopilot attention contract for now.
    fn requires_attention_hook(&self) -> bool {
        false
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
    /// node and whose `time_created` sits inside the recovery window.
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
    fn capabilities_descriptor_is_honest_empty_for_attention_and_transcript() {
        let caps = CLINE.capabilities();
        assert_eq!(caps.harness_id, "cline");
        assert!(caps.supports_resume);
        assert!(caps.auto_resume_on_startup);
        assert!(!caps.requires_attention_hook);
        assert_eq!(
            caps.attention_capability,
            crate::agent::capabilities::AttentionCapability::None
        );
        assert!(!caps.supports_passive_turn_watcher);
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

    // ── Issue #1774: session-id capture wiring ──────────────────────────

    /// Cline self-assigns and disables PTY capture. The fresh-spawn hook
    /// must run a SQLite poller (`services::cline_session`) — pinning
    /// both invariants in one place so a refactor that flips the PTY
    /// flag back to `true` *or* drops the after_fresh_spawn call fails
    /// this test instead of silently leaving `cli_session_id` null on
    /// every Cline node.
    #[test]
    fn self_assigns_disables_pty_capture_and_runs_a_poller() {
        assert!(
            CLINE.self_assigns_session_id(),
            "Cline mints its own session ids; auto-resume must drive --id"
        );
        assert!(
            !CLINE.captures_session_id_from_pty(),
            "Cline ids are <epochms>_<base36>, not UUIDs — PTY capture must stay off"
        );
    }

    /// `recover_suspended_session_id` is the durable path used by the
    /// startup sweep (issue #1774 / issue #1224 family). It must defer
    /// to the Cline SQLite helper rather than reimplementing the read,
    /// and it must surface `None` when no home is resolvable (e.g. an
    /// `$HOME`-less Linux container) — a real `None` is what lets the
    /// sweep skip the node instead of binding garbage.
    #[test]
    fn recover_suspended_session_id_delegates_to_cline_session_helper() {
        // No home resolvable in a bare test env: the helper returns None.
        // We don't assert on the positive path here — `services::cline_session`
        // covers it under controlled SQLite fixtures, and the adapter's job
        // is just to forward without re-implementing.
        let no_home_result = CLINE.recover_suspended_session_id("/no/such/path", EnvType::Wsl, 0, false);
        assert!(
            no_home_result.is_none(),
            "without a resolvable Cline home, the adapter must return None, not a synthesised id"
        );
    }
}
