//! Autopilot compatibility gate (issue #1152).
//!
//! Pure backend authority for "can Autopilot run on this Mesh?". One evaluator
//! walks the resolved Autopilot Spawn Option (explicit `meshes.autopilot_provider`
//! when present, else the mesh default → app default → `"claude"` chain),
//! resolves Proxied Provider Spawn Options (`harness:provider`) to their
//! underlying Agent Harness adapter, and rejects selections that cannot
//! satisfy the current Autopilot pipeline's requirements.
//!
//! The same evaluator feeds three call sites (the rule, not three checks):
//!
//! 1. The **Autopilot Probe UI** — disables enable/start controls and renders
//!    the actionable reason when `allowed = false`. Stop/reset remain
//!    available so the user can always turn an already-enabled Mesh off.
//! 2. The **backend enable commands** (`update_mesh_autopilot` and
//!    `set_mesh_autopilot_enabled`) — reject the write with a clear error
//!    string rather than letting the user enable an incompatible Mesh.
//! 3. The **scheduler revalidation** (the polling daemon before each spawn)
//!    — never create an Agent Node for an incompatible harness; persist
//!    `autopilot_enabled = 0` through the existing narrow update path so
//!    stale state can't re-launch a turned-off Mesh.
//!
//! ## Shape: pure core, thin impure seam
//!
//! [`evaluate`] is pure (no I/O, no globals, no DB). The caller supplies the
//! resolved Spawn Option + the capability descriptor + the mesh's worktree
//! flag; the evaluator returns a [`AutopilotCompatibility`] struct the UI can
//! render verbatim. [`resolve_autopilot_spawn_option`] + [`lookup_capabilities`]
//! are the two impure-shape helpers a Tauri command / the scheduler uses to
//! feed the pure evaluator — both small enough that pinning them at the seam
//! keeps the decision itself table-testable.
//!
//! ## Requirements evaluated
//!
//! Two autopilot-pipeline requirements, each with a stable reason code so
//! the UI can render a specific corrective action:
//!
//! Startup prompt delivery is capability-aware but is not a compatibility
//! gate: harnesses with prefill use it, while the rest receive a two-phase
//! PTY injection after launch.
//! 1. **Execution / turn signal** — `HarnessCapabilities::requires_attention_hook`
//!    or a supported passive transcript watcher. Autopilot drives turn
//!    evaluation from `node_turn::publish_*`; a harness without either signal
//!    would never trigger the state machine.
//! 2. **Worktree operation** — `HarnessCapabilities::is_plain_terminal` is the
//!    only "can't run inside a worktree" indicator today (Terminal isn't an
//!    Agent Harness), and the mesh's `use_worktree = false` flag is a
//!    separate config-level conflict that surfaces as its own reason.
//!
//! Plain Terminal is a non-Agent selection (issue #1152 AC #5: "Missing,
//! unknown, unavailable, Terminal, or otherwise incompatible selections
//! produce stable actionable reasons") — it's the "no LLM loop" sentinel.
//!
//! ## What "Proxied Provider" means here
//!
//! A composite Spawn Option id (`"harness:provider"`, ADR-0016 §6) names the
//! executor by its harness half. The provider half is just a credential key
//! — it doesn't change what the harness can do. So `claude:minimax` and
//! `anthropic:minimax` and the bare `claude` / `anthropic` rows all reach
//! the **same** [`HarnessCapabilities`] descriptor (issue #1148 AC #12: "Native
//! and Proxied Provider Spawn Options consume the same application-default
//! layer"). The capability check naturally inherits that property — there is
//! no per-pairing compatibility override.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agent::capabilities::{capabilities_for, HarnessCapabilities};
use crate::agent::provider::SpawnOptionId;
use crate::models::Provider;

// ---------------------------------------------------------------------------
// Wire types — generated to TypeScript via `ts-rs` (issue #359, drift-gated)
// ---------------------------------------------------------------------------

