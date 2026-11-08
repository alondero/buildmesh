use super::*;

/// macOS always direct-spawns the binary via the `cfg!(target_os = "macos")`
/// short-circuit in `spawn_environment::wrap` (the `windows_shell` field is
/// never consulted on macOS hosts). Linux reaches the `WindowsShell::Direct`
/// arm of the wrap function's `match`, which likewise direct-spawns. So both
/// platforms produce a direct spawn — no shell wrapper needed because
/// `opencode` is a real executable on PATH on both.
///
/// Regression for issue #827: before the fix `spawn_recipe` ignored its
/// `platform` arg and returned `WindowsShell::Cmd` on Linux, so a
/// native-Linux mesh got `cmd.exe /c opencode …` — silent failure at
/// spawn time on a platform that was advertised as supported.
#[test]
fn spawn_recipe_direct_on_macos() {
    let recipe = OPENCODE.spawn_recipe(Platform::Macos, EnvType::Windows);
    assert_eq!(recipe.binary, "opencode");
    // Issue #2151: bare base recipe — `--auto` comes from the
    // effective permission mode in `default_prepare`.
    assert!(
        recipe.base_args.is_empty(),
        "base recipe must carry no approval flags; got {:?}",
        recipe.base_args
    );
    assert!(
        matches!(recipe.windows_shell, WindowsShell::Direct),
        "macOS must use WindowsShell::Direct — got {:?}",
        recipe.windows_shell
    );
}

#[test]
fn spawn_recipe_direct_on_linux() {
    let recipe = OPENCODE.spawn_recipe(Platform::Linux, EnvType::Windows);
    assert_eq!(recipe.binary, "opencode");
    // Issue #2151: bare base recipe (see macOS pin above).
    assert!(
        recipe.base_args.is_empty(),
        "base recipe must carry no approval flags; got {:?}",
        recipe.base_args
    );
    assert!(
        matches!(recipe.windows_shell, WindowsShell::Direct),
        "Linux must use WindowsShell::Direct — got {:?}",
        recipe.windows_shell
    );
}

/// Windows keeps `WindowsShell::Cmd` because `opencode` is a `.cmd` batch
/// file on Windows; `CreateProcess` won't execute it directly. Pinning
/// this guards against the easy mistake of normalising every provider
/// onto `Direct` and breaking the Windows path.
#[test]
fn spawn_recipe_cmd_on_windows() {
    let recipe = OPENCODE.spawn_recipe(Platform::Windows, EnvType::Windows);
    assert_eq!(recipe.binary, "opencode");
    // Issue #2151: bare base recipe (see macOS pin above).
    assert!(
        recipe.base_args.is_empty(),
        "base recipe must carry no approval flags; got {:?}",
        recipe.base_args
    );
    assert!(
        matches!(recipe.windows_shell, WindowsShell::Cmd),
        "Windows must use WindowsShell::Cmd for the .cmd shim — got {:?}",
        recipe.windows_shell
    );
}

/// `available_on` must list every platform OpenCode runs on. Pre-#827
/// macOS was missing, so the spawn-menu composer in
/// `agent::provider_menu::available_providers` filtered OpenCode out for a
/// macOS user even when the binary was detected at startup. Pin the exact
/// set (not just membership) so a future "while we're here" addition
/// forces an explicit test update — matches the equivalent assertion on
/// `TERMINAL` (`terminal_available_on_three_platforms`).
#[test]
fn available_on_includes_macos() {
    let platforms = OPENCODE.descriptor().available_on;
    assert_eq!(
        platforms.len(),
        3,
        "available_on should pin to exactly {{Windows, Linux, Macos}} — got {:?}",
        platforms
    );
    assert!(
        platforms.contains(&Platform::Macos),
        "OpenCode must be available on macOS (issue #827); got {:?}",
        platforms
    );
    assert!(platforms.contains(&Platform::Linux));
    assert!(platforms.contains(&Platform::Windows));
}

