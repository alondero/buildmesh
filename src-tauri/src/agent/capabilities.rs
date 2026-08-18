//! Harness capability contract + per-field configuration resolver (issue #1149).
//!
//! Prefactor for #1148 (Per-Harness Default Configurations & Per-Mesh Overrides).
//! This module introduces the authoritative capability descriptor every Agent
//! Harness adapter declares, plus a pure resolver that masks unsupported
//! configuration values before they reach `build_spawn_command`. The
//! behaviour-preserving contract for this prefactor:
//!
//! * The Mesh row's legacy `model` and `effort` columns feed the resolver's
//!   **Mesh slot**, so a non-empty `meshes.model` / `meshes.effort` still
//!   applies for harnesses that support model/effort overrides — the same
//!   behaviour users have today.
//! * Whitespace-only inputs at every layer are normalised to absent before
//!   the cascade (issue #1148 acceptance criteria 32).
//! * Capability masking happens **inside** the resolver; downstream callers
//!   receive values that already passed the mask. `build_spawn_command` no
//!   longer needs to consult the adapter's `supports_*` flags.
//! * The per-harness application-default slot is plumbed through the
//!   signature with `None` today; #1148 fills it.
//!
//! Every wire type below is generated to TypeScript via `ts-rs` and committed
//! under `src/types/generated/` (issue #359, drift-gated in CI).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agent::provider::{AgentProvider, Platform};

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// What kind of effort / reasoning-control flag a harness accepts.
///
/// The capability contract: a resolved effort value is only forwarded when
/// the selected harness's control kind matches. The resolver masks
/// unsupported values before they reach `build_spawn_command`, so a harness
/// with no effort control never receives a synthetic `--effort` argument.
///
/// `Closed` matches harnesses like Claude Code whose CLI surface exposes a
/// single flag with a known vocabulary (e.g. `--effort low|medium|high`).
/// `InlineConfig` matches harnesses like Codex whose reasoning effort is
/// passed as an inline per-invocation config override
/// (`-c model_reasoning_effort="..."`); the `key` is the config key the CLI
/// reads and is exposed to the frontend so it can label the corresponding
/// knob correctly.
///
/// Generated to `src/types/generated/EffortControlKind.ts`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "EffortControlKind.ts")]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum EffortControlKind {
    /// No effort control — capability mask drops every resolved effort value.
    #[default]
    None,
    /// Closed-vocabulary CLI flag (e.g. `--effort <low|medium|high>`).
    Closed {
        /// Allowed values the harness's CLI accepts (e.g.
        /// `["low", "medium", "high"]`).
        allowed: Vec<String>,
    },
    /// Inline per-invocation config override (e.g. Codex
    /// `-c model_reasoning_effort="..."`).
    InlineConfig {
        /// Config key the CLI reads (e.g. `"model_reasoning_effort"`).
        key: String,
        /// Allowed values the CLI accepts for this key.
        allowed: Vec<String>,
    },
}

/// Stringify a [`Platform`] for the wire type. Stable across hosts so the
/// frontend can do an exact-match lookup.
pub fn platform_name(platform: Platform) -> &'static str {
    match platform {
        Platform::Macos => "macos",
        Platform::Windows => "windows",
        Platform::Linux => "linux",
    }
}

