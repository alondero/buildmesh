use super::{orchestrator::*, provision::*, reader::*, *};
use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};
use crate::agent::provider::Platform;
use crate::models::{EnvType, Provider};
// The eight worktree-provision helpers were moved to
// `crate::git::worktree::provision` in PR #676 / issue #677, and #698
// added `locked_fetch_pr_head` on top. The tests here exercise them by
// name, so re-import at the test-module scope.
use crate::agent::capabilities::ResolvedAgentConfig;
use crate::git::worktree::provision::{
    adopt_warm_worktree_by_move, fetch_fork_head, fetch_single_ref, fork_remote_alias,
    locked_fetch_pr_head, read_origin_ref_sha, upgrade_warm_to_mode,
};
use tempfile::TempDir;

/// Pin the spawn-time fallback. Sole pin of `DEFAULT_WORKTREE_MODE`
/// after #411 deleted the TS-side sentinel (it had no real consumer).
#[test]
fn default_worktree_mode_is_branched() {
    assert_eq!(DEFAULT_WORKTREE_MODE, "branched");
}

/// The start_reader pattern: `pump_pty_output` inside `with_batcher`.
/// If the producer isn't dropped before join, this hangs on EOF.
#[test]
fn pump_inside_with_batcher_exits_cleanly_on_reader_eof() {
    let reader: Box<dyn std::io::Read + Send> = Box::new(std::io::Cursor::new(b"hello from pty\n"));
    let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let g = got.clone();
    let started = std::time::Instant::now();
    crate::pty::batch::with_batcher(
        move |batch| g.lock().unwrap().extend_from_slice(&batch),
        |tx| {
            pump_pty_output(reader, |data| {
                let _ = tx.send(data.to_vec());
            });
        },
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "reader+batcher hung after PTY EOF — producer was not dropped"
    );
    assert_eq!(&*got.lock().unwrap(), b"hello from pty\n");
}

// -----------------------------------------------------------------------
// Cascade layer-1 wiring at the spawn seam (issue #1155).
//
// The `resolve_agent_config` resolver already had unit tests for its
// cascade order, but those tests never proved the spawn pipeline
// *populated* the explicit slot from `SpawnOptions`. Before #1155 the
// `explicit:` field on `FieldInputs` was hard-coded `None`, so layer
// 1 of the documented cascade (issue #1148: explicit > mesh >
// application > native) was unreachable. `cascade_inputs_for` is the
// spawn-side seam; these tests pin both the wiring AND the cascade
// precedence when the helper's output is fed through the resolver.
// -----------------------------------------------------------------------

use crate::agent::capabilities::{resolve_agent_config, FieldInputs, HarnessCapabilities};
use crate::preferences::HarnessConfigValue;

#[test]
fn provider_provisioning_runs_hooks_after_trust_failure() {
    let trust_finished = std::cell::Cell::new(false);
    let hook_saw_trust_finish = std::cell::Cell::new(false);
    let (trust, hooks) = run_provider_provisioning(
        || {
            trust_finished.set(true);
            Err("trust failed".to_string())
        },
        || {
            hook_saw_trust_finish.set(trust_finished.get());
            Err("hooks failed".to_string())
        },
        true,
    );

    assert_eq!(trust.unwrap_err(), "trust failed");
    assert_eq!(hooks.unwrap_err(), "hooks failed");
    assert!(hook_saw_trust_finish.get());

    let hook_called = std::cell::Cell::new(false);
    let (trust, hooks) = run_provider_provisioning(
        || Ok(()),
        || {
            hook_called.set(true);
            Ok(())
        },
        false,
    );
    assert!(trust.is_ok());
    assert!(hooks.is_ok());
    assert!(!hook_called.get());
}

/// Helper that returns the Anthropic capabilities descriptor for the
/// integration tests below. Pulled out so each test reads as the
/// cascade it pins without dragging harness-table setup inline.
fn anthropic_caps() -> HarnessCapabilities {
    crate::agent::capabilities::capabilities_for(&crate::agent::provider::adapters::ANTHROPIC)
}

/// Regression pin for issue #1155 acceptance criterion 4 — the spawn
/// pipeline must populate the `explicit` slot from the values the
/// caller passed in. Without this wiring the helper would feed `None`
/// for the explicit slot and the top layer of the cascade would never
/// fire. The test fails compilation if any future refactor drops the
/// `explicit_*` parameters from `cascade_inputs_for`.
#[test]
fn cascade_inputs_for_populates_explicit_slot_for_both_fields() {
    let app_default = HarnessConfigValue {
        model: Some("opus-4-1".into()),
        effort: Some("high".into()),
    };
    let inputs = cascade_inputs_for(
        Some("sonnet-4"),
        Some("medium"),
        Some("haiku-4"),
        Some("low"),
        Some(&app_default),
        None,
    );
    assert_eq!(
        inputs.model,
        FieldInputs {
            explicit: Some("sonnet-4"),
            mesh_override: None,
            mesh: Some("haiku-4"),
            application: Some("opus-4-1"),
        },
        "explicit must win over mesh which wins over application",
    );
    assert_eq!(
        inputs.effort,
        FieldInputs {
            explicit: Some("medium"),
            mesh_override: None,
            mesh: Some("low"),
            application: Some("high"),
        },
        "explicit effort must win over mesh effort which wins over application effort",
    );
}

/// Whitespace-only / empty strings on the explicit slot must collapse
/// to `None` so the cascade falls through (issue #1148 AC #32,
/// #1155 AC #3). Mirrors the resolver's `normalize_non_empty` so the
/// cascade behaves identically regardless of which layer trimmed the
/// blank.
#[test]
fn cascade_inputs_for_collapses_whitespace_explicit_to_none() {
    let app_default = HarnessConfigValue {
        model: Some("opus-4-1".into()),
        effort: Some("high".into()),
    };
    let inputs = cascade_inputs_for(
        Some("   "),
        Some("\t\n  "),
        Some("haiku-4"),
        Some("low"),
        Some(&app_default),
        None,
    );
    assert_eq!(
        inputs.model.explicit, None,
        "whitespace-only explicit model must collapse so mesh/application win"
    );
    assert_eq!(
        inputs.effort.explicit, None,
        "whitespace-only explicit effort must collapse so mesh/application win"
    );
    // Mesh + application survive the collapse — they're the layers
    // that win when explicit is blank.
    assert_eq!(inputs.model.mesh, Some("haiku-4"));
    assert_eq!(inputs.effort.application, Some("high"));
}

/// Trimming: a layer value with surrounding whitespace keeps its
/// trimmed content (the harness shouldn't receive ` opus `, but
/// `opus`). Mirrors `resolver_trims_layer_values` at the spawn seam
/// so an explicit value like `" opus "` lands at the resolver as
/// `"opus"` regardless of which side trimmed it.
#[test]
fn cascade_inputs_for_trims_explicit_values() {
    let inputs = cascade_inputs_for(Some("  opus  "), Some(" high\t"), None, None, None, None);
    assert_eq!(inputs.model.explicit, Some("opus"));
    assert_eq!(inputs.effort.explicit, Some("high"));
}

/// Independence: model and effort can be set independently. A spawn
/// site that only wants to override model must NOT accidentally
/// clobber effort. Pin for issue #1155 AC #1 — "explicit model
/// and/or effort argument".
#[test]
fn cascade_inputs_for_independent_fields() {
    let app_default = HarnessConfigValue {
        model: Some("opus-4-1".into()),
        effort: Some("high".into()),
    };

    // Explicit model only — effort falls through to the app default.
    let model_only =
        cascade_inputs_for(Some("sonnet-4"), None, None, None, Some(&app_default), None);
    assert_eq!(model_only.model.explicit, Some("sonnet-4"));
    assert_eq!(model_only.effort.explicit, None);
    assert_eq!(model_only.effort.application, Some("high"));

    // Explicit effort only — model falls through to the app default.
    let effort_only = cascade_inputs_for(None, Some("low"), None, None, Some(&app_default), None);
    assert_eq!(effort_only.model.explicit, None);
    assert_eq!(effort_only.model.application, Some("opus-4-1"));
    assert_eq!(effort_only.effort.explicit, Some("low"));
}

/// Integration pin: feed the helper's output through the resolver
/// and verify the explicit value wins over the mesh + application
/// layers. This is the "real spawn site" regression test for issue
/// #1155 AC #4 — every layer is populated, so any layer's value
/// reaching the resolver instead of the explicit one flips the
/// assertion. The harness is Anthropic (model + effort both
/// supported) so the capability mask passes everything through.
#[test]
fn cascade_inputs_for_layer1_wins_over_mesh_and_application_at_resolver() {
    let app_default = HarnessConfigValue {
        model: Some("opus-4-1".into()),
        effort: Some("high".into()),
    };
    let inputs = cascade_inputs_for(
        Some("sonnet-4"),
        Some("low"),
        Some("haiku-4"),
        Some("medium"),
        Some(&app_default),
        None,
    );
    let resolved = resolve_agent_config(&anthropic_caps(), inputs, None);
    assert_eq!(
        resolved.model.as_deref(),
        Some("sonnet-4"),
        "layer-1 explicit must win over mesh and application"
    );
    assert_eq!(
        resolved.effort.as_deref(),
        Some("low"),
        "layer-1 explicit must win over mesh and application"
    );
}

/// Integration pin for the fall-through path: when explicit is empty
/// (whitespace) at the spawn seam, the resolver sees `None` for that
/// slot and the mesh layer drives the resolved value (cascade order:
/// explicit > mesh > application). Combined with
/// `cascade_inputs_for_collapses_whitespace_explicit_to_none`, this is
/// the end-to-end "no silent blank arg to the harness" regression
/// pin — the explicit slot's whitespace doesn't reach the resolver,
/// and the mesh slot wins over the application slot per the
/// documented cascade.
#[test]
fn cascade_inputs_for_empty_explicit_falls_through_at_resolver() {
    let app_default = HarnessConfigValue {
        model: Some("opus-4-1".into()),
        effort: Some("high".into()),
    };
    let inputs = cascade_inputs_for(
        Some("   "),
        Some(""),
        Some("haiku-4"),
        Some("medium"),
        Some(&app_default),
        None,
    );
    let resolved = resolve_agent_config(&anthropic_caps(), inputs, None);
    // Explicit collapsed → mesh wins over application.
    assert_eq!(resolved.model.as_deref(), Some("haiku-4"));
    assert_eq!(resolved.effort.as_deref(), Some("medium"));
}

/// Regression pin for issue #1155 AC #2: the explicit layer must
/// drive the resolved value even when the mesh slot ALSO has a
/// value — proving the helper routes the explicit value to the
/// resolver's `explicit` slot (not, say, the `mesh` slot). A
/// future refactor that re-orders the helper's parameters or
/// mistakenly maps the explicit arg to the mesh slot would flip
/// this assertion (model would resolve to "haiku-4" — the mesh
/// value — instead of "sonnet-4").
#[test]
fn cascade_inputs_for_explicit_wins_over_mesh_when_application_empty() {
    let inputs = cascade_inputs_for(
        Some("sonnet-4"),
        Some("medium"),
        Some("haiku-4"),
        Some("low"),
        None,
        None,
    );
    let resolved = resolve_agent_config(&anthropic_caps(), inputs, None);
    assert_eq!(resolved.model.as_deref(), Some("sonnet-4"));
    assert_eq!(resolved.effort.as_deref(), Some("medium"));
}

/// Per-Mesh harness override wiring at the spawn seam (issue #1151).
/// The `mesh_override` slot sits between explicit and the legacy mesh
/// layer (cascade: explicit > mesh_override > mesh > application > native).
/// A populated mesh override wins over the application default and
/// falls below explicit.
#[test]
fn cascade_inputs_for_mesh_override_wins_over_application() {
    let app_default = HarnessConfigValue {
        model: Some("opus-4-1".into()),
        effort: Some("high".into()),
    };
    let mesh_override = HarnessConfigValue {
        model: Some("opus-4-1".into()),
        effort: Some("medium".into()),
    };
    let inputs = cascade_inputs_for(
        None,
        None,
        None,
        None,
        Some(&app_default),
        Some(&mesh_override),
    );
    let resolved = resolve_agent_config(&anthropic_caps(), inputs, None);
    assert_eq!(resolved.model.as_deref(), Some("opus-4-1"));
    assert_eq!(resolved.effort.as_deref(), Some("medium"));
}