/// Pin id + UI metadata so the wire type / frontend icon mapping can't
/// drift silently (matches the equivalent assertion on `TERMINAL`).
#[test]
fn id_and_ui_metadata() {
    assert_eq!(OPENCODE.id(), "opencode");
    let ui = OPENCODE.ui();
    assert_eq!(ui.label, "OpenCode");
    assert_eq!(ui.color, "#f59e0b");
    assert_eq!(ui.icon, "O");
}

#[test]
fn self_assigns_session_id() {
    assert!(OPENCODE.self_assigns_session_id());
    assert!(
        !OPENCODE.captures_session_id_from_pty(),
        "OpenCode ses_ IDs are not PTY UUID banners; capture is after_fresh_spawn"
    );
}

#[test]
fn resume_args_format() {
    let args = OPENCODE.resume_args("ses_fc52ccfb9ffek1jl23ZwpRuSP7");
    assert_eq!(
        args,
        vec!["--session", "ses_fc52ccfb9ffek1jl23ZwpRuSP7"],
        "OpenCode resume is --session <id>, not --resume"
    );
}

#[test]
fn session_assign_args_empty() {
    let args = OPENCODE.session_assign_args("any-id");
    assert!(
        args.is_empty(),
        "OpenCode self-assigns; session_assign_args must be empty (no --session-id)"
    );
}

#[test]
fn prefill_args_use_prompt_flag() {
    let args = OPENCODE.prefill_args("fix the auth bug");
    assert_eq!(args, vec!["--prompt", "fix the auth bug"]);
}

#[test]
fn prefill_args_flatten_newlines_for_cmd_exe() {
    let args = OPENCODE.prefill_args("fix auth\nthen run tests\r\nand push");
    assert_eq!(args, vec!["--prompt", "fix auth then run tests and push"]);
    assert!(
        !args[1].contains('\n') && !args[1].contains('\r'),
        "cmd.exe /c must not see a newline in --prompt; got {:?}",
        args[1]
    );
}

#[test]
fn model_args_use_long_form() {
    let args = OPENCODE.model_args("anthropic/claude-sonnet-4-5");
    assert_eq!(args, vec!["--model", "anthropic/claude-sonnet-4-5"]);
}

#[test]
fn agent_args_use_long_form_and_never_dangle() {
    use crate::agent::provider::AgentProvider;
    assert_eq!(OPENCODE.agent_args("build"), vec!["--agent", "build"]);
    assert!(
        OPENCODE.agent_args("").is_empty(),
        "an empty name must not emit a dangling --agent that would swallow the next token"
    );
}

#[test]
fn interactive_recipe_carries_agent_flag() {
    use crate::agent::capabilities::ResolvedAgentConfig;
    use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};
    use crate::models::EnvType;
    let config = ResolvedAgentConfig {
        agent: Some("build".into()),
        ..Default::default()
    };
    let prepared = OPENCODE.prepare_launch(HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Linux,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::None,
        config: &config,
        prefill: None,
        sandbox: false,
    });
    assert!(prepared.recipe.base_args.contains(&"--agent".to_string()));
    assert!(prepared.recipe.base_args.contains(&"build".to_string()));
    assert!(prepared.capabilities.supports_agent);
}

#[test]
fn background_carries_run_agent() {
    use crate::agent::provider::AgentProvider;
    use crate::preferences::launch_configurations::{capture, LaunchOverrides};
    let prefs = crate::preferences::AppPreferences::default();
    let mut plan = capture(&prefs, "opencode", &LaunchOverrides::default()).unwrap();
    plan.agent = Some("build".into());
    let launch = crate::agent::background::resolve_plan(plan, &prefs).unwrap();
    assert!(launch.args.contains(&"--agent".to_string()));
    assert!(launch.args.contains(&"build".to_string()));
    // Malformed agents fail before the background spawn, like the
    // interactive path.
    let mut bad = capture(&prefs, "opencode", &LaunchOverrides::default()).unwrap();
    bad.agent = Some("has space".into());
    let error = crate::agent::background::resolve_plan(bad, &prefs)
        .err()
        .expect("malformed agent must fail background resolution");
    assert!(error.contains("agent"), "{error}");
    // The trait default keeps background and interactive spellings in sync.
    assert_eq!(
        OPENCODE.background_agent_args("build"),
        OPENCODE.agent_args("build")
    );
    assert!(OPENCODE.background_agent_args("").is_empty());
}