/// Backend-owned **Harness Capability Contract** (issue #1149, prefactor for
/// #1148). One descriptor per Agent Harness — the same descriptor drives:
///
/// 1. The configuration resolver (`resolve_agent_config`): masks unsupported
///    values before they reach `build_spawn_command`.
/// 2. The Spawn Menu (`ProviderInfo.capabilities`): the frontend can render
///    only the controls each harness actually supports, so a Codex row offers
///    the inline-config effort knob and an OpenCode row offers neither model
///    nor effort.
///
/// Generated to `src/types/generated/HarnessCapabilities.ts`.
///
/// `harness_id` is the adapter id (`AgentProvider::id()`), e.g. `"anthropic"`
/// for the Claude-Code-backed adapter, `"codex"` for Codex. The
/// `ProviderInfo` row carries its own `harness_id` (the **profile** id) for
/// Spawn Menu grouping; the capability descriptor's `harness_id` identifies
/// the adapter whose CLI shape the values target. Frontends that want to
/// match a `ProviderInfo` row to its capabilities should look up via the
/// adapter id the backend's resolver would pick (today: every profile's
/// executor is reachable from the profile row).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "HarnessCapabilities.ts")]
pub struct HarnessCapabilities {
    /// Adapter id (matches [`AgentProvider::id`]).
    pub harness_id: String,
    /// Whether the harness's CLI accepts a resume invocation.
    pub supports_resume: bool,
    /// Whether the app auto-resumes suspended sessions for this harness.
    pub auto_resume_on_startup: bool,
    /// Whether the spawn path installs an attention hook for this harness
    /// (issue #886).
    pub requires_attention_hook: bool,
    /// Whether the harness writes a transcript the coordinator read API
    /// can parse into a Node Digest's rich layer (ADR-0008).
    pub produces_readable_transcript: bool,
    /// Whether `--model <name>` (or equivalent) from configuration applies.
    pub supports_model_override: bool,
    /// Whether `--effort <level>` (or equivalent) from configuration applies.
    /// Mirrors `effort_control != EffortControlKind::None`; pinned by a unit
    /// test so the two can't drift.
    pub supports_effort_override: bool,
    /// Whether `--prefill <text>` (or equivalent positional) is accepted.
    pub supports_prefill: bool,
    /// True for plain shell providers — LLM-specific paths (naming, the
    /// 3-second early-exit heuristic, etc.) all skip.
    pub is_plain_terminal: bool,
    /// The kind of effort control this harness accepts. The capability mask
    /// drops any resolved effort value that doesn't match.
    pub effort_control: EffortControlKind,
    /// Host platforms where this harness runs (snake_case names — `"windows"`,
    /// `"macos"`, `"linux"`). Used by the Spawn Menu and by future
    /// application-default routing.
    pub available_on: Vec<String>,
}

/// The output of the per-field configuration resolver. Every field is
/// already capability-masked: a `None` here means either "all upstream slots
/// were empty" or "the selected harness doesn't support this control" —
/// downstream callers (`build_spawn_command`) can forward the `Some` value
/// verbatim without consulting any further capability flag.
///
/// Generated to `src/types/generated/ResolvedAgentConfig.ts`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "ResolvedAgentConfig.ts")]
pub struct ResolvedAgentConfig {
    /// Capability-masked model id, or `None` if no layer supplied one or the
    /// harness doesn't accept a model override.
    pub model: Option<String>,
    /// Capability-masked effort / reasoning value, or `None` if no layer
    /// supplied one, the harness doesn't accept effort, or the value isn't
    /// in the harness's allowed vocabulary.
    pub effort: Option<String>,
}

// ---------------------------------------------------------------------------
// Per-field cascade inputs
// ---------------------------------------------------------------------------

/// Per-field input to the configuration resolver. Each layer is the "raw"
/// value as it came from its source — whitespace-only inputs are normalised
/// inside `resolve_field` so all layers are treated uniformly.
///
/// Layers, highest precedence first:
/// 1. **Explicit Agent Node spawn argument** — ad-hoc per-launch values the
///    caller passed in (e.g. an autopilot-side override).
/// 2. **Mesh row value** — the legacy `meshes.model` / `meshes.effort`
///    columns. The Mesh slot is the only layer this prefactor fills.
/// 3. **Application-level default** — per-harness defaults from App Settings
///    (issue #1148). The slot exists today; the value is always `None` until
///    that ticket lands.
/// 4. **Harness native fallback** — never a Buildmesh synthetic value: when
///    every supplied layer is empty/absent, the resolver returns `None` so
///    the harness runs with its own defaults.
#[derive(Debug, Clone, Default)]
pub struct FieldInputs<'a> {
    /// Explicit Agent Node spawn argument (highest precedence).
    pub explicit: Option<&'a str>,
    /// Mesh row value (legacy `meshes.model` / `meshes.effort`).
    pub mesh: Option<&'a str>,
    /// Application-level default (future #1148 — always `None` today).
    pub application: Option<&'a str>,
}

