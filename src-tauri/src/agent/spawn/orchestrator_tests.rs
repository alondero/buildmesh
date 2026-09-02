#![allow(unused_imports)]

use super::{command::*, intent, orchestrator::*, prepare::*, *};
use crate::agent::capabilities::{resolve_agent_config, FieldInputs, HarnessCapabilities};
use crate::models::Provider;
use crate::preferences::HarnessConfigValue;

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
        worktree_policy: WorktreePolicy::RespectMesh,
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
        worktree_policy: WorktreePolicy::RespectMesh,
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