/// Stable, actionable reason Autopilot is not allowed on this Mesh.
///
/// Every variant carries the harness id (or a sentinel like `"<none>"`) so
/// the UI can render a specific corrective action rather than a generic
/// "Autopilot cannot run here" message. The variant set is **closed** —
/// adding a new reason is a wire-shape change that needs a generated TS
/// update, but that's the point: every reason is stable and testable.
///
/// `serde(rename_all = "snake_case")` keeps the wire form simple for the
/// renderer (`{ kind: "missing_attention_hook", harness_id: "opencode" }`) and
/// `ts-rs` reads the same attribute so the generated TS discriminated union
/// matches by `kind`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "AutopilotCompatibilityReason.ts")]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AutopilotCompatibilityReason {
    /// No Spawn Option resolved — should be unreachable in practice because
    /// the resolve helper falls through to `"claude"`, but listed so an
    /// edge case in the harness-id normalisation (e.g. all inputs being
    /// whitespace) still produces a typed reason rather than a panic.
    NoResolvedHarness,

    /// The Spawn Option's harness half doesn't match any known Agent
    /// Harness. Covers hand-edited `preferences.json`, future harness
    /// un-installs leaving stale selections, and any third-party composite
    /// id the user typed in directly.
    UnknownHarness {
        harness_id: String,
    },

    /// The harness is a plain shell (Terminal) — there is no LLM agent
    /// loop, so the entire Autopilot pipeline has nothing to drive.
    PlainTerminal,

    /// The harness has neither an attention hook nor a supported passive
    /// transcript watcher, so the turn-driven Autopilot pipeline
    /// (`autopilot::pipeline::on_turn`) never fires for its nodes.
    MissingAttentionHook {
        harness_id: String,
    },

    /// The mesh disabled worktrees (`meshes.use_worktree = 0`). Autopilot
    /// forces worktree usage on every spawn (the wrap-up PR needs a real
    /// branch to push); this Mesh's selection contradicts that. Distinct
    /// from "harness can't operate in a worktree" — that case is captured
    /// by [`PlainTerminal`](Self::PlainTerminal) plus the current harness
    /// set having no other "I refuse worktrees" signal.
    WorktreeDisabled,
}

/// Pure compatibility decision for one Mesh.
///
/// `allowed = reasons.is_empty()`. The frontend can render the reasons
/// verbatim — each variant is user-facing copy (see the test cases for the
/// user-visible text each maps to).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "AutopilotCompatibility.ts")]
pub struct AutopilotCompatibility {
    pub allowed: bool,
    pub reasons: Vec<AutopilotCompatibilityReason>,
    /// The harness half of the resolved Autopilot Spawn Option. `None` when
    /// no Spawn Option was resolvable (reasons then contains
    /// [`NoResolvedHarness`](AutopilotCompatibilityReason::NoResolvedHarness)).
    pub resolved_harness_id: Option<String>,
    /// The Spawn Option id Autopilot will launch (composite or bare). `None`
    /// when no Spawn Option was resolvable.
    pub resolved_spawn_option: Option<String>,
    /// `true` when the resolved Spawn Option came from the Mesh's explicit
    /// Autopilot selection (`meshes.autopilot_provider`); `false` when it
    /// fell through to the mesh default → app default → `"claude"` chain.
    /// The frontend uses this to label the user-facing copy ("Autopilot
    /// selection 'Claude: MiniMax' is incompatible…" vs "Default harness
    /// 'Claude Code' is incompatible…").
    pub explicit_autopilot_provider: bool,
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

/// Resolved Autopilot Spawn Option + the precedence it came from.
///
/// Pure data; produced by [`resolve_autopilot_spawn_option`] and consumed by
/// [`evaluate`]. The struct is the explicit seam between "what does the user
/// want Autopilot to launch" and "is that allowed?" — both halves are
/// independently testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAutopilotSpawnOption {
    /// Spawn Option id, including the provider half for Proxied rows
    /// (`"harness:provider"`). Always non-empty — the resolver falls through
    /// to `"claude"` if every layer was empty/whitespace.
    pub spawn_option: String,
    /// Harness half of the Spawn Option. Always non-empty.
    pub harness_id: String,
    /// `true` when `autopilot_provider` was present (after trim); `false`
    /// when the resolver fell through to the mesh default chain.
    pub explicit_autopilot_provider: bool,
}

/// Pure Spawn Option resolver for Autopilot.
///
/// Cascade order (issue #1152 acceptance criteria 3):
/// 1. The Mesh's explicit Autopilot Spawn Option (`meshes.autopilot_provider`)
///    when present and non-empty after trim.
/// 2. The Mesh default Spawn Option (`meshes.default_provider`).
/// 3. The App-wide default Spawn Option (`preferences::default_provider`).
/// 4. Hardcoded `"claude"` fallback (matches `resolve_default_provider`).
///
/// Whitespace-only values at every layer collapse to absent so a stray
/// blank `meshes.autopilot_provider` cell doesn't block the next layer
/// (mirrors `resolve_default_provider`'s `non_empty` filter).
///
/// The provider half of a composite id (`"harness:provider"`) is preserved
/// intact — the capability check downstream ignores it, since the executor
/// capability is determined by the harness half alone.
pub fn resolve_autopilot_spawn_option(
    autopilot_provider: Option<&str>,
    mesh_default_provider: Option<&str>,
    app_default_provider: Option<&str>,
) -> ResolvedAutopilotSpawnOption {
    fn non_empty(s: Option<&str>) -> Option<&str> {
        s.map(str::trim).filter(|v| !v.is_empty())
    }
    let explicit = non_empty(autopilot_provider).map(str::to_string);
    let spawn_option = explicit
        .clone()
        .or_else(|| non_empty(mesh_default_provider).map(str::to_string))
        .or_else(|| non_empty(app_default_provider).map(str::to_string))
        .unwrap_or_else(|| "claude".to_string());
    let id = SpawnOptionId::from(spawn_option.as_str());
    let harness_id_string = id.harness_id().to_string();
    ResolvedAutopilotSpawnOption {
        spawn_option,
        harness_id: harness_id_string,
        explicit_autopilot_provider: explicit.is_some(),
    }
}