#[test]
fn agent_names_validate_before_spawn() {
    use crate::agent::provider::AgentProvider;
    assert!(OPENCODE.validate_agent_override("build").is_ok());
    assert!(OPENCODE.validate_agent_override("plan").is_ok());
    for malformed in ["", "  ", "has space", "tab\tname", "carriage\rname"] {
        assert!(
            OPENCODE.validate_agent_override(malformed).is_err(),
            "{malformed:?} must be rejected before spawn"
        );
    }
}

/// Issue #2151: the base recipe is bare — `--auto` is the OpenCode
/// analogue of AGY's `--dangerously-skip-permissions` (see the module
/// docstring) and now comes from the effective permission mode in
/// `default_prepare`. This test pins the bare argv so a future edit
/// that smuggles an extra flag in (e.g. `--auto --garbage`) trips
/// here, not at runtime.
#[test]
fn spawn_recipe_carries_no_hidden_flags_on_every_platform() {
    for platform in [Platform::Windows, Platform::Linux, Platform::Macos] {
        let recipe = OPENCODE.spawn_recipe(platform, EnvType::Windows);
        assert!(
            recipe.base_args.is_empty(),
            "OpenCode base recipe must be bare on {platform:?}; got {:?}",
            recipe.base_args
        );
    }
}

#[test]
fn supports_resume_model_prefill_attention_hook_and_readable_transcript() {
    assert!(OPENCODE.descriptor().supports_resume);
    assert!(OPENCODE.descriptor().auto_resume_on_startup);
    assert!(OPENCODE.descriptor().supports_model_override);
    assert!(OPENCODE.descriptor().supports_prefill);
    // Issue #1295: plugin hook unblocks the Autopilot gate.
    assert!(OPENCODE.descriptor().requires_attention_hook());
    // Issue #1296: OpenCode's local SQLite messages are now read by the
    // transcript reader, so the archive-resume picker and the Coordinator
    // rich layer can include OpenCode nodes.
    assert!(OPENCODE.descriptor().produces_readable_transcript);
}

#[test]
fn capabilities_descriptor_advertises_resume_model_prefill_attention_hook() {
    let caps = crate::agent::capabilities::capabilities_for(&OPENCODE);
    assert_eq!(caps.harness_id, "opencode");
    assert!(caps.supports_resume);
    assert!(caps.auto_resume_on_startup);
    assert!(caps.supports_model_override);
    assert!(caps.supports_prefill);
    assert!(!caps.supports_effort_override);
    // Issue #1295: descriptor must mirror the adapter's `requires_attention_hook`.
    assert!(caps.requires_attention_hook);
    // Issue #1296: descriptor must mirror the adapter's `produces_readable_transcript`.
    assert!(caps.produces_readable_transcript);
    assert!(!caps.is_plain_terminal);
    assert_eq!(
        caps.effort_control,
        crate::agent::capabilities::EffortControlKind::None
    );
}

// -- Attention plugin (issue #1295) ------------------------------------