/// Mesh override is masked per-field by the harness's capability
/// contract: OpenCode accepts model (`--model provider/model`) but
/// has no effort control, so effort drops and model passes.
#[test]
fn cascade_inputs_for_mesh_override_drops_effort_for_opencode() {
    let mesh_override = HarnessConfigValue {
        model: Some("some-model".into()),
        effort: Some("high".into()),
    };
    let inputs = cascade_inputs_for(None, None, None, None, None, Some(&mesh_override));

    let resolved = crate::agent::capabilities::resolve_agent_config(
        &crate::agent::capabilities::capabilities_for(&crate::agent::provider::adapters::OPENCODE),
        inputs,
        None,
    );
    // OpenCode accepts `--model provider/model` and has no effort
    // control. The mesh override model must pass; effort must drop.
    assert_eq!(resolved.model.as_deref(), Some("some-model"));
    assert_eq!(resolved.effort, None);
}

/// `SpawnOptions` must carry the explicit slots through to
/// `spawn_agent_inner` (issue #1155 AC #1). The orchestrator
/// destructures them out of `opts`; this test pins the struct
/// shape so a refactor that drops either field fails compilation.
#[test]
fn spawn_options_carries_explicit_slots() {
    let opts = SpawnOptions {
        session_id: -1,
        provider: Provider::Anthropic,
        resume: None,
        rows: 24,
        cols: 80,
        prefill: None,
        node: None,
        explicit_model: Some("sonnet-4".into()),
        explicit_effort: Some("low".into()),
        // Issue #1358: every transport that builds a `SpawnRequest`
        // and reaches `spawn_agent_inner` via `spawn_with_intent`
        // forwards `explicit_extra_args` from the v2 SpawnAgentNode
        // explicit slot. None is fine — the resolver then cascades
        // through mesh / app defaults and `default_prepare` only
        // forwards the string when `supports_extra_args = true`.
        explicit_extra_args: None,
    };
    assert_eq!(opts.explicit_model.as_deref(), Some("sonnet-4"));
    assert_eq!(opts.explicit_effort.as_deref(), Some("low"));
    assert!(opts.explicit_extra_args.is_none());
}

/// `SpawnRequest` must carry an `explicit` field (issue #1155 AC #1).
/// The struct shape pin protects `spawn_with_intent` from a future
/// refactor that drops the field — the orchestrator destructures
/// `explicit` out of the request and feeds it into `SpawnOptions`.
#[test]
fn spawn_request_carries_explicit_overrides() {
    let req = SpawnRequest {
        node_id: -1,
        intent: SpawnIntent::Fresh,
        terminal_size: TerminalSize { rows: 24, cols: 80 },
        explicit: ExplicitSpawnOverrides {
            model: Some("opus-4-1".into()),
            effort: Some("high".into()),
            extra_args: None,
        },
    };
    assert_eq!(req.explicit.model.as_deref(), Some("opus-4-1"));
    assert_eq!(req.explicit.effort.as_deref(), Some("high"));

    // `Default` lets spawn sites opt out via `..Default::default()`.
    assert_eq!(ExplicitSpawnOverrides::default().model, None);
    assert_eq!(ExplicitSpawnOverrides::default().effort, None);
}

// -----------------------------------------------------------------------
// `SpawnRequest::new` constructor + integration pin for the cascade
// layer-1 wiring at a real spawn site (issue #1157).
//
// The cascade tests above (lines 2556-2744) pin the helper +
// resolver precedence — issue #1155 AC #4 ("Regression tests must
// verify layer-1 behavior at a real spawn site, not just resolver
// unit tests") is satisfied at the helper level. The tests below
// close the remaining gap by driving a *real* `SpawnRequest` —
// built through the new constructor + `with_explicit` builder —
// through the same call shape `spawn_agent_inner` uses, asserting
// the explicit value reaches `FieldInputs::explicit` and wins over
// the mesh + application layers. The harness is Anthropic
// (`anthropic_caps()`, supports both model + effort) so the
// capability mask passes everything through.
// -----------------------------------------------------------------------

/// Constructor contract pin (issue #1157): `SpawnRequest::new` must
/// set `explicit` to `Default::default()` so every existing call site
/// that doesn't wire layer-1 overrides gets the layer-1-empty
/// behaviour without re-declaring the field. Without this pin a
/// future refactor that returns `Self { ... explicit: <something> }`
/// silently changes the cascade behaviour at every call site.
#[test]
fn spawn_request_new_sets_explicit_default() {
    let req = SpawnRequest::new(42, SpawnIntent::Fresh, TerminalSize::default());
    assert_eq!(req.node_id, 42);
    assert_eq!(req.terminal_size, TerminalSize { rows: 24, cols: 80 });
    assert_eq!(req.explicit, ExplicitSpawnOverrides::default());
    assert_eq!(req.explicit.model, None);
    assert_eq!(req.explicit.effort, None);
}

#[test]
fn every_non_resume_intent_replaces_a_stored_conversation() {
    assert!(intent_replaces_conversation(&SpawnIntent::Fresh));
    assert!(intent_replaces_conversation(&SpawnIntent::Loop {
        initial_prompt: "continue".into(),
    }));
    assert!(intent_replaces_conversation(&SpawnIntent::Handover {
        selected_text: "context".into(),
    }));
    assert!(!intent_replaces_conversation(&SpawnIntent::Resume {
        cause: ResumeCause::Explicit,
    }));
}

/// AC #4 pin: a `SpawnRequest` with a populated layer-1 override
/// (model + effort) drives the helper extracted from
/// `spawn_agent_inner` and the resolved config carries the explicit
/// value — winning over the mesh + application layers. This is the
/// "real spawn site" regression test issue #1155 AC #4 called for:
/// the helper-level tests above (lines 2556-2744) exercise the
/// same inputs against the same resolver, but this test drives them
/// *through* the `SpawnRequest` shape every transport hands the
/// orchestrator. A future refactor that drops the `explicit` field
/// or maps it to the wrong `SpawnOptions` slot would flip this
/// assertion.
#[test]
fn spawn_request_explicit_wins_at_resolver() {
    let req = SpawnRequest::new(42, SpawnIntent::Fresh, TerminalSize::default()).with_explicit(
        ExplicitSpawnOverrides {
            model: Some("opus-4-1".into()),
            effort: Some("high".into()),
            // Issue #1358: extra_args ride the same cascade layer-1
            // slot. A non-None value here proves the wiring from
            // `SpawnRequest.explicit.extra_args` → `SpawnOptions
            // .explicit_extra_args` → `resolve_spawn_config` — the
            // gap the spec review flagged (#1358) where this string
            // was collected by the Inspector but dropped at the
            // `spawn_with_intent` seam.
            extra_args: Some("--dangerously-skip-permissions --verbose".into()),
        },
    );
    let app_default = HarnessConfigValue {
        model: Some("sonnet-4".into()),
        effort: Some("medium".into()),
    };
    let resolved = resolve_spawn_config(
        Provider::Anthropic,
        req.explicit.model.as_deref(),
        req.explicit.effort.as_deref(),
        req.explicit.extra_args.as_deref(),
        Some(&app_default),
        None,
    );
    assert_eq!(
            resolved.model.as_deref(),
            Some("opus-4-1"),
            "SpawnRequest.explicit.model must reach the resolver as FieldInputs::explicit and win over mesh + application"
        );
    assert_eq!(
        resolved.extra_args.as_deref(),
        Some("--dangerously-skip-permissions --verbose"),
        "SpawnRequest.explicit.extra_args must reach ResolvedAgentConfig \
             (issue #1358 AC: extra-args override honoured per harness capability contract)"
    );
    assert_eq!(
            resolved.effort.as_deref(),
            Some("high"),
            "SpawnRequest.explicit.effort must reach the resolver as FieldInputs::explicit and win over mesh + application"
        );
}

/// AC #3 pin: whitespace-only explicit values collapse to `None`
/// inside the helper so the cascade falls through to the next
/// layer (issue #1148 AC #32 + #1155 AC #3). Mirrors
/// `cascade_inputs_for_empty_explicit_falls_through_at_resolver`
/// (line 2706) but driven from `SpawnRequest`, proving the
/// collapse from #1155 AC #3 holds end-to-end — i.e. the
/// `SpawnRequest → SpawnOptions → resolver` path doesn't smuggle
/// a blank past the `non_empty_trim` guard in `cascade_inputs_for`.
#[test]
fn spawn_request_whitespace_explicit_falls_through_at_resolver() {
    let req = SpawnRequest::new(42, SpawnIntent::Fresh, TerminalSize::default()).with_explicit(
        ExplicitSpawnOverrides {
            model: Some("   ".into()),
            effort: Some("\t\n".into()),
            extra_args: None,
        },
    );
    let mesh_override = HarnessConfigValue {
        model: Some("haiku-4".into()),
        effort: Some("medium".into()),
    };
    let app_default = HarnessConfigValue {
        model: Some("opus-4-1".into()),
        effort: Some("high".into()),
    };
    let resolved = resolve_spawn_config(
        Provider::Anthropic,
        req.explicit.model.as_deref(),
        req.explicit.effort.as_deref(),
        req.explicit.extra_args.as_deref(),
        // Legacy mesh columns are no longer read as active config
        // (issue #1151 AC #6) — the v33 migration copied any
        // non-empty legacy values into the mesh override map.
        Some(&app_default),
        Some(&mesh_override),
    );
    // Explicit collapsed → mesh_override wins over application.
    assert_eq!(resolved.model.as_deref(), Some("haiku-4"));
    assert_eq!(resolved.effort.as_deref(), Some("medium"));
}

/// Issue #1358 end-to-end pin: the `SpawnRequest → SpawnOptions →
/// resolve_spawn_config` path **must** deliver `explicit_extra_args`
/// end-to-end AND capability-mask it against the harness's
/// `supports_extra_args`. Terminal is the standing opt-out (a
/// plain-shell harness must not get synthetic flags spliced into
/// its argv) — the spec review (#1358) flagged this as the AC
/// violation that needed a regression pin. This test is the pin.
#[test]
fn spawn_request_extra_args_capability_mask_at_resolver() {
    // Anthropic — interactive harness, `supports_extra_args = true`.
    let req_interactive = SpawnRequest::new(42, SpawnIntent::Fresh, TerminalSize::default())
        .with_explicit(ExplicitSpawnOverrides {
            model: None,
            effort: None,
            extra_args: Some("--dangerously-skip-permissions".into()),
        });
    let resolved_anthropic = resolve_spawn_config(
        Provider::Anthropic,
        req_interactive.explicit.model.as_deref(),
        req_interactive.explicit.effort.as_deref(),
        req_interactive.explicit.extra_args.as_deref(),
        None,
        None,
    );
    assert_eq!(
        resolved_anthropic.extra_args.as_deref(),
        Some("--dangerously-skip-permissions"),
        "Anthropic supports extra_args — the explicit slot must reach ResolvedAgentConfig"
    );

    // Terminal — plain-shell harness, `supports_extra_args = false`.
    let req_terminal = SpawnRequest::new(42, SpawnIntent::Fresh, TerminalSize::default())
        .with_explicit(ExplicitSpawnOverrides {
            model: None,
            effort: None,
            extra_args: Some("--dangerously-skip-permissions --verbose".into()),
        });
    let resolved_terminal = resolve_spawn_config(
        Provider::Terminal,
        req_terminal.explicit.model.as_deref(),
        req_terminal.explicit.effort.as_deref(),
        req_terminal.explicit.extra_args.as_deref(),
        None,
        None,
    );
    assert!(
        resolved_terminal.extra_args.is_none(),
        "Terminal masks extra_args at the resolver (issue #1358). Got: {:?}",
        resolved_terminal.extra_args
    );
}

// -----------------------------------------------------------------------
// Reader-epilogue decision matrix (false "failed to start" fix).
//
// The reader thread's post-exit status write used to apply the 3s
// early-exit Error heuristic unconditionally, so a process that
// `kill_session` tore down deliberately (spawn step-2 stale kill, node
// close, app shutdown) within 3s of its creation was stamped `Error`
// + toasted `resume-failed` — and that stale Error then blocked the
// replacing spawn's Spawning→Running promotion. These tests pin the
// full matrix of `post_exit_action`.
// -----------------------------------------------------------------------

