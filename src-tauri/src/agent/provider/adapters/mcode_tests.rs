//! Tests for `agent::provider::adapters::mcode`.
//!
//! Split out of `src-tauri/src/agent/provider/adapters/mcode.rs` by issue #2451. The conventions this
//! layout depends on — why the module keeps the name `tests`, and why every
//! `#[path]` module needs an entry in `RUST_PATH_INCLUDES` — are
//! documented once in `docs/development/rust-conventions.md`, under *Rust unit
//! tests live in sibling `*_tests.rs` files*.
//!
//! Run with: cd src-tauri && cargo test agent::provider::adapters::mcode::tests

use super::*;
use crate::agent::capabilities::ResolvedAgentConfig;
use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

#[test]
fn id_and_ui_metadata() {
    assert_eq!(MCODE.id(), "mcode");
    let ui = MCODE.ui();
    assert_eq!(ui.label, "MiniMax Code");
    assert_eq!(ui.color, "#6366f1");
    assert_eq!(ui.icon, "M");
}

#[test]
fn spawn_recipe_direct_on_macos_and_linux() {
    for platform in [Platform::Linux, Platform::Macos] {
        let recipe = MCODE.spawn_recipe(platform, EnvType::Windows);
        assert_eq!(recipe.binary, "mcode");
        assert!(recipe.base_args.is_empty());
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Direct),
            "{:?} must use WindowsShell::Direct — got {:?}",
            platform,
            recipe.windows_shell
        );
    }
}

#[test]
fn spawn_recipe_cmd_on_windows() {
    let recipe = MCODE.spawn_recipe(Platform::Windows, EnvType::Windows);
    assert_eq!(recipe.binary, "mcode");
    assert!(recipe.base_args.is_empty());
    assert!(
        matches!(recipe.windows_shell, WindowsShell::Cmd),
        "Windows must use WindowsShell::Cmd for the .cmd shim — got {:?}",
        recipe.windows_shell
    );
}

#[test]
fn available_on_all_three_platforms() {
    let platforms = MCODE.available_on();
    assert_eq!(
        platforms.len(),
        3,
        "available_on should pin to exactly {{Windows, Linux, Macos}} — got {:?}",
        platforms
    );
    assert!(platforms.contains(&Platform::Windows));
    assert!(platforms.contains(&Platform::Linux));
    assert!(platforms.contains(&Platform::Macos));
}

#[test]
fn self_assigns_session_id() {
    assert!(MCODE.self_assigns_session_id());
}

/// Native callbacks capture identity; PTY must not bind UUIDs from tool output.
#[test]
fn self_assigns_but_does_not_capture_from_pty() {
    assert!(
        MCODE.self_assigns_session_id(),
        "mcode mints its own session ids"
    );
    assert!(
        !MCODE.captures_session_id_from_pty(),
        "mcode ids are not PTY banners; capture is after_fresh_spawn (issue #1798)"
    );
}

#[test]
fn resume_args_format() {
    let args = MCODE.resume_args("abc-123");
    assert_eq!(args, vec!["--session", "abc-123"]);
}

#[test]
fn session_assign_args_empty() {
    let args = MCODE.session_assign_args("any-id");
    assert!(
        args.is_empty(),
        "MiniMax Code self-assigns; session_assign_args must be empty"
    );
}

#[test]
fn prefill_args_is_positional() {
    // mcode accepts `[prompt]` as a positional on the TUI — there is
    // no `--prefill` flag. The trait default (`vec!["--prefill", t]`)
    // is wrong here.
    let args = MCODE.prefill_args("fix the auth bug");
    assert_eq!(args, vec!["fix the auth bug"]);
}

#[test]
fn prefill_args_preserves_multiline_text() {
    // Issue/handover prefills are often multi-line; the adapter must not
    // collapse or wrap them — they're appended verbatim.
    let multi = "first line\nsecond line\n  indented";
    let args = MCODE.prefill_args(multi);
    assert_eq!(args, vec![multi]);
}

#[test]
fn supports_prefill_via_positional() {
    assert!(MCODE.supports_prefill());
}

#[test]
fn supports_resume_and_session_model_override() {
    assert!(MCODE.supports_resume());
    assert!(MCODE.supports_model_override());
}

#[test]
fn model_override_accepts_native_and_byok_references() {
    for model in [
        "minimax/MiniMax-M3",
        "minimax/MiniMax-M3#variant",
        "custom_provider:local/M3",
        "custom_provider:my-endpoint_1/M3#fast",
    ] {
        assert!(MCODE.validate_model_override(model).is_ok(), "{model}");
    }
    for model in [
        "custom_provider:/M3",
        "custom_provider:local:extra/M3",
        "other:local/M3",
        "custom_provider:local/M3#",
        "custom_provider:local/M3#variant:extra",
        "custom_provider:local/M3&echo",
    ] {
        assert!(MCODE.validate_model_override(model).is_err(), "{model}");
    }
}

/// Pin the capability descriptor end-to-end: the harness-id,
/// model override support, and the absence of effort
/// controls. Drift here means the Spawn Menu or autopilot
/// compatibility gate will misroute mcode.
#[test]
fn capabilities_descriptor_supports_model_without_effort() {
    let caps = MCODE.capabilities();
    assert_eq!(caps.harness_id, "mcode");
    assert!(caps.supports_resume);
    assert!(caps.supports_prefill);
    assert!(caps.supports_model_override);
    assert!(!caps.supports_effort_override);
    // Issue #1797 — the hook is provisioned and Stop delivery was
    // validated against a live 0.4.12 TUI.
    assert!(caps.requires_attention_hook);
    assert!(caps.produces_readable_transcript);
    assert!(!caps.is_plain_terminal);
    assert_eq!(
        caps.effort_control,
        crate::agent::capabilities::EffortControlKind::None
    );
}