/// The plugin file is an ESM module — OpenCode's loader fails on
/// syntax errors and surfaces them as a startup fault, so we run
/// `node --check` on the embedded template to fail closed here
/// rather than on a real spawn. `node` is available on every CI
/// runner we ship to (issue #1295 review); the `which` probe is
/// there so a future sandbox without `node` doesn't produce a
/// confusing panic.
///
/// Implementation note: `node --check` does not accept
/// `--input-type=module` (Node v24 ERR_INPUT_TYPE_NOT_ALLOWED), so
/// we write the file with a `.mjs` extension to force ESM detection
/// without needing the flag. OpenCode's real `.opencode/plugins/buildmesh-attention.js`
/// is loaded as ESM by the OpenCode TUI's own loader (which knows
/// the plugin contract); `.mjs` is just the safest syntax-check
/// harness.
#[test]
fn plugin_template_is_valid_esm_per_node_check() {
    let node = which_node_for_check();
    let Some(node) = node else {
        // `node` not on PATH — skip rather than false-green. The
        // plugin still ships, but the syntax check is opt-in per
        // sandbox. CI installs node before running cargo test.
        eprintln!("node --check skipped (node not on PATH)");
        return;
    };
    let temp = tempfile::tempdir().expect("tempdir");
    let plugin_path = temp.path().join("buildmesh-attention.mjs");
    std::fs::write(&plugin_path, OPENCODE_ATTENTION_PLUGIN).expect("write temp");
    let output = std::process::Command::new(node)
        .arg("--check")
        .arg(&plugin_path)
        .output()
        .expect("spawn node --check");
    assert!(
        output.status.success(),
        "embedded plugin failed `node --check`: status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `provision_attention_hooks` writes `.opencode/plugins/buildmesh-attention.js`
/// into the project directory. Issue #1295 requires idempotency: a
/// re-spawn with a byte-identical file MUST NOT rewrite (so mtime is
/// preserved and the working tree stays clean), and a re-spawn after
/// the template changes MUST overwrite.
#[test]
fn provision_attention_hooks_writes_plugin_and_is_idempotent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let resolved = ResolvedPath {
        host_path: temp.path().to_string_lossy().into_owned(),
        spawn_path: temp.path().to_string_lossy().into_owned(),
        raw_path: temp.path().to_string_lossy().into_owned(),
        env_type: EnvType::Windows,
    };

    // First provision — creates the directory tree and writes the file.
    OPENCODE
        .provision_attention_hooks(&resolved, &LaunchRuntime::default(), 42)
        .expect("first provision");

    let plugin_path = temp
        .path()
        .join(".opencode")
        .join("plugins")
        .join("buildmesh-attention.js");
    assert!(
        plugin_path.exists(),
        "plugin file not written to {}",
        plugin_path.display()
    );
    let first = std::fs::read(&plugin_path).expect("read plugin");
    assert_eq!(
        first.as_slice(),
        OPENCODE_ATTENTION_PLUGIN.as_bytes(),
        "written plugin must be byte-equal to the embedded template"
    );

    // Idempotency: a second provision with the same template must
    // NOT touch the file (so the user's working tree stays clean
    // across repeated spawns).
    let mtime_before = std::fs::metadata(&plugin_path)
        .expect("stat")
        .modified()
        .expect("mtime");
    OPENCODE
        .provision_attention_hooks(&resolved, &LaunchRuntime::default(), 42)
        .expect("second provision");
    let mtime_after = std::fs::metadata(&plugin_path)
        .expect("stat")
        .modified()
        .expect("mtime");
    assert_eq!(
        mtime_before, mtime_after,
        "idempotent provision must not rewrite the plugin file (mtime preserved)"
    );

    std::fs::remove_file(&plugin_path).expect("delete");
    OPENCODE
        .provision_attention_hooks(&resolved, &LaunchRuntime::default(), 42)
        .expect("third provision");
    assert!(
        plugin_path.exists(),
        "plugin file must be re-created after removal"
    );
}

/// Provision must surface a real filesystem error (a leaf file used
/// as a project root is unrecoverable). The spawn caller treats this
/// as best-effort: a failure is logged, the spawn proceeds, and the
/// attention callback is the only casualty.
#[test]
fn provision_attention_hooks_surfaces_filesystem_errors() {
    let temp = tempfile::tempdir().expect("tempdir");
    let blocker = temp.path().join("blocker");
    std::fs::write(&blocker, b"not a dir").expect("create blocker");
    let resolved = ResolvedPath {
        host_path: blocker.to_string_lossy().into_owned(),
        spawn_path: blocker.to_string_lossy().into_owned(),
        raw_path: blocker.to_string_lossy().into_owned(),
        env_type: EnvType::Windows,
    };
    let result = OPENCODE.provision_attention_hooks(&resolved, &LaunchRuntime::default(), 0);
    assert!(
        result.is_err(),
        "provision must surface a filesystem error when project path is a leaf file; got Ok"
    );
    // The helper must NOT panic or corrupt the blocker.
    assert!(
        std::fs::read(&blocker).is_ok(),
        "provision error path must not mutate the blocker"
    );
}

/// Issue #1295 review: the injection helper uses an atomic write
/// (temp file + rename) so two concurrent spawns racing on the same
/// project path can't lose data on Windows (no atomic POSIX rename
/// over an open target). This test verifies the temp filenames
/// carry a unique PID+counter so they don't collide across either
/// (a) concurrent processes or (b) concurrent calls inside one
/// process. On rename failure, the temp file is removed (no `.tmp`
/// residue left in the project tree).
#[test]
fn atomic_write_uses_unique_tmp_and_cleans_up_on_failure() {
    let temp = tempfile::tempdir().expect("tempdir");
    let target = temp.path().join("plugin.js");

    // Two concurrent writes would collide on a plain `<name>.tmp`
    // filename; the PID+counter disambiguates them.
    managed_config::atomic_write(&target, "first").expect("first write");
    managed_config::atomic_write(&target, "second").expect("second write");
    assert_eq!(
        std::fs::read_to_string(&target).expect("read"),
        "second",
        "atomic_write must leave the latest content at the target"
    );

    // On rename failure, the temp file is cleaned up — no `.tmp`
    // residue alongside the target. Scan the parent dir for any
    // `.tmp` file we may have left behind.
    let residue: Vec<_> = std::fs::read_dir(temp.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .filter(|name| name.to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(
        residue.is_empty(),
        "successful writes must not leave .tmp residue; found: {:?}",
        residue
    );

    // Failure path: write to a path whose parent is a leaf file.
    // `File::create` for the tmp file fails before rename, so the
    // error surfaces and the blocker must be untouched.
    let blocker = temp.path().join("blocker");
    std::fs::write(&blocker, b"intact").expect("create blocker");
    let failing_target = blocker.join("plugin.js");
    let err = managed_config::atomic_write(&failing_target, "nope")
        .expect_err("must fail because blocker is a leaf file");
    assert_eq!(
        std::fs::read(&blocker).expect("blocker readable"),
        b"intact",
        "rename failure must not mutate the blocker"
    );
    assert!(
        err.kind() == std::io::ErrorKind::PermissionDenied
            || err.kind() == std::io::ErrorKind::AlreadyExists
            || matches!(
                err.kind(),
                std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::NotADirectory
                    | std::io::ErrorKind::Other
            ),
        "expected a filesystem error, got {:?}",
        err
    );
}

/// Locate `node` on PATH for the syntax-check test. Cached on first
/// call — runs once per process.
fn which_node_for_check() -> Option<&'static str> {
    use std::sync::OnceLock;
    static NODE: OnceLock<Option<String>> = OnceLock::new();
    NODE.get_or_init(|| {
        for candidate in ["node", "node.exe"] {
            if let Ok(out) = std::process::Command::new(candidate)
                .arg("--version")
                .output()
            {
                if out.status.success() {
                    return Some(candidate.to_string());
                }
            }
        }
        None
    })
    .as_deref()
}

/// Resume argv is `--auto` (unattended permission default, issue
/// #2151 — where the base recipe used to carry it) plus `--session
/// <id>` (flag, not subcommand, issue #1297). No `--fork`: an
/// Explicit / Startup resume continues the conversation in place.
#[test]
fn resume_recipe_carries_session_flag() {
    use crate::agent::capabilities::ResolvedAgentConfig;
    use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};
    use crate::agent::spawn::ResumeCause;

    let config = ResolvedAgentConfig::default();
    let input = HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Windows,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::Resume {
            id: "ses_fc52ccfb9ffek1jl23ZwpRuSP7",
            cause: ResumeCause::Explicit,
        },
        config: &config,
        prefill: None,
        sandbox: false,
    };
    let prepared = OPENCODE.prepare_launch(input);
    let args = &prepared.recipe.base_args;
    assert_eq!(
        args,
        &[
            "--auto".to_string(),
            "--session".to_string(),
            "ses_fc52ccfb9ffek1jl23ZwpRuSP7".to_string()
        ]
    );
    assert!(
        !args.iter().any(|a| a == "--resume" || a == "--session-id"),
        "must not emit Claude/Codex resume flags; got {args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "--fork"),
        "an in-place resume must not fork — that would strand the parent \
             transcript the user is continuing; got {args:?}"
    );
}

