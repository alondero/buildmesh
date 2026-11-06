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
    // Issue #1704: Muse's effort flag is `--reasoning-effort` and has no
    // `--effort` alias, so the recipe shape is adapter-owned — pinned
    // alongside the flag-followed-by-value recipe pin below.
    assert_eq!(MUSE.effort_args("high"), ["--reasoning-effort", "high"]);
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
    for platform in MUSE.descriptor().available_on {
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
        // Issue #2151: bare base recipe — `--disable-approval` +
        // `--disable-sandbox` (approval policy #1705; sandbox off so
        // the agent shell reaches the OS credential keyring, #1788)
        // come from the effective permission mode in
        // `default_prepare`, never from a hidden argv.
        assert!(
            recipe.base_args.is_empty(),
            "muse base recipe must be bare on {platform:?}; got {:?}",
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

/// Issue #1704 — recipe pin. When the resolver forwards an effort value,
/// `default_prepare` must append `--reasoning-effort <level>` to the
/// recipe, and must never append the trait default's `--effort`: Muse
/// documents no such alias, so the wrong flag turns a silently-ignored
/// effort setting into a launch the CLI rejects outright. The table-driven
/// `spawn::command_tests::capability_recipe_coherence` covers the
/// forwarding for every adapter; this focused pin makes the Muse shape —
/// including the flag that must not appear — explicit.
#[test]
fn muse_recipe_appends_reasoning_effort_arg_when_resolved() {
    use crate::agent::capabilities::ResolvedAgentConfig;
    use crate::agent::launch::{
        assert_flag_followed_by_value, HarnessLaunchInput, SessionIdModeRef,
    };

    // The advertised vocabulary and the shared constant must agree, so a
    // future refactor that moves the vocabulary cannot desync the resolver
    // mask from the UI options.
    let caps = crate::agent::capabilities::capabilities_for(&MUSE);
    let advertised: Vec<String> = match &caps.effort_control {
        EffortControlKind::Closed { allowed } => allowed.clone(),
        other => panic!("expected Closed, got {other:?}"),
    };
    let expected: Vec<String> = MUSE_EFFORT_ALLOWED.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        advertised, expected,
        "Muse effort vocabulary must be exactly MUSE_EFFORT_ALLOWED"
    );

    // `ultra` is the level that proves the eight-value vocabulary reached
    // the recipe rather than a shorter one copied from a sibling harness.
    let config = ResolvedAgentConfig {
        model: None,
        effort: Some("ultra".into()),
        extra_args: None,
        permission_mode: None,
    };
    let input = HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Linux,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::None,
        config: &config,
        prefill: None,
        sandbox: false,
    };
    let prepared = MUSE.prepare_launch(input);
    assert_flag_followed_by_value(&prepared.recipe.base_args, "--reasoning-effort", "ultra");
    assert!(
        !prepared.recipe.base_args.iter().any(|a| a == "--effort"),
        "Muse has no `--effort` alias — the trait default must never reach \
             the child argv; got {:?}",
        prepared.recipe.base_args
    );

    // Without an effort layer the flag is absent: the recipe carries only
    // what the effective permission mode contributes. The exact
    // `--disable-approval --disable-sandbox` argv of a no-effort launch is
    // already pinned exactly by
    // `default_prepare_fresh_launch_carries_disable_approval_with_model_and_prefill`,
    // so this assertion stays scoped to the effort flag.
    let without_effort = MUSE.prepare_launch(HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Linux,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::None,
        config: &ResolvedAgentConfig::default(),
        prefill: None,
        sandbox: false,
    });
    assert!(
        !without_effort
            .recipe
            .base_args
            .iter()
            .any(|a| a == "--reasoning-effort"),
        "no resolved effort must mean no --reasoning-effort; got {:?}",
        without_effort.recipe.base_args
    );
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
    let platforms = MUSE.descriptor().available_on;
    assert_eq!(
        platforms.len(),
        3,
        "available_on should pin to exactly {{Windows, Linux, Macos}} — got {:?}",
        platforms
    );
    assert!(
        platforms.contains(&Platform::Windows),
        "muse is available on Windows since 1.3.0; got {:?}",
        platforms
    );
    assert!(platforms.contains(&Platform::Linux));
    assert!(platforms.contains(&Platform::Macos));
}

// -- Prepared-launch evidence (issue #1705 round-1 review) ------------
//
// The per-platform pin above proves `spawn_recipe()` itself stays bare
// (issue #2151: no hidden unattended argv); it does not prove the
// effective permission mode lands in the final argv. The two tests
// below route fresh + resume launches through the real orchestration
// seam (`agent::launch::default_prepare`) so the permission-layer
// flags are proven to land in the final argv alongside the model
// override, the prefill text, and the resume id, in the documented
// order. Without these, a future refactor that reorders the layers
// (e.g. prepending `--model` before `--disable-approval`) would slip
// past the per-platform pin but break a real spawn.
//
// Mirrors OpenCode's `fresh_recipe_forwards_model_and_prompt_without_session_id`
// and `resume_recipe_carries_session_flag` — the engineering contract
// (`docs/agents/engineering.md`) requires testing fresh AND resume paths
// for changed launch recipes.