/// Stop requests attention; SessionStart captures identity and UserPromptSubmit
/// resumes work. These observers do not add attention-requesting capabilities.
/// Buildmesh launches mcode with its default
/// (auto-approving) permission policy, so no permission prompt is raised
/// and `PermissionRequested` must NOT be advertised. The `min_version` pin
/// fails a refactor that drops it.
#[test]
fn attention_capability_advertises_validated_stop_only() {
    let caps = MCODE.capabilities();
    assert!(caps.requires_attention_hook);
    let capability = caps.attention_capability;
    match &capability {
        AttentionCapability::Hook {
            events,
            launch_mode,
            trust,
            min_version,
        } => {
            assert_eq!(
                events,
                &vec![LifecycleKind::TurnCompleted],
                "only Stop requests attention under the Full Access launch policy"
            );
            assert!(
                !events.contains(&LifecycleKind::PermissionRequested),
                "Buildmesh launches mcode with an auto-approving permission \
                     policy — no approval prompt is raised, so a permission \
                     signal is impossible by construction"
            );
            assert_eq!(*launch_mode, AttentionLaunchMode::SkipPermissions);
            assert!(
                trust.is_none(),
                "mcode requires no workspace-trust step and we take none: {trust:?}"
            );
            assert_eq!(min_version.as_deref(), Some(MCODE_MIN_HOOK_VERSION));
        }
        _ => panic!("expected Hook, got {capability:?}"),
    }
}

#[test]
fn mcode_interactive_recipe_carries_model_before_positional_prompt() {
    let config = ResolvedAgentConfig {
        model: Some("minimax/MiniMax-M3#variant".to_string()),
        effort: None,
        extra_args: None,
        permission_mode: None,
    };
    for platform in [Platform::Windows, Platform::Macos, Platform::Linux] {
        let prepared = default_prepare(
            &MCODE,
            HarnessLaunchInput {
                platform,
                runtime: EnvType::Windows,
                session: SessionIdModeRef::None,
                config: &config,
                prefill: Some("Review this workspace"),
                sandbox: false,
            },
        );
        assert_eq!(
            prepared.recipe.base_args,
            vec![
                "--model",
                "minimax/MiniMax-M3#variant",
                "Review this workspace",
            ]
        );
        assert_eq!(prepared.recipe.windows_shell, shell_for(platform));
    }
}

#[test]
fn mcode_resume_recipe_carries_session_and_model_before_prompt() {
    let config = ResolvedAgentConfig {
        model: Some("minimax/MiniMax-M3".to_string()),
        effort: None,
        extra_args: None,
        permission_mode: None,
    };
    for platform in [Platform::Windows, Platform::Macos, Platform::Linux] {
        let prepared = default_prepare(
            &MCODE,
            HarnessLaunchInput {
                platform,
                runtime: EnvType::Windows,
                session: SessionIdModeRef::Resume {
                    id: "mvs_abc123",
                    cause: crate::agent::spawn::ResumeCause::Explicit,
                },
                config: &config,
                prefill: Some("Continue the review"),
                sandbox: false,
            },
        );
        assert_eq!(
            prepared.recipe.base_args,
            vec![
                "--session",
                "mvs_abc123",
                "--model",
                "minimax/MiniMax-M3",
                "Continue the review",
            ]
        );
    }
}

#[test]
fn mcode_recipe_without_model_preserves_native_default() {
    let config = ResolvedAgentConfig {
        model: None,
        effort: None,
        extra_args: None,
        permission_mode: None,
    };
    for session in [
        SessionIdModeRef::None,
        SessionIdModeRef::Resume {
            id: "mvs_abc123",
            cause: crate::agent::spawn::ResumeCause::Explicit,
        },
    ] {
        let prepared = default_prepare(
            &MCODE,
            HarnessLaunchInput {
                platform: Platform::Windows,
                runtime: EnvType::Windows,
                session,
                config: &config,
                prefill: None,
                sandbox: false,
            },
        );
        let expected = match session {
            SessionIdModeRef::None => vec![],
            _ => vec!["--session", "mvs_abc123"],
        };
        assert_eq!(prepared.recipe.base_args, expected);
    }
}

#[test]
fn produces_readable_transcript() {
    // mcode's canonical `messages.jsonl` history is parsed via
    // `TranscriptFormat::Mcode` (see
    // `services::transcript_reader::adapters::mcode`).
    assert!(MCODE.produces_readable_transcript());
}

// -----------------------------------------------------------------
// Issue #1796 provisioning + issue #1797 validation.
//
// Contract pinned here:
//   1. Fresh install writes a mcode 0.4.0+ manifest at
//      `<data-dir>/plugins/io.buildmesh.attention/.claude-plugin/plugin.json`
//      with `Stop` AND `PermissionRequest` INLINED on the manifest.
//   2. Additive merge preserves sibling user-authored handlers and
//      events outside our ownership — never clobbers user plugins.
//   3. Malformed user files are refused (not silently overwritten).
//   4. Idempotent re-provision dedupes our Buildmesh entry rather
//      than appending duplicates.
//   5. Atomic write leaves no `.tmp` residue.
//   6. Unresolvable data dir returns `Ok(())` with no side effects.
//   7. The hook invocation POSTs stdin JSON to
//      `/api/attention/mcode` through a real localhost listener,
//      with NO reliance on inherited environment (mcode env_clears
//      `BUILDMESH_*`, so the URL is baked).
//   8. One baked invocation reaches EVERY live instance from a single
//      stdin read, including across an instance that has since exited
//      (issue #2343), and an unreachable target in ANY position — last
//      included — still leaves the hook exiting 0.
// -----------------------------------------------------------------

fn plugin_manifest_path(home: &Path) -> std::path::PathBuf {
    home.join("plugins")
        .join(MCODE_PLUGIN_DIR)
        .join(".claude-plugin")
        .join("plugin.json")
}

/// Drive `provision_attention_hooks` against a temporary directory
/// masquerading as the mcode data dir, returning the resolved
/// `.claude-plugin/plugin.json` path so the test can inspect the JSON.
fn provision_test_home(home: &Path, env_type: EnvType) -> std::path::PathBuf {
    let path = home.to_string_lossy().to_string();
    MCODE
        .provision_attention_hooks(
            &ResolvedPath {
                host_path: path.clone(),
                spawn_path: path,
                raw_path: home.to_string_lossy().to_string(),
                env_type,
            },
            &LaunchRuntime {
                harness_home: Some(home.to_string_lossy().to_string()),
                wsl_distro: None,
            },
            7,
        )
        .expect("provision_attention_hooks should succeed");
    plugin_manifest_path(home)
}

/// The executable entry under one event's first handler group.
fn first_executable(value: &serde_json::Value, event: &str) -> serde_json::Value {
    value["hooks"][event][0]["hooks"][0].clone()
}