/// Resolve the Spawn Option's harness half to its underlying Agent Harness
/// adapter id (the half `HarnessCapabilities` accepts). Returns `None` for
/// unknown harness ids so the caller can surface
/// [`UnknownHarness`](AutopilotCompatibilityReason::UnknownHarness) with the
/// raw id the user typed.
///
/// Catalog-driven, not a hand-written per-harness match (issue #1822): any id
/// equal to a [`Provider::all`] adapter id resolves to itself, so a newly
/// registered harness cannot be forgotten here and silently fail the
/// Autopilot / reviewer gates as `UnknownHarness`. The match is kept only for
/// legacy aliases that are *not* catalog ids:
/// - `"claude"` / `"claude_code"` → `"anthropic"` (the
///   `HarnessProfile.id = "claude"` row points at the Anthropic executor; see
///   `preferences::default_harness_profiles`).
/// - `"antigravity"` → `"agy"`.
/// - `"minimax-code"` → `"mcode"` (legacy id, kept in
///   `Provider::from_db_str`).
/// - `"deepseek-harness"` / `"deepseek"` → `"dsh"`.
/// - `"command-code"` / `"cmdc"` / `"cmd"` → `"commandcode"` (legacy ids the
///   frontend `harnessIdForProvider` and stored provider rows still carry —
///   issue #1816 review: every id the frontend judges must resolve here too,
///   or the backend gate answers `UnknownHarness` where the UI claims
///   eligibility).
///
/// Stable case-folded lookup — `"Claude"`, `"  CLAUDE  "`, and `"ANTHROPIC"`
/// all resolve to the same adapter.
pub fn resolve_harness_adapter_id(harness_id: &str) -> Option<&'static str> {
    let normalized = harness_id.trim().to_ascii_lowercase();
    let candidate = match normalized.as_str() {
        "claude" | "claude_code" => "anthropic",
        "antigravity" => "agy",
        "minimax-code" => "mcode",
        "deepseek-harness" | "deepseek" => "dsh",
        "command-code" | "cmdc" | "cmd" => "commandcode",
        other => other,
    };
    Provider::all()
        .iter()
        .find(|p| p.adapter().id() == candidate)
        .map(|p| p.adapter().id())
}

/// Look up the [`HarnessCapabilities`] descriptor for a Spawn Option's
/// harness half. Returns `None` when the id is unknown — that path drives
/// the [`UnknownHarness`](AutopilotCompatibilityReason::UnknownHarness)
/// reason in [`evaluate`].
///
/// Thin wrapper over `Provider::all()` + `capabilities_for` so the adapter
/// lookup stays in lock-step with the canonical Provider enum (issue #1148
/// AC #22: "Frontend and backend agree on harness capability surface").
pub fn lookup_capabilities(harness_id: &str) -> Option<HarnessCapabilities> {
    let adapter_id = resolve_harness_adapter_id(harness_id)?;
    Provider::all()
        .iter()
        .find(|p| p.adapter().id() == adapter_id)
        .map(|p| capabilities_for(p.adapter()))
}

// ---------------------------------------------------------------------------
// Shared turn-signal predicate + reviewer gate (issue #1816)
// ---------------------------------------------------------------------------

/// Whether the harness supplies a turn-completion signal: a native
/// attention hook or a backend-owned passive turn watcher that publishes
/// standard Node Turns. The adapter owns watcher startup, while this
/// evaluator owns the question of whether that signal is sufficient.
///
/// Shared by [`evaluate`]'s harness half and the circuit reviewer gate
/// ([`reviewer_harness_reason`]) so the two predicates cannot drift: when
/// a harness gains a signal, both sides flip together.
pub fn harness_has_turn_signal(caps: &HarnessCapabilities) -> bool {
    caps.requires_attention_hook || caps.supports_passive_turn_watcher
}

/// Reviewer-side harness check: the harness half of [`evaluate`] **minus**
/// the `UnknownHarness` fail-closed arm. Unknown harness ids return `None`
/// (fail-open) — `AgentNode.provider` holds user-defined harness profile
/// ids the spawn seam resolves to a real executor, and refusing those
/// would take a working review away from the user. That divergence from
/// `evaluate` (which fails unknown harnesses closed) is deliberate: the
/// reviewer gate only blocks what it can prove ineligible, mirroring the
/// frontend `blocksReviewCircuit` predicate.
pub fn reviewer_harness_reason(harness_id: &str) -> Option<AutopilotCompatibilityReason> {
    let caps = lookup_capabilities(harness_id)?;
    if caps.is_plain_terminal {
        return Some(AutopilotCompatibilityReason::PlainTerminal);
    }
    if !harness_has_turn_signal(&caps) {
        return Some(AutopilotCompatibilityReason::MissingAttentionHook {
            harness_id: harness_id.trim().to_string(),
        });
    }
    None
}