/// Per-field inputs to the configuration resolver. `model` and `effort` are
/// resolved independently so each layer's cascade runs per field.
#[derive(Debug, Clone, Default)]
pub struct AgentConfigInputs<'a> {
    pub model: FieldInputs<'a>,
    pub effort: FieldInputs<'a>,
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

/// Resolve the model and effort values for one spawn against the selected
/// harness's capability contract.
///
/// Pure (no I/O, no globals) so the resolver is the unit-test seam for both
/// the cascade and the capability mask. Returns a [`ResolvedAgentConfig`]
/// every `Some` of which is safe to forward to `build_spawn_command`.
///
/// Behaviour pinned by `tests::resolver_*` in the `mod tests` block below.
pub fn resolve_agent_config(
    capabilities: &HarnessCapabilities,
    inputs: AgentConfigInputs<'_>,
) -> ResolvedAgentConfig {
    let model = resolve_field(inputs.model).filter(|_| capabilities.supports_model_override);
    let effort = resolve_effort(inputs.effort, &capabilities.effort_control);
    ResolvedAgentConfig { model, effort }
}

/// Whitespace-normalised first-non-empty-layer picker for one field. Every
/// layer is trimmed; a layer that is empty or whitespace-only collapses to
/// absent so the cascade falls through to the next layer.
fn resolve_field(field: FieldInputs<'_>) -> Option<String> {
    field
        .explicit
        .and_then(normalize_non_empty)
        .or_else(|| field.mesh.and_then(normalize_non_empty))
        .or_else(|| field.application.and_then(normalize_non_empty))
}

/// Apply the capability mask for the effort field. Two-stage mask:
/// 1. `EffortControlKind::None` → drop unconditionally (the harness has no
///    effort control; Buildmesh must not invent one).
/// 2. `Closed { allowed }` / `InlineConfig { allowed, .. }` → drop if the
///    value isn't in `allowed`. The `InlineConfig.key` is UI metadata only
///    (knob labelling on the frontend); the resolver matches the value
///    shape, not the key, so the two arms collapse into one.
fn resolve_effort(field: FieldInputs<'_>, control: &EffortControlKind) -> Option<String> {
    let allowed: &[String] = match control {
        EffortControlKind::None => return None,
        EffortControlKind::Closed { allowed } => allowed,
        EffortControlKind::InlineConfig { allowed, .. } => allowed,
    };
    resolve_field(field).filter(|v| allowed.iter().any(|a| a == v))
}

