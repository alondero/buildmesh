//! Circuit launch validation against harness capabilities and worktree policy.

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
#[ts(export, export_to = "CircuitCompatibilityReason.ts")]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CircuitCompatibilityReason {
    /// No Spawn Option resolved — should be unreachable in practice because
    /// the resolve helper falls through to `"claude"`, but listed so an
    /// edge case in the harness-id normalisation (e.g. all inputs being
    /// whitespace) still produces a typed reason rather than a panic.
    NoResolvedHarness,

    /// The Spawn Option's harness half doesn't match any known Agent
    /// Harness. Covers hand-edited `preferences.json`, future harness
    /// un-installs leaving stale selections, and any third-party composite
    /// id the user typed in directly.
    UnknownHarness { harness_id: String },

    /// The harness is a plain shell (Terminal) — there is no LLM agent
    /// loop, so the entire Circuit has nothing to drive.
    PlainTerminal,

    /// The harness has no attention hook, no supported passive transcript
    /// watcher, and has opted out of result-file handoff, so the turn-driven
    /// Circuit (the Circuit observation worker) never fires for its nodes.
    MissingAttentionHook { harness_id: String },

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
#[ts(export, export_to = "CircuitCompatibility.ts")]
pub struct CircuitCompatibility {
    pub allowed: bool,
    pub reasons: Vec<CircuitCompatibilityReason>,
    /// The harness half of the resolved Autopilot Spawn Option. `None` when
    /// no Spawn Option was resolvable (reasons then contains
    /// [`NoResolvedHarness`](CircuitCompatibilityReason::NoResolvedHarness)).
    pub resolved_harness_id: Option<String>,
    /// The Spawn Option id Autopilot will launch (composite or bare). `None`
    /// when no Spawn Option was resolvable.
    pub resolved_spawn_option: Option<String>,
    /// `true` when the resolved Spawn Option came from the Mesh's explicit
    /// Autopilot selection (`Circuit agent step provider`); `false` when it
    /// fell through to the mesh default → app default → `"claude"` chain.
    /// The frontend uses this to label the user-facing copy ("Autopilot
    /// selection 'Claude: MiniMax' is incompatible…" vs "Default harness
    /// 'Claude Code' is incompatible…").
    pub explicit_circuit_provider: bool,
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

/// Resolved Autopilot Spawn Option + the precedence it came from.
///
/// Pure data; produced by [`resolve_circuit_spawn_option`] and consumed by
/// [`evaluate`]. The struct is the explicit seam between "what does the user
/// want Autopilot to launch" and "is that allowed?" — both halves are
/// independently testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCircuitSpawnOption {
    /// Spawn Option id, including the provider half for Proxied rows
    /// (`"harness:provider"`). Always non-empty — the resolver falls through
    /// to `"claude"` if every layer was empty/whitespace.
    pub spawn_option: String,
    /// Harness half of the Spawn Option. Always non-empty.
    pub harness_id: String,
    /// `true` when `circuit_provider` was present (after trim); `false`
    /// when the resolver fell through to the mesh default chain.
    pub explicit_circuit_provider: bool,
}