/// The args of one handler group's first executable, joined for matching.
fn joined_args(entry: &serde_json::Value) -> String {
    entry["args"]
        .as_array()
        .map(|args| {
            args.iter()
                .filter_map(|a| a.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

#[test]
fn provision_fresh_install_writes_manifest_with_inlined_hooks() {
    let home = tempfile::tempdir().unwrap();
    let manifest = provision_test_home(home.path(), EnvType::Windows);
    assert!(
        manifest.is_file(),
        "plugin.json was not written: {manifest:?}"
    );

    // Retain the inline manifest layout verified on 0.4.12 and 0.6.13;
    // do not emit the superseded v0.3.x document alongside it.
    let legacy = home
        .path()
        .join("plugins")
        .join(MCODE_PLUGIN_DIR)
        .join("hooks")
        .join("hooks.json");
    assert!(
        !legacy.exists(),
        "the inline plugin must not also emit a legacy hooks document: {legacy:?}"
    );

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();

    // A manifest is only loaded when it carries a legal name; the
    // reverse-domain directory name is not a legal manifest name.
    assert_eq!(value["name"].as_str(), Some(MCODE_PLUGIN_NAME));
    assert!(
        MCODE_PLUGIN_NAME
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        "mcode requires lowercase letters, digits and single hyphens: {MCODE_PLUGIN_NAME}"
    );

    for event in MCODE_PROVISIONED_EVENTS {
        let entry = first_executable(&value, event);
        assert_eq!(
            entry["type"].as_str(),
            Some("command"),
            "{event} must carry the 0.4.0+ command entry: {entry:?}"
        );
        let joined = joined_args(&entry);
        assert!(
            joined.contains(BUILDMESH_HOOK_MARKER),
            "{event} handler must POST to the attention route: {joined}"
        );
        assert!(
            joined.contains("/api/attention/mcode"),
            "{event} must use the shared callback URL: {joined}"
        );
        assert!(
            !joined.contains("BUILDMESH_PORT") && !joined.contains("BUILDMESH_SESSION_ID"),
            "{event} must not rely on inherited env; mcode env_clears \
                 BUILDMESH_*: {joined}"
        );
    }
}

/// Additive merge: a user-authored sibling handler (e.g. an audit log)
/// MUST round-trip. The Kimi / Grok precedent pins this contract — we
/// never silently clobber sibling entries.
#[test]
fn provision_preserves_user_authored_sibling_handlers() {
    let home = tempfile::tempdir().unwrap();
    let manifest = plugin_manifest_path(home.path());
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    // A pre-existing Buildmesh manifest the user has extended: a sibling
    // `Stop` handler plus an event outside our ownership (`SessionStart`).
    let user = r#"{
            "name": "buildmesh-attention",
            "hooks": {
                "Stop": [
                    {
                        "matcher": "*",
                        "hooks": [
                            { "type": "command", "command": "node", "args": ["audit.mjs"] }
                        ]
                    }
                ],
                "SessionStart": [
                    {
                        "matcher": "*",
                        "hooks": [
                            { "type": "command", "command": "node", "args": ["startup.mjs"] }
                        ]
                    }
                ]
            }
        }"#;
    std::fs::write(&manifest, user).unwrap();

    let written = provision_test_home(home.path(), EnvType::Windows);
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&written).unwrap()).unwrap();

    // The user-authored sibling `Stop` handler must survive.
    let stop = value["hooks"]["Stop"].as_array().expect("Stop array");
    assert!(
        stop.iter()
            .any(|h| h["hooks"][0]["command"].as_str() == Some("node")),
        "user-authored Stop handler must round-trip; got {stop:?}"
    );
    assert!(
        stop.iter().any(is_buildmesh_handler),
        "Buildmesh Stop handler must be installed alongside the user handler; got {stop:?}"
    );
    assert_eq!(
        stop.len(),
        2,
        "Stop must carry both the user and Buildmesh entries (additive merge); got {stop:?}"
    );

    // SessionStart must retain the user handler alongside Buildmesh capture.
    let session_start = value["hooks"]["SessionStart"]
        .as_array()
        .expect("SessionStart array");
    assert_eq!(session_start.len(), 2);
    assert!(session_start.iter().any(is_buildmesh_handler));
    assert_eq!(
        session_start[0]["hooks"][0]["args"][0].as_str(),
        Some("startup.mjs")
    );

    // `PermissionRequest` is provisioned fresh (no user entry to merge with).
    let permission = value["hooks"]["PermissionRequest"]
        .as_array()
        .expect("PermissionRequest array");
    assert_eq!(
        permission.len(),
        1,
        "PermissionRequest must carry exactly one Buildmesh entry"
    );
}

/// Re-running provision on a manifest that already carries a stale
/// Buildmesh `Stop` entry (marker matches, URL drifted) must replace it in
/// place — appending would create duplicates (issue #886 idempotency).
#[test]
fn provision_dedupes_existing_buildmesh_stop_entry() {
    let home = tempfile::tempdir().unwrap();
    let manifest = plugin_manifest_path(home.path());
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    let existing = r#"{
            "name": "buildmesh-attention",
            "hooks": {
                "Stop": [
                    {
                        "matcher": "*",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "cmd.exe",
                                "args": ["/c", "curl.exe -s http://localhost:1999/api/attention/0"]
                            }
                        ]
                    }
                ]
            }
        }"#;
    std::fs::write(&manifest, existing).unwrap();

    let written = provision_test_home(home.path(), EnvType::Windows);
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&written).unwrap()).unwrap();

    let stop = value["hooks"]["Stop"].as_array().expect("Stop array");
    assert_eq!(
        stop.len(),
        1,
        "stale Buildmesh entry must be replaced, not appended; got {stop:?}"
    );
    let joined = joined_args(&first_executable(&value, "Stop"));
    assert!(
        !joined.contains("localhost:1999"),
        "stale URL must be replaced, not preserved: {joined}"
    );
    assert!(
        joined.contains("/api/attention/mcode"),
        "replacement must carry the shared callback URL: {joined}"
    );
}