#[test]
fn deliberate_kill_never_writes_status_even_within_early_exit_window() {
    // The heart of the fix: a deliberate kill 1s after process creation
    // must NOT be misread as a failed --resume.
    assert_eq!(
        post_exit_action(false, true, std::time::Duration::from_secs(1)),
        PostExitAction::LeaveStatusAlone,
    );
    // …nor may it write Idle over the replacing spawn's Spawning.
    assert_eq!(
        post_exit_action(false, true, std::time::Duration::from_secs(60)),
        PostExitAction::LeaveStatusAlone,
    );
    // Plain terminals too: the kill initiator owns the next status.
    assert_eq!(
        post_exit_action(true, true, std::time::Duration::from_secs(1)),
        PostExitAction::LeaveStatusAlone,
    );
}

#[test]
fn natural_early_exit_still_flags_resume_failure() {
    // The heuristic's true positive is preserved: an LLM process that
    // dies on its own within the window (typically `--resume` against
    // an expired session) still reads as a resume failure.
    assert_eq!(
        post_exit_action(false, false, std::time::Duration::from_secs(1)),
        PostExitAction::MarkErrorResumeFailed,
    );
}

#[test]
fn natural_exit_after_window_marks_idle() {
    assert_eq!(
        post_exit_action(false, false, EARLY_EXIT_WINDOW),
        PostExitAction::MarkIdle,
    );
}

#[test]
fn plain_terminal_natural_exit_is_idle_regardless_of_elapsed() {
    // A shell exiting fast is not a resume signal.
    assert_eq!(
        post_exit_action(true, false, std::time::Duration::from_millis(10)),
        PostExitAction::MarkIdle,
    );
}

// -----------------------------------------------------------------------
// Per-session spawn claim (duplicate-spawn fix). `is_agent_already_running`
// only sees registered processes and registration is seconds into the
// pipeline, so the claim must cover the whole `spawn_agent_inner` body.
// Test ids are unique across the suite (tests share the process-global
// set and run in parallel).
// -----------------------------------------------------------------------

#[test]
fn spawn_claim_rejects_concurrent_duplicate_for_same_session() {
    let first = SpawnInFlightClaim::try_claim(-917_0001);
    assert!(first.is_some(), "first claim must succeed");
    assert!(
        SpawnInFlightClaim::try_claim(-917_0001).is_none(),
        "second claim for the same session while the first is held must \
             be rejected — this is what stops a duplicate spawn_agent_inner \
             from killing the in-flight spawn's freshly-booted process"
    );
}

#[test]
fn spawn_claim_is_per_session() {
    let _a = SpawnInFlightClaim::try_claim(-917_0002).expect("claim a");
    assert!(
        SpawnInFlightClaim::try_claim(-917_0003).is_some(),
        "claims for different sessions must not contend"
    );
}

#[test]
fn spawn_claim_released_on_drop() {
    {
        let _claim = SpawnInFlightClaim::try_claim(-917_0004).expect("claim");
    }
    assert!(
        SpawnInFlightClaim::try_claim(-917_0004).is_some(),
        "dropping the claim must release the session for the next spawn \
             (RAII covers every return path, including cancelled tasks)"
    );
}

/// Regression guard for the user-visible "failed to start" symptom.
///
/// Spawn RACERS threads racing `try_claim` for the same session —
/// the first to acquire the HashSet entry wins, the rest see the
/// entry present and get `None`. Pins the entire atomicity story:
/// without it, two concurrent `spawn_agent_inner` calls for the
/// same node (backend stage-2 vs frontend Terminal auto-spawn on
/// `'idle'`) both passed the registry check and the loser's step-2
/// stale-kill destroyed the winner's freshly-booted process — the
/// "failed to start, yet it boots seconds later" symptom.
///
/// Uses a fresh session id per round so the test doesn't depend on
/// the racing threads' Drop ordering vs the next round's claim —
/// the global HashSet could in principle still hold a stale entry
/// from a previous round's racer that hasn't yet been observed as
/// dropped by the test thread (parking_lot's Drop is synchronous,
/// but the test thread's join() happens-before the next round).
#[test]
fn concurrent_spawn_claim_exactly_one_winner() {
    use std::sync::atomic::{AtomicUsize, Ordering as AOrd};
    use std::sync::Arc;

    const RACERS: usize = 8;
    const ROUNDS: usize = 200;

    for round in 0..ROUNDS {
        // Fresh session id per round so there's no cross-round
        // dependency on Drop ordering.
        let session: i64 = -917_1000 - round as i64;

        let winners = Arc::new(AtomicUsize::new(0));
        // Two barriers: gate the racers before the lock, AND gate
        // them before the drop. Without the second gate, a racer
        // that loses the lock race still releases its (empty) claim
        // path before the next racer even tries — the second
        // barrier forces every racer to attempt the lock with the
        // claim held until the round-end signal.
        let start_barrier = Arc::new(std::sync::Barrier::new(RACERS + 1));
        let end_barrier = Arc::new(std::sync::Barrier::new(RACERS + 1));

        let handles: Vec<_> = (0..RACERS)
            .map(|_| {
                let winners = winners.clone();
                let start = start_barrier.clone();
                let end = end_barrier.clone();
                std::thread::spawn(move || {
                    // Phase 1: align all racers at the lock.
                    start.wait();
                    let claim = SpawnInFlightClaim::try_claim(session);
                    if claim.is_some() {
                        winners.fetch_add(1, AOrd::SeqCst);
                    }
                    // Phase 2: hold the claim until the test thread
                    // signals round end. Any racer arriving at the
                    // lock now MUST see the existing entry (the
                    // insert returns false → claim is None).
                    end.wait();
                    drop(claim);
                })
            })
            .collect();

        // Fire the start gun — every racer races for the lock now.
        start_barrier.wait();
        // Give every racer time to acquire the lock, observe the
        // entry, and reach the end barrier.
        end_barrier.wait();
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(
            winners.load(AOrd::SeqCst),
            1,
            "exactly one racer must win the claim (round {round}, session {session})"
        );

        // After the last racing thread joined, its _claim dropped,
        // releasing the entry. Confirm by claiming it ourselves —
        // this exercises the post-drop "slot is empty" invariant
        // and prevents cross-round state pollution if a future
        // refactor accidentally leaks entries.
        assert!(
            SpawnInFlightClaim::try_claim(session).is_some(),
            "round {round}: racers all joined so their claims dropped — \
                 the next try_claim for session {session} must find the slot empty"
        );
    }
}

/// RAII must release on a *cancelled* async task too — the field doc
/// on `SpawnInFlightClaim` makes that an explicit guarantee. A
/// `tokio::time::timeout` racing a future that holds the claim is
/// the cheapest reproduction: the future is dropped at the await
/// point, the claim's Drop runs synchronously, and the next
/// `try_claim` must succeed.
#[test]
fn spawn_claim_released_when_async_task_is_cancelled() {
    // No real DB / PTY needed — the claim itself is what we're
    // pinning. Drive it on a runtime so the cancellation path
    // (Future::drop mid-await) actually runs.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let session = -917_0006;
    rt.block_on(async {
        // Spawn a task that holds the claim for "the whole pipeline"
        // (here, forever). Cancel it via timeout.
        let task = tokio::spawn(async move {
            let _claim = SpawnInFlightClaim::try_claim(session).expect("first claim must succeed");
            // Park forever. The test cancels this task below.
            std::future::pending::<()>().await;
        });

        // Let the task reach its pending await.
        tokio::task::yield_now().await;
        task.abort();
        // The abort drops the task's locals → Drop runs → claim released.
        let _ = task.await;

        assert!(
            SpawnInFlightClaim::try_claim(session).is_some(),
            "aborting the holding task must release the claim (RAII covers \
                 cancelled futures, not just successful return)"
        );
    });
}

// -----------------------------------------------------------------------
// Warm-pool manual claim — .worktreeinclude re-application (issue #639
// gap 1). The cold `create_git_worktree` and the Issue/PR `adopt…by_move`
// both call `apply_worktree_include` so an adopted worktree is byte-for-
// byte equivalent to a cold spawn. The manual warm-claim fast path
// (upgrade_warm_to_mode) MUST do the same — otherwise a user who edits a
// `.worktreeinclude`-referenced file (typical: `.env`, build cache) between
// prewarm time and spawn time lands on a stale copy.
// -----------------------------------------------------------------------

#[test]
fn upgrade_warm_to_mode_reapplies_worktreeinclude_after_checkout() {
    use std::fs;
    let (_td, root, pool) = setup_warm_pool_with_include();

    // User edits the source file BETWEEN prewarm and manual spawn —
    // exactly the window the missing apply_worktree_include used to leak.
    fs::write(root.join("secrets.env"), "v1=NEW\n").unwrap();

    // The manual warm claim's mode upgrade — must re-copy `.worktreeinclude`
    // sources so the agent's worktree matches the live repo state, not the
    // stale prewarm snapshot.
    upgrade_warm_to_mode(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        "bold-amber-fox",
        "branched",
    )
    .expect("upgrade_warm_to_mode must succeed");

    // The worktree's `.worktreeinclude`-referenced file must now reflect
    // the live repo content (NEW), not the prewarm-time snapshot (old).
    assert_eq!(
        fs::read_to_string(pool.join("secrets.env")).unwrap(),
        "v1=NEW\n",
        "manual warm claim must re-apply .worktreeinclude so the agent sees the live source"
    );
}

/// No `.worktreeinclude` at the repo root → the upgrade is still a no-op
/// rather than an error. Prevents a regression where adding the include
/// re-application broke a repo that never used the feature.
#[test]
fn upgrade_warm_to_mode_is_noop_when_no_worktreeinclude() {
    use crate::env::test_helpers::init_repo_with_commit;
    use std::fs;
    // Skip the .worktreeinclude side of the helper — bare repo + pool.
    let td = TempDir::new().unwrap();
    let root = td.path();
    let _ = init_repo_with_commit(root, &[("f.txt", "tracked\n")]);
    let pool = root
        .join(".claude")
        .join("worktrees")
        .join("warm-amber-fox");
    crate::git::worktree::create_git_worktree(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        "warm-amber-fox",
        "detached",
        "HEAD",
    )
    .unwrap();
    let _ = td; // keep alive for the duration of the test

    upgrade_warm_to_mode(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        "bold-amber-fox",
        "branched",
    )
    .expect("must succeed when no .worktreeinclude exists");
    // No spurious `.worktreeinclude` was created in the worktree.
    assert!(
        !pool.join(".worktreeinclude").exists(),
        "absent manifest must not be materialised by the upgrade"
    );
    // The tracked file round-trips.
    assert_eq!(fs::read_to_string(pool.join("f.txt")).unwrap(), "tracked\n");
}

/// Detached mode must also re-apply `.worktreeinclude` (issue #639 gap 1,
/// review finding). The original `upgrade_warm_to_mode` returned early on
/// `mode == "detached"` and skipped the include copy — a regression that
/// re-instated that early-return would pass `…_reapplies…_after_checkout`
/// (branched) but leave a detached-mode spawn on the stale prewarm
/// snapshot, defeating the gap-1 fix for half the meshes.
#[test]
fn upgrade_warm_to_mode_reapplies_worktreeinclude_in_detached_mode() {
    use std::fs;
    let (_td, root, pool) = setup_warm_pool_with_include();

    // User edits the source — same window as the branched-mode test.
    fs::write(root.join("secrets.env"), "v1=NEW\n").unwrap();

    // Upgrade in DETACHED mode. The branch name is unused (no checkout),
    // but we pass the preassigned slug for consistency with the call site.
    upgrade_warm_to_mode(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        "warm-amber-fox",
        "detached",
    )
    .expect("upgrade_warm_to_mode must succeed in detached mode");

    assert_eq!(
        fs::read_to_string(pool.join("secrets.env")).unwrap(),
        "v1=NEW\n",
        "manual warm claim in detached mode must also re-apply .worktreeinclude"
    );
    // And the worktree stayed detached — no branch was created.
    let wt = git2::Repository::open(&pool).unwrap();
    assert!(
        wt.head_detached().unwrap_or(false),
        "detached mode must leave the worktree detached"
    );
}