/// Pure Spawn Option resolver for Autopilot.
///
/// Cascade order (issue #1152 acceptance criteria 3):
/// 1. The Circuit step's explicit Spawn Option (`Circuit agent step provider`)
///    when present and non-empty after trim.
/// 2. The Mesh default Spawn Option (`meshes.default_provider`).
/// 3. The App-wide default Spawn Option (`preferences::default_provider`).
/// 4. Hardcoded `"claude"` fallback (matches `resolve_default_provider`).
///
/// Whitespace-only values at every layer collapse to absent so a stray
/// blank `Circuit agent step provider` cell doesn't block the next layer
/// (mirrors `resolve_default_provider`'s `non_empty` filter).
///
/// The provider half of a composite id (`"harness:provider"`) is preserved
/// intact — the capability check downstream ignores it, since the executor
/// capability is determined by the harness half alone.
pub fn resolve_circuit_spawn_option(
    circuit_provider: Option<&str>,
    mesh_default_provider: Option<&str>,
    app_default_provider: Option<&str>,
) -> ResolvedCircuitSpawnOption {
    fn non_empty(s: Option<&str>) -> Option<&str> {
        s.map(str::trim).filter(|v| !v.is_empty())
    }
    let explicit = non_empty(circuit_provider).map(str::to_string);
    let spawn_option = explicit
        .clone()
        .or_else(|| non_empty(mesh_default_provider).map(str::to_string))
        .or_else(|| non_empty(app_default_provider).map(str::to_string))
        .unwrap_or_else(|| "claude".to_string());
    let selected = crate::preferences::launch_configurations::selection_option(&spawn_option)
        .unwrap_or_else(|_| spawn_option.clone());
    let id = SpawnOptionId::from(selected.as_str());
    let harness_id_string = id.harness_id().to_string();
    ResolvedCircuitSpawnOption {
        spawn_option,
        harness_id: harness_id_string,
        explicit_circuit_provider: explicit.is_some(),
    }
}

/// Resolve the Spawn Option's harness half to its underlying Agent Harness
/// adapter id (the half `HarnessCapabilities` accepts). Returns `None` for
/// unknown harness ids so the caller can surface
/// [`UnknownHarness`](CircuitCompatibilityReason::UnknownHarness) with the
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
/// the [`UnknownHarness`](CircuitCompatibilityReason::UnknownHarness)
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

/// Whether the harness reports its own turn lifecycle: a native attention
/// hook or a backend-owned passive turn watcher that publishes standard Node
/// Turns. The adapter owns watcher startup, while this evaluator owns the
/// question of whether that signal is sufficient.
pub fn harness_has_lifecycle_signal(caps: &HarnessCapabilities) -> bool {
    caps.requires_attention_hook || caps.supports_passive_turn_watcher
}

/// Whether a Circuit can only learn that this harness finished a turn from
/// the result file its prompt asks for. Such a harness gets a result file on
/// every Circuit prompt, because nothing else will ever end its turn.
pub fn completes_by_result_file_only(caps: &HarnessCapabilities) -> bool {
    !harness_has_lifecycle_signal(caps) && caps.supports_result_file_handoff
}

/// Whether the harness supplies any turn-completion signal a Circuit can
/// wait on: its own lifecycle, or the result file it is asked to write when
/// it is done. The result file is the lower-fidelity source: it proves a
/// finished report but never a question, a permission or owned background
/// work, so those stay explicit gaps in the observation diagnostics.
///
/// Shared by [`evaluate`]'s harness half and the circuit reviewer gate
/// ([`reviewer_harness_reason`]) so the two predicates cannot drift: when
/// a harness gains a signal, both sides flip together.
pub fn harness_has_turn_signal(caps: &HarnessCapabilities) -> bool {
    harness_has_lifecycle_signal(caps) || caps.supports_result_file_handoff
}