/// Gate one reviewer Spawn Option id (bare `<harness>` or composite
/// `harness:provider`, issue #1659 first-`:`-split rule) against the
/// attention-compatibility predicate. Blank collapses to `Ok(())`
/// (inherit) — callers decide what inherit means. Unknown harness ids are
/// fail-open (see [`reviewer_harness_reason`]). `Err` carries the
/// user-facing refusal mapped from the reason enum in
/// [`reviewer_refusal_message`], never a second vocabulary.
pub fn validate_reviewer_provider_id(value: &str) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let harness_id = SpawnOptionId::from(trimmed).harness_id().trim().to_string();
    match reviewer_harness_reason(&harness_id) {
        None => Ok(()),
        Some(reason) => Err(reviewer_refusal_message(&reason)),
    }
}

/// User-facing refusal text for a reviewer-harness rejection, mapped from
/// the existing reason enum in one place. `PlainTerminal` keeps its
/// distinct message; `MissingAttentionHook` names the harness
/// (Inspector/docs label from the harness catalog) and the missing
/// turn-completion signal. Any other variant is unreachable from
/// [`reviewer_harness_reason`]; the arm stays total with a generic message
/// rather than panicking on a future variant.
pub fn reviewer_refusal_message(reason: &AutopilotCompatibilityReason) -> String {
    match reason {
        AutopilotCompatibilityReason::PlainTerminal => {
            "Terminal cannot be used as the reviewer provider.".to_string()
        }
        AutopilotCompatibilityReason::MissingAttentionHook { harness_id } => {
            format!(
                "{} cannot be used as the reviewer provider: it has no turn-completion signal.",
                harness_display_label(harness_id)
            )
        }
        _ => "This provider cannot be used as the reviewer provider.".to_string(),
    }
}