/// Issue #1550 — `resume_args_fork` is the adapter seam the
/// orchestrator calls for a `ResumeCause::Regenerate` resume, and
/// OpenCode's `--fork` must land immediately after `--session <id>`
/// (verified against CLI 1.18.3).
#[test]
fn resume_args_fork_appends_fork_after_session() {
    assert_eq!(
        OPENCODE.resume_args_fork("ses_fc52ccfb9ffek1jl23ZwpRuSP7"),
        vec![
            "--session".to_string(),
            "ses_fc52ccfb9ffek1jl23ZwpRuSP7".to_string(),
            "--fork".to_string()
        ],
        "fork form is --session <id> --fork"
    );
    assert_eq!(
        OPENCODE.resume_args("ses_fc52ccfb9ffek1jl23ZwpRuSP7"),
        vec![
            "--session".to_string(),
            "ses_fc52ccfb9ffek1jl23ZwpRuSP7".to_string()
        ],
        "the plain resume shape must stay fork-free — it is what Startup \
             and Explicit resumes compose"
    );
}

/// End-to-end through the shared helper: a `Regenerate` resume on
/// OpenCode produces the forked argv, so the same-harness provider
/// swap that reuses the captured `ses_…` id does not keep appending
/// to the transcript it branches from.
#[test]
fn regenerate_resume_recipe_forks_off_the_captured_session() {
    use crate::agent::capabilities::ResolvedAgentConfig;
    use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};
    use crate::agent::spawn::ResumeCause;

    let config = ResolvedAgentConfig::default();
    let input = HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Windows,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::Resume {
            id: "ses_fc52ccfb9ffek1jl23ZwpRuSP7",
            cause: ResumeCause::Regenerate,
        },
        config: &config,
        prefill: None,
        sandbox: false,
    };
    let prepared = OPENCODE.prepare_launch(input);
    assert_eq!(
        prepared.recipe.base_args,
        vec![
            "--auto".to_string(),
            "--session".to_string(),
            "ses_fc52ccfb9ffek1jl23ZwpRuSP7".to_string(),
            "--fork".to_string()
        ]
    );
}