/// Reviewer-side harness check: the harness half of [`evaluate`] **minus**
/// the `UnknownHarness` fail-closed arm. Unknown harness ids return `None`
/// (fail-open) — `AgentNode.provider` holds user-defined harness profile
/// ids the spawn seam resolves to a real executor, and refusing those
/// would take a working review away from the user. That divergence from
/// `evaluate` (which fails unknown harnesses closed) is deliberate: the
/// reviewer gate only blocks what it can prove ineligible, mirroring the
/// frontend `blocksReviewCircuit` predicate.
pub fn reviewer_harness_reason(harness_id: &str) -> Option<CircuitCompatibilityReason> {
    let caps = lookup_capabilities(harness_id)?;
    if caps.is_plain_terminal {
        return Some(CircuitCompatibilityReason::PlainTerminal);
    }
    if !harness_has_turn_signal(&caps) {
        return Some(CircuitCompatibilityReason::MissingAttentionHook {
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
    let selected = crate::preferences::launch_configurations::selection_option(trimmed)?;
    let harness_id = SpawnOptionId::from(selected.as_str())
        .harness_id()
        .trim()
        .to_string();
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
pub fn reviewer_refusal_message(reason: &CircuitCompatibilityReason) -> String {
    match reason {
        CircuitCompatibilityReason::PlainTerminal => {
            "Terminal cannot be used as the reviewer provider.".to_string()
        }
        CircuitCompatibilityReason::MissingAttentionHook { harness_id } => {
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
pub struct CircuitCompatibilityInput<'a> {
    /// The Spawn Option id Autopilot will launch.
    pub resolved_spawn_option: &'a str,
    /// The harness half of the Spawn Option.
    pub resolved_harness_id: &'a str,
    /// The harness's authoritative capability descriptor. `None` when the
    /// harness id is unknown — the evaluator emits
    /// [`UnknownHarness`](CircuitCompatibilityReason::UnknownHarness) and
    /// skips the per-capability reasons.
    pub capabilities: Option<HarnessCapabilities>,
    /// The mesh's `use_worktree` flag. Autopilot forces worktrees on, so a
    /// `false` here is the user expressing "I don't want worktrees" — the
    /// gate rejects the spawn with [`WorktreeDisabled`](CircuitCompatibilityReason::WorktreeDisabled).
    pub mesh_use_worktree: bool,
    /// Whether the Spawn Option came from the mesh's explicit Autopilot
    /// selection (vs the default chain).
    pub explicit_circuit_provider: bool,
}

/// Pure compatibility evaluator (issue #1152). Returns the full
/// [`CircuitCompatibility`] struct — never a bare bool — so the caller
/// always has the reason text for the user and the explicit-selection flag
/// for labelling.
///
/// The decision is **total**: every input combination produces a defined
/// result. There is no panic path; `Option::None` for the capabilities
/// falls through to `UnknownHarness` rather than aborting.
pub fn evaluate(input: CircuitCompatibilityInput<'_>) -> CircuitCompatibility {
    let mut reasons: Vec<CircuitCompatibilityReason> = Vec::new();

    match input.capabilities.as_ref() {
        None => reasons.push(CircuitCompatibilityReason::UnknownHarness {
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
                reasons.push(CircuitCompatibilityReason::PlainTerminal);
            }
            // Shared predicate with the reviewer gate (issue #1816) —
            // same verdict here as in `reviewer_harness_reason`.
            if !harness_has_turn_signal(caps) {
                reasons.push(CircuitCompatibilityReason::MissingAttentionHook {
                    harness_id: input.resolved_harness_id.to_string(),
                });
            }
        }
    }

    if !input.mesh_use_worktree {
        reasons.push(CircuitCompatibilityReason::WorktreeDisabled);
    }

    CircuitCompatibility {
        allowed: reasons.is_empty(),
        reasons,
        resolved_harness_id: Some(input.resolved_harness_id.to_string()),
        resolved_spawn_option: Some(input.resolved_spawn_option.to_string()),
        explicit_circuit_provider: input.explicit_circuit_provider,
    }
}

/// Compose [`resolve_circuit_spawn_option`] + [`lookup_capabilities`] +
/// [`evaluate`] for the conventional mesh-row → verdict path. Thin wrapper
/// so callers don't have to thread every argument through by hand; the
/// seam still lives in the pure helpers above for unit testing.
pub fn compute_for_mesh(
    circuit_provider: Option<&str>,
    mesh_default_provider: Option<&str>,
    app_default_provider: Option<&str>,
    mesh_use_worktree: bool,
) -> CircuitCompatibility {
    let resolved = resolve_circuit_spawn_option(
        circuit_provider,
        mesh_default_provider,
        app_default_provider,
    );
    let capabilities = lookup_capabilities(&resolved.harness_id);
    evaluate(CircuitCompatibilityInput {
        resolved_spawn_option: &resolved.spawn_option,
        resolved_harness_id: &resolved.harness_id,
        capabilities,
        mesh_use_worktree,
        explicit_circuit_provider: resolved.explicit_circuit_provider,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "compatibility_tests.rs"]
mod tests;
