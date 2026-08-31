#![allow(unused_imports)]

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