/// A shared plugin must retain the same URL across launches and resumes,
/// including standalone processes that load the manifest.
#[test]
fn provision_is_stable_across_nodes_sharing_one_data_dir() {
    let home = tempfile::tempdir().unwrap();
    let manifest = plugin_manifest_path(home.path());

    let provision = |node_id: i64| -> String {
        let path = home.path().to_string_lossy().to_string();
        MCODE
            .provision_attention_hooks(
                &ResolvedPath {
                    host_path: path.clone(),
                    spawn_path: path.clone(),
                    raw_path: path,
                    env_type: EnvType::Windows,
                },
                &LaunchRuntime {
                    harness_home: Some(home.path().to_string_lossy().to_string()),
                    wsl_distro: None,
                },
                node_id,
            )
            .expect("provision_attention_hooks should succeed");
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        joined_args(&first_executable(&value, "Stop"))
    };

    let first = provision(101);
    assert!(first.contains("/api/attention/mcode"), "{first}");
    let second = provision(202);
    assert_eq!(
        first, second,
        "another spawn cannot redirect an existing process"
    );
    assert_eq!(provision(101), first);

    // The merge never accumulates handlers across nodes.
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    for event in MCODE_PROVISIONED_EVENTS {
        assert_eq!(
            value["hooks"][*event].as_array().unwrap().len(),
            1,
            "{event} must hold exactly one Buildmesh handler no matter how many \
                 nodes have provisioned into this data dir"
        );
    }
}

/// Issue #1797 review (finding 3): the preflight must inspect the distro
/// the node actually runs in, not `wsl.exe`'s default. A bare `wsl.exe`
/// reads whatever `wsl --set-default` points at, which can be a different
/// distro with different networking (or no `wslinfo` at all).
#[test]
fn wsl_preflight_targets_the_nodes_own_distro() {
    assert_eq!(
        wsl_wslinfo_args(Some("Ubuntu-22.04")),
        vec!["-d", "Ubuntu-22.04", "--", "wslinfo", "--networking-mode"]
    );
    assert_eq!(
        wsl_wslinfo_args(Some("  Ubuntu  ")),
        vec!["-d", "Ubuntu", "--", "wslinfo", "--networking-mode"],
        "the distro name is trimmed"
    );
    // No distro resolved (or a blank one) → the default-distro form, which
    // is still the best available target.
    assert_eq!(
        wsl_wslinfo_args(None),
        vec!["--", "wslinfo", "--networking-mode"]
    );
    assert_eq!(
        wsl_wslinfo_args(Some("   ")),
        vec!["--", "wslinfo", "--networking-mode"]
    );
}

/// Issue #1797 review (finding 2): the WSL preflight is what turns a silent
/// callback black hole into an actionable provisioning failure. The accept
/// predicate is split out so it is testable without a WSL host.
#[test]
fn wsl_networking_preflight_only_accepts_mirrored() {
    assert!(wsl_networking_is_mirrored(Some("mirrored")));
    assert!(wsl_networking_is_mirrored(Some("  mirrored\n")));
    assert!(wsl_networking_is_mirrored(Some("Mirrored")));
    assert!(!wsl_networking_is_mirrored(Some("nat")));
    assert!(!wsl_networking_is_mirrored(Some("")));
    assert!(!wsl_networking_is_mirrored(None));
}

/// Non-Windows runtimes get the POSIX invocation: `sh -c`, `/dev/null`
/// redirection and `|| true` — never Windows `>nul` or `curl.exe`.
#[test]
fn provision_writes_posix_invocation_on_unix() {
    let url = "http://localhost:2992/api/attention/7".to_string();
    let (command, args) = attention_invocation(EnvType::Wsl, std::slice::from_ref(&url));
    assert_eq!(command, "sh");
    assert_eq!(args[0], "-c");
    assert!(
        args[1].contains(&url),
        "POSIX invocation must carry the baked url: {}",
        args[1]
    );
    assert!(
        args[1].contains("|| true"),
        "POSIX hook must suppress curl failures: {}",
        args[1]
    );
    assert!(
        !args[1].contains(">nul"),
        "POSIX hook must not emit Windows `>nul`: {}",
        args[1]
    );
    assert!(
        !args[1].contains("curl.exe"),
        "POSIX hook must not use curl.exe: {}",
        args[1]
    );
}

/// A malformed user-authored manifest (trailing comma, partial edit,
/// syntax error) must NOT cause provisioning to silently overwrite. The
/// function returns an `Err`, the spawn path surfaces it as a provision
/// failure, and the user's content survives intact.
#[test]
fn provision_refuses_to_overwrite_malformed_user_file() {
    let home = tempfile::tempdir().unwrap();
    let manifest = plugin_manifest_path(home.path());
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    let malformed = "{ \"hooks\": [],, }";
    std::fs::write(&manifest, malformed).unwrap();

    let path_str = home.path().to_string_lossy().to_string();
    let resolved = ResolvedPath {
        host_path: path_str.clone(),
        spawn_path: path_str.clone(),
        raw_path: path_str,
        env_type: EnvType::Windows,
    };
    let result = MCODE.provision_attention_hooks(
        &resolved,
        &LaunchRuntime {
            harness_home: Some(home.path().to_string_lossy().to_string()),
            wsl_distro: None,
        },
        0,
    );
    assert!(
        result.is_err(),
        "provision must refuse a malformed existing manifest; got {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&manifest).unwrap(),
        malformed,
        "malformed manifest content must NOT be overwritten"
    );
}

/// A valid JSON top-level that isn't an object is a misconfiguration —
/// return `Err` rather than clobbering the user's payload with `{}`.
#[test]
fn provision_refuses_top_level_array() {
    let home = tempfile::tempdir().unwrap();
    let manifest = plugin_manifest_path(home.path());
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    std::fs::write(&manifest, "[1, 2, 3]").unwrap();

    let path_str = home.path().to_string_lossy().to_string();
    let resolved = ResolvedPath {
        host_path: path_str.clone(),
        spawn_path: path_str.clone(),
        raw_path: path_str,
        env_type: EnvType::Windows,
    };
    let result = MCODE.provision_attention_hooks(
        &resolved,
        &LaunchRuntime {
            harness_home: Some(home.path().to_string_lossy().to_string()),
            wsl_distro: None,
        },
        0,
    );
    assert!(
        result.is_err(),
        "provision must refuse a top-level array; got {result:?}"
    );
    assert_eq!(std::fs::read_to_string(&manifest).unwrap(), "[1, 2, 3]");
}