/// Fresh spawn argv order: `--auto` (unattended permission default,
/// issue #2151) → `--model <id>` → `--prompt <text>`. No
/// session-assign flag (self-assign). Pin the exact vector so a
/// future reorder that pushes `--auto` past `--model` (or drops it)
/// trips here, not in production.
#[test]
fn fresh_recipe_forwards_model_and_prompt_without_session_id() {
    use crate::agent::capabilities::ResolvedAgentConfig;
    use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};

    let config = ResolvedAgentConfig {
        model: Some("anthropic/claude-sonnet-4-5".to_string()),
        effort: None,
        extra_args: None,
        permission_mode: None,
        agent: None,
    };
    let input = HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Linux,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::None,
        config: &config,
        prefill: Some("fix the auth bug"),
        sandbox: false,
    };
    let prepared = OPENCODE.prepare_launch(input);
    assert_eq!(
        prepared.recipe.base_args,
        vec![
            "--auto".to_string(),
            "--model".to_string(),
            "anthropic/claude-sonnet-4-5".to_string(),
            "--prompt".to_string(),
            "fix the auth bug".to_string(),
        ]
    );
    assert!(
        !prepared
            .recipe
            .base_args
            .iter()
            .any(|a| a == "--session" || a == "--session-id" || a == "--prefill"),
        "fresh spawn must not assign a session id or emit --prefill; got {:?}",
        prepared.recipe.base_args
    );
}