/// Display label for a harness-half string: resolves aliases and case
/// through the adapter table, then returns the Inspector/docs label from
/// the harness catalog (`"dsh"` → `"DeepSeek Harness"`, `"CLINE"` →
/// `"Cline"`). Falls back to the trimmed input with a capitalised first
/// letter for ids the catalog does not know — unreachable from the
/// reviewer gate (unknown ids are fail-open, so no message is ever built
/// for them), total anyway.
fn harness_display_label(harness_half: &str) -> String {
    let normalized = harness_half.trim().to_ascii_lowercase();
    if let Some(adapter) = resolve_harness_adapter_id(&normalized) {
        if let Some(label) = crate::agent::harness_catalog::inspector_label_for_adapter(adapter) {
            return label.to_string();
        }
    }
    let mut chars = normalized.chars();
    match chars.next() {
        None => "Unknown harness".to_string(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

// ---------------------------------------------------------------------------
// Evaluator
// ---------------------------------------------------------------------------

/// Inputs to the pure evaluator. Caller-owned — no DB or preferences reads
/// here. The harness capabilities are passed in so this module stays free of
/// the harness profile / preferences cache plumbing; `lookup_capabilities`
/// is the conventional seam.
#[derive(Debug, Clone)]
pub struct AutopilotCompatibilityInput<'a> {
    /// The Spawn Option id Autopilot will launch.
    pub resolved_spawn_option: &'a str,
    /// The harness half of the Spawn Option.
    pub resolved_harness_id: &'a str,
    /// The harness's authoritative capability descriptor. `None` when the
    /// harness id is unknown — the evaluator emits
    /// [`UnknownHarness`](AutopilotCompatibilityReason::UnknownHarness) and
    /// skips the per-capability reasons.
    pub capabilities: Option<HarnessCapabilities>,
    /// The mesh's `use_worktree` flag. Autopilot forces worktrees on, so a
    /// `false` here is the user expressing "I don't want worktrees" — the
    /// gate rejects the spawn with [`WorktreeDisabled`](AutopilotCompatibilityReason::WorktreeDisabled).
    pub mesh_use_worktree: bool,
    /// Whether the Spawn Option came from the mesh's explicit Autopilot
    /// selection (vs the default chain).
    pub explicit_autopilot_provider: bool,
}

/// Pure compatibility evaluator (issue #1152). Returns the full
/// [`AutopilotCompatibility`] struct — never a bare bool — so the caller
/// always has the reason text for the user and the explicit-selection flag
/// for labelling.
///
/// The decision is **total**: every input combination produces a defined
/// result. There is no panic path; `Option::None` for the capabilities
/// falls through to `UnknownHarness` rather than aborting.
pub fn evaluate(input: AutopilotCompatibilityInput<'_>) -> AutopilotCompatibility {
    let mut reasons: Vec<AutopilotCompatibilityReason> = Vec::new();

    match input.capabilities.as_ref() {
        None => reasons.push(AutopilotCompatibilityReason::UnknownHarness {
            harness_id: input.resolved_harness_id.to_string(),
        }),
        Some(caps) => {
            // Plain Terminal is the "no agent loop" sentinel. We surface it
            // as its own reason rather than combining multiple low-level gaps
            // because the user's corrective action is "pick an Agent Harness",
            // not "make Terminal accept prompts". The harness id is intentionally
            // omitted from the variant — `PlainTerminal` is itself the harness
            // classification the user needs to see.
            if caps.is_plain_terminal {
                reasons.push(AutopilotCompatibilityReason::PlainTerminal);
            }
            // Shared predicate with the reviewer gate (issue #1816) —
            // same verdict here as in `reviewer_harness_reason`.
            if !harness_has_turn_signal(caps) {
                reasons.push(AutopilotCompatibilityReason::MissingAttentionHook {
                    harness_id: input.resolved_harness_id.to_string(),
                });
            }
        }
    }

    if !input.mesh_use_worktree {
        reasons.push(AutopilotCompatibilityReason::WorktreeDisabled);
    }

    AutopilotCompatibility {
        allowed: reasons.is_empty(),
        reasons,
        resolved_harness_id: Some(input.resolved_harness_id.to_string()),
        resolved_spawn_option: Some(input.resolved_spawn_option.to_string()),
        explicit_autopilot_provider: input.explicit_autopilot_provider,
    }
}

/// Compose [`resolve_autopilot_spawn_option`] + [`lookup_capabilities`] +
/// [`evaluate`] for the conventional mesh-row → verdict path. Thin wrapper
/// so callers don't have to thread every argument through by hand; the
/// seam still lives in the pure helpers above for unit testing.
pub fn compute_for_mesh(
    autopilot_provider: Option<&str>,
    mesh_default_provider: Option<&str>,
    app_default_provider: Option<&str>,
    mesh_use_worktree: bool,
) -> AutopilotCompatibility {
    let resolved = resolve_autopilot_spawn_option(
        autopilot_provider,
        mesh_default_provider,
        app_default_provider,
    );
    let capabilities = lookup_capabilities(&resolved.harness_id);
    evaluate(AutopilotCompatibilityInput {
        resolved_spawn_option: &resolved.spawn_option,
        resolved_harness_id: &resolved.harness_id,
        capabilities,
        mesh_use_worktree,
        explicit_autopilot_provider: resolved.explicit_autopilot_provider,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::capabilities::EffortControlKind;

    // -- Resolve helper ----------------------------------------------------

    /// Cascade: explicit Autopilot selection wins over the mesh default.
    /// Pin: acceptance criteria 3 ("An explicit Autopilot Spawn Option takes
    /// precedence over the Mesh default when determining compatibility").
    #[test]
    fn resolve_explicit_autopilot_wins_over_mesh_default() {
        let r = resolve_autopilot_spawn_option(
            Some("codex"),
            Some("claude"),
            Some("agy"),
        );
        assert_eq!(r.spawn_option, "codex");
        assert_eq!(r.harness_id, "codex");
        assert!(r.explicit_autopilot_provider);
    }

    /// Cascade: empty / whitespace explicit falls through to mesh default.
    /// Pin: acceptance criteria 11 ("Ensure configuration changes that do
    /// not affect compatibility, such as changing a supported model or
    /// effort value, do not disable Autopilot"). A blank explicit selection
    /// is not the same as "explicitly empty" — it falls through.
    #[test]
    fn resolve_whitespace_explicit_falls_through_to_mesh_default() {
        let r = resolve_autopilot_spawn_option(
            Some("   "),
            Some("claude"),
            None,
        );
        assert_eq!(r.spawn_option, "claude");
        assert_eq!(r.harness_id, "claude");
        assert!(
            !r.explicit_autopilot_provider,
            "whitespace explicit must fall through to the mesh default layer"
        );
    }

    /// Cascade: empty mesh default falls through to app default.
    #[test]
    fn resolve_empty_mesh_default_falls_through_to_app_default() {
        let r = resolve_autopilot_spawn_option(None, Some(""), Some("codex"));
        assert_eq!(r.spawn_option, "codex");
        assert_eq!(r.harness_id, "codex");
        assert!(!r.explicit_autopilot_provider);
    }

    /// Cascade: every layer empty falls through to the hardcoded `"claude"`.
    /// Pin: the resolver must always return a non-empty Spawn Option —
    /// the evaluator's `NoResolvedHarness` path should be unreachable in
    /// practice.
    #[test]
    fn resolve_all_layers_empty_falls_through_to_claude() {
        let r = resolve_autopilot_spawn_option(None, None, None);
        assert_eq!(r.spawn_option, "claude");
        assert_eq!(r.harness_id, "claude");
        assert!(!r.explicit_autopilot_provider);
    }

    /// Composite id is split: the harness half drives capability checks,
    /// the provider half is preserved in `spawn_option` (so the rendered
    /// label still shows "Claude Code · MiniMax" on the UI).
    #[test]
    fn resolve_proxied_provider_splits_harness_and_preserves_provider() {
        let r = resolve_autopilot_spawn_option(
            Some("claude:minimax"),
            None,
            None,
        );
        assert_eq!(r.spawn_option, "claude:minimax");
        assert_eq!(r.harness_id, "claude");
        assert!(r.explicit_autopilot_provider);
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
        assert_eq!(resolve_harness_adapter_id("  ANTHROPIC  "), Some("anthropic"));
    }

    #[test]
    fn resolve_harness_adapter_id_maps_commandcode_and_aliases() {
        assert_eq!(resolve_harness_adapter_id("commandcode"), Some("commandcode"));
        assert_eq!(resolve_harness_adapter_id("command-code"), Some("commandcode"));
        assert_eq!(resolve_harness_adapter_id("cmdc"), Some("commandcode"));
        assert_eq!(resolve_harness_adapter_id("CommandCode"), Some("commandcode"));
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
        assert_eq!(resolve_harness_adapter_id("command-code"), Some("commandcode"));
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
    /// `AutopilotCompatibility` carries every field the UI needs to render
    /// the green "Autopilot is ready" copy.
    #[test]
    fn evaluate_anthropic_with_worktree_is_allowed() {
        let caps = lookup_capabilities("claude").expect("claude known");
        let result = evaluate(AutopilotCompatibilityInput {
            resolved_spawn_option: "claude",
            resolved_harness_id: "claude",
            capabilities: Some(caps),
            mesh_use_worktree: true,
            explicit_autopilot_provider: false,
        });
        assert!(result.allowed);
        assert!(result.reasons.is_empty());
        assert_eq!(result.resolved_harness_id.as_deref(), Some("claude"));
        assert_eq!(result.resolved_spawn_option.as_deref(), Some("claude"));
        assert!(!result.explicit_autopilot_provider);
    }

    /// Codex (native) — also compatible. Verifies the harness half of a
    /// composite Spawn Option reaches the evaluator unchanged.
    #[test]
    fn evaluate_codex_native_is_allowed() {
        let caps = lookup_capabilities("codex").expect("codex known");
        let result = evaluate(AutopilotCompatibilityInput {
            resolved_spawn_option: "codex",
            resolved_harness_id: "codex",
            capabilities: Some(caps),
            mesh_use_worktree: true,
            explicit_autopilot_provider: true,
        });
        assert!(result.allowed, "codex should be allowed: {:?}", result.reasons);
        assert!(result.explicit_autopilot_provider);
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
        let result = evaluate(AutopilotCompatibilityInput {
            resolved_spawn_option: "terminal",
            resolved_harness_id: "terminal",
            capabilities: Some(caps),
            mesh_use_worktree: true,
            explicit_autopilot_provider: false,
        });
        assert!(!result.allowed);
        assert!(result
            .reasons
            .iter()
            .any(|r| matches!(r, AutopilotCompatibilityReason::PlainTerminal)));
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
        let result = evaluate(AutopilotCompatibilityInput {
            resolved_spawn_option: "opencode",
            resolved_harness_id: "opencode",
            capabilities: Some(caps),
            mesh_use_worktree: true,
            explicit_autopilot_provider: false,
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
            supports_passive_turn_watcher: false,
            produces_readable_transcript: false,
            supports_model_override: true,
            supports_effort_override: false,
            supports_prefill: true,
            is_plain_terminal: false,
            effort_control: EffortControlKind::None,
            available_on: vec!["windows".into()],
        };
        let result = evaluate(AutopilotCompatibilityInput {
            resolved_spawn_option: "opencode",
            resolved_harness_id: "opencode",
            capabilities: Some(caps),
            mesh_use_worktree: true,
            explicit_autopilot_provider: false,
        });
        assert!(!result.allowed);
        assert!(
            result.reasons.iter().any(|r| matches!(
                r,
                AutopilotCompatibilityReason::MissingAttentionHook { harness_id } if harness_id == "opencode"
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
        let result = evaluate(AutopilotCompatibilityInput {
            resolved_spawn_option: "opencode",
            resolved_harness_id: "opencode",
            capabilities: lookup_capabilities("opencode"),
            mesh_use_worktree: false,
            explicit_autopilot_provider: true,
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
            AutopilotCompatibilityReason::WorktreeDisabled
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
        let result = evaluate(AutopilotCompatibilityInput {
            resolved_spawn_option: "commandcode",
            resolved_harness_id: "commandcode",
            capabilities: Some(caps),
            mesh_use_worktree: true,
            explicit_autopilot_provider: false,
        });
        assert!(result.allowed, "Command Code watcher should allow Autopilot: {:?}", result.reasons);
        assert!(result.reasons.is_empty());
    }

    /// Issue #1816: freebuff and cline have neither an attention hook nor a
    /// passive turn watcher, so the gate emits `MissingAttentionHook` naming
    /// the harness — the same reason the reviewer gate reuses. Cursor has a
    /// native hook and stays allowed.
    #[test]
    fn evaluate_freebuff_and_cline_emit_missing_attention_hook_cursor_allowed() {
        for harness in ["freebuff", "cline", "dsh"] {
            let caps = lookup_capabilities(harness).unwrap_or_else(|| panic!("{harness} known"));
            assert!(!caps.requires_attention_hook, "{harness} must lack a hook");
            assert!(!caps.supports_passive_turn_watcher, "{harness} must lack a watcher");
            let result = evaluate(AutopilotCompatibilityInput {
                resolved_spawn_option: harness,
                resolved_harness_id: harness,
                capabilities: Some(caps),
                mesh_use_worktree: true,
                explicit_autopilot_provider: false,
            });
            assert!(!result.allowed, "{harness} must be rejected");
            assert!(
                result.reasons.iter().any(|r| matches!(
                    r,
                    AutopilotCompatibilityReason::MissingAttentionHook { harness_id }
                    if harness_id == harness
                )),
                "{harness} must surface MissingAttentionHook; got {:?}",
                result.reasons
            );
        }
        let cursor = lookup_capabilities("cursor").expect("cursor known");
        assert!(cursor.requires_attention_hook);
        let allowed = evaluate(AutopilotCompatibilityInput {
            resolved_spawn_option: "cursor",
            resolved_harness_id: "cursor",
            capabilities: Some(cursor),
            mesh_use_worktree: true,
            explicit_autopilot_provider: false,
        });
        assert!(allowed.allowed, "cursor must be allowed: {:?}", allowed.reasons);
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
            reviewer_refusal_message(&AutopilotCompatibilityReason::PlainTerminal),
            "Terminal cannot be used as the reviewer provider."
        );
        assert!(matches!(
            reviewer_harness_reason("terminal"),
            Some(AutopilotCompatibilityReason::PlainTerminal)
        ));
        // MissingAttentionHook names the harness via the catalog label.
        // (Composite ids split at the `validate_...` seam; `reason` takes
        // the bare harness half.)
        for (harness, label) in [("cline", "Cline"), ("freebuff", "Freebuff"), ("dsh", "DeepSeek Harness")] {
            match reviewer_harness_reason(harness) {
                Some(AutopilotCompatibilityReason::MissingAttentionHook { harness_id }) => {
                    assert_eq!(harness_id, harness);
                    let message = reviewer_refusal_message(
                        &AutopilotCompatibilityReason::MissingAttentionHook { harness_id },
                    );
                    assert_eq!(
                        message,
                        format!("{label} cannot be used as the reviewer provider: it has no turn-completion signal.")
                    );
                }
                other => panic!("{harness} must yield MissingAttentionHook, got {other:?}"),
            }
        }
        // Uppercase input resolves through the same table for the message.
        let message = reviewer_refusal_message(
            &AutopilotCompatibilityReason::MissingAttentionHook { harness_id: "CLINE".to_string() },
        );
        assert_eq!(
            message,
            "Cline cannot be used as the reviewer provider: it has no turn-completion signal."
        );
        // Eligible harnesses (hook or passive watcher) yield no reason.
        for harness in ["claude", "codex", "cursor", "agy", "commandcode", "muse", "antigravity", "claude_code", "cmd"] {
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
        let err = validate_reviewer_provider_id("cline:minimax").unwrap_err();
        assert_eq!(
            err,
            "Cline cannot be used as the reviewer provider: it has no turn-completion signal."
        );
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
            supports_passive_turn_watcher: false,
            produces_readable_transcript: true,
            supports_model_override: true,
            supports_effort_override: false,
            supports_prefill: false,
            is_plain_terminal: false,
            effort_control: EffortControlKind::None,
            available_on: vec!["windows".into()],
        };
        let result = evaluate(AutopilotCompatibilityInput {
            resolved_spawn_option: "futuristic",
            resolved_harness_id: "futuristic",
            capabilities: Some(caps),
            mesh_use_worktree: true,
            explicit_autopilot_provider: true,
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
        let result = evaluate(AutopilotCompatibilityInput {
            resolved_spawn_option: "made-up",
            resolved_harness_id: "made-up",
            capabilities: None,
            mesh_use_worktree: true,
            explicit_autopilot_provider: true,
        });
        assert!(!result.allowed);
        assert_eq!(result.reasons.len(), 1);
        assert!(matches!(
            &result.reasons[0],
            AutopilotCompatibilityReason::UnknownHarness { harness_id } if harness_id == "made-up"
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
        let result = evaluate(AutopilotCompatibilityInput {
            resolved_spawn_option: "claude",
            resolved_harness_id: "claude",
            capabilities: Some(caps),
            mesh_use_worktree: false,
            explicit_autopilot_provider: false,
        });
        assert!(!result.allowed);
        assert!(result
            .reasons
            .iter()
            .any(|r| matches!(r, AutopilotCompatibilityReason::WorktreeDisabled)));
    }

    /// Compatibility-pin (acceptance criteria 10): "Changing model or
    /// effort within a compatible harness does not disable Autopilot."
    /// We exercise the same harness with the same worktree state and
    /// assert `allowed`; the resolver is independent of model/effort
    /// inputs, so a config change there cannot flip the verdict.
    #[test]
    fn evaluate_compatible_harness_stays_compatible_regardless_of_model_effort() {
        let caps = lookup_capabilities("claude").expect("claude known");
        let inputs = AutopilotCompatibilityInput {
            resolved_spawn_option: "claude",
            resolved_harness_id: "claude",
            capabilities: Some(caps),
            mesh_use_worktree: true,
            explicit_autopilot_provider: false,
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
        let result_native = evaluate(AutopilotCompatibilityInput {
            resolved_spawn_option: "claude",
            resolved_harness_id: "claude",
            capabilities: Some(caps_native.clone()),
            mesh_use_worktree: true,
            explicit_autopilot_provider: false,
        });
        let caps_proxied = lookup_capabilities("claude").expect("claude alias");
        let result_proxied = evaluate(AutopilotCompatibilityInput {
            resolved_spawn_option: "claude:minimax",
            resolved_harness_id: "claude",
            capabilities: Some(caps_proxied),
            mesh_use_worktree: true,
            explicit_autopilot_provider: true,
        });
        assert_eq!(result_native.allowed, result_proxied.allowed);
        assert_eq!(result_native.reasons, result_proxied.reasons);
        // The spawn_option field differs by construction (one is bare, one
        // is composite), but the resolved_harness_id and the verdict are
        // identical.
        assert_eq!(result_native.resolved_harness_id, result_proxied.resolved_harness_id);
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
            supports_passive_turn_watcher: false,
            produces_readable_transcript: true,
            supports_model_override: true,
            supports_effort_override: false,
            supports_prefill: true,
            is_plain_terminal: false,
            effort_control: EffortControlKind::None,
            available_on: vec!["windows".into()],
        };
        let result = evaluate(AutopilotCompatibilityInput {
            resolved_spawn_option: "futuristic-no-hook",
            resolved_harness_id: "futuristic-no-hook",
            capabilities: Some(caps),
            mesh_use_worktree: false,
            explicit_autopilot_provider: true,
        });
        assert!(!result.allowed);
        // Two reasons: MissingAttentionHook + WorktreeDisabled.
        assert_eq!(result.reasons.len(), 2, "got {:?}", result.reasons);
    }

    /// The `explicit_autopilot_provider` flag round-trips through the
    /// evaluator unchanged (the UI uses it to label the reason text).
    #[test]
    fn evaluate_preserves_explicit_autopilot_provider_flag() {
        let caps = lookup_capabilities("claude").expect("claude known");
        let inputs = AutopilotCompatibilityInput {
            resolved_spawn_option: "claude",
            resolved_harness_id: "claude",
            capabilities: Some(caps),
            mesh_use_worktree: true,
            explicit_autopilot_provider: true,
        };
        let result = evaluate(inputs);
        assert!(result.explicit_autopilot_provider);
    }

    // -- compute_for_mesh (the Tauri-command seam) ------------------------

    /// `compute_for_mesh` threads explicit → mesh default → app default →
    /// "claude" fall-through. With explicit "codex" the harness is Codex
    /// and the verdict is `allowed = true` (worktree on).
    #[test]
    fn compute_for_mesh_uses_explicit_when_present() {
        let result = compute_for_mesh(Some("codex"), Some("claude"), Some("agy"), true);
        assert!(result.allowed, "codex should be allowed: {:?}", result.reasons);
        assert_eq!(result.resolved_harness_id.as_deref(), Some("codex"));
        assert_eq!(result.resolved_spawn_option.as_deref(), Some("codex"));
        assert!(result.explicit_autopilot_provider);
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
    /// a live 0.4.12 TUI, so the turn-driven Autopilot pipeline has a signal to
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
            vec![AutopilotCompatibilityReason::WorktreeDisabled],
            "worktrees off must be the only reason a Cursor mesh is blocked"
        );
    }

    /// `compute_for_mesh` falls through to mesh default when explicit is
    /// empty. The `explicit_autopilot_provider` flag is `false` so the UI
    /// labels the verdict "default harness is incompatible".
    #[test]
    fn compute_for_mesh_falls_through_when_explicit_empty() {
        let result = compute_for_mesh(None, Some("claude"), None, true);
        assert!(result.allowed);
        assert_eq!(result.resolved_harness_id.as_deref(), Some("claude"));
        assert!(!result.explicit_autopilot_provider);
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
            .any(|r| matches!(r, AutopilotCompatibilityReason::UnknownHarness { harness_id } if harness_id == "made-up-harness")));
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
            .any(|r| matches!(r, AutopilotCompatibilityReason::WorktreeDisabled)));
    }
}
