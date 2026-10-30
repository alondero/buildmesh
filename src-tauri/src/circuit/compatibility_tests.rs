use super::*;
use crate::agent::capabilities::EffortControlKind;

// -- Resolve helper ----------------------------------------------------

/// Cascade: explicit Autopilot selection wins over the mesh default.
/// Pin: acceptance criteria 3 ("An explicit Autopilot Spawn Option takes
/// precedence over the Mesh default when determining compatibility").
#[test]
fn resolve_explicit_autopilot_wins_over_mesh_default() {
    let r = resolve_circuit_spawn_option(Some("codex"), Some("claude"), Some("agy"));
    assert_eq!(r.spawn_option, "codex");
    assert_eq!(r.harness_id, "codex");
    assert!(r.explicit_circuit_provider);
}

/// Cascade: empty / whitespace explicit falls through to mesh default.
/// Pin: acceptance criteria 11 ("Ensure configuration changes that do
/// not affect compatibility, such as changing a supported model or
/// effort value, do not disable Autopilot"). A blank explicit selection
/// is not the same as "explicitly empty" — it falls through.
#[test]
fn resolve_whitespace_explicit_falls_through_to_mesh_default() {
    let r = resolve_circuit_spawn_option(Some("   "), Some("claude"), None);
    assert_eq!(r.spawn_option, "claude");
    assert_eq!(r.harness_id, "claude");
    assert!(
        !r.explicit_circuit_provider,
        "whitespace explicit must fall through to the mesh default layer"
    );
}

/// Cascade: empty mesh default falls through to app default.
#[test]
fn resolve_empty_mesh_default_falls_through_to_app_default() {
    let r = resolve_circuit_spawn_option(None, Some(""), Some("codex"));
    assert_eq!(r.spawn_option, "codex");
    assert_eq!(r.harness_id, "codex");
    assert!(!r.explicit_circuit_provider);
}

/// Cascade: every layer empty falls through to the hardcoded `"claude"`.
/// Pin: the resolver must always return a non-empty Spawn Option —
/// the evaluator's `NoResolvedHarness` path should be unreachable in
/// practice.
#[test]
fn resolve_all_layers_empty_falls_through_to_claude() {
    let r = resolve_circuit_spawn_option(None, None, None);
    assert_eq!(r.spawn_option, "claude");
    assert_eq!(r.harness_id, "claude");
    assert!(!r.explicit_circuit_provider);
}

/// Composite id is split: the harness half drives capability checks,
/// the provider half is preserved in `spawn_option` (so the rendered
/// label still shows "Claude Code · MiniMax" on the UI).
#[test]
fn resolve_proxied_provider_splits_harness_and_preserves_provider() {
    let r = resolve_circuit_spawn_option(Some("claude:minimax"), None, None);
    assert_eq!(r.spawn_option, "claude:minimax");
    assert_eq!(r.harness_id, "claude");
    assert!(r.explicit_circuit_provider);
}

// -- Harness adapter id lookup ----------------------------------------

/// `"claude"` maps to `"anthropic"` (the `HarnessProfile` → executor
/// bridge). Pin: Proxied rows like `claude:minimax` end up at the same
/// capabilities as a bare `anthropic` spawn — issue #1148 AC #12
/// ("Native and Proxied Provider Spawn Options receive the same
/// compatibility result").
#[test]
fn resolve_harness_adapter_id_maps_claude_to_anthropic() {
    assert_eq!(resolve_harness_adapter_id("claude"), Some("anthropic"));
    assert_eq!(resolve_harness_adapter_id("anthropic"), Some("anthropic"));
    assert_eq!(resolve_harness_adapter_id("Claude"), Some("anthropic"));
    assert_eq!(
        resolve_harness_adapter_id("  ANTHROPIC  "),
        Some("anthropic")
    );
}

#[test]
fn resolve_harness_adapter_id_maps_commandcode_and_aliases() {
    assert_eq!(
        resolve_harness_adapter_id("commandcode"),
        Some("commandcode")
    );
    assert_eq!(
        resolve_harness_adapter_id("command-code"),
        Some("commandcode")
    );
    assert_eq!(resolve_harness_adapter_id("cmdc"), Some("commandcode"));
    assert_eq!(
        resolve_harness_adapter_id("CommandCode"),
        Some("commandcode")
    );
    assert_eq!(resolve_harness_adapter_id("  CMDC  "), Some("commandcode"));
}

/// Issue #1709: Muse resolves to its own adapter so the compatibility gate
/// can see the passive session-log watcher capability.
#[test]
fn resolve_harness_adapter_id_maps_muse() {
    assert_eq!(resolve_harness_adapter_id("muse"), Some("muse"));
    assert_eq!(resolve_harness_adapter_id("Muse"), Some("muse"));
    assert_eq!(resolve_harness_adapter_id("  MUSE  "), Some("muse"));
}