/// The issue #1796 acceptance invariant: when the hook config root cannot
/// be resolved, the function returns `Ok(())` WITHOUT side effects on disk.
/// We exercise the inner `provision_at` helper directly with
/// `plugin_root = None` (the unresolvable case) and verify both the return
/// value AND the absence of any side effects in a sandboxed tempdir.
#[test]
fn provision_returns_ok_without_side_effects_when_home_unresolvable() {
    let sandbox = tempfile::tempdir().unwrap();
    let sandbox_root = sandbox.path().to_path_buf();
    let before: std::collections::BTreeSet<_> = walk_dir_files(&sandbox_root)
        .into_iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    let result = provision_at(None, EnvType::Windows, 7);
    assert!(
        result.is_ok(),
        "unresolvable hook config root must return Ok(()) per issue #1796; got {result:?}"
    );

    let after: std::collections::BTreeSet<_> = walk_dir_files(&sandbox_root)
        .into_iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    assert_eq!(
        after, before,
        "provision_at(plugin_root = None) must NOT touch the filesystem; \
             the sandbox has changed: before={before:?}, after={after:?}"
    );

    for forbidden in [
        "minimax",
        "plugins",
        "io.buildmesh.attention",
        "plugin.json",
    ] {
        let found = walk_dir_files(&sandbox_root)
            .into_iter()
            .any(|p| p.to_string_lossy().contains(forbidden));
        assert!(
            !found,
            "provision_at(None) must NOT create a `{forbidden}` artifact; walked: {:?}",
            walk_dir_files(&sandbox_root)
        );
    }
}

/// Recursively walk `root` and collect every file path under it.
fn walk_dir_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    out.push(path);
                } else if path.is_dir() {
                    visit(&path, out);
                }
            }
        }
    }
    visit(root, &mut out);
    out
}

/// Atomic write leaves no `.tmp` residue beside the manifest.
#[test]
fn provision_atomic_write_leaves_no_tmp_residue() {
    let home = tempfile::tempdir().unwrap();
    provision_test_home(home.path(), EnvType::Windows);

    let manifest_dir = plugin_manifest_path(home.path())
        .parent()
        .expect("manifest dir")
        .to_path_buf();
    let entries = std::fs::read_dir(&manifest_dir).unwrap();
    let tmp_files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(
        tmp_files.is_empty(),
        "atomic write must not leave .tmp residue; found {tmp_files:?}"
    );
}

// ---- Full Access pin ---------------------------------------------------

/// The `config.yaml` a temp data dir is expected to hold.
fn config_path(home: &Path) -> PathBuf {
    home.join(MCODE_CONFIG_FILE)
}

/// Write `body` as the data dir's `config.yaml`.
fn seed_config(home: &Path, body: &str) {
    std::fs::create_dir_all(home).unwrap();
    std::fs::write(config_path(home), body).unwrap();
}

/// A representative real-world `config.yaml`: a leading comment, a nested
/// provider map holding the API key and a model catalog, and a trailing
/// comment. Every byte must survive the pin except the one line we touch.
const REALISTIC_CONFIG: &str = "\
logLevel: info
# managed by the MiniMax installer
provider:
  minimax:
    name: MiniMax
    options:
      apiKey: sk-xxx
      baseURL: https://agent.minimax.io/mavis/api/v1/llm/v1
    models:
      - id: MiniMax-M3
        thinking:
          effort: xhigh
defaultModel: minimax/MiniMax-M3 # the one we default to
defaultModelThinking:
  effort: xhigh
";

#[test]
fn permission_mode_constants_name_a_value_mcode_accepts() {
    // mcode validates the key against ["default", "bypassPermissions",
    // "auto", "off"] and silently discards anything else, falling back to
    // "auto" — the exact mode this pin exists to escape.
    assert_eq!(MCODE_PERMISSION_MODE_KEY, "permissionMode");
    assert_eq!(MCODE_PERMISSION_MODE_VALUE, "bypassPermissions");
    assert_eq!(MCODE_CONFIG_FILE, "config.yaml");
}

#[test]
fn pin_appends_the_key_when_the_config_has_none() {
    let home = tempfile::tempdir().unwrap();
    seed_config(home.path(), REALISTIC_CONFIG);

    assert_eq!(pin_permission_mode(home.path()), Ok(true));

    let after = std::fs::read_to_string(config_path(home.path())).unwrap();
    assert!(
        after.starts_with(REALISTIC_CONFIG),
        "every pre-existing byte must survive the append; got:\n{after}"
    );
    assert!(
        after.contains("permissionMode: bypassPermissions\n"),
        "the key must be appended; got:\n{after}"
    );
}

#[test]
fn pin_overwrites_auto_with_full_access() {
    let home = tempfile::tempdir().unwrap();
    seed_config(home.path(), "logLevel: info\npermissionMode: auto\n");

    assert_eq!(pin_permission_mode(home.path()), Ok(true));

    let after = std::fs::read_to_string(config_path(home.path())).unwrap();
    assert_eq!(after, "logLevel: info\npermissionMode: bypassPermissions\n");
}

#[test]
fn pin_overwrites_every_other_mode() {
    // mcode accepts four values; all three non-Full-Access ones must be
    // replaced, or the fix silently no-ops for someone on "ask".
    for mode in ["default", "auto", "off"] {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), &format!("permissionMode: {mode}\n"));
        assert_eq!(pin_permission_mode(home.path()), Ok(true), "mode {mode}");
        let after = std::fs::read_to_string(config_path(home.path())).unwrap();
        assert_eq!(after, "permissionMode: bypassPermissions\n", "mode {mode}");
    }
}

#[test]
fn pin_is_idempotent_and_leaves_the_file_byte_identical() {
    let home = tempfile::tempdir().unwrap();
    seed_config(home.path(), "permissionMode: bypassPermissions\n");
    let before = std::fs::read_to_string(config_path(home.path())).unwrap();

    assert_eq!(pin_permission_mode(home.path()), Ok(false));
    assert_eq!(pin_permission_mode(home.path()), Ok(false));

    let after = std::fs::read_to_string(config_path(home.path())).unwrap();
    assert_eq!(after, before, "a second pin must be a no-op");
}

#[test]
fn pin_tolerates_padded_and_commented_pinned_values() {
    let home = tempfile::tempdir().unwrap();
    seed_config(home.path(), "permissionMode:  bypassPermissions  \n");

    assert_eq!(
        pin_permission_mode(home.path()),
        Ok(false),
        "extra whitespace around an already-correct value is still correct"
    );
}

