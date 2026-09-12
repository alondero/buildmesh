//! Data models for Buildmesh.
//!
//! Split by domain (issue #1655). `mod.rs` re-exports every type so
//! `crate::models::Mesh` / ts-rs `export_to` paths stay stable.

mod agent;
mod mesh;
mod git;
mod circuit;

pub use agent::*;
pub use mesh::*;
pub use git::*;
pub use circuit::*;

/// Re-export the wire-level Agent Harness configuration value type from
/// the private `preferences` module so [`crate::models::Mesh`] / [`MeshRow`]
/// can include it in their public type signatures without leaking the
/// `preferences` module through the public API (`preferences` stays private
/// because the rest of its surface is internal-only). The same type is
/// used by the application-level defaults map
/// (`AppPreferences.harness_defaults`), the per-Mesh overrides map
/// (`Mesh.harness_overrides`), and the spawn-config resolver
/// (`ResolvedAgentConfig`).
pub use crate::preferences::HarnessConfigValue;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn sample_mesh() -> Mesh {
        // Spread `..Default::default()` for every field this fixture doesn't
        // intentionally exercise (issue #518 follow-up to #457). Keeping only
        // the fields the consumer tests assert on: name + base_ref + the
        // `Option<T>` columns that need a non-default value, plus the two
        // scalar toggles (`use_worktree`, `sandbox`) whose values the tests
        // pin explicitly. A future `Mesh` column just needs to be added to
        // the struct — this fixture stays unchanged.
        Mesh {
            id: 1,
            name: "demo".to_string(),
            path: "/repo".to_string(),
            build_command: Some("npm run build".to_string()),
            model: Some("opus".to_string()),
            worktree_mode: Some("branched".to_string()),
            base_ref: "origin/main".to_string(),
            use_worktree: false,
            sandbox: true,
            ..Default::default()
        }
    }

    #[test]
    fn mesh_row_from_mesh_maps_all_fields() {
        let cfg = MeshRow::from(&sample_mesh());
        assert_eq!(cfg.name.as_deref(), Some("demo"));
        assert_eq!(cfg.build_command.as_deref(), Some("npm run build"));
        assert_eq!(cfg.run_command, None);
        assert_eq!(cfg.model.as_deref(), Some("opus"));
        assert_eq!(cfg.effort, None);
        // base_ref always carries a value (DB COALESCEs to 'origin/main')
        assert_eq!(cfg.base_ref.as_deref(), Some("origin/main"));
        assert!(!cfg.use_worktree);
        assert_eq!(cfg.worktree_mode.as_deref(), Some("branched"));
        assert_eq!(cfg.default_provider, None);
        assert!(cfg.sandbox, "sandbox toggle must map through MeshRow::from");
        // #802 — root_* commands are None on a mesh that never set them, so
        // the build_run resolver falls back to build_command / run_command.
        assert_eq!(cfg.root_build_command, None);
        assert_eq!(cfg.root_run_command, None);
        // Wayfinder #990 / ticket #991 — looping autopilot config mirrors
        // through MeshRow::from exactly like the other Mesh fields.
        assert_eq!(cfg.autopilot_mode, AutopilotMode::IssueDriven);
        assert_eq!(cfg.loop_initial_prompt, None);
        assert_eq!(cfg.loop_suffix_prompt, None);
        assert_eq!(cfg.loop_max_iterations, None);
        assert_eq!(cfg.loop_interval_seconds, 0);
        assert_eq!(cfg.loop_consecutive_failures, 0);
    }

    /// #802 — a mesh that DID configure per-context commands must round-trip
    /// both new columns through `MeshRow::from`.
    #[test]
    fn mesh_row_from_mesh_maps_root_commands() {
        let mut mesh = sample_mesh();
        mesh.root_build_command = Some("cargo build --workspace".to_string());
        mesh.root_run_command = Some("cargo run -p app".to_string());
        let cfg = MeshRow::from(&mesh);
        assert_eq!(cfg.root_build_command.as_deref(), Some("cargo build --workspace"));
        assert_eq!(cfg.root_run_command.as_deref(), Some("cargo run -p app"));
    }

    #[test]
    fn mesh_row_from_mesh_blank_name_is_none() {
        let mut mesh = sample_mesh();
        mesh.name = String::new();
        assert_eq!(MeshRow::from(&mesh).name, None);
    }

    /// Wayfinder #990 / ticket #991 — a mesh that DID configure looping
    /// autopilot must round-trip ALL six columns through `MeshRow::from`.
    #[test]
    fn mesh_row_from_mesh_maps_loop_config() {
        let mut mesh = sample_mesh();
        mesh.autopilot_mode = AutopilotMode::Looping;
        mesh.loop_initial_prompt = Some("iterate the planner".to_string());
        mesh.loop_suffix_prompt = Some("now write tests".to_string());
        mesh.loop_max_iterations = Some(7);
        mesh.loop_interval_seconds = 60;
        mesh.loop_consecutive_failures = 2;
        let cfg = MeshRow::from(&mesh);
        assert_eq!(cfg.autopilot_mode, AutopilotMode::Looping);
        assert_eq!(cfg.loop_initial_prompt.as_deref(), Some("iterate the planner"));
        assert_eq!(cfg.loop_suffix_prompt.as_deref(), Some("now write tests"));
        assert_eq!(cfg.loop_max_iterations, Some(7));
        assert_eq!(cfg.loop_interval_seconds, 60);
        assert_eq!(cfg.loop_consecutive_failures, 2);
    }

    /// Regression test for issue #457: `AgentNode::default()` exists so future
    /// optional columns only need to be added to the struct, not to 8 test
    /// fixtures. Defaults are chosen to match each enum's `from_db_str`
    /// fallback semantics (`EnvType::Windows`, `Provider::Anthropic`,
    /// `SessionStatus::Idle`) so an AgentNode built from `..Default::default()`
    /// is a meaningful "no row loaded" stub — not a value that would silently
    /// masquerade as a real row.
    #[test]
    fn agent_node_default_matches_fallback_semantics() {
        let n = AgentNode::default();
        assert_eq!(n.id, 0);
        assert_eq!(n.mesh_id, 0);
        assert_eq!(n.name, "");
        assert_eq!(n.path, "");
        assert_eq!(n.branch, "");
        assert_eq!(n.env, EnvType::Windows);
        // provider is now an opaque String; default is "" (resolver treats it
        // as anthropic), so the fallback semantics are preserved (issue #535).
        assert_eq!(n.provider, "");
        assert_eq!(n.status, SessionStatus::Idle);
        assert_eq!(n.cli_session_id, None);
        assert_eq!(n.worktree_name, None);
        assert!(!n.use_worktree);
        assert_eq!(n.source_issue, None);
        assert_eq!(n.source_pr, None);
        assert_eq!(n.head_repo_owner, None);
        assert_eq!(n.head_repo_clone_url, None);
        assert_eq!(n.source_pr_pinned_sha, None);
        assert_eq!(n.position, 0);
        // DateTime<Utc>::default() == UNIX epoch — not "now", but a
        // well-defined placeholder that won't accidentally match a real row.
        assert_eq!(n.created_at, chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap());
    }

    /// Companion to the above: a partially-overridden literal must compile
    /// cleanly via `..Default::default()`, which is the migration pattern the
    /// 8 fixtures will adopt. Pins the spread-syntax contract so a future
    /// `non_exhaustive` or similar can't quietly break it.
    #[test]
    fn agent_node_partial_with_default_spread_works() {
        let n = AgentNode {
            id: 7,
            path: "/tmp/fix-login".to_string(),
            ..Default::default()
        };
        assert_eq!(n.id, 7);
        assert_eq!(n.path, "/tmp/fix-login");
        // Untouched fields keep their defaults.
        assert_eq!(n.env, EnvType::Windows);
        assert_eq!(n.provider, "");
        assert_eq!(n.status, SessionStatus::Idle);
    }

    /// Regression test for issue #518: `Mesh::default()` exists so future
    /// optional columns only need to be added to the struct, not to
    /// `sample_mesh()`. Follow-up to #457 (which did the same for
    /// `AgentNode`).
    ///
    /// Option A semantics (zero-value stub — see issue body): every field
    /// at its mechanical zero. There is no enum on `Mesh` to align with a
    /// `from_db_str` fallback, so the choice is uniform. A `Mesh` built
    /// from `..Default::default()` is a "no row loaded" placeholder — not
    /// a value that would silently masquerade as a real DB row.
    #[test]
    fn mesh_default_matches_fallback_semantics() {
        let m = Mesh::default();
        assert_eq!(m.id, 0);
        assert_eq!(m.name, "");
        assert_eq!(m.path, "");
        assert_eq!(m.layout, "");
        assert_eq!(m.position, 0);
        assert_eq!(
            m.created_at,
            chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap()
        );
        assert_eq!(m.build_command, None);
        assert_eq!(m.run_command, None);
        assert_eq!(m.model, None);
        assert_eq!(m.effort, None);
        assert!(!m.use_worktree);
        assert_eq!(m.worktree_mode, None);
        assert_eq!(m.default_provider, None);
        assert_eq!(m.base_ref, "");
        assert_eq!(m.scratchpad, "");
        assert!(!m.sandbox);
        assert_eq!(m.root_build_command, None);
        assert_eq!(m.root_run_command, None);
        // Wayfinder #990 / ticket #991 — looping autopilot config: zero /
        // default per Option A (issue #518), with the `#[default]` enum
        // variant on the autopilot_mode carrying its pre-v30 behaviour.
        assert_eq!(m.autopilot_mode, AutopilotMode::IssueDriven);
        assert_eq!(m.loop_initial_prompt, None);
        assert_eq!(m.loop_suffix_prompt, None);
        assert_eq!(m.loop_max_iterations, None);
        assert_eq!(m.loop_interval_seconds, 0);
        assert_eq!(m.loop_consecutive_failures, 0);
    }

    /// Companion to the above: a partially-overridden literal must compile
    /// cleanly via `..Default::default()`, which is the migration pattern
    /// `sample_mesh()` will adopt. Pins the spread-syntax contract so a
    /// future `non_exhaustive` or similar can't quietly break it.
    #[test]
    fn mesh_partial_with_default_spread_works() {
        let m = Mesh {
            id: 7,
            name: "fixture".to_string(),
            path: "/repo".to_string(),
            ..Default::default()
        };
        assert_eq!(m.id, 7);
        assert_eq!(m.name, "fixture");
        assert_eq!(m.path, "/repo");
        // Untouched fields keep their defaults.
        assert!(!m.use_worktree);
        assert!(!m.sandbox);
        assert_eq!(m.base_ref, "");
    }

    #[test]
    fn session_status_round_trip_all_variants() {
        let variants = [
            SessionStatus::Pending,
            SessionStatus::Spawning,
            SessionStatus::Running,
            SessionStatus::Idle,
            SessionStatus::AwaitingInput,
            SessionStatus::Error,
            SessionStatus::Archived,
            SessionStatus::Suspended,
        ];
        for status in variants {
            let db_str = status.to_db_str();
            let parsed = SessionStatus::from_db_str(db_str);
            assert_eq!(parsed, status, "round-trip failed for {:?}", status);
        }
    }

    #[test]
    fn session_status_serializes_to_wire_as_its_db_string() {
        // `AgentNode` is serialised straight from the serde derive on both the
        // Tauri and HTTP transports, so the JSON wire value of `status` MUST
        // equal the DB string (which the frontend also compares against).
        // `rename_all = "lowercase"` silently emitted "awaitinginput" (no
        // underscore) for the one multi-word variant — a value no consumer
        // ever matched, masked because the UI sets that status client-side.
        // Issue #359.
        let variants = [
            SessionStatus::Pending,
            SessionStatus::Spawning,
            SessionStatus::Running,
            SessionStatus::Idle,
            SessionStatus::AwaitingInput,
            SessionStatus::Error,
            SessionStatus::Archived,
            SessionStatus::Suspended,
        ];
        for status in variants {
            let wire = serde_json::to_value(status).unwrap();
            assert_eq!(
                wire,
                serde_json::Value::String(status.to_db_str().to_string()),
                "wire serialization of {status:?} must match its DB string",
            );
        }
    }

    #[test]
    fn session_status_unknown_string_defaults_to_idle() {
        assert_eq!(SessionStatus::from_db_str("garbage"), SessionStatus::Idle);
        assert_eq!(SessionStatus::from_db_str(""), SessionStatus::Idle);
        assert_eq!(SessionStatus::from_db_str("RUNNING"), SessionStatus::Idle);
    }

    // Wayfinder #990 / ticket #991 — AutopilotMode is a wire-shape mirror of
    // SessionStatus: same snake_case rename, same "DB string == wire value"
    // contract, same fail-open unknown-strings-degrade-to-default semantics.
    // The defensive tests below pin all three.

    #[test]
    fn autopilot_mode_round_trip_all_variants() {
        for &mode in [AutopilotMode::IssueDriven, AutopilotMode::Looping].iter() {
            let db_str = match mode {
                AutopilotMode::IssueDriven => "issue_driven",
                AutopilotMode::Looping => "looping",
            };
            assert_eq!(
                AutopilotMode::from_db_str(db_str),
                mode,
                "round-trip failed for {mode:?}"
            );
        }
    }

    #[test]
    fn autopilot_mode_serializes_to_wire_as_its_snake_case_string() {
        // Wire shape MUST equal the DB string the column stores, the same
        // way SessionStatus does (issue #359). The `rename_all = "snake_case"`
        // serde attribute is the bridge; without it, `Looping` would land
        // on the wire as `"Looping"` and the frontend enum-compare would
        // silently miss every Looping-mode mesh.
        for mode in [AutopilotMode::IssueDriven, AutopilotMode::Looping] {
            let wire = serde_json::to_value(mode).unwrap();
            let expected = match mode {
                AutopilotMode::IssueDriven => "issue_driven",
                AutopilotMode::Looping => "looping",
            };
            assert_eq!(
                wire,
                serde_json::Value::String(expected.to_string()),
                "wire serialization of {mode:?} must match its snake_case DB string"
            );
        }
    }

    #[test]
    fn autopilot_mode_unknown_string_defaults_to_issue_driven() {
        // Fail-open: a row written by a future build with an unknown mode
        // string degrades to IssueDriven (the pre-v30 behaviour), so the
        // poller keeps working — the alternative (degrading to Looping)
        // would silently spin up a configured-but-failed Looping mesh.
        assert_eq!(AutopilotMode::from_db_str("garbage"), AutopilotMode::IssueDriven);
        assert_eq!(AutopilotMode::from_db_str(""), AutopilotMode::IssueDriven);
        assert_eq!(AutopilotMode::from_db_str("Looping"), AutopilotMode::IssueDriven);
    }

    #[test]
    fn provider_adapter_recipe_windows() {
        use crate::agent::provider::Platform;
        // MiniMax and Kimi were retired from the legacy enum (#538) — Claude-compatible
        // endpoints are now harness profiles whose per-account env is injected separately
        // by the unified `anthropic` adapter via `claude_direct_recipe`.
        assert_eq!(Provider::Anthropic.adapter().spawn_recipe(Platform::Windows, EnvType::Windows).binary, "claude.exe");
        assert_eq!(Provider::Agy.adapter().spawn_recipe(Platform::Windows, EnvType::Windows).binary, "agy");
        assert_eq!(Provider::OpenCode.adapter().spawn_recipe(Platform::Windows, EnvType::Windows).binary, "opencode");
        assert_eq!(Provider::Codex.adapter().spawn_recipe(Platform::Windows, EnvType::Windows).binary, "codex");
        // Plain terminal spawns the OS-preferred shell directly — powershell.exe on Windows
        // host, routed through wsl.exe by spawn_environment::wrap when env_type is WSL.
        assert_eq!(Provider::Terminal.adapter().spawn_recipe(Platform::Windows, EnvType::Windows).binary, "powershell.exe");
    }

    #[test]
    fn provider_adapter_recipe_macos_anthropic_uses_claude() {
        use crate::agent::provider::Platform;
        assert_eq!(Provider::Anthropic.adapter().spawn_recipe(Platform::Macos, EnvType::Windows).binary, "claude");
    }

    #[test]
    fn provider_capabilities_split_correctly() {
        assert!(Provider::Anthropic.adapter().supports_resume());
        assert!(Provider::Agy.adapter().supports_resume());
        assert!(Provider::OpenCode.adapter().supports_resume());
        assert!(Provider::OpenCode.adapter().supports_model_override());
        assert!(Provider::OpenCode.adapter().supports_prefill());
        assert!(Provider::OpenCode.adapter().auto_resume_on_startup());
        // Issue #1295: plugin hook unblocks the Autopilot gate.
        assert!(Provider::OpenCode.adapter().requires_attention_hook());
        assert!(Provider::Codex.adapter().supports_resume());
        // Kimi Code (wayfinder #918) is a native TUI harness like Codex/Grok
        // — its adapter declares resume + model override, no prefill, no
        // Claude-style attention hook. Pin the matrix so a future adapter
        // refactor that drops Kimi from the resume path trips this test.
        assert!(Provider::Kimi.adapter().supports_resume());
        assert!(Provider::Kimi.adapter().supports_model_override());
        assert!(Provider::Kimi.adapter().requires_attention_hook());
        assert!(Provider::Mcode.adapter().supports_resume());
        // Issue #1179: mcode's interactive TUI rejects `--model`, so the
        // override is no longer advertised.
        assert!(!Provider::Mcode.adapter().supports_model_override());
        assert!(!Provider::Mcode.adapter().requires_attention_hook());
        // Issue #1365: `dsh` is a launcher with no validated profile —
        // gates resume + model to false so the Spawn Menu hides the
        // Resume button and the resolver drops `--model`. The
        // orchestrator routes `SessionIdMode::None` when
        // `supports_resume = false`, so no `--session-id` flag ever
        // reaches the recipe.
        assert!(!Provider::Dsh.adapter().supports_resume());
        assert!(!Provider::Dsh.adapter().auto_resume_on_startup());
        assert!(!Provider::Dsh.adapter().supports_model_override());
        assert!(!Provider::Dsh.adapter().requires_attention_hook());
        // Issue #1437: Freebuff is an interactive TUI harness with session resumption
        // via --continue but no model override (the interactive TUI does not accept
        // --model). Pinning the matrix so a future adapter change trips this test.
        assert!(Provider::Freebuff.adapter().supports_resume());
        assert!(!Provider::Freebuff.adapter().supports_model_override());
        assert!(!Provider::Freebuff.adapter().requires_attention_hook());
    }

    /// The "produces a readable transcript" capability (#317) — the
    /// Claude-backed `anthropic` adapter (which also runs custom
    /// MiniMax/DeepSeek profiles), Codex, Cursor, Antigravity, Grok, and
    /// Command Code write
    /// transcripts the coordinator read API can drill into. Kimi Code's
    /// `wire.jsonl` is standard JSONL (#911 research), but the reader's path
    /// resolver isn't wired for `~/.kimi/` yet. Everything else degrades to
    /// spine-only with `unsupported`; this matrix is load-bearing.
    #[test]
    fn only_transcript_writing_providers_produce_a_readable_transcript() {
        assert!(Provider::Anthropic.adapter().produces_readable_transcript());
        // Codex's rollout format is parsed via TranscriptFormat::Codex (#887).
        assert!(Provider::Codex.adapter().produces_readable_transcript());
        // Kimi Code (#918) — reader wiring is the follow-up; capability
        // claim is honest at `false` until then.
        assert!(!Provider::Kimi.adapter().produces_readable_transcript());
        // Cursor's workspace-scoped JSONL is parsed by TranscriptFormat::Cursor.
        assert!(Provider::Cursor.adapter().produces_readable_transcript());
        // Grok Code (#1281) — chat_history.jsonl / updates.jsonl parsed via
        // TranscriptFormat::Grok; this flip is what surfaces Grok in the
        // archived-node resume picker (provider_menu derives
        // `resumable` from `supports_resume && produces_readable_transcript`).
        assert!(Provider::Grok.adapter().produces_readable_transcript());
        assert!(!Provider::Mcode.adapter().produces_readable_transcript());
        assert!(!Provider::Dsh.adapter().produces_readable_transcript());
        // Issue #1283: AGY's per-conversation JSONL is parsed via
        // TranscriptFormat::Agy, so the archive resume picker surfaces it.
        assert!(
            Provider::Agy.adapter().produces_readable_transcript(),
            "AGY must advertise produces_readable_transcript=true so the \
             archived-node resume picker surfaces it (#1283)",
        );
        // Issue #1296: OpenCode's local `opencode.db` is read by
        // TranscriptFormat::OpenCode, so the archive resume picker and the
        // Coordinator rich layer include OpenCode nodes.
        assert!(
            Provider::OpenCode.adapter().produces_readable_transcript(),
            "OpenCode must advertise produces_readable_transcript=true so the \
             archived-node resume picker surfaces it (#1296)",
        );
        assert!(!Provider::Terminal.adapter().produces_readable_transcript());
        // Issue #1437: Freebuff does not write a transcript the coordinator can parse.
        assert!(!Provider::Freebuff.adapter().produces_readable_transcript());
    }

    #[test]
    fn provider_from_db_str_round_trip_for_known_values() {
        for &p in Provider::all() {
            let s = p.to_string();
            assert_eq!(Provider::from_db_str(&s), p);
        }
    }

    /// Documents the intentional silent fallback: unknown DB values default to
    /// Anthropic rather than erroring. This preserves pre-refactor behaviour
    /// where the inline `match` in db/mod.rs had a `_ => Provider::Anthropic` arm.
    /// The fallback now also emits a `tracing::warn!` (covered by
    /// `provider_from_db_str_unknown_logs_warning`).
    #[test]
    fn provider_from_db_str_unknown_falls_back_to_anthropic() {
        assert_eq!(Provider::from_db_str("garbage"), Provider::Anthropic);
        assert_eq!(Provider::from_db_str(""), Provider::Anthropic);
    }

    /// Regression test for #297: hand-edited `preferences.json` with a
    /// capitalised value like `"Terminal"` previously silently resolved to
    /// `Anthropic` while the DB persisted the capitalised string — a
    /// particularly nasty mismatch. The fix lowercases + trims the input
    /// so every recognised name resolves regardless of case.
    #[test]
    fn provider_from_db_str_is_case_insensitive() {
        assert_eq!(Provider::from_db_str("Terminal"), Provider::Terminal);
        assert_eq!(Provider::from_db_str("TERMINAL"), Provider::Terminal);
        assert_eq!(Provider::from_db_str("TeRmInAl"), Provider::Terminal);
        assert_eq!(Provider::from_db_str("ANTHROPIC"), Provider::Anthropic);
        assert_eq!(Provider::from_db_str("AGY"), Provider::Agy);
        assert_eq!(Provider::from_db_str("OpenCode"), Provider::OpenCode);
        assert_eq!(Provider::from_db_str("Codex"), Provider::Codex);
        assert_eq!(Provider::from_db_str("Mcode"), Provider::Mcode);
        assert_eq!(Provider::from_db_str("Dsh"), Provider::Dsh);
    }

    /// Hard cutover (issue #538): "minimax" is no longer a first-class executor.
    /// With no configured harness profile it falls through to the Anthropic
    /// default — `resolve_harness_provider` checks profiles first, so a
    /// configured custom account still resolves to the right backend env.
    ///
    /// "kimi" USED to fall through here too — Kimi Code (wayfinder #918) is now
    /// a native binary executor, so it resolves to `Provider::Kimi` directly.
    /// See `provider_from_db_str_kimi_resolves_to_native_harness` below.
    #[test]
    fn provider_from_db_str_legacy_minimax_falls_back_to_anthropic() {
        assert_eq!(Provider::from_db_str("minimax"), Provider::Anthropic);
        assert_eq!(Provider::from_db_str("Minimax"), Provider::Anthropic);
    }

    /// Kimi Code (wayfinder #918) ships a native CLI on PATH as `kimi` — the
    /// legacy "Kimi LLM endpoint via Claude Code" interpretation has been
    /// retired (the `kimi` ProviderAccount is now self_auth, matching `grok`).
    /// A bare `"kimi"` in `AgentNode.provider` (or a user `default_provider`
    /// setting) now resolves to the native Kimi Code executor directly, no
    /// profile lookup needed.
    #[test]
    fn provider_from_db_str_kimi_resolves_to_native_harness() {
        assert_eq!(Provider::from_db_str("kimi"), Provider::Kimi);
        assert_eq!(Provider::from_db_str("Kimi"), Provider::Kimi);
        assert_eq!(Provider::from_db_str("KIMI"), Provider::Kimi);
    }

    /// MiniMax Code CLI (`mcode`) is a native binary executor on PATH as `mcode`.
    #[test]
    fn provider_from_db_str_mcode_resolves_to_native_harness() {
        assert_eq!(Provider::from_db_str("mcode"), Provider::Mcode);
        assert_eq!(Provider::from_db_str("Mcode"), Provider::Mcode);
        assert_eq!(Provider::from_db_str("MCODE"), Provider::Mcode);
        assert_eq!(Provider::from_db_str("minimax-code"), Provider::Mcode);
    }

    /// DeepSeek Harness CLI (`dsh`) is a native binary executor on PATH as `dsh`.
    #[test]
    fn provider_from_db_str_dsh_resolves_to_native_harness() {
        assert_eq!(Provider::from_db_str("dsh"), Provider::Dsh);
        assert_eq!(Provider::from_db_str("Dsh"), Provider::Dsh);
        assert_eq!(Provider::from_db_str("DSH"), Provider::Dsh);
        assert_eq!(Provider::from_db_str("deepseek-harness"), Provider::Dsh);
        assert_eq!(Provider::from_db_str("deepseek"), Provider::Dsh);
    }

    /// Freebuff CLI (`freebuff`) is a native binary executor on PATH as `freebuff`.
    #[test]
    fn provider_from_db_str_freebuff_resolves_to_native_harness() {
        assert_eq!(Provider::from_db_str("freebuff"), Provider::Freebuff);
        assert_eq!(Provider::from_db_str("Freebuff"), Provider::Freebuff);
        assert_eq!(Provider::from_db_str("FREEBUFF"), Provider::Freebuff);
    }

    /// Whitespace around the value shouldn't break matching either — a
    /// hand-edited JSON file with `"Terminal "` (trailing space) should
    /// still resolve correctly.
    #[test]
    fn provider_from_db_str_trims_whitespace() {
        assert_eq!(Provider::from_db_str("  terminal  "), Provider::Terminal);
        assert_eq!(Provider::from_db_str("\tcodex\n"), Provider::Codex);
    }

    // Per-thread capture buffer for tracing events emitted inside
    // `from_db_str`. `thread_local!` (rather than a per-test
    // `Arc<Mutex<Vec<u8>>>`) guarantees events from other test threads
    // can't bleed into this thread's buffer under parallel `cargo test` —
    // issue #1007. The buffer persists across tests scheduled on the same
    // OS thread, so `capture_warnings` drains it on entry.
    thread_local! {
        static WARN_BUFFER: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    /// `MakeWriter` that appends to the thread-local `WARN_BUFFER`. A unit
    /// struct because there's nothing to clone — the address lives in the
    /// `thread_local!`.
    struct ThreadLocalWriter;

    impl Write for ThreadLocalWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            WARN_BUFFER.with(|cell| cell.borrow_mut().extend_from_slice(buf));
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ThreadLocalWriter {
        type Writer = ThreadLocalWriter;
        fn make_writer(&'a self) -> Self::Writer {
            ThreadLocalWriter
        }
    }

    /// Run `body` under a subscriber that captures WARN-level tracing events
    /// into the thread-local `WARN_BUFFER`, returning the captured string.
    /// Used by the `provider_from_db_str_*_warning` tests to assert on log
    /// output.
    fn capture_warnings<F: FnOnce()>(body: F) -> String {
        // Drain in case a prior test on this OS thread left bytes behind.
        WARN_BUFFER.with(|cell| cell.borrow_mut().clear());

        let subscriber = tracing_subscriber::fmt()
            .with_writer(ThreadLocalWriter)
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();

        tracing::subscriber::with_default(subscriber, body);

        let result = String::from_utf8(WARN_BUFFER.with(|cell| cell.borrow().clone())).unwrap();
        // Tidy up so the buffer doesn't accumulate across tests on this OS thread.
        WARN_BUFFER.with(|cell| cell.borrow_mut().clear());
        result
    }

    /// Issue #297: a misspelled provider id in `preferences.json` (e.g.
    /// `"antropic"`, `"claude"`, `"gpt"`) used to be invisible. After the
    /// fix the function emits a `warn!` so the silent fallback shows up in
    /// `buildmesh.log` next to the offending value.
    #[test]
    fn provider_from_db_str_unknown_logs_warning() {
        // Typo: missing the second 'h' — must fall back AND warn.
        let captured = capture_warnings(|| {
            assert_eq!(Provider::from_db_str("antropic"), Provider::Anthropic);
        });
        assert!(
            captured.contains("unrecognized provider") && captured.contains("antropic"),
            "expected warn! mentioning 'antropic', got: {}",
            captured
        );
    }

    /// Known values — even when capitalised — should NOT emit a warning;
    /// the case-insensitive match is the intended path, not a fallback.
    #[test]
    fn provider_from_db_str_known_value_does_not_warn() {
        let captured = capture_warnings(|| {
            assert_eq!(Provider::from_db_str("Terminal"), Provider::Terminal);
            assert_eq!(Provider::from_db_str("ANTHROPIC"), Provider::Anthropic);
            assert_eq!(Provider::from_db_str("  codex  "), Provider::Codex);
        });
        assert!(
            !captured.contains("unrecognized provider"),
            "expected no warn! for known (case-insensitive) value, got: {}",
            captured
        );
    }

    /// Regression test for #1007: the previous `VecWriter` (with per-test
    /// `Arc<Mutex<Vec<u8>>>`) allowed events from other test threads to
    /// bleed into the capture buffer under parallel `cargo test`. This test
    /// runs `capture_warnings` on 16 threads simultaneously via a `Barrier`
    /// and asserts each thread's captured output contains ONLY its own
    /// marker — under `ThreadLocalWriter` this passes deterministically.
    ///
    /// Markers use fixed-width zero-padded suffixes (`_00`…`_15`) so that
    /// `_1` cannot be a substring of `_10`…`_15` (a false positive that
    /// bit the first version of this test).
    #[test]
    fn capture_warnings_isolates_buffers_per_thread() {
        use std::sync::{Arc, Barrier};
        use std::thread;
        const THREADS: usize = 16;
        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::with_capacity(THREADS);
        for t in 0..THREADS {
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                let marker = format!("capture_warnings_thread_marker_{t:02}");
                barrier.wait();
                capture_warnings(|| {
                    tracing::warn!("{marker}");
                })
            }));
        }
        let results: Vec<String> = handles
            .into_iter()
            .map(|h| h.join().expect("worker thread panicked"))
            .collect();
        for (t, captured) in results.iter().enumerate() {
            let my_marker = format!("capture_warnings_thread_marker_{t:02}");
            assert!(
                captured.contains(&my_marker),
                "thread {t} must see its own marker, got: {captured}"
            );
            for other in 0..THREADS {
                if other == t {
                    continue;
                }
                let other_marker = format!("capture_warnings_thread_marker_{other:02}");
                assert!(
                    !captured.contains(&other_marker),
                    "thread {t} saw thread {other}'s marker — cross-thread bleed: {captured}"
                );
            }
        }
    }

    #[test]
    fn provider_display_lowercase() {
        assert_eq!(format!("{}", Provider::Anthropic), "anthropic");
        assert_eq!(format!("{}", Provider::Agy), "agy");
        assert_eq!(format!("{}", Provider::OpenCode), "opencode");
        assert_eq!(format!("{}", Provider::Codex), "codex");
        assert_eq!(format!("{}", Provider::Cursor), "cursor");
        assert_eq!(format!("{}", Provider::Grok), "grok");
        assert_eq!(format!("{}", Provider::Kimi), "kimi");
        assert_eq!(format!("{}", Provider::Mcode), "mcode");
        assert_eq!(format!("{}", Provider::Terminal), "terminal");
    }

    #[test]
    fn codex_uses_subcommand_resume_recipe() {
        use crate::agent::provider::Platform;
        let adapter = Provider::Codex.adapter();
        let resume = adapter.spawn_recipe_for_resume(Platform::Macos, "abc-123");
        assert!(resume.is_some());
        let recipe = resume.unwrap();
        assert_eq!(recipe.binary, "codex");
        assert_eq!(recipe.base_args[0], "resume");
        assert_eq!(recipe.trailing_args, vec!["abc-123".to_string()]);
        assert!(recipe.base_args.contains(&"--ask-for-approval".into()));
    }

    #[test]
    fn codex_self_assigns_session_ids() {
        let adapter = Provider::Codex.adapter();
        assert!(adapter.self_assigns_session_id());
        assert!(adapter.session_assign_args("test-id").is_empty());
        assert!(adapter.resume_args("test-id").is_empty());
    }

    #[test]
    fn anthropic_and_terminal_do_not_self_assign() {
        assert!(!Provider::Anthropic.adapter().self_assigns_session_id());
        assert!(Provider::OpenCode.adapter().self_assigns_session_id());
        assert!(!Provider::OpenCode.adapter().captures_session_id_from_pty());
        assert!(!Provider::Terminal.adapter().self_assigns_session_id());
        assert!(!Provider::Freebuff.adapter().self_assigns_session_id());
    }

    /// `is_plain_terminal` is the single trait method that switches the
    /// spawn pipeline's reader EOF handling. Only the Terminal provider
    /// overrides the default `false`. This test guards against accidental
    /// flipping by future refactors — if any LLM provider were ever to
    /// claim "plain terminal" semantics, the spawn path would silently
    /// stop emitting `resume-failed` events for it, breaking a real
    /// LLM-resume signal.
    #[test]
    fn is_plain_terminal_only_for_terminal() {
        assert!(Provider::Terminal.adapter().is_plain_terminal());
        assert!(!Provider::Anthropic.adapter().is_plain_terminal());
        assert!(!Provider::Agy.adapter().is_plain_terminal());
        assert!(!Provider::OpenCode.adapter().is_plain_terminal());
        assert!(!Provider::Codex.adapter().is_plain_terminal());
    }

    #[test]
    fn codex_prefill_is_positional() {
        let adapter = Provider::Codex.adapter();
        let args = adapter.prefill_args("fix the auth bug");
        assert_eq!(args, vec!["fix the auth bug"]);
    }

    #[test]
    fn env_type_display_lowercase() {
        assert_eq!(format!("{}", EnvType::Windows), "windows");
        assert_eq!(format!("{}", EnvType::Wsl), "wsl");
    }

    #[test]
    fn session_status_serde_json_snake_case() {
        // Was `session_status_serde_json_lowercase`, which asserted the buggy
        // "awaitinginput" — it pinned the wire format without ever checking it
        // against the "awaiting_input" the DB and frontend use. Issue #359
        // switched the enum to snake_case; see
        // `session_status_serializes_to_wire_as_its_db_string` for the full
        // per-variant guard.
        let json = serde_json::to_string(&SessionStatus::AwaitingInput).unwrap();
        assert_eq!(json, "\"awaiting_input\"");
        let json = serde_json::to_string(&SessionStatus::Running).unwrap();
        assert_eq!(json, "\"running\"");
    }
}