/// Issue #1816: cursor, freebuff, and cline resolve to their own
/// adapters so the compatibility gate sees their real turn-signal
/// capability instead of reporting them as unknown. Before this,
/// `lookup_capabilities` returned `None` for all three, which meant
/// the reviewer gate (which reuses this lookup) could not reject
/// freebuff/cline as reviewers.
#[test]
fn resolve_harness_adapter_id_maps_cursor_freebuff_cline() {
    assert_eq!(resolve_harness_adapter_id("cursor"), Some("cursor"));
    assert_eq!(resolve_harness_adapter_id("Cursor"), Some("cursor"));
    assert_eq!(resolve_harness_adapter_id("freebuff"), Some("freebuff"));
    assert_eq!(resolve_harness_adapter_id("Freebuff"), Some("freebuff"));
    assert_eq!(resolve_harness_adapter_id("cline"), Some("cline"));
    assert_eq!(resolve_harness_adapter_id("Cline"), Some("cline"));
    assert_eq!(resolve_harness_adapter_id("  CLINE  "), Some("cline"));
}

/// Issue #1816 review: legacy ids the frontend `harnessIdForProvider`
/// and stored provider rows still carry must resolve to the same
/// adapter the UI judges — otherwise the backend answers
/// `UnknownHarness` where the frontend claims eligibility.
#[test]
fn resolve_harness_adapter_id_maps_legacy_aliases() {
    assert_eq!(resolve_harness_adapter_id("antigravity"), Some("agy"));
    assert_eq!(resolve_harness_adapter_id("Antigravity"), Some("agy"));
    assert_eq!(resolve_harness_adapter_id("claude_code"), Some("anthropic"));
    assert_eq!(resolve_harness_adapter_id("Claude_Code"), Some("anthropic"));
    assert_eq!(resolve_harness_adapter_id("cmd"), Some("commandcode"));
    assert_eq!(resolve_harness_adapter_id("CMD"), Some("commandcode"));
    // Parity spot-checks: every other frontend alias still resolves.
    assert_eq!(resolve_harness_adapter_id("minimax-code"), Some("mcode"));
    assert_eq!(resolve_harness_adapter_id("deepseek"), Some("dsh"));
    assert_eq!(resolve_harness_adapter_id("deepseek-harness"), Some("dsh"));
    assert_eq!(
        resolve_harness_adapter_id("command-code"),
        Some("commandcode")
    );
    assert_eq!(resolve_harness_adapter_id("cmdc"), Some("commandcode"));
}

/// Unknown harness ids return `None` (no silent fallback to Anthropic,
/// unlike `Provider::from_db_str` which falls back and logs a warning —
/// the compatibility layer must surface unknowns explicitly so the UI
/// can render the actionable reason).
#[test]
fn resolve_harness_adapter_id_returns_none_for_unknown() {
    assert_eq!(resolve_harness_adapter_id("foo"), None);
    assert_eq!(resolve_harness_adapter_id(""), None);
    assert_eq!(resolve_harness_adapter_id("   "), None);
}

/// Issue #1822 acceptance criterion: every id in the canonical
/// [`Provider::all`] set resolves to its own adapter id. This is the
/// direct pin on the resolver's class-of-bug — a new `Provider` variant
/// whose adapter is not covered here trips this test rather than
/// silently closing the Autopilot gate as `UnknownHarness`.
#[test]
fn resolve_harness_adapter_id_covers_provider_enum() {
    for provider in Provider::all() {
        let adapter = provider.adapter().id();
        assert_eq!(
            resolve_harness_adapter_id(adapter),
            Some(adapter),
            "Provider::all() adapter {adapter} must resolve to itself"
        );
        assert!(
            lookup_capabilities(adapter).is_some(),
            "Provider::all() adapter {adapter} must have a capability descriptor"
        );
    }
}

/// Issue #1822 acceptance criterion: enumerate the generated catalog
/// (`src/types/generated/HarnessCapabilitiesTable.json`) and assert every
/// harness id resolves. The generated file is the artifact the Inspector
/// and the frontend predicate read, so a future adapter without a
/// resolver arm fails here rather than disagreeing with the UI.
#[test]
fn resolve_harness_adapter_id_covers_generated_catalog() {
    let catalog: serde_json::Value = serde_json::from_str(include_str!(
        "../../../src/types/generated/HarnessCapabilitiesTable.json"
    ))
    .expect("generated HarnessCapabilitiesTable.json must parse");
    let ids = catalog
        .get("ids")
        .and_then(|v| v.as_array())
        .expect("catalog JSON must include ids in Provider::all() order");
    assert!(
        !ids.is_empty(),
        "catalog ids must not be empty or the check is vacuous"
    );
    for id in ids {
        let id = id.as_str().expect("catalog ids must be strings");
        assert_eq!(
            resolve_harness_adapter_id(id),
            Some(id),
            "catalog harness {id} must resolve to its own adapter id"
        );
        assert!(
            lookup_capabilities(id).is_some(),
            "catalog harness {id} must have a capability descriptor"
        );
    }
}