#[test]
fn pin_preserves_a_trailing_comment_on_the_key_line() {
    let home = tempfile::tempdir().unwrap();
    seed_config(
        home.path(),
        "permissionMode: ask  # deliberately strict\nlogLevel: info\n",
    );

    assert_eq!(pin_permission_mode(home.path()), Ok(true));

    let after = std::fs::read_to_string(config_path(home.path())).unwrap();
    assert_eq!(
        after, "permissionMode: bypassPermissions  # deliberately strict\nlogLevel: info\n",
        "the user's own comment and its alignment must not be discarded"
    );
}

#[test]
fn pin_leaves_every_other_line_untouched() {
    let home = tempfile::tempdir().unwrap();
    let original =
        REALISTIC_CONFIG.replace("logLevel: info\n", "logLevel: info\npermissionMode: auto\n");
    seed_config(home.path(), &original);

    assert_eq!(pin_permission_mode(home.path()), Ok(true));

    let after = std::fs::read_to_string(config_path(home.path())).unwrap();
    assert_eq!(
        after,
        original.replace("permissionMode: auto", "permissionMode: bypassPermissions"),
        "the API key, model catalog and comments must round-trip"
    );
}

#[test]
fn pin_ignores_an_indented_nested_permission_mode() {
    // Only the top-level key selects the mode. An indented one belongs to
    // some other mapping; rewriting it would corrupt an unrelated part of
    // the user's config.
    let home = tempfile::tempdir().unwrap();
    seed_config(
        home.path(),
        "provider:\n  minimax:\n    permissionMode: ask\n",
    );

    assert_eq!(pin_permission_mode(home.path()), Ok(true));

    let after = std::fs::read_to_string(config_path(home.path())).unwrap();
    assert!(
        after.contains("    permissionMode: ask\n"),
        "the nested key must be untouched; got:\n{after}"
    );
    assert!(
        after.ends_with("permissionMode: bypassPermissions\n"),
        "the top-level key must be appended; got:\n{after}"
    );
}

#[test]
fn pin_does_not_match_a_key_that_merely_starts_the_same() {
    let home = tempfile::tempdir().unwrap();
    seed_config(
        home.path(),
        "permissionModeOverride: keep\npermissionModes: keep\n",
    );

    assert_eq!(pin_permission_mode(home.path()), Ok(true));

    let after = std::fs::read_to_string(config_path(home.path())).unwrap();
    assert!(
        after.contains("permissionModeOverride: keep\n"),
        "a longer sibling key must not be rewritten; got:\n{after}"
    );
    assert!(
        after.contains("permissionModes: keep\n"),
        "a longer sibling key must not be rewritten; got:\n{after}"
    );
}

#[test]
fn pin_does_not_match_a_commented_out_key() {
    let home = tempfile::tempdir().unwrap();
    seed_config(home.path(), "# permissionMode: ask\n");

    assert_eq!(pin_permission_mode(home.path()), Ok(true));

    let after = std::fs::read_to_string(config_path(home.path())).unwrap();
    assert!(
        after.contains("# permissionMode: ask\n"),
        "a comment is not a key; got:\n{after}"
    );
}

#[test]
fn pin_appends_correctly_when_the_file_has_no_trailing_newline() {
    let home = tempfile::tempdir().unwrap();
    seed_config(home.path(), "logLevel: info");

    assert_eq!(pin_permission_mode(home.path()), Ok(true));

    let after = std::fs::read_to_string(config_path(home.path())).unwrap();
    assert_eq!(
        after, "logLevel: info\n\npermissionMode: bypassPermissions\n",
        "a missing final newline must not glue the key onto the last value"
    );
}

#[test]
fn pin_preserves_crlf_line_endings() {
    // A Windows user who hand-edited the file must not get it silently
    // half-converted to LF.
    let home = tempfile::tempdir().unwrap();
    seed_config(home.path(), "logLevel: info\r\npermissionMode: auto\r\n");

    assert_eq!(pin_permission_mode(home.path()), Ok(true));

    let after = std::fs::read_to_string(config_path(home.path())).unwrap();
    assert_eq!(
        after,
        "logLevel: info\r\npermissionMode: bypassPermissions\r\n"
    );
    assert!(
        !after.replace("\r\n", "").contains('\n'),
        "no bare LF may be introduced; got {after:?}"
    );
}

#[test]
fn pin_never_fabricates_a_config_file_that_does_not_exist() {
    // mcode bootstraps its own config on first run and *skips* that
    // bootstrap when the file already exists. Writing a file holding only
    // `permissionMode` would suppress the bootstrap and leave the CLI with
    // no `provider` block at all — a worse failure than running in "auto".
    // So an absent config is left absent.
    let home = tempfile::tempdir().unwrap();

    assert_eq!(pin_permission_mode(home.path()), Ok(false));
    assert_eq!(pin_permission_mode(home.path()), Ok(false));
    assert!(
        !config_path(home.path()).exists(),
        "no config.yaml may be created; the dir now holds {:?}",
        walk_dir_files(home.path())
    );
}