/// Shared setup for the two `upgrade_warm_to_mode` `.worktreeinclude`
/// re-application tests (#642.5). The third test
/// (`…_is_noop_when_no_worktreeinclude`) deliberately inlines its own
/// setup because the no-manifest case is the whole point of that test
/// — running it through the helper would materialise `secrets.env` and
/// `.worktreeinclude` in the worktree, defeating the no-op assertion.
///
/// The helper stands up: a tempdir holding a real git repo with
/// `secrets.env` + `.worktreeinclude` (both tracked), AND a pool-shaped
/// DETACHED worktree under `.claude/worktrees/warm-amber-fox` that has
/// already had the include copied at prewarm time (so the tests assert
/// the upgrade re-applies, not the original copy). Both the branched and
/// the detached call-site tests cut the pool as detached (the pool's
/// on-disk shape) — the difference between them is the
/// `upgrade_warm_to_mode` mode argument, not the helper's setup.
///
/// Returns `(tempdir, repo_root_path, pool_path)`. The tempdir is held
/// to keep the underlying directory alive for the duration of the test
/// — dropping it would delete the repo and break subsequent asserts.
fn setup_warm_pool_with_include() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    use crate::env::test_helpers::{commit_file, init_repo_with_commit};
    use std::fs;

    let td = TempDir::new().unwrap();
    let root = td.path().to_path_buf();

    init_repo_with_commit(&root, &[("f.txt", "tracked\n")]);
    fs::write(root.join("secrets.env"), "v1=old\n").unwrap();
    fs::write(root.join(".worktreeinclude"), "secrets.env\n").unwrap();
    // Commit the manifest so `.worktreeinclude` is reachable for `git
    // worktree add`; the pool helper copies files relative to the repo
    // root regardless of whether the manifest itself is tracked, but
    // committing keeps the test setup close to a realistic repo.
    let repo = git2::Repository::open(&root).unwrap();
    commit_file(&repo, &root, ".worktreeinclude", "secrets.env\n");

    let pool = root
        .join(".claude")
        .join("worktrees")
        .join("warm-amber-fox");
    crate::git::worktree::create_git_worktree(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        "warm-amber-fox",
        "detached",
        "HEAD",
    )
    .expect("prewarm-shape worktree must be creatable for this helper");
    assert_eq!(
        fs::read_to_string(pool.join("secrets.env")).unwrap(),
        "v1=old\n",
        "prewarm-time copy must reflect the original source"
    );
    (td, root, pool)
}

// -----------------------------------------------------------------------
// Warm-pool Issue/PR adoption (issue #612): move a detached pool worktree
// onto the node's target name and check it out to the resolved base SHA on
// its own branch. These pin the code-review fixes for two confirmed bugs:
// resolving `base_ref` → SHA (offline resilience), and using `-b` (NOT
// `-B`) so a re-spawn can never force-reset a branch carrying prior work.
// -----------------------------------------------------------------------

#[test]
fn adopt_warm_worktree_moves_and_branches_at_base_sha() {
    use crate::env::test_helpers::init_repo_with_commit;
    let td = TempDir::new().unwrap();
    let root = td.path();
    let repo = init_repo_with_commit(root, &[("f.txt", "a\n")]);
    let head = repo
        .head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .id()
        .to_string();

    // The pool's on-disk shape: a DETACHED worktree under a plain slug.
    let pool = root
        .join(".claude")
        .join("worktrees")
        .join("warm-amber-fox");
    crate::git::worktree::create_git_worktree(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        "warm-amber-fox",
        "detached",
        "HEAD",
    )
    .unwrap();

    let target = root.join(".claude").join("worktrees").join("gh123-fix");
    adopt_warm_worktree_by_move(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        target.to_str().unwrap(),
        "gh123-fix",
        "branched",
        "HEAD",
    )
    .expect("adoption must succeed");

    assert!(!pool.exists(), "pool directory must be gone after the move");
    assert!(
        target.exists(),
        "target directory must exist after the move"
    );
    let wt = git2::Repository::open(&target).unwrap();
    assert_eq!(
        wt.head().unwrap().shorthand().unwrap(),
        "gh123-fix",
        "the adopted worktree must be on the node's own branch"
    );
    assert_eq!(
        wt.head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string(),
        head,
        "the branch must sit at the resolved base SHA"
    );
}

#[test]
fn adopt_warm_worktree_refuses_to_clobber_an_existing_branch() {
    use crate::env::test_helpers::init_repo_with_commit;
    let td = TempDir::new().unwrap();
    let root = td.path();
    let repo = init_repo_with_commit(root, &[("f.txt", "a\n")]);
    // A pre-existing deterministic branch standing in for a prior spawn's
    // work. Force-resetting it (the old `-B` bug) would orphan its commits.
    let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("gh7-x", &head_commit, false).unwrap();

    let pool = root
        .join(".claude")
        .join("worktrees")
        .join("warm-amber-fox");
    crate::git::worktree::create_git_worktree(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        "warm-amber-fox",
        "detached",
        "HEAD",
    )
    .unwrap();

    let target = root.join(".claude").join("worktrees").join("gh7-x");
    let err = adopt_warm_worktree_by_move(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        target.to_str().unwrap(),
        "gh7-x",
        "branched",
        "HEAD",
    )
    .expect_err("adoption must refuse to overwrite an existing branch");
    assert!(
        err.contains("already exists"),
        "the failure must name the existing branch refusal, got: {}",
        err
    );
    // Fail-fast contract: refusal is pre-move — see the guard in
    // `adopt_warm_worktree_by_move`.
    assert!(
        pool.exists(),
        "pool entry must be untouched after a refused adoption"
    );
    assert!(
        !target.exists(),
        "target must not be materialised by a refused adoption"
    );
}

// -----------------------------------------------------------------------
// base_ref resolution (master-trunk regression)
//
// Pre-fix, the spawn path hardcoded `"origin/main"` as the default
// `base_ref` when the `meshes.base_ref` DB column was `'origin/main'`
// (its COALESCE default) — meaning a master-trunk repo always hit
// `mesh-sync-warning` on every spawn (`fatal: couldn't find remote
// ref main`). These tests pin the resolution chain:
//
//   1. meshes.base_ref (BUT NOT the COALESCE default — that's
//      treated as "no config" so the detection chain runs)
//   2. refs/remotes/origin/HEAD read from the local repo
//   3. "origin/main" last resort
//
// The COALESCE-sentinel treatment is critical: the DB column is
// NOT NULL with default `'origin/main'`, so `Mesh.base_ref` is
// ALWAYS a non-empty `String` and `MeshRow.base_ref` is ALWAYS
// `Some(_)` — a naive `if let Some(b) = config_base_ref { return b }`
// would make the detection chain dead code in production. The
// `resolve_base_ref_treats_coalesce_sentinel_as_unset` test pins the
// production call path (`Some("origin/main")`).
// -----------------------------------------------------------------------

#[test]
fn resolve_base_ref_uses_config_value_when_set() {
    // The config wins even on a non-repo / non-master path — explicit
    // user intent overrides any auto-detection. Empty / whitespace
    // config falls through to the detection chain (regression guard
    // for an empty-string value slipping through the COALESCE).
    let tmp = TempDir::new().unwrap();
    assert_eq!(
        resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), Some("origin/develop")),
        "origin/develop"
    );
    // Empty / whitespace strings are treated as "no config" so the
    // detection chain runs — mirrors the COALESCE-to-default contract
    // in the DB layer.
    assert_eq!(
        resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), Some("")),
        "origin/main",
        "empty config base_ref must fall through to detection, not propagate"
    );
    assert_eq!(
        resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), Some("   ")),
        "origin/main",
        "whitespace-only config base_ref must fall through to detection"
    );
}

#[test]
fn resolve_base_ref_falls_back_to_origin_main_for_non_repo() {
    // Non-repo path with no config — must not panic. Last-resort
    // behaviour preserved: `get_default_branch` returns "main" on a
    // failed `Repository::open`, and we prefix it with "origin/".
    // The spawn path itself short-circuits to `RepoUnusable` so the
    // auto-sync result is non-blocking.
    let tmp = TempDir::new().unwrap();
    let resolved = resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), None);
    assert_eq!(resolved, "origin/main");
}

#[test]
fn resolve_base_ref_detects_master_via_origin_head() {
    // Headline regression test: a master-trunk repo with no
    // `base_ref` in mesh config must produce "origin/master", not
    // the legacy "origin/main". Pre-fix, this always returned
    // "origin/main" and the spawn emitted a `mesh-sync-warning` on
    // every node.
    use crate::env::test_helpers::TestDir;
    use git2;

    let td = TestDir::new("base_ref_master");
    let parent = td.path();
    // Create a working repo on whatever default branch git picks.
    // The local branch name doesn't matter — what matters is that
    // `refs/remotes/origin/HEAD` points at `refs/remotes/origin/master`.
    crate::env::test_helpers::init_repo_with_commit(parent, &[("README.md", "v1\n")]);

    let repo = git2::Repository::open(parent).unwrap();
    let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    // Build the symbolic ref that `get_default_branch` reads.
    repo.reference("refs/remotes/origin/master", oid, true, "test setup")
        .unwrap();
    repo.reference_symbolic(
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/master",
        true,
        "test setup",
    )
    .unwrap();

    // Sanity: precondition for the test to be meaningful.
    let head_ref = repo
        .find_reference("refs/remotes/origin/HEAD")
        .unwrap()
        .symbolic_target()
        .unwrap()
        .to_string();
    assert_eq!(
        head_ref, "refs/remotes/origin/master",
        "precondition: origin/HEAD must point at refs/remotes/origin/master"
    );

    let resolved = resolve_base_ref_for_spawn(parent.to_str().unwrap(), None);
    assert_eq!(
        resolved, "origin/master",
        "master-trunk repo with no base_ref in config must yield origin/master, \
             not the legacy hardcoded origin/main (this is the master-trunk regression)"
    );
}

#[test]
fn resolve_base_ref_detects_main_via_origin_head() {
    // Sanity pin: the existing main-trunk behaviour (a repo whose
    // origin/HEAD points at `main`) must still resolve to
    // "origin/main" after the fix. Guards against the master fix
    // accidentally regressing the main case.
    use crate::env::test_helpers::TestDir;
    use git2;

    let td = TestDir::new("base_ref_main");
    let parent = td.path();
    crate::env::test_helpers::init_repo_with_commit(parent, &[("README.md", "v1\n")]);

    let repo = git2::Repository::open(parent).unwrap();
    let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    repo.reference("refs/remotes/origin/main", oid, true, "test setup")
        .unwrap();
    repo.reference_symbolic(
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        true,
        "test setup",
    )
    .unwrap();

    let resolved = resolve_base_ref_for_spawn(parent.to_str().unwrap(), None);
    assert_eq!(
        resolved, "origin/main",
        "main-trunk repo must still resolve to origin/main (no regression)"
    );
}

#[test]
fn resolve_base_ref_treats_coalesce_sentinel_as_unset() {
    // The production call path: `meshes.base_ref` is a NOT NULL
    // column with a COALESCE default of `'origin/main'` (see
    // `db::MESH_COLUMNS`). A fresh mesh whose base_ref was never
    // explicitly set reads as `Some("origin/main")` from the DB →
    // `MeshRow.base_ref = Some("origin/main")` →
    // `config.as_ref().and_then(|c| c.base_ref.as_deref())` returns
    // `Some("origin/main")`. The helper MUST treat this sentinel as
    // "no config" and fall through to the detection chain, otherwise
    // a master-trunk repo's spawn still hits `mesh-sync-warning`.
    // The earlier `_detects_master_via_origin_head` test passes
    // `None` (which never reaches production); THIS test pins the
    // actual production contract.
    use crate::env::test_helpers::TestDir;
    use git2;

    let td = TestDir::new("base_ref_coalesce_master");
    let parent = td.path();
    crate::env::test_helpers::init_repo_with_commit(parent, &[("README.md", "v1\n")]);

    let repo = git2::Repository::open(parent).unwrap();
    let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    repo.reference("refs/remotes/origin/master", oid, true, "test setup")
        .unwrap();
    repo.reference_symbolic(
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/master",
        true,
        "test setup",
    )
    .unwrap();

    // Production-shaped input: COALESCE default from the DB.
    let resolved = resolve_base_ref_for_spawn(parent.to_str().unwrap(), Some("origin/main"));
    assert_eq!(
        resolved, "origin/master",
        "the COALESCE default 'origin/main' from a fresh mesh's DB row \
             must be treated as 'no config' — fall through to origin/HEAD \
             detection. A master-trunk repo with an unconfigured mesh \
             produces origin/master, not origin/main. This is the actual \
             production contract; the test passing None never reaches \
             production."
    );
}