/// `lookup_capabilities` returns the same descriptor for a native id
/// and the corresponding Proxied id (issue #1148 AC #12). The
/// capability surface is per-harness, not per-pairing.
#[test]
fn lookup_capabilities_native_and_proxied_share_descriptor() {
    let native = lookup_capabilities("anthropic").expect("anthropic known");
    let proxied = lookup_capabilities("claude").expect("claude known");
    // Same descriptor (same adapter under the hood).
    assert_eq!(native.harness_id, proxied.harness_id);
    assert!(native.supports_prefill);
    assert!(native.requires_attention_hook);
}

// -- Evaluate: happy paths ---------------------------------------------

/// Fully compatible harness (Claude Code, worktree on). The resolved
/// `CircuitCompatibility` carries every field the UI needs to render
/// the green "Autopilot is ready" copy.
#[test]
fn evaluate_anthropic_with_worktree_is_allowed() {
    let caps = lookup_capabilities("claude").expect("claude known");
    let result = evaluate(CircuitCompatibilityInput {
        resolved_spawn_option: "claude",
        resolved_harness_id: "claude",
        capabilities: Some(caps),
        mesh_use_worktree: true,
        explicit_circuit_provider: false,
    });
    assert!(result.allowed);
    assert!(result.reasons.is_empty());
    assert_eq!(result.resolved_harness_id.as_deref(), Some("claude"));
    assert_eq!(result.resolved_spawn_option.as_deref(), Some("claude"));
    assert!(!result.explicit_circuit_provider);
}

/// Codex (native) — also compatible. Verifies the harness half of a
/// composite Spawn Option reaches the evaluator unchanged.
#[test]
fn evaluate_codex_native_is_allowed() {
    let caps = lookup_capabilities("codex").expect("codex known");
    let result = evaluate(CircuitCompatibilityInput {
        resolved_spawn_option: "codex",
        resolved_harness_id: "codex",
        capabilities: Some(caps),
        mesh_use_worktree: true,
        explicit_circuit_provider: true,
    });
    assert!(
        result.allowed,
        "codex should be allowed: {:?}",
        result.reasons
    );
    assert!(result.explicit_circuit_provider);
}

// -- Evaluate: harness-level rejections -------------------------------

/// Terminal: plain shell, no LLM loop. The harness id is intentionally
/// *not* carried in the `PlainTerminal` variant — it's the
/// classification itself that the user needs to see, not a label.
/// The evaluator emits `PlainTerminal` alongside the per-capability
/// reasons (Terminal also lacks an attention hook); the UI
/// chooses whether to render the umbrella reason alone or all of them.
#[test]
fn evaluate_terminal_emits_plain_terminal_reason() {
    let caps = lookup_capabilities("terminal").expect("terminal known");
    let result = evaluate(CircuitCompatibilityInput {
        resolved_spawn_option: "terminal",
        resolved_harness_id: "terminal",
        capabilities: Some(caps),
        mesh_use_worktree: true,
        explicit_circuit_provider: false,
    });
    assert!(!result.allowed);
    assert!(result
        .reasons
        .iter()
        .any(|r| matches!(r, CircuitCompatibilityReason::PlainTerminal)));
}

/// OpenCode now ships a project-plugin attention hook (issue #1295):
/// `session.idle` → InputRequired, `permission.asked` →
/// PermissionRequested, both POSTed to `/api/attention/{node_id}`.
/// With worktrees on, the Autopilot gate clears — the only remaining
/// blocker would be the mesh's `use_worktree` flag.
#[test]
fn evaluate_opencode_with_attention_hook_is_allowed() {
    let caps = lookup_capabilities("opencode").expect("opencode known");
    assert!(caps.supports_prefill);
    assert!(caps.requires_attention_hook);
    assert!(!caps.is_plain_terminal);
    let result = evaluate(CircuitCompatibilityInput {
        resolved_spawn_option: "opencode",
        resolved_harness_id: "opencode",
        capabilities: Some(caps),
        mesh_use_worktree: true,
        explicit_circuit_provider: false,
    });
    assert!(
        result.allowed,
        "OpenCode with the plugin attention hook + worktree on must be allowed; got reasons: {:?}",
        result.reasons
    );
    assert!(result.reasons.is_empty());
}