#[test]
#[cfg(unix)]
fn pin_reports_an_unreadable_config_rather_than_overwriting_it() {
    // A config we cannot read is a config we must not clobber. `chmod 000`
    // only denies the owner on Unix, and Windows has no equivalent, hence
    // the compile-time gate.
    let home = tempfile::tempdir().unwrap();
    seed_config(home.path(), REALISTIC_CONFIG);
    std::fs::set_permissions(
        config_path(home.path()),
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();

    let result = pin_permission_mode(home.path());
    std::fs::set_permissions(
        config_path(home.path()),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();

    assert!(
        result.is_err(),
        "an unreadable config must surface an error, not Ok(false); got {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(config_path(home.path())).unwrap(),
        REALISTIC_CONFIG,
        "the unreadable file must be left byte-identical"
    );
}

#[test]
fn pin_leaves_no_tmp_residue() {
    let home = tempfile::tempdir().unwrap();
    seed_config(home.path(), REALISTIC_CONFIG);
    pin_permission_mode(home.path()).unwrap();

    let tmp_files: Vec<_> = std::fs::read_dir(home.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(
        tmp_files.is_empty(),
        "atomic write must not leave .tmp residue; found {tmp_files:?}"
    );
}

/// The pin runs on every spawn next to the attention provisioner, so the
/// two must coexist in one data dir.
#[test]
fn provision_attention_hooks_pins_full_access_and_writes_the_plugin() {
    let home = tempfile::tempdir().unwrap();
    seed_config(home.path(), REALISTIC_CONFIG);

    let manifest = provision_test_home(home.path(), EnvType::Windows);

    assert!(
        manifest.exists(),
        "the attention plugin must still be provisioned alongside the pin"
    );
    let after = std::fs::read_to_string(config_path(home.path())).unwrap();
    assert!(
        after.contains("permissionMode: bypassPermissions"),
        "a normal spawn must pin Full Access; got:\n{after}"
    );
}

#[test]
#[cfg(unix)]
fn provision_attention_hooks_still_provisions_when_the_config_is_unreadable() {
    let home = tempfile::tempdir().unwrap();
    seed_config(home.path(), REALISTIC_CONFIG);
    std::fs::set_permissions(
        config_path(home.path()),
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();

    let path = home.path().to_string_lossy().to_string();
    let result = MCODE.provision_attention_hooks(
        &ResolvedPath {
            host_path: path.clone(),
            spawn_path: path.clone(),
            raw_path: path.clone(),
            env_type: EnvType::Windows,
        },
        &LaunchRuntime {
            harness_home: Some(path),
            wsl_distro: None,
        },
        7,
    );
    std::fs::set_permissions(
        config_path(home.path()),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();

    assert!(
        result.is_ok(),
        "an unpinnable config must not fail the spawn; got {result:?}"
    );
    assert!(
        plugin_manifest_path(home.path()).exists(),
        "the attention plugin must be provisioned regardless"
    );
}

/// Pin the descriptor-related constants so a refactor that flips the values
/// trips the test before the wire shape drifts.
#[test]
fn mcode_hook_constants_are_pinned() {
    assert_eq!(MCODE_MIN_HOOK_VERSION, "0.4.12");
    assert!(
        !MCODE_MIN_HOOK_VERSION.contains(".."),
        "version must not contain '..' (would silently match anything): {MCODE_MIN_HOOK_VERSION}"
    );
    assert!(
        !MCODE_PLUGIN_DIR.is_empty(),
        "plugin dir name must be non-empty: {MCODE_PLUGIN_DIR:?}"
    );
    assert_eq!(
        MCODE_PROVISIONED_EVENTS,
        &[
            "SessionStart",
            "UserPromptSubmit",
            "Stop",
            "PermissionRequest"
        ],
        "SessionStart captures identity, UserPromptSubmit resumes work, and Stop completes a turn"
    );
}

/// Issue #1797: `requires_attention_hook` is `true` — live mcode 0.4.12
/// testing validated the Stop event shape on the historical per-node URL.
/// The current shared URL remains unvalidated in the running app. Reverting
/// this without new evidence would re-close the Autopilot gate.
#[test]
fn requires_attention_hook_is_enabled_after_tui_validation() {
    assert!(
        MCODE.requires_attention_hook(),
        "issue #1797 validated historical Stop delivery against a live 0.4.12 TUI; \
             reverting to false would close the Autopilot gate without cause"
    );
}

/// The plugin directory is only loaded when it carries a manifest at the
/// Claude-compatible location; mcode silently skips a bare directory (the
/// issue #1797 root cause). Pin the path so a refactor back to the ignored
/// v0.3.x `hooks/hooks.json` trips here.
#[test]
fn manifest_path_is_the_claude_plugin_location() {
    let manifest = plugin_manifest_path(Path::new("/tmp/mcode-home"));
    assert_eq!(
        manifest.file_name().and_then(|n| n.to_str()),
        Some("plugin.json")
    );
    assert_eq!(
        manifest
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str()),
        Some(".claude-plugin"),
        "mcode 0.4.0+ reads `.claude-plugin/plugin.json`; got {manifest:?}"
    );
    assert!(
        manifest.to_string_lossy().contains(MCODE_PLUGIN_DIR),
        "manifest must live under our plugin dir: {manifest:?}"
    );
}

/// The Windows invocation wraps the baked curl line in `cmd.exe /c` (mcode
/// applies no shell interpretation of its own). Non-Windows hosts must never
/// emit the cmd-shape line.
#[test]
fn attention_invocation_matches_host_platform() {
    let url = "http://localhost:1992/api/attention/42".to_string();
    let (command, args) = attention_invocation(EnvType::Windows, std::slice::from_ref(&url));
    if cfg!(target_os = "windows") {
        assert_eq!(command, "cmd.exe");
        assert_eq!(args[0], "/c");
        assert!(
            args[1].contains(&url),
            "Windows invocation must carry the url: {}",
            args[1]
        );
        assert!(
            args[1].contains("curl.exe"),
            "Windows invocation must use curl.exe: {}",
            args[1]
        );
        assert!(
            args[1].contains(">nul"),
            "Windows invocation must redirect to nul: {}",
            args[1]
        );
        // The redirection does not touch the exit code; without this the
        // hook fails whenever a baked target is unreachable.
        assert!(
            args[1].ends_with("& exit 0"),
            "Windows invocation must fail open: {}",
            args[1]
        );
    } else {
        assert_eq!(command, "sh", "non-Windows hosts must not emit cmd.exe");
        assert!(
            !args[1].contains(">nul"),
            "non-Windows hosts must NOT emit Windows `>nul` (creates literal files): {}",
            args[1]
        );
    }
}

/// The one user-global manifest names **every** live instance, not just the
/// one that provisioned it. Both URLs follow a single `--data-binary @-`,
/// so the stdin body is read once and POSTed to each — the shape issue
/// #2343 needs, since the owner is decided by the payload, not by the port.
#[test]
fn every_instance_url_follows_the_single_stdin_read() {
    let urls = vec![
        "http://localhost:1992/api/attention/mcode".to_string(),
        "http://localhost:2992/api/attention/mcode".to_string(),
        "http://localhost:4992/api/attention/mcode".to_string(),
    ];
    let (_, args) = attention_invocation(EnvType::Wsl, &urls);
    let line = &args[1];
    assert_eq!(
        line.matches("--data-binary @-").count(),
        1,
        "one stdin read must serve every URL: {line}"
    );
    for url in &urls {
        assert!(line.contains(url.as_str()), "missing {url} in {line}");
    }
    // Order is preserved, so the invocation stays byte-stable across
    // re-provisions (the idempotency invariant a sibling instance relies on).
    let positions: Vec<usize> = urls
        .iter()
        .map(|url| line.find(url.as_str()).expect("url present"))
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{line}");
}

/// Accept one HTTP request and hand back its request head and body, standing in
/// for the attention route of one running Buildmesh instance.
fn serve_one_hook() -> (u16, std::thread::JoinHandle<(String, Vec<u8>)>) {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("mcode hook did not reach listener: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut headers = Vec::new();
        let mut byte = [0];
        while !headers.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
        }
        let headers = String::from_utf8(headers).unwrap();
        let length: usize = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|value| value.trim().parse().unwrap())
            })
            .unwrap();
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        // Even a nonempty successful response must not leak into the mcode
        // hook protocol.
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
            .unwrap();
        (headers, body)
    });
    (port, server)
}