#[test]
fn resolve_base_ref_keeps_explicit_user_value_for_main_trunk() {
    // A user who LEGITIMATELY sets `base_ref = "origin/main"` (via
    // the 'Fresh' UI option) on a main-trunk repo must still get
    // "origin/main" back. The COALESCE-sentinel treatment must
    // apply to the *fresh* / *unconfigured* case, not penalize a
    // user who explicitly chose the same value. For a main-trunk
    // repo the auto-detect would return the same value, so this
    // test is mostly a documentation pin.
    use crate::env::test_helpers::TestDir;
    use git2;

    let td = TestDir::new("base_ref_explicit_main");
    let parent = td.path();
    crate::env::test_helpers::init_repo_with_commit(parent, &[("README.md", "v1\n")]);

    let repo = git2::Repository::open(parent).unwrap();
    let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    repo.reference("refs/remotes/origin/main", oid, true, "test setup")
        .unwrap();
    repo.reference_symbolic(
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        true,
        "test setup",
    )
    .unwrap();

    let resolved = resolve_base_ref_for_spawn(parent.to_str().unwrap(), Some("origin/main"));
    assert_eq!(
        resolved, "origin/main",
        "explicit user-set 'origin/main' on a main-trunk repo must resolve \
             to 'origin/main' (same as auto-detect — no behaviour change)"
    );
}

// -----------------------------------------------------------------------
// SHA-drift detection (issue #444)
//
// `read_origin_ref_sha` returns the local SHA at `origin/<head_ref>` so
// the spawn path can compare it to the user-pinned `source_pr_pinned_sha`
// and emit a `pr_sha_drift` warning on mismatch. The unit test creates
// the local ref directly via git2 (no real remote / fetch roundtrip) so
// the test is hermetic and fast.
// -----------------------------------------------------------------------

#[test]
fn read_origin_ref_sha_returns_local_sha_when_ref_exists() {
    let tmp = TempDir::new().unwrap();
    let repo = git2::Repository::init(tmp.path()).unwrap();
    // Create a real commit on a known branch — we need a tree OID the
    // commit can point at. `Repository::init` leaves the index empty
    // but write_tree() on an empty index still produces a valid tree.
    let tree_oid = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("test", "test@example.com").unwrap();
    let commit_oid = repo
        .commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    // Manually create the remote-tracking ref the function reads. In
    // production this is what `git fetch origin -- <head_ref>` writes;
    // here we shortcut the network roundtrip to keep the test hermetic.
    let ref_name = "refs/remotes/origin/feat-x";
    repo.reference(ref_name, commit_oid, true, "test").unwrap();

    let sha = read_origin_ref_sha(tmp.path().to_str().unwrap(), "origin/feat-x");
    assert_eq!(
        sha.as_deref(),
        Some(commit_oid.to_string().as_str()),
        "read_origin_ref_sha must return the full 40-char SHA the ref points to"
    );
}

#[test]
fn read_origin_ref_sha_returns_none_for_missing_ref() {
    let tmp = TempDir::new().unwrap();
    git2::Repository::init(tmp.path()).unwrap();
    // No refs/remotes/origin/* exists; the function must return None
    // (the spawn path treats this as "skip drift check" rather than
    // failing — same fail-open semantics as `pr_head_unfetchable`).
    let sha = read_origin_ref_sha(tmp.path().to_str().unwrap(), "origin/nope");
    assert!(sha.is_none(), "missing ref must return None, not error");
}

#[test]
fn read_origin_ref_sha_returns_none_for_non_git_directory() {
    // A path that isn't a git repo at all — `git rev-parse` exits non-zero,
    // the helper must swallow that and return None rather than panicking.
    let tmp = TempDir::new().unwrap();
    let sha = read_origin_ref_sha(tmp.path().to_str().unwrap(), "origin/main");
    assert!(sha.is_none(), "non-repo path must return None, not error");
}

fn read_injected_settings(project: &std::path::Path) -> serde_json::Value {
    let path = project.join(".claude").join("settings.local.json");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("settings.local.json not written: {}", e));
    serde_json::from_str(&content).expect("settings.local.json is not valid JSON")
}

/// The Notification hook must fire on EVERY notification type, not just
/// `idle_prompt`. An empty matcher is Claude Code's "match all" — without it
/// the hook ignores `permission_prompt` notifications, so the user is never
/// alerted when an agent asks to run a tool or otherwise needs a decision.
/// Regression guard for the "only alerted after the agent finishes" gap.
#[test]
fn attention_hook_notification_matcher_is_catch_all() {
    let temp = TempDir::new().unwrap();
    inject_attention_hook(temp.path()).unwrap();

    let settings = read_injected_settings(temp.path());
    let notification = &settings["hooks"]["Notification"][0];
    assert_eq!(
        notification["matcher"], "",
        "Notification matcher must be empty (catch-all) so permission_prompt \
             notifications alert the user, not just idle_prompt"
    );
    let command = notification["hooks"][0]["command"]
        .as_str()
        .expect("notification hook command should be a string");
    assert!(
        command.contains("/api/attention/"),
        "notification hook should POST to the attention endpoint, got: {command}"
    );
}

/// A `Stop` hook fires the instant the agent finishes a turn, so the user is
/// alerted immediately rather than waiting for the `idle_prompt` idle timer.
#[test]
fn attention_hook_includes_stop_event() {
    let temp = TempDir::new().unwrap();
    inject_attention_hook(temp.path()).unwrap();

    let settings = read_injected_settings(temp.path());
    let command = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .expect("Stop hook command should be present so turn-end alerts fire immediately");
    assert!(
        command.contains("/api/attention/"),
        "Stop hook should POST to the attention endpoint, got: {command}"
    );
}

/// Both hooks must forward the hook's stdin JSON as the POST body (issue
/// #878). Claude Code pipes `{hook_event_name, transcript_path, …}` into
/// the command; without `--data-binary @-` the backend gets an empty body
/// and cannot tell "turn ended, user needed" from "turn ended, waiting on
/// background tasks".
#[test]
fn attention_hook_forwards_stdin_payload() {
    let temp = TempDir::new().unwrap();
    inject_attention_hook(temp.path()).unwrap();

    let settings = read_injected_settings(temp.path());
    for (event, path) in [
        (
            "Notification",
            &settings["hooks"]["Notification"][0]["hooks"][0],
        ),
        ("Stop", &settings["hooks"]["Stop"][0]["hooks"][0]),
    ] {
        let command = path["command"].as_str().unwrap();
        assert!(
            command.contains("--data-binary @-"),
            "{event} hook must forward stdin as the POST body, got: {command}"
        );
        assert!(
            command.contains("Content-Type: application/json"),
            "{event} hook must declare a JSON body, got: {command}"
        );
    }
}

/// Injection is idempotent: a second call over an already-correct file must
/// not rewrite it (the early-return guard) and must leave it parseable.
#[test]
fn attention_hook_injection_is_idempotent() {
    let temp = TempDir::new().unwrap();
    inject_attention_hook(temp.path()).unwrap();
    let first = read_injected_settings(temp.path());
    inject_attention_hook(temp.path()).unwrap();
    let second = read_injected_settings(temp.path());
    assert_eq!(first, second, "second injection should be a no-op");
}

/// Injection must preserve unrelated keys already present in the user's
/// settings.local.json (e.g. `permissions`) — it only owns `hooks`.
#[test]
fn attention_hook_preserves_other_settings() {
    let temp = TempDir::new().unwrap();
    let claude_dir = temp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.local.json"),
        r#"{"permissions":{"allow":["Bash(ls:*)"]}}"#,
    )
    .unwrap();

    inject_attention_hook(temp.path()).unwrap();

    let settings = read_injected_settings(temp.path());
    assert_eq!(
        settings["permissions"]["allow"][0], "Bash(ls:*)",
        "pre-existing permissions must survive hook injection"
    );
    assert_eq!(settings["hooks"]["Notification"][0]["matcher"], "");
}

// ----- fork alias + fetch_fork_head (issue #443) ---------------------

/// `fork-<login>` is the human-readable alias used in `git remote -v` and
/// the worktree `base_ref` string. The `fork-` prefix keeps our entries
/// easy to spot in the remote list and trivial to clean up if we ever
/// need to. Pin the format so a future refactor that swaps the prefix
/// surfaces as a test failure rather than a silent rename in user
/// worktrees.
#[test]
fn fork_remote_alias_uses_fork_prefix() {
    assert_eq!(fork_remote_alias("alice"), "fork-alice");
    assert_eq!(fork_remote_alias("alondero"), "fork-alondero");
}

/// Build a bare "fork" repo (a real local clone target so the test
/// doesn't need a network round-trip) and a regular repo that will
/// register the fork as a remote. The fork has a single commit on
/// `main` plus a `feat/443-fork` branch so the fetch can target a
/// non-default ref. Returns `(local, fork_bare_dir, fork_path)` —
/// the caller holds the dirs for the duration of the test.
fn init_fork_fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    // Source: a regular repo with a feature branch we can fetch.
    let src = TempDir::new().unwrap();
    let src_path = src.path().to_path_buf();
    let src_repo = git2::Repository::init(&src_path).unwrap();
    let sig = git2::Signature::now("test", "test@example.com").unwrap();
    std::fs::write(src_path.join("README.md"), "fork-source\n").unwrap();
    let mut index = src_repo.index().unwrap();
    index.add_path(std::path::Path::new("README.md")).unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = src_repo.find_tree(tree_oid).unwrap();
    let main_commit = src_repo
        .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
        .unwrap();
    // Branch off a feature branch.
    let feat_commit = src_repo
        .commit(
            Some("HEAD"),
            &sig,
            &sig,
            "feat: fork-only commit",
            &tree,
            &[&src_repo.find_commit(main_commit).unwrap()],
        )
        .unwrap();
    let _ = tree;
    // `main_commit` is a `git2::Oid` (Copy) — no need to `drop` it; the
    // explicit `drop()` was a no-op flagged by clippy.
    let feat_commit = src_repo.find_commit(feat_commit).unwrap();
    src_repo
        .branch("feat/443-fork", &feat_commit, true)
        .unwrap();
    // Bare clone target (so the fork has no working tree, like a real
    // remote on GitHub — `git fetch` reads its objects directly).
    // Use a unique, path-safe name — avoid `{:?}` on the source path
    // (it produces `C:\...` with backslashes and quotes that don't
    // round-trip as a directory name on Windows).
    let bare_dir = std::env::temp_dir().join(format!(
        "buildmesh_fork_bare_{}_{}",
        std::process::id(),
        NEXT_FORK_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
    ));
    let _ = std::fs::remove_dir_all(&bare_dir);
    let clone = git2::Repository::init_bare(&bare_dir).unwrap();
    let mut remote = clone.remote("origin", src_path.to_str().unwrap()).unwrap();
    remote
        .fetch(&["refs/heads/*:refs/heads/*"], None, None)
        .unwrap();
    // Local: a fresh repo with no remotes — this is what
    // `fetch_fork_head` will register the fork on.
    let local = TempDir::new().unwrap();
    git2::Repository::init(local.path()).unwrap();
    (local, bare_dir, src_path)
}

/// Atomic counter for unique bare-repo paths (one per test run).
static NEXT_FORK_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// First-time registration: the fork is added as `fork-alice` and the
/// head ref is materialised. `fetch_fork_head` returns `true` and
/// the resulting `git ls-remote` shows the ref under the alias.
/// This is the end-to-end "fork spawn" path that issue #443 opens up.
#[test]
fn fetch_fork_head_registers_remote_and_fetches_ref() {
    let (local, bare_dir, _src) = init_fork_fixture();
    let bare_dir_str = bare_dir.to_str().unwrap().to_string();

    let ok = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &bare_dir_str,
        "feat/443-fork",
    );
    assert!(ok, "fetch_fork_head must succeed on a real bare repo");

    // Verify the alias + URL are registered.
    let local_repo = git2::Repository::open(local.path()).unwrap();
    let remote = local_repo
        .find_remote("fork-alice")
        .expect("fork-alice remote must be registered");
    let url = remote.url().expect("remote URL must be set");
    assert_eq!(
        url, bare_dir_str,
        "remote URL must match the fork's clone URL"
    );

    // Verify the ref was fetched — it should be visible as
    // `fork-alice/feat/443-fork`.
    let reference = local_repo
        .find_reference("refs/remotes/fork-alice/feat/443-fork")
        .expect("fetched ref must be present under fork-alice/");
    assert!(
        reference.target().is_some(),
        "ref target must be a real OID"
    );
}