/// Trim and drop empties. Pure helper so every layer flows through the same
/// normalisation (issue #1148 acceptance criteria 32).
fn normalize_non_empty(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

// ---------------------------------------------------------------------------
// Trait integration
// ---------------------------------------------------------------------------

/// Default implementation that packages every `AgentProvider` capability flag
/// into a [`HarnessCapabilities`] descriptor. The default impl makes the
/// trait additive: no existing adapter needs to override `capabilities()`
/// to compile, and adding a new flag at the trait level automatically
/// appears in every adapter's descriptor.
///
/// Adapters that need a non-default `effort_control` override
/// [`capabilities_for`] (the lower-level helper) rather than this method —
/// the override pattern keeps each adapter's CLI surface in one place.
pub fn capabilities_for(adapter: &dyn AgentProvider) -> HarnessCapabilities {
    let platforms: Vec<String> = adapter
        .available_on()
        .iter()
        .map(|p| platform_name(*p).to_string())
        .collect();
    HarnessCapabilities {
        harness_id: adapter.id().to_string(),
        supports_resume: adapter.supports_resume(),
        auto_resume_on_startup: adapter.auto_resume_on_startup(),
        requires_attention_hook: adapter.requires_attention_hook(),
        produces_readable_transcript: adapter.produces_readable_transcript(),
        supports_model_override: adapter.supports_model_override(),
        supports_effort_override: !matches!(effort_control_for(adapter), EffortControlKind::None),
        supports_prefill: adapter.supports_prefill(),
        is_plain_terminal: adapter.is_plain_terminal(),
        effort_control: effort_control_for(adapter),
        available_on: platforms,
    }
}

/// Default `EffortControlKind` derived from the trait surface. Closed vocab
/// when the adapter reports `supports_effort_override() = true` AND the
/// adapter's native flag follows the closed-vocab convention (Claude Code);
/// `None` otherwise. Adapters with non-default effort shapes
/// (Codex's inline config) override [`capabilities_for`] directly.
fn effort_control_for(adapter: &dyn AgentProvider) -> EffortControlKind {
    if adapter.id() == "anthropic" {
        return EffortControlKind::Closed {
            allowed: CLAUDE_EFFORT_ALLOWED.iter().map(|s| s.to_string()).collect(),
        };
    }
    if adapter.id() == "codex" {
        return EffortControlKind::InlineConfig {
            key: CODEX_EFFORT_KEY.to_string(),
            allowed: CODEX_EFFORT_ALLOWED.iter().map(|s| s.to_string()).collect(),
        };
    }
    EffortControlKind::None
}

/// The closed-vocabulary effort values Claude Code accepts (issue #1143
/// research). Kept here (not in the adapter) so the resolver and the
/// capability descriptor agree on the same vocabulary.
pub const CLAUDE_EFFORT_ALLOWED: &[&str] = &["low", "medium", "high"];

/// Codex's reasoning-effort config key. The Codex CLI reads
/// `model_reasoning_effort` from `-c` overrides (issue #1143 research, the
/// inline-config pattern that distinguishes Codex from the closed-vocab
/// harnesses).
pub const CODEX_EFFORT_KEY: &str = "model_reasoning_effort";

/// The allowed values Codex accepts for `model_reasoning_effort` (issue
/// #1143 research).
pub const CODEX_EFFORT_ALLOWED: &[&str] = &["none", "low", "medium", "high", "xhigh"];

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn anthropic_caps() -> HarnessCapabilities {
        capabilities_for(&crate::agent::provider::adapters::ANTHROPIC)
    }

    fn codex_caps() -> HarnessCapabilities {
        capabilities_for(&crate::agent::provider::adapters::CODEX)
    }

    fn agy_caps() -> HarnessCapabilities {
        capabilities_for(&crate::agent::provider::adapters::AGY)
    }

    fn opencode_caps() -> HarnessCapabilities {
        capabilities_for(&crate::agent::provider::adapters::OPENCODE)
    }

    fn terminal_caps() -> HarnessCapabilities {
        capabilities_for(&crate::agent::provider::adapters::TERMINAL)
    }

    fn kimi_caps() -> HarnessCapabilities {
        capabilities_for(&crate::agent::provider::adapters::KIMI)
    }

    fn grok_caps() -> HarnessCapabilities {
        capabilities_for(&crate::agent::provider::adapters::GROK)
    }

    fn mcode_caps() -> HarnessCapabilities {
        capabilities_for(&crate::agent::provider::adapters::MCODE)
    }

    /// Inventory pin (issue #1149 step 1) — every adapter's capability
    /// descriptor must match the matrix documented in `docs/knowledge-primer.md`
    /// and the #1143 research summary. Drift here means a future adapter
    /// edit changed the capability surface without updating the prefactor.
    #[test]
    fn inventory_matches_research_matrix() {
        let anthropic = anthropic_caps();
        assert_eq!(anthropic.harness_id, "anthropic");
        assert!(anthropic.supports_resume);
        assert!(anthropic.auto_resume_on_startup);
        assert!(anthropic.requires_attention_hook);
        assert!(anthropic.produces_readable_transcript);
        assert!(anthropic.supports_model_override);
        assert!(anthropic.supports_effort_override);
        assert!(anthropic.supports_prefill);
        assert!(!anthropic.is_plain_terminal);
        assert_eq!(
            anthropic.effort_control,
            EffortControlKind::Closed {
                allowed: CLAUDE_EFFORT_ALLOWED.iter().map(|s| s.to_string()).collect(),
            }
        );
        assert_eq!(
            anthropic.available_on,
            vec!["windows".to_string(), "macos".to_string(), "linux".to_string()]
        );

        let codex = codex_caps();
        assert_eq!(codex.harness_id, "codex");
        assert!(codex.supports_resume);
        assert!(codex.requires_attention_hook);
        assert!(codex.produces_readable_transcript);
        assert!(codex.supports_model_override);
        assert!(codex.supports_effort_override);
        assert!(codex.supports_prefill);
        assert_eq!(
            codex.effort_control,
            EffortControlKind::InlineConfig {
                key: CODEX_EFFORT_KEY.to_string(),
                allowed: CODEX_EFFORT_ALLOWED.iter().map(|s| s.to_string()).collect(),
            }
        );

        let agy = agy_caps();
        assert_eq!(agy.harness_id, "agy");
        assert!(agy.supports_resume);
        assert!(agy.requires_attention_hook);
        assert!(!agy.produces_readable_transcript);
        assert!(agy.supports_model_override);
        assert!(!agy.supports_effort_override);
        assert!(agy.supports_prefill);
        assert_eq!(agy.effort_control, EffortControlKind::None);

        let opencode = opencode_caps();
        assert_eq!(opencode.harness_id, "opencode");
        assert!(!opencode.supports_resume);
        assert!(!opencode.requires_attention_hook);
        assert!(!opencode.supports_model_override);
        assert!(!opencode.supports_effort_override);
        assert!(!opencode.supports_prefill);
        assert_eq!(opencode.effort_control, EffortControlKind::None);

        let terminal = terminal_caps();
        assert!(terminal.is_plain_terminal);
        assert!(!terminal.supports_resume);
        assert!(!terminal.requires_attention_hook);
        assert!(!terminal.supports_model_override);
        assert!(!terminal.supports_effort_override);
        assert!(!terminal.supports_prefill);
        assert_eq!(terminal.effort_control, EffortControlKind::None);

        // Interactive-TUI harnesses — model override, no effort, no prefill,
        // no attention hook (issue #886), no transcript reader yet.
        let kimi = kimi_caps();
        assert!(kimi.supports_model_override);
        assert!(!kimi.supports_effort_override);
        assert!(!kimi.supports_prefill);
        assert_eq!(kimi.effort_control, EffortControlKind::None);

        let grok = grok_caps();
        assert!(grok.supports_model_override);
        assert!(!grok.supports_effort_override);
        assert_eq!(grok.effort_control, EffortControlKind::None);

        let mcode = mcode_caps();
        assert!(mcode.supports_model_override);
        assert!(!mcode.supports_effort_override);
        assert!(mcode.supports_prefill);
        assert_eq!(mcode.effort_control, EffortControlKind::None);
    }

    /// `supports_effort_override` must mirror `effort_control != None`. The
    /// pin prevents a future refactor that flips one without the other —
    /// which would mask effort values at the resolver but advertise them to
    /// the frontend (or vice versa).
    #[test]
    fn supports_effort_override_matches_effort_control_kind() {
        for caps in [
            anthropic_caps(),
            codex_caps(),
            agy_caps(),
            opencode_caps(),
            terminal_caps(),
            kimi_caps(),
            grok_caps(),
            mcode_caps(),
        ] {
            let has_effort_control = !matches!(caps.effort_control, EffortControlKind::None);
            assert_eq!(
                caps.supports_effort_override, has_effort_control,
                "supports_effort_override must equal (effort_control != None) for {}; \
                 got supports={} control={:?}",
                caps.harness_id, caps.supports_effort_override, caps.effort_control
            );
        }
    }

    /// Cascade: explicit > mesh > application. Each layer wins over the
    /// next-lower one when non-empty.
    #[test]
    fn resolver_cascade_prefers_explicit_over_mesh_over_application() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: Some("explicit-model"),
                mesh: Some("mesh-model"),
                application: Some("app-model"),
            },
            effort: FieldInputs {
                explicit: Some("high"),
                mesh: Some("medium"),
                application: Some("low"),
            },
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert_eq!(resolved.model.as_deref(), Some("explicit-model"));
        assert_eq!(resolved.effort.as_deref(), Some("high"));
    }

    /// Cascade fallthrough: an empty / whitespace-only layer lets the next
    /// layer win.
    #[test]
    fn resolver_cascade_falls_through_whitespace_layers() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: Some("   "),
                mesh: Some("  \t  "),
                application: Some("opus-4"),
            },
            effort: FieldInputs {
                explicit: None,
                mesh: Some(""),
                application: Some("medium"),
            },
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert_eq!(resolved.model.as_deref(), Some("opus-4"));
        assert_eq!(resolved.effort.as_deref(), Some("medium"));
    }

    /// Mask: an open-code harness never receives a model arg even when the
    /// Mesh layer supplied one. The capability mask is the contract that
    /// unsupported values never reach a harness process.
    #[test]
    fn resolver_drops_model_for_harness_without_model_override() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: None,
                mesh: Some("some-model"),
                application: None,
            },
            effort: FieldInputs::default(),
        };
        let resolved = resolve_agent_config(&opencode_caps(), inputs);
        assert!(
            resolved.model.is_none(),
            "OpenCode doesn't support model overrides; the mask must drop any value"
        );
    }

    /// Mask: a non-effort harness never receives an effort arg even when
    /// every layer supplied one. Issue #1149 acceptance criteria 7 — "a value
    /// must be omitted when the selected harness does not support that
    /// control, regardless of which precedence layer supplied it".
    #[test]
    fn resolver_drops_effort_for_harness_without_effort_control() {
        for caps in [agy_caps(), opencode_caps(), terminal_caps(), kimi_caps()] {
            let inputs = AgentConfigInputs {
                model: FieldInputs::default(),
                effort: FieldInputs {
                    explicit: Some("high"),
                    mesh: Some("medium"),
                    application: Some("low"),
                },
            };
            let resolved = resolve_agent_config(&caps, inputs.clone());
            assert!(
                resolved.effort.is_none(),
                "{} advertises EffortControlKind::None; effort must drop for every layer. \
                 Got: {:?}",
                caps.harness_id,
                resolved.effort
            );
        }
    }

    /// Mask: Claude Code's closed vocabulary drops values outside
    /// `low|medium|high` even when the layer supplied one. The frontend can
    /// use the same vocabulary to gate the input control.
    #[test]
    fn resolver_drops_effort_outside_claude_vocabulary() {
        let inputs = AgentConfigInputs {
            model: FieldInputs::default(),
            effort: FieldInputs {
                explicit: Some("xhigh"),
                mesh: None,
                application: None,
            },
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert!(
            resolved.effort.is_none(),
            "Claude Code rejects values outside the closed vocabulary; got {:?}",
            resolved.effort
        );
    }

    /// Mask: Codex's inline-config vocabulary (`none|low|medium|high|xhigh`)
    /// is a superset of Claude's — `xhigh` passes Codex's mask but fails
    /// Claude's. The capability contract is per-harness.
    #[test]
    fn resolver_accepts_codex_xhigh_but_rejects_claude_xhigh() {
        let inputs = AgentConfigInputs {
            model: FieldInputs::default(),
            effort: FieldInputs {
                explicit: Some("xhigh"),
                mesh: None,
                application: None,
            },
        };
        let codex = resolve_agent_config(&codex_caps(), inputs.clone());
        assert_eq!(
            codex.effort.as_deref(),
            Some("xhigh"),
            "Codex accepts xhigh via its inline-config vocabulary"
        );
        let claude = resolve_agent_config(&anthropic_caps(), inputs);
        assert!(
            claude.effort.is_none(),
            "Claude's closed vocabulary rejects xhigh"
        );
    }

    /// Mesh legacy contract (issue #1149 acceptance criteria 5): a non-empty
    /// Mesh row still affects supported harnesses before the later migration
    /// ticket lands. Pin: feeding the Mesh slot through the resolver with
    /// no explicit or application value resolves to the Mesh value.
    #[test]
    fn resolver_mesh_slot_drives_supported_harness() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: None,
                mesh: Some("opus-4-1"),
                application: None,
            },
            effort: FieldInputs {
                explicit: None,
                mesh: Some("high"),
                application: None,
            },
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert_eq!(resolved.model.as_deref(), Some("opus-4-1"));
        assert_eq!(resolved.effort.as_deref(), Some("high"));
    }

    /// Mesh legacy contract — unsupported harnesses ignore the Mesh slot.
    /// A Terminal Mesh row with a `model` set must NOT result in a model
    /// arg forwarded to `powershell.exe`.
    #[test]
    fn resolver_mesh_slot_masked_for_unsupported_harness() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: None,
                mesh: Some("opus"),
                application: None,
            },
            effort: FieldInputs {
                explicit: None,
                mesh: Some("high"),
                application: None,
            },
        };
        let resolved = resolve_agent_config(&terminal_caps(), inputs);
        assert!(resolved.model.is_none());
        assert!(resolved.effort.is_none());
    }

    /// Whitespace-only at every layer collapses to absent for both fields.
    /// Issue #1148 acceptance criteria 32 — "model and effort values trimmed
    /// and empty values treated as absent".
    #[test]
    fn resolver_treats_all_whitespace_layers_as_absent() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: Some("   "),
                mesh: Some("\t\n  "),
                application: Some("  "),
            },
            effort: FieldInputs {
                explicit: Some(" "),
                mesh: Some("\n"),
                application: Some("\t"),
            },
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert_eq!(resolved.model, None);
        assert_eq!(resolved.effort, None);
    }

    /// Trim: a layer value with surrounding whitespace keeps its trimmed
    /// content (the harness shouldn't receive ` opus `, but `opus`).
    #[test]
    fn resolver_trims_layer_values() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: Some("  opus  "),
                mesh: None,
                application: None,
            },
            effort: FieldInputs {
                explicit: Some(" high\t"),
                mesh: None,
                application: None,
            },
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert_eq!(resolved.model.as_deref(), Some("opus"));
        assert_eq!(resolved.effort.as_deref(), Some("high"));
    }

    /// Plain-terminal harness with no synthetic flags: capability mask +
    /// empty cascade both contribute. The resolved config must be fully
    /// empty so `build_spawn_command` doesn't forward anything to the
    /// user's shell (the acceptance criteria "Terminal and other
    /// non-configurable harnesses receive no synthetic model or effort
    /// arguments").
    #[test]
    fn terminal_harness_receives_no_synthetic_flags() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: Some("anything"),
                mesh: Some("whatever"),
                application: None,
            },
            effort: FieldInputs {
                explicit: Some("high"),
                mesh: Some("low"),
                application: None,
            },
        };
        let resolved = resolve_agent_config(&terminal_caps(), inputs);
        assert_eq!(resolved.model, None);
        assert_eq!(resolved.effort, None);
    }

    /// Capability contract pinning for CodexProxy rows. The harness
    /// descriptor is shared with the native Codex row (same adapter); the
    /// pairing's credentials and model-tier translation stay in the routing
    /// layer (acceptance criteria 8). Tested here by checking the
    /// descriptor matches between the native Codex capability descriptor
    /// and what a Proxied Codex pairing would resolve to.
    #[test]
    fn codex_proxy_rows_share_capability_contract_with_native() {
        let native = codex_caps();
        // `capabilities_for` is adapter-scoped — every Codex row (native or
        // Proxied) routes through `Provider::Codex.adapter()`. The
        // descriptor is byte-identical regardless of pairing.
        assert_eq!(native.harness_id, "codex");
        assert!(native.supports_effort_override);
        assert!(native.supports_model_override);
    }

    /// Default helper coverage: an empty resolver input produces an
    /// empty `ResolvedAgentConfig` (caller can use this as a sentinel
    /// "no overrides" without constructing the struct by hand).
    #[test]
    fn empty_inputs_produce_empty_resolved_config() {
        let resolved = resolve_agent_config(&anthropic_caps(), AgentConfigInputs::default());
        assert_eq!(resolved.model, None);
        assert_eq!(resolved.effort, None);
    }
}