/// Issue #1705 + #1788 fresh launch, via the #2151 permission layer:
/// the unattended default's `--disable-approval` + `--disable-sandbox`
/// must land ahead of the model override and the prefill text in the
/// final argv. Pin the exact `base_args` vector so a future reorder
/// that pushes `--disable-approval` past `--model` (or drops either
/// flag during layer composition) trips here, not in production.
#[test]
fn default_prepare_fresh_launch_carries_disable_approval_with_model_and_prefill() {
    use crate::agent::capabilities::ResolvedAgentConfig;
    use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};

    let config = ResolvedAgentConfig {
        model: Some("claude-sonnet-4-5".to_string()),
        effort: None,
        extra_args: None,
        permission_mode: None,
    };
    let input = HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Linux,
        runtime: EnvType::Wsl,
        session: SessionIdModeRef::None,
        config: &config,
        prefill: Some("fix the auth bug"),
        sandbox: false,
    };
    let prepared = MUSE.prepare_launch(input);
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
        !prepared
            .recipe
            .base_args
            .iter()
            .any(|a| a == "--session" || a == "--session-id" || a == "--prefill" || a == "--yolo"),
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
    use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};

    let config = ResolvedAgentConfig::default();
    let input = HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Linux,
        runtime: EnvType::Wsl,
        session: SessionIdModeRef::Resume {
            id: "12345678-1234-4234-8234-123456789abc",
            cause: crate::agent::spawn::ResumeCause::Explicit,
        },
        config: &config,
        prefill: None,
        sandbox: false,
    };
    let prepared = MUSE.prepare_launch(input);
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

/// Issue #1710 — the never-emit pin for every flag outside the baked set.
///
/// `extra_args` is the documented escape hatch and is deliberately **not**
/// filtered here: a mesh may legitimately need a flag this table has not
/// seen, and #1358 keeps that path verbatim. What this pins is the argv
/// Buildmesh composes *itself* — `spawn_recipe` plus the model, prefill
/// and session layers of `default_prepare` — across every supported
/// platform and **all three** session modes. A future "while we're here"
/// flag addition, or a config field that renders one, trips here instead
/// of shipping a harness-owned worktree (ADR-0003) or an `exec`-only flag
/// that makes the CLI reject the launch before a TUI ever opens.
///
/// `Assign` is the mode that earns its place in this list: it is the one
/// that reaches [`AgentProvider::session_assign_args`], whose trait
/// default emits `--session-id <uuid>`. That flag is `exec`-only, so an
/// adapter that inherited the default would break every interactive
/// launch routed through `Assign`. Fresh and `Resume` never touch it.
///
/// The list spans every non-modeled verdict in the #1710 table —
/// never-pass, passthrough-only and deferred alike, because for argv
/// composition they behave identically: Buildmesh does not write them.
///
/// Deliberately **absent**: the flags owned by sibling issues
/// (`--approval-mode` / `--permission-profile` /
/// `--approval-judge` #1705, `--trust-workspace` #1706, the
/// `--sandbox-network` / `--disable-write` / `--disable-shell` /
/// `--enable-shell-tool` group #1707/#1788, and `--no-session-log`'s
/// watcher consequences #1709). Those verdicts win on overlap and may
/// legitimately start being emitted, so pinning them here would only
/// obstruct the sibling work. `--reasoning-effort` (#1704) used to sit in
/// that group and has since graduated: Buildmesh now emits it as
/// `--reasoning-effort <level>` from resolved configuration, so it is not
/// a declined flag and would fail this assertion if listed. Its emitted
/// form is pinned by `muse_recipe_appends_reasoning_effort_arg_when_resolved`
/// instead. The lists mirror the tables in
/// `docs/learning/muse-harness-capabilities.md`, verified against the
/// installed 1.3.0 `muse --help` / `muse exec --help`.
#[test]
fn composed_argv_never_carries_a_flag_buildmesh_declined_to_model() {
    use crate::agent::capabilities::ResolvedAgentConfig;
    use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};

    /// Flags Buildmesh must never place in a composed Muse argv. Matched on
    /// the flag name, so both `--flag value` and `--flag=value` are caught.
    const DECLINED: &[&str] = &[
        // -- never-pass (the #1710 verdict table) --
        "-w",
        "--worktree",
        "--worktree-base",
        "--worktree-existing",
        "--workspace",
        "--provider",
        "--base-url",
        "--subagent-worktree-isolation",
        "--no-session-log",
        // -- passthrough-only --
        "--preset",
        "--parallel-tool-calls",
        "--no-parallel-tool-calls",
        "--agents",
        // -- deferred --
        "--image",
        // Interactive-only, and meaningless without the test-double
        // provider that is itself never-pass above.
        "--echo-delay-ms",
        // -- exec-only: absent from the interactive TUI's grammar --
        "--json",
        "--prompt-file",
        "--api-key-stdin",
        "--output-schema",
        "--max-model-steps",
        "--max-tool-output-bytes",
        "--context-compaction-strategy",
        "--context-compaction-soft-threshold",
        "--context-compaction-hard-threshold",
        "--session-id",
        "--allow-workspace-switch",
        "--user-input-auto-resolve",
        "--disable-web-tools",
        "--no-foreign-personal-context",
    ];

    let declined = |arg: &str| {
        let name = arg.split_once('=').map_or(arg, |(name, _)| name);
        DECLINED.contains(&name)
    };

    let config = ResolvedAgentConfig {
        model: Some("claude-sonnet-4-5".to_string()),
        effort: None,
        // The escape hatch itself is out of scope; the pin covers what
        // Buildmesh composes around it.
        extra_args: None,
        permission_mode: None,
    };
    let session_id = "12345678-1234-4234-8234-123456789abc";
    let modes = [
        ("fresh", SessionIdModeRef::None, Some("fix the auth bug")),
        (
            "resume",
            SessionIdModeRef::Resume {
                id: session_id,
                cause: crate::agent::spawn::ResumeCause::Explicit,
            },
            None,
        ),
        ("assign", SessionIdModeRef::Assign(session_id), None),
    ];

    let mut composed = 0usize;
    for platform in MUSE.descriptor().available_on {
        for (mode, session, prefill) in modes {
            let prepared = MUSE.prepare_launch(HarnessLaunchInput {
                routing: None,
                node_id: None,
                platform: *platform,
                runtime: env_type_for(*platform),
                session,
                config: &config,
                prefill,
                sandbox: false,
            });
            // Anti-vacuity: this pass must have composed real argv before
            // the negative assertions below can mean anything. Without it a
            // regression that emptied the recipe would turn the whole pin
            // into a silent pass.
            assert!(
                prepared
                    .recipe
                    .argv()
                    .any(|arg| arg == "--disable-approval"),
                "muse {mode} launch on {platform:?} composed no baked policy \
                     flag, so the declined-flag scan below would be vacuous; \
                     got {:?}",
                prepared.recipe.argv().collect::<Vec<_>>()
            );
            composed += 1;
            // `SpawnRecipe::argv()` is the same ordering the child sees:
            // options, then trailing positionals.
            let argv: Vec<&str> = prepared.recipe.argv().collect();
            for arg in &argv {
                assert!(
                    !declined(arg),
                    "muse {mode} launch on {platform:?} must not carry {arg:?} — \
                         issue #1710 declines that flag (see \
                         docs/learning/muse-harness-capabilities.md, \"Launch \
                         flags: explicit verdicts\"); composed argv: {argv:?}"
                );
            }
        }
    }
    assert_eq!(
        composed,
        MUSE.descriptor().available_on.len() * modes.len(),
        "the pin must cover every supported platform on all three session \
             modes; composed {composed} launches"
    );
}