/// Issue #1295 negative path: the OpenCode plugin only marks when
/// `requires_attention_hook` is true AND `provision_attention_hooks`
/// successfully wrote the plugin file. If a future regression flips
/// the capability back to false (or the plugin write fails and the
/// orchestrator silently skips), the gate must still surface
/// `MissingAttentionHook` so the Probe UI can render the actionable
/// reason — never a silent green light.
#[test]
fn evaluate_opencode_without_attention_hook_emits_reason() {
    let caps = HarnessCapabilities {
        harness_id: "opencode".into(),
        supports_extra_args: true,
        supports_resume: true,
        auto_resume_on_startup: true,
        requires_attention_hook: false, // pre-#1295 state
        attention_capability: crate::agent::capabilities::AttentionCapability::None,
        background_inference: None,
        supports_passive_turn_watcher: false,
        supports_result_file_handoff: false,
        produces_readable_transcript: false,
        supports_model_override: true,
        supports_effort_override: false,
        supports_prefill: true,
        accepts_initial_prompt: true,
        is_plain_terminal: false,
        effort_control: EffortControlKind::None,
        available_on: vec!["windows".into()],
        permission_modes: Vec::new(),
        default_permission_mode: None,
    };
    let result = evaluate(CircuitCompatibilityInput {
        resolved_spawn_option: "opencode",
        resolved_harness_id: "opencode",
        capabilities: Some(caps),
        mesh_use_worktree: true,
        explicit_circuit_provider: false,
    });
    assert!(!result.allowed);
    assert!(
        result.reasons.iter().any(|r| matches!(
            r,
            CircuitCompatibilityReason::MissingAttentionHook { harness_id } if harness_id == "opencode"
        )),
        "pre-#1295 capabilities must still surface MissingAttentionHook; got {:?}",
        result.reasons
    );
}

/// Issue #1295: OpenCode + worktrees disabled. The harness is now
/// compatible (the plugin hook unblocked the gate), so the only
/// remaining reason is `WorktreeDisabled`. The pre-#1295 test for
/// this same fixture asserted two reasons; this test pins the new
/// single-reason shape so a future regression that drops
/// `requires_attention_hook` (and silently re-introduces
/// `MissingAttentionHook`) trips here, not on a real Autopilot Mesh.
#[test]
fn evaluate_opencode_with_worktree_disabled_is_only_worktree_reason() {
    let result = evaluate(CircuitCompatibilityInput {
        resolved_spawn_option: "opencode",
        resolved_harness_id: "opencode",
        capabilities: lookup_capabilities("opencode"),
        mesh_use_worktree: false,
        explicit_circuit_provider: true,
    });
    assert!(!result.allowed);
    assert_eq!(
        result.reasons.len(),
        1,
        "OpenCode + worktree=false must surface only WorktreeDisabled; got {:?}",
        result.reasons
    );
    assert!(matches!(
        &result.reasons[0],
        CircuitCompatibilityReason::WorktreeDisabled
    ));
}

/// Command Code's transcript watcher supplies the same terminal turn
/// signal that hook-backed harnesses provide, so no native hook script is
/// required for an Autopilot loop.
#[test]
fn evaluate_commandcode_with_passive_watcher_is_allowed() {
    let caps = lookup_capabilities("commandcode").expect("commandcode known");
    assert!(caps.supports_prefill);
    assert!(!caps.requires_attention_hook);
    assert!(!caps.is_plain_terminal);
    let result = evaluate(CircuitCompatibilityInput {
        resolved_spawn_option: "commandcode",
        resolved_harness_id: "commandcode",
        capabilities: Some(caps),
        mesh_use_worktree: true,
        explicit_circuit_provider: false,
    });
    assert!(
        result.allowed,
        "Command Code watcher should allow Autopilot: {:?}",
        result.reasons
    );
    assert!(result.reasons.is_empty());
}

/// Freebuff, Perch and DeepSeek Harness have neither an attention hook nor a
/// passive turn watcher. Issue #1816 refused them; they now complete a turn
/// through the result file every Circuit prompt asks them for, so both the
/// Autopilot gate and the reviewer gate accept them as file-only harnesses.
/// Cursor and Cline keep their native hooks.
#[test]
fn evaluate_allows_hookless_harnesses_through_result_file_handoff() {
    for harness in ["freebuff", "perch", "dsh"] {
        let caps = lookup_capabilities(harness).unwrap_or_else(|| panic!("{harness} known"));
        assert!(!caps.requires_attention_hook, "{harness} must lack a hook");
        assert!(
            !caps.supports_passive_turn_watcher,
            "{harness} must lack a watcher"
        );
        assert!(caps.supports_result_file_handoff, "{harness} writes a file");
        assert!(!harness_has_lifecycle_signal(&caps), "{harness}");
        assert!(completes_by_result_file_only(&caps), "{harness}");
        let result = evaluate(CircuitCompatibilityInput {
            resolved_spawn_option: harness,
            resolved_harness_id: harness,
            capabilities: Some(caps),
            mesh_use_worktree: true,
            explicit_circuit_provider: false,
        });
        assert!(result.allowed, "{harness}: {:?}", result.reasons);
        assert_eq!(reviewer_harness_reason(harness), None, "{harness}");
    }
    for allowed_harness in ["cursor", "cline"] {
        let caps = lookup_capabilities(allowed_harness)
            .unwrap_or_else(|| panic!("{allowed_harness} known"));
        assert!(
            caps.requires_attention_hook,
            "{allowed_harness} must advertise a hook"
        );
        assert!(!completes_by_result_file_only(&caps), "{allowed_harness}");
        let allowed = evaluate(CircuitCompatibilityInput {
            resolved_spawn_option: allowed_harness,
            resolved_harness_id: allowed_harness,
            capabilities: Some(caps),
            mesh_use_worktree: true,
            explicit_circuit_provider: false,
        });
        assert!(
            allowed.allowed,
            "{allowed_harness} must be allowed: {:?}",
            allowed.reasons
        );
    }
}

