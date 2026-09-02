#![allow(unused_imports)]

use super::*;
use crate::agent::capabilities::ResolvedAgentConfig;
use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};
use crate::agent::provider::Platform;
use crate::models::{EnvType, Provider};

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
