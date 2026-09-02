use super::command::{cascade_inputs_for, resolve_spawn_config};
use super::{ExplicitSpawnOverrides, SpawnIntent, SpawnRequest, TerminalSize};
use crate::agent::capabilities::{
    resolve_agent_config, FieldInputs, HarnessCapabilities, ResolvedAgentConfig,
};
use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};
use crate::agent::provider::Platform;
use crate::models::{EnvType, Provider};
use crate::preferences::HarnessConfigValue;

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
    let args: Vec<&str> = prepared.recipe.argv().collect();
    assert!(args.contains(&"resume"));
    assert!(args.contains(&"sess-xyz"));
    assert_eq!(prepared.recipe.trailing_args, vec!["sess-xyz".to_string()]);
    // Codex resume recipe is the subcommand form; no `--resume <id>`
    // flag is appended.
    assert!(!args.contains(&"--resume"));
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

// -----------------------------------------------------------------------
// SpawnRequest → resolve_spawn_config integration (issue #1157 / #1358).
//
// The cascade tests above pin the helper + resolver precedence. These
// drive a real SpawnRequest through the same call shape launch_process
// uses, so a refactor that drops explicit or maps it to the wrong slot
// fails here.
// -----------------------------------------------------------------------
/// AC #4 pin: a `SpawnRequest` with a populated layer-1 override
/// (model + effort) drives the helper extracted from
/// `spawn_agent_inner` and the resolved config carries the explicit
/// value — winning over the mesh + application layers. This is the
/// "real spawn site" regression test issue #1155 AC #4 called for:
/// the helper-level cascade tests above exercise the
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
/// but driven from `SpawnRequest`, proving the
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