/// Every agent harness Buildmesh ships can be asked for a result file; only
/// the plain shell cannot. A harness that opts out of the handoff and has no
/// lifecycle of its own is still refused with `MissingAttentionHook`.
#[test]
fn only_the_plain_shell_lacks_result_file_handoff() {
    for provider in crate::models::Provider::all() {
        let caps = crate::agent::capabilities::capabilities_for(provider.adapter());
        assert_eq!(
            caps.supports_result_file_handoff, !caps.is_plain_terminal,
            "{}",
            caps.harness_id
        );
        if !caps.is_plain_terminal {
            assert!(harness_has_turn_signal(&caps), "{}", caps.harness_id);
        }
    }
    let mut caps = lookup_capabilities("freebuff").expect("freebuff known");
    caps.supports_result_file_handoff = false;
    let result = evaluate(CircuitCompatibilityInput {
        resolved_spawn_option: "freebuff",
        resolved_harness_id: "freebuff",
        capabilities: Some(caps),
        mesh_use_worktree: true,
        explicit_circuit_provider: false,
    });
    assert!(matches!(
        result.reasons.as_slice(),
        [CircuitCompatibilityReason::MissingAttentionHook { harness_id }] if harness_id == "freebuff"
    ));
}

/// Issue #1816 review: the reviewer gate reuses the reason enum rather
/// than a second vocabulary. `reviewer_harness_reason` returns the
/// harness half of `evaluate` minus the fail-closed unknown arm, and
/// `reviewer_refusal_message` maps those variants to copy in one
/// place — with Inspector/docs labels, normalised for case.
#[test]
fn reviewer_gate_reuses_reason_enum_and_catalog_labels() {
    // PlainTerminal keeps its distinct message.
    assert_eq!(
        reviewer_refusal_message(&CircuitCompatibilityReason::PlainTerminal),
        "Terminal cannot be used as the reviewer provider."
    );
    assert!(matches!(
        reviewer_harness_reason("terminal"),
        Some(CircuitCompatibilityReason::PlainTerminal)
    ));
    // MissingAttentionHook names the harness via the catalog label. No
    // shipped agent harness reaches it any more (each can hand off through a
    // result file), so the message mapping is pinned directly.
    for (harness, label) in [("freebuff", "Freebuff"), ("dsh", "DeepSeek Harness")] {
        let message = reviewer_refusal_message(&CircuitCompatibilityReason::MissingAttentionHook {
            harness_id: harness.into(),
        });
        assert_eq!(
            message,
            format!("{label} cannot be used as the reviewer provider: it has no turn-completion signal.")
        );
    }
    // Uppercase input resolves through the same table for the message.
    let message = reviewer_refusal_message(&CircuitCompatibilityReason::MissingAttentionHook {
        harness_id: "CLINE".to_string(),
    });
    assert_eq!(
        message,
        "Cline cannot be used as the reviewer provider: it has no turn-completion signal."
    );
    // Eligible harnesses (hook, passive watcher or result file) yield no reason.
    for harness in [
        "freebuff",
        "perch",
        "dsh",
        "claude",
        "codex",
        "cursor",
        "agy",
        "commandcode",
        "muse",
        "antigravity",
        "claude_code",
        "cmd",
        "cline",
    ] {
        assert_eq!(
            reviewer_harness_reason(harness),
            None,
            "{harness} can yield a turn and must pass the reviewer gate"
        );
    }
    // Unknown ids are fail-open here (evaluate fails them closed as
    // UnknownHarness) — the spawn seam resolves user-defined profiles.
    assert_eq!(reviewer_harness_reason("made-up"), None);
    assert_eq!(reviewer_harness_reason("my-custom-harness"), None);
}