/// Issue #1710 — `Assign` mode must forward nothing (the defect this
/// override closes).
///
/// The `AgentProvider` default for `session_assign_args` emits
/// `--session-id <uuid>`, and that flag exists only on `muse exec`: the
/// interactive TUI rejects it. Muse is self-assigning, so it is the one
/// adapter that must never inherit that default. This mirrors
/// `commands::agent_tests::agy_assign_omits_session_flag` and
/// `codex_assign_omits_session_flag` at the command layer, and the
/// `session_assign_args` overrides in the eight sibling self-assigning
/// adapters.
#[test]
fn assign_mode_forwards_no_session_flag() {
    assert!(
        MUSE.session_assign_args("any-id").is_empty(),
        "muse self-assigns its session ids; Assign mode must not forward \
             --session-id (exec-only flag, would reject the interactive \
             launch); got {:?}",
        MUSE.session_assign_args("any-id")
    );
    assert!(
        MUSE.self_assigns_session_id(),
        "this override is only correct while muse self-assigns; if that \
             flips, the trait default (and this test's premise) must change"
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
    let dir = root.path().join("sessions/1970/01/01").join(id);
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
    value["projects"][key]["decision"]
        .as_str()
        .map(str::to_string)
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
        assert!(!key.contains('/'), "a Windows key uses backslashes: {key}");
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
    assert_eq!(
        trust_key("Q:/buildmesh-trust-probe/nope"),
        "\\\\?\\Q:\\buildmesh-trust-probe\\nope"
    );
    assert_eq!(
        trust_key("Q:\\buildmesh-trust-probe\\nope"),
        "\\\\?\\Q:\\buildmesh-trust-probe\\nope"
    );
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
    let seeded =
        format!(r#"{{ "projects": {{ {key}: {{ "decision": "untrusted", "note": "keep" }} }} }}"#);
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
        let seeded = format!(r#"{{ "projects": {{ {key}: {{ "decision": {decision} }} }} }}"#);
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