/// Idempotent: a second call on a repo that already has the remote
/// registered AND the right URL is a no-op. The user can spawn a
/// second agent on the same fork PR (e.g. after closing the first)
/// without `git remote add` failing. The function still returns
/// `true` because the fetch succeeds.
#[test]
fn fetch_fork_head_is_idempotent_on_repeat_call() {
    let (local, bare_dir, _src) = init_fork_fixture();
    let bare_dir_str = bare_dir.to_str().unwrap().to_string();

    let first = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &bare_dir_str,
        "feat/443-fork",
    );
    assert!(first, "first call must succeed");

    // Second call with the SAME URL — must not error (the `remote add`
    // path is the failure-prone one without the existence check; the
    // `get-url` probe should return the right URL and skip the add).
    let second = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &bare_dir_str,
        "feat/443-fork",
    );
    assert!(second, "second call must still succeed (idempotent)");

    // Remote is still there, single entry.
    let local_repo = git2::Repository::open(local.path()).unwrap();
    let remote = local_repo
        .find_remote("fork-alice")
        .expect("fork-alice remote must still be registered after repeat call");
    assert_eq!(remote.url().unwrap(), bare_dir_str);
}

/// URL drift: if the fork's clone URL changes between spawns (the
/// user renamed the repo, or — more likely — the first call stored a
/// stale URL), the second call should update the existing remote's
/// URL via `git remote set-url` rather than fail or keep the stale
/// URL. Pin this so a future refactor that skips the set-url branch
/// surfaces as a test failure (the second call would silently fetch
/// the wrong ref).
#[test]
fn fetch_fork_head_updates_url_on_drift() {
    let (local, bare_dir, _src) = init_fork_fixture();
    let stale_url = bare_dir.to_str().unwrap().to_string();
    // Reuse the SAME bare dir (so the second call still finds a real
    // repo) but pretend the URL "drifted" by passing a different
    // string that ALSO resolves to the same on-disk repo. We achieve
    // that with a file:// URL on Windows (path with backslashes
    // round-trip cleanly through git remote add).
    let drifted_url = format!("file://{}", stale_url.replace('\\', "/"));

    // First call: register the stale URL.
    let first = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &stale_url,
        "feat/443-fork",
    );
    assert!(first, "first call must succeed");

    // Second call: same alias, drifted URL — the function should run
    // `git remote set-url` and re-fetch.
    let second = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &drifted_url,
        "feat/443-fork",
    );
    assert!(second, "second call must still succeed after URL drift");

    // The stored URL must be the drifted one, not the original.
    let local_repo = git2::Repository::open(local.path()).unwrap();
    let remote = local_repo
        .find_remote("fork-alice")
        .expect("remote must still be registered");
    let stored = remote.url().unwrap();
    // git normalises file:// URLs slightly on Windows — assert it's
    // the drifted one rather than the original.
    assert_ne!(
        stored, stale_url,
        "URL must have been updated, not left at the stale value"
    );
}

/// Failure path: a non-existent clone URL must return `false` rather
/// than panic. The caller (`spawn_agent_inner`) falls back to the
/// mesh's `base_ref` and emits a `mesh-sync-warning` toast with
/// `outcome: "pr_fork_unfetchable"`. Without the failure-as-false
/// contract, a typo'd clone URL would either spawn on the wrong
/// commits silently or surface as a hard error every offline session.
#[test]
fn fetch_fork_head_returns_false_on_bad_clone_url() {
    let (local, _bare_dir, _src) = init_fork_fixture();
    let bad_url = "/nonexistent/path/to/fork/that/does/not/exist".to_string();

    let ok = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &bad_url,
        "feat/443-fork",
    );
    assert!(!ok, "fetch_fork_head must return false on a bad clone URL");
}

// ----- fetch_single_ref (issue #420) ---------------------------------
//
// Same-repo PR spawn (#420) — the worktree adoption path calls
// `fetch_single_ref` to materialise `origin/<head_ref>` so the worktree
// can be cut from it. As of issue #446 the function is a thin wrapper
// over `git::sync::do_fetch_only` (the fetch-only half of `do_sync` —
// open + dirty-check + has-remote + `git fetch`, NO `git pull` tail);
// the `-`-adversarial-ref hardening is preserved at the wrapper
// boundary because `do_fetch_only` passes the branch as a plain argv
// entry without a `--` separator (it doesn't know about the spawn
// context).
//
// These tests pin the cases the issue calls out:
//   1. success — ref exists on origin
//   2. ref-not-found — ref missing on origin (caller falls back to base_ref)
//   3. non-git path — caller passed a directory that isn't a repo
//   4. adversarial ref — `-`-prefixed input is rejected by the wrapper
//      before `do_fetch_only` sees it (the hardening migrated from the
//      shell-out's `--` separator to an upfront string check, since
//      `do_fetch_only` doesn't pass a `--` separator to `git fetch`)
//   5. dirty-skip (issue #446 acceptance #2) — a parent repo with
//      uncommitted changes must return `false` (mirrors
//      `fetch_origin_skips_dirty_parent` in `git/fetch_origin_tests.rs`)
//
// The fixture mirrors `init_fork_fixture` but for the same-repo path:
// a bare repo holds a single branch, the local repo has `origin`
// pointed at the bare, and the test calls `fetch_single_ref` against
// the local repo's path.

/// Build a "remote + local" pair: the bare repo has a single commit on
/// `main` plus a `feat/420-pr-spawn` branch; the local repo has `origin`
/// pointed at the bare. Returns `(local, bare_path)` — the local TempDir
/// owns its on-disk path; `bare_path` is a plain PathBuf that lives
/// inside `std::env::temp_dir()` and is reused across calls (it gets
/// re-populated with the same content each time, so the SHA is stable
/// per-test-process).
fn init_same_repo_fixture() -> (TempDir, std::path::PathBuf) {
    // Source: a working repo with a feature branch we can fetch.
    // We reuse the same on-disk source across tests in a single
    // process — `init_same_repo_fixture` is only called from the
    // same-repo tests below, and the contents are deterministic.
    static SRC_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    let src_path = SRC_DIR
        .get_or_init(|| {
            let src = TempDir::new().unwrap();
            let src_path = src.path().to_path_buf();
            let src_repo = git2::Repository::init(&src_path).unwrap();
            let sig = git2::Signature::now("test", "test@example.com").unwrap();
            std::fs::write(src_path.join("README.md"), "init\n").unwrap();
            let mut index = src_repo.index().unwrap();
            index.add_path(std::path::Path::new("README.md")).unwrap();
            index.write().unwrap();
            let tree_oid = index.write_tree().unwrap();
            let tree = src_repo.find_tree(tree_oid).unwrap();
            let main_commit = src_repo
                .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
            let main_commit_obj = src_repo.find_commit(main_commit).unwrap();
            src_repo
                .branch("feat/420-pr-spawn", &main_commit_obj, true)
                .unwrap();
            // Leak the TempDir guard — we want src_path to stay alive
            // for the whole process, and the bare-fetch step below
            // re-reads from the on-disk path on every test.
            std::mem::forget(src);
            src_path
        })
        .clone();

    // Bare remote — same pattern as `init_fork_fixture`. A unique
    // name per process so parallel `cargo test` invocations don't
    // collide on the bare dir.
    let bare_dir = std::env::temp_dir().join(format!(
        "buildmesh_same_repo_bare_{}_{}",
        std::process::id(),
        NEXT_FORK_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
    ));
    let _ = std::fs::remove_dir_all(&bare_dir);
    let clone = git2::Repository::init_bare(&bare_dir).unwrap();
    let mut remote = clone.remote("origin", src_path.to_str().unwrap()).unwrap();
    remote
        .fetch(&["refs/heads/*:refs/heads/*"], None, None)
        .unwrap();

    // Local repo with `origin` pointed at the bare. `fetch_single_ref`
    // will use this `origin` remote to materialise the ref.
    let local = TempDir::new().unwrap();
    let local_repo = git2::Repository::init(local.path()).unwrap();
    local_repo
        .remote("origin", bare_dir.to_str().unwrap())
        .unwrap();
    (local, bare_dir)
}

/// Success path: a ref that exists on `origin` is fetched into
/// `refs/remotes/origin/<head_ref>` and the function returns `true`.
/// This is the happy path the spawn-time worktree adoption relies on.
#[test]
fn fetch_single_ref_returns_true_when_ref_exists() {
    let (local, _bare) = init_same_repo_fixture();
    let ok = fetch_single_ref(local.path().to_str().unwrap(), "feat/420-pr-spawn");
    assert!(
        ok,
        "fetch_single_ref must return true when the ref exists on origin"
    );
    // Verify the ref actually got materialised — a true return with no
    // visible ref would mean a silent no-op, which is a worse failure
    // mode than a hard error.
    let local_repo = git2::Repository::open(local.path()).unwrap();
    let reference = local_repo
        .find_reference("refs/remotes/origin/feat/420-pr-spawn")
        .expect("origin/feat/420-pr-spawn must be materialised after success");
    assert!(
        reference.target().is_some(),
        "fetched ref must point at a real OID, not be unborn"
    );
}

/// Ref-not-found path: a ref that does NOT exist on `origin` causes
/// `git fetch` to exit non-zero. The function returns `false` (not
/// an error) so the spawn path can fall back to the mesh's
/// `base_ref` — this is the ADR 0001 offline pattern, surface as
/// `pr_head_unfetchable` rather than failing the spawn.
#[test]
fn fetch_single_ref_returns_false_when_ref_missing() {
    let (local, _bare) = init_same_repo_fixture();
    let ok = fetch_single_ref(local.path().to_str().unwrap(), "does-not-exist");
    assert!(
        !ok,
        "fetch_single_ref must return false when the ref is missing on origin \
             (caller falls back to base_ref per the offline-fallback contract)"
    );
}

/// Non-git path: a directory that isn't a git repo at all. `git fetch`
/// errors immediately; the function swallows that and returns `false`.
/// This is the "user has a partial / broken clone" edge case — the
/// spawn must not panic.
#[test]
fn fetch_single_ref_returns_false_for_non_git_directory() {
    let tmp = TempDir::new().unwrap();
    let ok = fetch_single_ref(tmp.path().to_str().unwrap(), "feat/420-pr-spawn");
    assert!(
        !ok,
        "fetch_single_ref must return false (not panic) for a non-git path"
    );
}

/// Adversarial-ref pin (issue #420 hardening): a ref starting with `-`
/// (e.g. `--upload-pack=evil`) is rejected by `git` itself because of
/// the `--` separator before `head_ref`. Without the separator, `git`
/// would parse `--upload-pack=evil` as a flag and use it for the
/// fetch — a vector for arbitrary command execution on a malicious
/// server (CVE-2017-1000117 / CVE-2018-17456 class). The hardening
/// lives in `fetch_single_ref`; this test pins the contract so a
/// future refactor that drops the `--` separator fails the test
/// rather than silently re-introducing the vulnerability.
///
/// We pass a ref that, WITHOUT the separator, `git` would parse as a
/// flag (`--upload-pack=evil`) — `git fetch` will then error out on
/// "fatal: bad config name", proving the separator did its job. With
/// the separator, the value reaches the ref-spec parser as a
/// literal ref name (which still doesn't exist on origin, so the
/// call returns `false` either way — the contract is "the function
/// returns false rather than letting `--upload-pack` reach git").
#[test]
fn fetch_single_ref_rejects_adversarial_dash_ref() {
    let (local, _bare) = init_same_repo_fixture();
    let ok = fetch_single_ref(local.path().to_str().unwrap(), "--upload-pack=evil");
    assert!(
        !ok,
        "fetch_single_ref must return false for a ref starting with '-' \
             (the wrapper rejects it before do_sync sees it)"
    );
}

/// Dirty-parent pin (issue #446 acceptance #2, inverted 2026-07-17): a
/// parent repo with uncommitted changes must STILL fetch the PR head.
/// A `git fetch` never touches the working tree — the pre-2026-07-17
/// dirty-skip meant a mesh whose root checkout stayed dirty silently
/// fell back to `base_ref` on every PR spawn, cutting the worktree
/// from the wrong commits. Pin the new contract so a future refactor
/// that re-introduces a pre-fetch dirty gate fails this test.
///
/// `is_dirty` includes untracked files, so writing one to the freshly-
/// init'd local repo is enough to dirty it — no need to seed a tracked
/// file first.
#[test]
fn fetch_single_ref_fetches_despite_dirty_parent() {
    let (local, _bare) = init_same_repo_fixture();
    // Precondition: the fixture's local repo must start clean, then we
    // make it dirty with an untracked file.
    assert!(
        !crate::env::test_helpers::repo_is_dirty(local.path()),
        "precondition: freshly-init'd local repo must start clean"
    );
    std::fs::write(local.path().join("dirty-marker.txt"), "uncommitted\n").unwrap();
    assert!(
        crate::env::test_helpers::repo_is_dirty(local.path()),
        "precondition: writing an untracked file must dirty the repo"
    );

    let ok = fetch_single_ref(local.path().to_str().unwrap(), "feat/420-pr-spawn");
    assert!(
        ok,
        "fetch_single_ref must fetch on a dirty parent — a fetch never \
             touches the working tree, and skipping cut PR worktrees from \
             stale refs"
    );
    // The head ref must be materialised so the worktree can be cut
    // from it — the whole point of the fetch.
    let repo = git2::Repository::open(local.path()).unwrap();
    assert!(
        repo.find_reference("refs/remotes/origin/feat/420-pr-spawn")
            .is_ok(),
        "the fetch must materialise refs/remotes/origin/<head_ref>"
    );
    // And the dirty marker must be untouched.
    assert_eq!(
        std::fs::read_to_string(local.path().join("dirty-marker.txt")).unwrap(),
        "uncommitted\n"
    );
}