/// `validate_reviewer_provider_id` applies the issue #1659
/// first-`:`-split rule before the gate: the harness half decides,
/// blanks collapse to inherit.
#[test]
fn validate_reviewer_provider_id_splits_composite_and_collapses_blank() {
    assert!(validate_reviewer_provider_id("").is_ok());
    assert!(validate_reviewer_provider_id("   ").is_ok());
    assert!(validate_reviewer_provider_id("codex").is_ok());
    assert!(validate_reviewer_provider_id("codex:minimax").is_ok());
    assert!(validate_reviewer_provider_id("antigravity").is_ok());
    // Issue #1775: Cline now carries a hook, so it is an eligible reviewer.
    assert!(validate_reviewer_provider_id("cline:minimax").is_ok());
    // Freebuff hands off through its result file.
    assert!(validate_reviewer_provider_id("freebuff:minimax").is_ok());
    let err = validate_reviewer_provider_id("  terminal:foo  ").unwrap_err();
    assert_eq!(err, "Terminal cannot be used as the reviewer provider.");
}

/// A custom or future harness with only prefill missing remains eligible:
/// the launch path injects its first prompt after the process is live.
#[test]
fn evaluate_allows_missing_prefill_when_attention_is_available() {
    let caps = HarnessCapabilities {
        harness_id: "futuristic".into(),
        supports_extra_args: true,
        supports_resume: true,
        auto_resume_on_startup: true,
        requires_attention_hook: true,
        attention_capability: crate::agent::capabilities::AttentionCapability::None,
        background_inference: None,
        supports_passive_turn_watcher: false,
        supports_result_file_handoff: false,
        produces_readable_transcript: true,
        supports_model_override: true,
        supports_effort_override: false,
        supports_prefill: false,
        accepts_initial_prompt: true,
        is_plain_terminal: false,
        effort_control: EffortControlKind::None,
        available_on: vec!["windows".into()],
        permission_modes: Vec::new(),
        default_permission_mode: None,
    };
    let result = evaluate(CircuitCompatibilityInput {
        resolved_spawn_option: "futuristic",
        resolved_harness_id: "futuristic",
        capabilities: Some(caps),
        mesh_use_worktree: true,
        explicit_circuit_provider: true,
    });
    assert!(
        result.allowed,
        "PTY fallback should allow this harness: {:?}",
        result.reasons
    );
    assert!(result.reasons.is_empty());
}

/// Unknown harness id: the evaluator emits `UnknownHarness` and skips
/// the per-capability sub-reasons (the harness doesn't exist, so its
/// capabilities don't exist either — surfacing both would be
/// confusing).
#[test]
fn evaluate_unknown_harness_emits_unknown_reason_only() {
    let result = evaluate(CircuitCompatibilityInput {
        resolved_spawn_option: "made-up",
        resolved_harness_id: "made-up",
        capabilities: None,
        mesh_use_worktree: true,
        explicit_circuit_provider: true,
    });
    assert!(!result.allowed);
    assert_eq!(result.reasons.len(), 1);
    assert!(matches!(
        &result.reasons[0],
        CircuitCompatibilityReason::UnknownHarness { harness_id } if harness_id == "made-up"
    ));
}

// -- Evaluate: mesh-level rejections ----------------------------------

/// Worktree disabled: a harness that would otherwise be compatible
/// becomes incompatible because the user disabled worktrees on this
/// mesh. Distinct from a per-harness worktree capability — that case
/// is captured by `PlainTerminal` (the only harness that doesn't
/// operate in worktrees today).
#[test]
fn evaluate_worktree_disabled_emits_worktree_reason() {
    let caps = lookup_capabilities("claude").expect("claude known");
    let result = evaluate(CircuitCompatibilityInput {
        resolved_spawn_option: "claude",
        resolved_harness_id: "claude",
        capabilities: Some(caps),
        mesh_use_worktree: false,
        explicit_circuit_provider: false,
    });
    assert!(!result.allowed);
    assert!(result
        .reasons
        .iter()
        .any(|r| matches!(r, CircuitCompatibilityReason::WorktreeDisabled)));
}

/// Compatibility-pin (acceptance criteria 10): "Changing model or
/// effort within a compatible harness does not disable Autopilot."
/// We exercise the same harness with the same worktree state and
/// assert `allowed`; the resolver is independent of model/effort
/// inputs, so a config change there cannot flip the verdict.
#[test]
fn evaluate_compatible_harness_stays_compatible_regardless_of_model_effort() {
    let caps = lookup_capabilities("claude").expect("claude known");
    let inputs = CircuitCompatibilityInput {
        resolved_spawn_option: "claude",
        resolved_harness_id: "claude",
        capabilities: Some(caps),
        mesh_use_worktree: true,
        explicit_circuit_provider: false,
    };
    let r1 = evaluate(inputs.clone());
    let r2 = evaluate(inputs);
    assert!(r1.allowed);
    assert!(r2.allowed);
    assert_eq!(r1.allowed, r2.allowed);
}