/// Run the provisioned invocation exactly as mcode's hook runner does:
/// `command` + `args` with no shell interpretation and no `BUILDMESH_*` in
/// the environment, and `payload` on stdin.
fn run_attention_invocation(urls: &[String], payload: &[u8]) -> std::process::Output {
    use std::io::{Seek, Write};
    use std::time::Duration;

    let env_type = if cfg!(target_os = "windows") {
        EnvType::Windows
    } else {
        EnvType::Wsl
    };
    let (command, args) = attention_invocation(env_type, urls);

    let mut input = tempfile::tempfile().unwrap();
    input.write_all(payload).unwrap();
    input.rewind().unwrap();

    let mut invocation = crate::process_util::command_no_window(&command);
    invocation.args(&args);
    invocation
        // Deliberately do NOT provide BUILDMESH_*: the URL is baked, so the
        // callback must work under mcode's env_clear().
        .env_remove("BUILDMESH_PORT")
        .env_remove("BUILDMESH_SESSION_ID")
        .env_remove("BUILDMESH_WSL_HOST")
        .env("NO_PROXY", "localhost,127.0.0.1")
        .env("no_proxy", "localhost,127.0.0.1")
        .stdin(std::process::Stdio::from(input));
    crate::process_util::run_command_with_timeout(
        invocation,
        "mcode attention invocation",
        Duration::from_secs(10),
    )
    .unwrap()
}

/// Provisioning contract: the hook invocation targets the currently
/// configured `/api/attention/mcode` URL with stdin JSON as the body and
/// does not rely on inherited `BUILDMESH_*` environment. This local test
/// verifies the generated command against a listener; it does not validate
/// native delivery from the installed mcode TUI.
#[test]
fn attention_invocation_delivers_stdin_to_attention_route_without_env() {
    let (port, server) = serve_one_hook();

    // Bake the listener's ephemeral port into the URL the way the
    // provisioner bakes the live Buildmesh port.
    let url = vec![format!("http://localhost:{port}/api/attention/741")];
    let payload = br#"{"hook_event_name":"Stop","session_id":"8a979720-1cb0-408c-b29c-9f0f68f2982b","transcript_path":"/tmp/x.jsonl","permission_mode":"auto","message":"literal $HOME & %PATH%"}"#;
    let output = run_attention_invocation(&url, payload);

    let (headers, body) = server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "hook stdout: {:?}", output.stdout);
    assert!(
        headers.starts_with("POST /api/attention/741 HTTP/1.1\r\n"),
        "{headers}"
    );
    // Stop and PermissionRequest share the same invocation shape — assert
    // payload integrity so a regression that re-encodes or filters the body
    // trips here.
    assert_eq!(body, payload);
}

/// Issue #2343 acceptance, at the only layer a unit test can reach: one
/// manifest, one stdin body, several instances. A MiniMax node in any
/// instance posts the same payload to every named port, so the instance
/// that owns the node accepts it whichever instance spawned last. A port
/// that no longer answers (an instance that exited after the manifest was
/// written) sits between the two live ones and must not starve them.
#[test]
fn attention_invocation_reaches_every_live_instance() {
    let (hub_port, hub) = serve_one_hook();
    let (dev_port, dev) = serve_one_hook();
    let exited = closed_port();
    let urls = vec![
        format!("http://localhost:{hub_port}/api/attention/mcode"),
        format!("http://localhost:{exited}/api/attention/mcode"),
        format!("http://localhost:{dev_port}/api/attention/mcode"),
    ];
    let payload = br#"{"hook_event_name":"Stop","session_id":"8a979720-1cb0-408c-b29c-9f0f68f2982b","transcript_path":"/tmp/x.jsonl","permission_mode":"auto"}"#;
    let output = run_attention_invocation(&urls, payload);

    let (hub_request, dev_request) = (hub.join().unwrap(), dev.join().unwrap());
    assert!(
        output.status.success(),
        "a dead peer must not fail the hook: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for (instance, (headers, body)) in [("hub", &hub_request), ("dev", &dev_request)] {
        assert!(
            headers.starts_with("POST /api/attention/mcode HTTP/1.1\r\n"),
            "{instance} instance saw {headers}"
        );
        assert_eq!(body, payload, "{instance} instance got the wrong body");
    }
}

/// Issue #2343 review: the exit code is the one thing this hook must never
/// leak, and it is order-dependent. `curl` reports the **last** transfer's
/// status, and `cmd.exe /c` reports curl's, so an unreachable target placed
/// last is the case that decides whether a dead sibling fails the hook.
/// Each ordering below pins that, including the all-targets-gone one, which
/// is what a node sees after every instance has exited.
#[test]
fn an_unreachable_target_never_fails_the_hook() {
    let payload = br#"{"hook_event_name":"Stop","session_id":"8a979720-1cb0-408c-b29c-9f0f68f2982b","transcript_path":"/tmp/x.jsonl","permission_mode":"auto"}"#;

    // Dead target last: the failing transfer is also the final one.
    let (live_port, live) = serve_one_hook();
    let dead = closed_port();
    let output = run_attention_invocation(
        &[
            format!("http://localhost:{live_port}/api/attention/mcode"),
            format!("http://localhost:{dead}/api/attention/mcode"),
        ],
        payload,
    );
    live.join().expect("the live instance must still be served");
    assert!(
        output.status.success(),
        "a dead last target must not fail the hook: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Nothing left to reach at all.
    let output = run_attention_invocation(
        &[
            format!("http://localhost:{}/api/attention/mcode", closed_port()),
            format!("http://localhost:{}/api/attention/mcode", closed_port()),
        ],
        payload,
    );
    assert!(
        output.status.success(),
        "a hook with no reachable instance must still exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A port nothing is listening on, for the case above: bind to get a free
/// port, then release it.
fn closed_port() -> u16 {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    drop(taken);
    port
}