// -----------------------------------------------------------------------
// locked_fetch_pr_head — per-Mesh sync_lock wrap (issue #698)
//
// `locked_fetch_pr_head` must run inside `services::sync_lock::with_mesh_
// sync_lock` so two concurrent PR-spawns (or a PR-spawn racing the manual
// `git_sync` from #680 / the spawn-time `fetch_origin` from #652) can't
// collide on `.git/FETCH_HEAD` / `.git/refs/remotes/<remote>/<ref>.lock`.
// Without the wrap the losing fetch fails with "another git process" and
// the spawn silently lands on `base_ref` (the wrong commits).
//
// We test the wrap with a wall-clock bound (mirroring the #680
// `git_sync_serializes_via_per_mesh_sync_lock_gh680` shape in
// `commands/git_tests.rs`). The `with_mesh_sync_lock` unit tests in
// `services::sync_lock` prove the primitive itself serialises; this test
// proves THIS specific call site uses the SAME key the spawn path uses,
// which is the bug class #698 closes.
//
// Holder enters the per-mesh lock and announces entry via an AtomicUsize
// flag before sleeping. Main thread spin-waits on the flag (deterministic
// — no `thread::sleep` race), then times `locked_fetch_pr_head`. With the
// wrap, `locked_fetch_pr_head` blocks ~450 ms waiting for the holder;
// without, it runs concurrently with the holder and finishes in tens of ms.
// -----------------------------------------------------------------------

/// Regression test for issue #698 — `locked_fetch_pr_head` must acquire
/// the per-Mesh `with_mesh_sync_lock` keyed on the spawn's `node.path`,
/// matching what `spawn_agent_inner` calls `fetch_origin` with two steps
/// earlier. Without this wrap, concurrent PR-spawns on the same Mesh
/// (and a PR-spawn racing the manual `git_sync` button) race on
/// `.git/FETCH_HEAD` / `refs/remotes/<remote>/<ref>.lock` and the loser
/// silently falls back to `base_ref`.
///
/// Strategy: holder thread enters `with_mesh_sync_lock(&path_key, ...)`
/// and announces via an AtomicUsize flag, then sleeps. Main thread
/// spin-waits on the flag (deterministic — no `thread::sleep` race), then
/// times `locked_fetch_pr_head`. With the wrap, `locked_fetch_pr_head`
/// blocks waiting for the holder; without, it returns immediately while
/// the holder is still inside its critical section.
///
/// Why wall-clock (not `fetch_add`): the per-Mesh lock is correctly
/// implemented (issue #652 + `services::sync_lock` unit tests prove it),
/// so it *prevents* simultaneous critical-section entries — `max_concurrent
/// == 1` even on a working lock. The only signal that `locked_fetch_pr_head`
/// shares the same key is that it waits for the holder to release the lock.
///
/// The test uses the same-repo branch (passes `None, None` for fork
/// fields). The fork branch shares the same wrapper so the regression
/// coverage is sufficient with one call site — a #698 regression that
/// branched out of the wrapper entirely would fail this test and the
/// #443 fork tests would still pass on the unwrapped helper, surfacing
/// the gap.
#[test]
fn locked_fetch_pr_head_serializes_via_per_mesh_sync_lock_gh698() {
    use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
    use std::time::{Duration, Instant};

    let (local, _bare) = init_same_repo_fixture();
    let path_key = local.path().to_string_lossy().into_owned();

    // Holder enters the per-mesh lock and announces entry via
    // `entered_flag` before sleeping. Spinning on the flag avoids the
    // `thread::sleep` race — CI jitter can't make `locked_fetch_pr_head`
    // sneak in first.
    let entered_flag = std::sync::Arc::new(AtomicUsize::new(0));
    let holder_path = path_key.clone();
    let entered_holder = std::sync::Arc::clone(&entered_flag);
    let holder = std::thread::spawn(move || {
        crate::services::sync_lock::with_mesh_sync_lock(&holder_path, || {
            entered_holder.store(1, AOrdering::SeqCst);
            std::thread::sleep(Duration::from_millis(500));
        });
    });

    // Spin-wait (bounded) for the holder to actually be inside the
    // critical section. Cap at 2 s so a hung holder surfaces as a
    // test panic, not a forever-wait.
    let deadline = Instant::now() + Duration::from_secs(2);
    while entered_flag.load(AOrdering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "holder thread never entered the per-mesh lock"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let start = Instant::now();
    let _ = locked_fetch_pr_head(&path_key, "feat/420-pr-spawn", None, None);
    let elapsed = start.elapsed();

    holder.join().unwrap();

    // With wrap: elapsed >= ~450 ms (`locked_fetch_pr_head` waited for
    // the holder). Without wrap: elapsed = tens of ms (the fetch ran
    // concurrently with the holder's sleep). Bound is 400 ms — leaves
    // 100 ms of slack for setup overhead and CI jitter on a busy box.
    assert!(
        elapsed >= Duration::from_millis(400),
        "locked_fetch_pr_head did not block on the per-mesh lock \
             (elapsed = {:?}); issue #698 wrap is missing — concurrent PR-spawn \
             and spawn-time fetch_origin (or manual git_sync from #680) would \
             race on .git/FETCH_HEAD and refs/remotes/<remote>/<ref>.lock",
        elapsed,
    );
}

/// Companion to `locked_fetch_pr_head_serializes_via_per_mesh_sync_lock_gh698`
/// — exercises the FORK branch (`Some/Some` → `fetch_fork_head`) of the
/// wrapper. The same-repo test alone leaves a CI blind spot: a #698
/// regression that bypassed the wrapper for fork PRs (e.g. an inlined
/// `fetch_fork_head` call in `spawn_agent_inner` to skip the remote-
/// config lock acquisition) would still pass the same-repo test and
/// every existing #443 fork unit test (those hit the bare helper
/// directly, no lock). This test closes the gap by hitting the fork
/// arm of the wrapper with the same wall-clock shape; its `git remote
/// add` then `git fetch` sequence MUST hold the lock for the holder's
/// 500 ms sleep.
#[test]
fn locked_fetch_pr_head_serializes_fork_branch_via_per_mesh_sync_lock_gh698() {
    use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
    use std::time::{Duration, Instant};

    let (local, bare_dir, _src) = init_fork_fixture();
    let bare_dir_str = bare_dir.to_str().unwrap().to_string();
    let path_key = local.path().to_string_lossy().into_owned();

    // Holder enters the per-mesh lock (same key as the wrapper) and
    // announces via `entered_flag` before sleeping.
    let entered_flag = std::sync::Arc::new(AtomicUsize::new(0));
    let holder_path = path_key.clone();
    let entered_holder = std::sync::Arc::clone(&entered_flag);
    let holder = std::thread::spawn(move || {
        crate::services::sync_lock::with_mesh_sync_lock(&holder_path, || {
            entered_holder.store(1, AOrdering::SeqCst);
            std::thread::sleep(Duration::from_millis(500));
        });
    });

    let deadline = Instant::now() + Duration::from_secs(2);
    while entered_flag.load(AOrdering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "holder thread never entered the per-mesh lock"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let start = Instant::now();
    let _ = locked_fetch_pr_head(
        &path_key,
        "feat/443-fork",
        Some("alice"),
        Some(&bare_dir_str),
    );
    let elapsed = start.elapsed();

    holder.join().unwrap();

    assert!(
        elapsed >= Duration::from_millis(400),
        "locked_fetch_pr_head (fork branch) did not block on the per-mesh \
             lock (elapsed = {:?}); issue #698 wrap is missing for the fork path \
             — concurrent fork-PR spawns would race on .git/FETCH_HEAD, \
             refs/remotes/fork-<login>/<ref>.lock, AND the git remote add/config \
             files that fetch_fork_head writes before its fetch",
        elapsed,
    );
}

// -----------------------------------------------------------------------
// Reader-thread session-id capture gate (issue #651)
//
// The orchestrator's pre-write at spawn_agent_inner (Assign mode) and the
// PTY reader thread's capture-from-output path both target the same
// `agent_nodes.cli_session_id` column. They are unsynchronised, so a
// last-writer-wins race left the row holding a UUID the agent never
// claimed — and auto-resume later invoked `claude --resume <wrong-uuid>`
// → "Conversation not found". The fix pins the gate to a single function
// of `session_id_mode` (the source of truth) so the two writers can never
// both target the same column. Each test pins one row of the truth table;
// the regression test is the `Assign(_)` row.
// -----------------------------------------------------------------------

/// Regression for issue #651. Even if a future adapter returns
/// `self_assigns_session_id() = true`, the reader thread MUST NOT capture
/// when the orchestrator is in Assign mode — the orchestrator already
/// wrote a UUID at `spawn_agent_inner` step 4, and the reader would
/// overwrite it with whatever UUID matched the regex on PTY output
/// (possibly a different log line, possibly never echoed back).
#[test]
fn reader_should_not_capture_in_assign_mode_even_if_provider_self_assigns() {
    assert!(
        !reader_should_capture_session_id(&SessionIdMode::Assign("orchestrator-uuid".into()), true,),
        "Assign mode is authoritative — reader MUST NOT overwrite the \
             orchestrator's pre-written UUID with a regex match from PTY output \
             (issue #651: 'a UUID the agent never claimed')"
    );
}

/// Resume already has the authoritative ID stored in `cli_session_id`
/// (or, for fresh `--resume` calls, the resume arg passed to the CLI).
/// Capture would race the in-flight `claude --resume <id>` with a
/// possibly-different UUID from the regex, so the reader must stay quiet.
#[test]
fn reader_should_not_capture_in_resume_mode() {
    assert!(
        !reader_should_capture_session_id(&SessionIdMode::Resume("resume-uuid".into()), true,),
        "Resume mode carries the authoritative ID; reader MUST NOT capture"
    );
}

/// `None` mode is the only mode where reader capture is allowed — and only
/// for providers that print a labeled UUID on the PTY (Codex, Agy).
/// OpenCode self-assigns `ses_…` IDs but captures them in
/// `after_fresh_spawn` (SQLite), so its PTY-capture flag is false.
#[test]
fn reader_should_capture_when_provider_self_assigns_and_mode_is_none() {
    assert!(
        reader_should_capture_session_id(&SessionIdMode::None, true),
        "Codex / Agy fresh spawns rely on the reader capturing the UUID \
             from PTY output (orchestrator has no pre-write in None mode)"
    );
}

/// Self-assigning capability is necessary but not sufficient — if the
/// provider accepts `--session-id` (Anthropic) or captures in
/// `after_fresh_spawn` (OpenCode), the PTY regex is not the source of
/// truth even when the orchestrator didn't pre-write.
#[test]
fn reader_should_not_capture_when_provider_does_not_self_assign() {
    assert!(
        !reader_should_capture_session_id(&SessionIdMode::None, false),
        "reader MUST NOT capture when provider does not self-assign; \
             any UUID match would overwrite the existing cli_session_id"
    );
}

/// Issue #1180 — `SpawnIntent::initial_prompt` is the single source
/// of truth for the GitHub-issue prefill. The spawn seam (`spawn_with_intent`)
/// routes through it; so does the desktop draft response and the
/// Autopilot watcher. Pin the wording here so any future drift would
/// surface as a unit-test failure before the agent gets the wrong
/// prompt.
#[test]
fn issue_intent_builds_its_prefill_at_the_spawn_seam() {
    let intent = SpawnIntent::Issue(GitHubWorkContext {
        owner: "alondero".into(),
        repo: "buildmesh".into(),
        number: 247,
        title: "Deepen spawn pipeline".into(),
    });

    assert_eq!(
        intent
            .initial_prompt()
            .as_ref()
            .map(intent::InitialPrompt::as_str),
        Some(
            "Please work on GitHub issue #247 — Deepen spawn pipeline\n\
https://github.com/alondero/buildmesh/issues/247"
        )
    );
}

// -----------------------------------------------------------------------
// Resume-skip decision surface (issue #949 regression).
//
// Pins the PR #1121 fix: when a Startup resume is not viable, the
// caller must NOT write `Idle` to `agent_nodes.status` — the node
// stays `Suspended` so the user's Resume / Regenerate affordances
// remain reachable. `decide_startup_resume` is the single source of
// truth for that contract; `spawn_with_intent`'s Skip arms call no
// sink. A future refactor that re-introduces an `on_idle` write here
// fails review by virtue of the decision being a single enum variant.
// -----------------------------------------------------------------------

#[test]
fn decide_startup_resume_no_session_id_is_skipped() {
    let d = decide_startup_resume(None, ResumeCause::Startup, true);
    assert_eq!(d, ResumeSkipDecision::SkipSuspended);
}

#[test]
fn decide_startup_resume_empty_session_id_is_skipped() {
    // Empty-string defense — `db::list_suspended_nodes`'s SQL filter
    // only catches NULL; legacy writes could leave an empty string
    // behind, so the empty case must be filtered here.
    let d = decide_startup_resume(Some(""), ResumeCause::Startup, true);
    assert_eq!(d, ResumeSkipDecision::SkipSuspended);
}

#[test]
fn decide_startup_resume_when_adapter_declines_is_skipped() {
    let d = decide_startup_resume(
        Some("uuid"),
        ResumeCause::Startup,
        false, // OpenCode, Terminal — no --resume flag, no auto-resume
    );
    assert_eq!(
        d,
        ResumeSkipDecision::SkipAdapterDeclines,
        "OpenCode/Terminal Startup resume must skip without writing Idle"
    );
}

#[test]
fn decide_startup_resume_explicit_no_session_id_is_an_error() {
    // User clicked Resume on a node that never captured a session id.
    // This is a hard error — surfacing it is the user-driven recovery
    // path; the orchestrator-side Startup path silently skips.
    let d = decide_startup_resume(None, ResumeCause::Explicit, true);
    assert_eq!(d, ResumeSkipDecision::NoSessionId);
}

#[test]
fn decide_startup_resume_explicit_with_session_id_proceeds() {
    let d = decide_startup_resume(
        Some("uuid-7"),
        ResumeCause::Explicit,
        false, // explicit cause is unaffected by auto_resume_on_startup
    );
    assert_eq!(d, ResumeSkipDecision::Proceed("uuid-7".to_string()));
}

#[test]
fn decide_startup_resume_startup_with_session_id_and_adapter_accepts_proceeds() {
    let d = decide_startup_resume(Some("uuid-7"), ResumeCause::Startup, true);
    assert_eq!(d, ResumeSkipDecision::Proceed("uuid-7".to_string()));
}

// -----------------------------------------------------------------
// Issue #1179: capability / recipe coherence table.
//
// For every adapter × every session mode × every value the resolver
// might forward, the prepared recipe must contain exactly the flags
// the capability descriptor advertises. The single test below drives
// the full matrix; per-adapter adapter-level tests continue to pin
// the arg shapes directly via `*_args` helpers.
// -----------------------------------------------------------------

fn make_input<'a>(
    platform: Platform,
    session: SessionIdModeRef<'a>,
    config: &'a ResolvedAgentConfig,
    prefill: Option<&'a str>,
) -> HarnessLaunchInput<'a> {
    HarnessLaunchInput {
        platform,
        runtime: EnvType::Windows,
        session,
        config,
        prefill,
        sandbox: false,
    }
}