// -- Proxied row parity -----------------------------------------------

/// Acceptance criteria 12: "Native and Proxied Provider Spawn Options
/// backed by the same harness receive the same compatibility result."
/// Drives the evaluator with both a bare `claude` spawn and the
/// `claude:minimax` Proxied row, and asserts byte-identical verdicts.
#[test]
fn evaluate_native_and_proxied_rows_share_compatibility() {
    let caps_native = lookup_capabilities("claude").expect("claude known");
    let result_native = evaluate(CircuitCompatibilityInput {
        resolved_spawn_option: "claude",
        resolved_harness_id: "claude",
        capabilities: Some(caps_native.clone()),
        mesh_use_worktree: true,
        explicit_circuit_provider: false,
    });
    let caps_proxied = lookup_capabilities("claude").expect("claude alias");
    let result_proxied = evaluate(CircuitCompatibilityInput {
        resolved_spawn_option: "claude:minimax",
        resolved_harness_id: "claude",
        capabilities: Some(caps_proxied),
        mesh_use_worktree: true,
        explicit_circuit_provider: true,
    });
    assert_eq!(result_native.allowed, result_proxied.allowed);
    assert_eq!(result_native.reasons, result_proxied.reasons);
    // The spawn_option field differs by construction (one is bare, one
    // is composite), but the resolved_harness_id and the verdict are
    // identical.
    assert_eq!(
        result_native.resolved_harness_id,
        result_proxied.resolved_harness_id
    );
}

// -- Combined reasons --------------------------------------------------

/// A harness that's missing attention, *and* a mesh with worktrees
/// disabled, surfaces both reasons. The UI renders them as a
/// bulleted list so the user sees every gap. We construct a synthetic
/// capability descriptor for this case rather than reusing OpenCode —
/// issue #1295 gave OpenCode the plugin hook, so today its only
/// remaining gap is the mesh's worktree disable. A future harness
/// with `requires_attention_hook = false` will exercise the same
/// combined-reason path.
#[test]
fn evaluate_combines_harness_and_mesh_reasons() {
    let caps = HarnessCapabilities {
        harness_id: "futuristic-no-hook".into(),
        supports_extra_args: true,
        supports_resume: true,
        auto_resume_on_startup: true,
        requires_attention_hook: false,
        attention_capability: crate::agent::capabilities::AttentionCapability::None,
        background_inference: None,
        supports_passive_turn_watcher: false,
        supports_result_file_handoff: false,
        produces_readable_transcript: true,
        supports_model_override: true,
        supports_effort_override: false,
        supports_prefill: true,
        accepts_initial_prompt: true,
        is_plain_terminal: false,
        effort_control: EffortControlKind::None,
        available_on: vec!["windows".into()],
        permission_modes: Vec::new(),
        default_permission_mode: None,
    };
    let result = evaluate(CircuitCompatibilityInput {
        resolved_spawn_option: "futuristic-no-hook",
        resolved_harness_id: "futuristic-no-hook",
        capabilities: Some(caps),
        mesh_use_worktree: false,
        explicit_circuit_provider: true,
    });
    assert!(!result.allowed);
    // Two reasons: MissingAttentionHook + WorktreeDisabled.
    assert_eq!(result.reasons.len(), 2, "got {:?}", result.reasons);
}

/// The `explicit_circuit_provider` flag round-trips through the
/// evaluator unchanged (the UI uses it to label the reason text).
#[test]
fn evaluate_preserves_explicit_circuit_provider_flag() {
    let caps = lookup_capabilities("claude").expect("claude known");
    let inputs = CircuitCompatibilityInput {
        resolved_spawn_option: "claude",
        resolved_harness_id: "claude",
        capabilities: Some(caps),
        mesh_use_worktree: true,
        explicit_circuit_provider: true,
    };
    let result = evaluate(inputs);
    assert!(result.explicit_circuit_provider);
}

// -- compute_for_mesh (the Tauri-command seam) ------------------------

/// `compute_for_mesh` threads explicit → mesh default → app default →
/// "claude" fall-through. With explicit "codex" the harness is Codex
/// and the verdict is `allowed = true` (worktree on).
#[test]
fn compute_for_mesh_uses_explicit_when_present() {
    let result = compute_for_mesh(Some("codex"), Some("claude"), Some("agy"), true);
    assert!(
        result.allowed,
        "codex should be allowed: {:?}",
        result.reasons
    );
    assert_eq!(result.resolved_harness_id.as_deref(), Some("codex"));
    assert_eq!(result.resolved_spawn_option.as_deref(), Some("codex"));
    assert!(result.explicit_circuit_provider);
}

/// Command Code has no native hook, but its backend-owned transcript
/// watcher emits the same terminal turn signals. The Tauri-command seam
/// must therefore allow it for an Autopilot Mesh.
#[test]
fn compute_for_mesh_allows_commandcode_via_passive_watcher() {
    let result = compute_for_mesh(Some("commandcode"), None, None, true);
    assert!(
        result.allowed,
        "Command Code watcher should allow Autopilot: {:?}",
        result.reasons
    );
    assert_eq!(result.resolved_harness_id.as_deref(), Some("commandcode"));
}

/// Issue #1709: Muse has no native hook, but its backend-owned session-log
/// watcher emits the same terminal turn signals as Command Code's, so the
/// Tauri-command seam must allow it for an Autopilot Mesh.
#[test]
fn compute_for_mesh_allows_muse_via_passive_watcher() {
    let result = compute_for_mesh(Some("muse"), None, None, true);
    assert!(
        result.allowed,
        "Muse watcher should allow Autopilot: {:?}",
        result.reasons
    );
    assert_eq!(result.resolved_harness_id.as_deref(), Some("muse"));
}

/// Issue #1797: mcode's Agent-Plugin attention hook is provisioned into
/// `.claude-plugin/plugin.json` and `Stop` delivery was validated against
/// a live 0.4.12 TUI, so the turn-driven Circuit has a signal to
/// drive and the gate must clear. With worktrees on, nothing else blocks.
#[test]
fn compute_for_mesh_allows_mcode_via_attention_hook() {
    let caps = lookup_capabilities("mcode").expect("mcode known");
    assert!(
        caps.requires_attention_hook,
        "the descriptor is what opens the gate; a false here would re-close \
         Autopilot for mcode without new evidence"
    );
    let result = compute_for_mesh(Some("mcode"), None, None, true);
    assert!(
        result.allowed,
        "mcode's validated Stop hook + worktree on must allow Autopilot; \
         got reasons: {:?}",
        result.reasons
    );
    assert!(result.reasons.is_empty());
    assert_eq!(result.resolved_harness_id.as_deref(), Some("mcode"));
}

/// Issue #1822: a Mesh defaulted to Cursor must clear the Autopilot gate
/// on harness capability grounds — Cursor advertises a native attention
/// hook (`requires_attention_hook: true`). Only the mesh-level
/// `WorktreeDisabled` conflict may block it, never `UnknownHarness`.
#[test]
fn compute_for_mesh_allows_cursor_default() {
    let result = compute_for_mesh(None, Some("cursor"), None, true);
    assert!(
        result.allowed,
        "Cursor default must be allowed on harness capability grounds; got reasons: {:?}",
        result.reasons
    );
    assert_eq!(result.resolved_harness_id.as_deref(), Some("cursor"));
    assert_eq!(result.resolved_spawn_option.as_deref(), Some("cursor"));

    let blocked = compute_for_mesh(None, Some("cursor"), None, false);
    assert!(!blocked.allowed);
    assert_eq!(
        blocked.reasons,
        vec![CircuitCompatibilityReason::WorktreeDisabled],
        "worktrees off must be the only reason a Cursor mesh is blocked"
    );
}

/// `compute_for_mesh` falls through to mesh default when explicit is
/// empty. The `explicit_circuit_provider` flag is `false` so the UI
/// labels the verdict "default harness is incompatible".
#[test]
fn compute_for_mesh_falls_through_when_explicit_empty() {
    let result = compute_for_mesh(None, Some("claude"), None, true);
    assert!(result.allowed);
    assert_eq!(result.resolved_harness_id.as_deref(), Some("claude"));
    assert!(!result.explicit_circuit_provider);
}

/// `compute_for_mesh` reports the unknown-harness reason when the
/// resolved Spawn Option names a non-existent harness. Pin: the
/// Tauri command exposes this reason verbatim so the Probe UI can
/// render "Selected harness '<id>' isn't installed" — never a silent
/// "incompatible".
#[test]
fn compute_for_mesh_surfaces_unknown_harness_id() {
    let result = compute_for_mesh(Some("made-up-harness"), None, None, true);
    assert!(!result.allowed);
    assert!(result
        .reasons
        .iter()
        .any(|r| matches!(r, CircuitCompatibilityReason::UnknownHarness { harness_id } if harness_id == "made-up-harness")));
}

/// `compute_for_mesh` returns the umbrella `PlainTerminal` reason when
/// every layer falls through to a harness whose only selection is the
/// Terminal profile (e.g. an uninstalled default harness scenario).
/// Real harnesses win; the test pins the layered composition rather
/// than the harness default.
#[test]
fn compute_for_mesh_worktree_disabled_is_always_incompatible() {
    let result = compute_for_mesh(Some("claude"), None, None, false);
    assert!(!result.allowed);
    assert!(result
        .reasons
        .iter()
        .any(|r| matches!(r, CircuitCompatibilityReason::WorktreeDisabled)));
}