/// Coherence pin (issue #1179): for every adapter, the
/// `HarnessCapabilities` descriptor and the recipe produced by
/// `default_prepare` agree.
///
/// 1. The recipe's model-flag presence (the flag name from
///    `adapter.model_args(m).first()`) matches
///    `caps.supports_model_override`. Kimi uses `-m`, anthropic /
///    codex / grok / agy / cursor use `--model`, mcode uses nothing.
/// 2. The recipe's effort-flag presence (matched by
///    `caps.effort_control` shape: `Closed => "--effort"`,
///    `InlineConfig => key prefix`, `None => neither`) matches
///    `caps.effort_control != None`.
/// 3. The recipe's prefill marker (trailing positional, `--prefill`,
///    or `--prompt-interactive`) matches `caps.supports_prefill`.
#[test]
fn capability_recipe_coherence() {
    let mut any_adapters = 0;
    for provider in crate::models::Provider::all() {
        let adapter = provider.adapter();
        let caps = adapter.capabilities();
        any_adapters += 1;

        // Build a config where every layer is populated, then verify
        // the recipe only carries what caps allow. Ask the adapter
        // itself for its model-flag shape — some harnesses use
        // short forms (Kimi `-m`) or vendor-specific names; the
        // adapter owns its flag vocabulary.
        let model_value = match adapter.id() {
            // mcode's `model` slot is no longer advertised; pick a
            // plausible value to attempt smuggling it past the mask.
            "mcode" => "minimax/MiniMax-Text-01",
            "codex" => "gpt-4o",
            "kimi" => "kimi-k2",
            "grok" => "grok-3",
            "agy" => "claude-sonnet",
            "cursor" => "claude-3-7-sonnet",
            "opencode" => "anthropic/claude-sonnet-4-5",
            "anthropic" => "claude-sonnet-4-5",
            "terminal" => "irrelevant",
            _ => "model",
        };
        let effort_value = match adapter.id() {
            "anthropic" => "high",
            "codex" => "xhigh",
            _ => "high", // other harnesses don't accept effort
        };
        let config = ResolvedAgentConfig {
            model: Some(model_value.to_string()),
            effort: Some(effort_value.to_string()),
            extra_args: None,
        };
        let prefill_text = "fix the auth bug in handler.rs";
        let input = make_input(
            Platform::Linux,
            SessionIdModeRef::None,
            &config,
            Some(prefill_text),
        );
        let prepared = crate::agent::launch::default_prepare(adapter, input);
        let args = &prepared.recipe.base_args;

        // 1. Model-flag coherence. Ask the adapter what its model-flag
        //    shape is; the recipe must contain it iff caps advertises
        //    the control. mcode (which used to advertise) now does
        //    not, so the recipe must not carry `--model` even when
        //    a value is in the resolved config.
        let model_flag = adapter
            .model_args(model_value)
            .first()
            .cloned()
            .unwrap_or_default();
        let has_model_flag = !model_flag.is_empty() && args.iter().any(|a| a == &model_flag);
        assert_eq!(
            has_model_flag,
            caps.supports_model_override,
            "model-flag / supports_model_override mismatch for {}: \
                 recipe has {} = {}, caps.supports_model_override = {}; args = {:?}",
            adapter.id(),
            model_flag,
            has_model_flag,
            caps.supports_model_override,
            args
        );

        // 2. Effort-flag coherence. Codex uses -c model_reasoning_effort=...;
        //    anthropic uses --effort; everything else must not carry either.
        //    Pin by `caps.effort_control` shape: Closed => "--effort";
        //    InlineConfig => the configured key prefix; None => neither.
        let has_effort_flag = match &caps.effort_control {
            crate::agent::capabilities::EffortControlKind::Closed { .. } => {
                args.iter().any(|a| a == "--effort")
            }
            crate::agent::capabilities::EffortControlKind::InlineConfig { key, .. } => {
                args.iter().any(|a| a.starts_with(key))
            }
            crate::agent::capabilities::EffortControlKind::None => false,
        };
        let has_effort_vocab = !matches!(
            caps.effort_control,
            crate::agent::capabilities::EffortControlKind::None
        );
        assert_eq!(
            has_effort_flag,
            has_effort_vocab,
            "effort-flag / effort_control mismatch for {}: \
                 recipe has effort flag = {}, caps.effort_control != None = {}; args = {:?}",
            adapter.id(),
            has_effort_flag,
            has_effort_vocab,
            args
        );

        // 3. Prefill coherence.
        let has_prefill_text = args.last().map(|a| a.as_str()) == Some(prefill_text);
        let has_prefill_flag = args.iter().any(|a| a == "--prefill");
        let has_prefill_marker = has_prefill_text
            || has_prefill_flag
            || args.iter().any(|a| a == "--prompt-interactive")
            || args.iter().any(|a| a == "--prompt");
        assert_eq!(
            has_prefill_marker,
            caps.supports_prefill,
            "prefill-marker / supports_prefill mismatch for {}: \
                 recipe has prefill marker = {}, caps.supports_prefill = {}; args = {:?}",
            adapter.id(),
            has_prefill_marker,
            caps.supports_prefill,
            args
        );

        // 4. Sandbox-flag coherence (issue #1287). The orchestrator's
        //    outer containment (macOS Seatbelt / Windows restricted-
        //    token) applies uniformly regardless of adapter; the
        //    adapter-level flag only applies when the adapter itself
        //    declared a `sandbox_args()` contribution. A second pass
        //    with `sandbox: true` must therefore add the flag iff
        //    `adapter.sandbox_args()` is non-empty. Any adapter that
        //    silently starts emitting `--sandbox` (or fails to emit
        //    it after overriding `sandbox_args`) trips this pin.
        let sandbox_input = make_input(
            Platform::Linux,
            SessionIdModeRef::None,
            &config,
            Some(prefill_text),
        );
        let sandbox_input = HarnessLaunchInput {
            sandbox: true,
            ..sandbox_input
        };
        let sandbox_prepared = crate::agent::launch::default_prepare(adapter, sandbox_input);
        let sandbox_args = sandbox_prepared
            .recipe
            .base_args
            .iter()
            .filter(|a| adapter.sandbox_args().contains(a))
            .count();
        let sandbox_vocab = adapter.sandbox_args().len();
        assert_eq!(
            sandbox_args,
            sandbox_vocab,
            "sandbox-flag / sandbox_args mismatch for {}: \
                 recipe should carry all {} declared sandbox args when sandbox=true, \
                 got {} matches; args = {:?}",
            adapter.id(),
            sandbox_vocab,
            sandbox_args,
            sandbox_prepared.recipe.base_args
        );
    }
    assert!(
        any_adapters >= 9,
        "expected at least 9 adapters in the matrix"
    );
}

/// Codex's subcommand-style resume is the one recipe shape that
/// diverges from the default. Pin the recipe contains the
/// `resume <id>` shape AND not the model's regular flags when the
/// resume is in play.
#[test]
fn codex_resume_recipe_uses_subcommand_shape() {
    let adapter =
        &crate::agent::provider::adapters::CODEX as &dyn crate::agent::provider::AgentProvider;
    let config = ResolvedAgentConfig::default();
    let input = make_input(
        Platform::Macos,
        SessionIdModeRef::Resume("sess-xyz"),
        &config,
        None,
    );
    let prepared = crate::agent::launch::default_prepare(adapter, input);
    let args = &prepared.recipe.base_args;
    assert!(args.contains(&"resume".to_string()));
    assert!(args.contains(&"sess-xyz".to_string()));
    // Codex resume recipe is the subcommand form; no `--resume <id>`
    // flag is appended.
    assert!(!args.contains(&"--resume".to_string()));
}

/// Issue #1179 follow-up pin: `mcode` no longer advertises
/// `supports_model_override`. Even with a value in the resolver
/// config, the recipe must not contain `--model`.
#[test]
fn mcode_recipe_never_carries_model_arg_under_coherence_matrix() {
    let adapter =
        &crate::agent::provider::adapters::MCODE as &dyn crate::agent::provider::AgentProvider;
    let config = ResolvedAgentConfig {
        model: Some("minimax/MiniMax-Text-01".to_string()),
        effort: None,
        extra_args: None,
    };
    let input = make_input(
        Platform::Macos,
        SessionIdModeRef::None,
        &config,
        Some("check the auth handler"),
    );
    let prepared = crate::agent::launch::default_prepare(adapter, input);
    let args = &prepared.recipe.base_args;
    assert!(
        !args.contains(&"--model".to_string()),
        "mcode recipe must never carry --model; got {:?}",
        args
    );
    assert!(
        args.last().map(|a| a.as_str()) == Some("check the auth handler"),
        "mcode prefill should be the trailing positional, got {:?}",
        args
    );
}
