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
///    the inline-config effort knob and an OpenCode row offers model (but
///    not effort — TUI has no `--variant`).
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
/// Layers, highest precedence first (issue #1151 / slice 2 of #1148):
/// 1. **Explicit Agent Node spawn argument** — ad-hoc per-launch values the
///    caller passed in (e.g. an autopilot-side override).
/// 2. **Mesh harness override** — the sparse per-Mesh `harness_overrides`
///    map keyed by the selected harness's stable id. A mesh may override
///    multiple harnesses independently (acceptance criteria 8-9); a missing
///    harness entry means "no override on this mesh" and the cascade falls
///    through.
/// 3. **Mesh row legacy value** — the deprecated `meshes.model` /
///    `meshes.effort` columns. Physically present for positional
///    compatibility but never populated by new UI writes after the v33
///    migration; the v33 one-shot migration copies non-empty legacy values
///    into the `claude` entry of the new map, so the legacy columns are
///    inert on the resolved path for any Mesh that has been
///    post-migration-read by the resolver.
/// 4. **Application-level default** — per-harness defaults from App Settings
///    (issue #1150). Filled by `preferences::harness_default_for` at the
///    spawn seam.
/// 5. **Harness native fallback** — never a Buildmesh synthetic value: when
///    every supplied layer is empty/absent, the resolver returns `None` so
///    the harness runs with its own defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldInputs<'a> {
    /// Explicit Agent Node spawn argument (highest precedence).
    pub explicit: Option<&'a str>,
    /// Per-Mesh harness override (issue #1151, sparse layer keyed by
    /// the selected harness's stable id). `Some("")` is treated as
    /// "inherited" — the resolver normalises whitespace-only to absent
    /// so a "reset to inherit" UI click collapses cleanly.
    pub mesh_override: Option<&'a str>,
    /// Mesh row legacy value (`meshes.model` / `meshes.effort`). Kept
    /// here so a pre-v33 read shape still resolves; the v33+
    /// migration copied non-empty values into `mesh_override["claude"]`,
    /// so on a healthy v33+ DB this layer is always `None`.
    pub mesh: Option<&'a str>,
    /// Application-level default (issue #1150).
    pub application: Option<&'a str>,
}

/// Per-field inputs to the configuration resolver. `model` and `effort` are
/// resolved independently so each layer's cascade runs per field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
/// absent so the cascade falls through to the next layer. Cascade order
/// mirrors the issue #1148 cascade (slice 2 settles the per-Mesh override
/// layer between explicit and the legacy Mesh row):
///   explicit > mesh_override > mesh (legacy) > application > native
fn resolve_field(field: FieldInputs<'_>) -> Option<String> {
    field
        .explicit
        .and_then(normalize_non_empty)
        .or_else(|| field.mesh_override.and_then(normalize_non_empty))
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
/// Derive a [`HarnessCapabilities`] descriptor for one adapter.
///
/// Thin pass-through to [`AgentProvider::capabilities`] (issue #1179).
/// Retained as a free function so the ~9 external call sites
/// (`provider_menu.rs`, `autopilot/compatibility.rs`, `preferences.rs`,
/// `commands/agent_tests.rs`, `spawn.rs`, the inventory tests) keep
/// their existing import surface; the body is one line so any future
/// harness that overrides `capabilities()` automatically flows through.
pub fn capabilities_for(adapter: &dyn AgentProvider) -> HarnessCapabilities {
    adapter.capabilities()
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

/// Grok Code (1.0.5) accepts `--reasoning-effort <level>` (alias `--effort`)
/// with the seven canonical levels documented at
/// `~/.grok/docs/user-guide/14-headless-mode.md` (issue #1280 research, also
/// `docs/learning/grok-harness-capabilities.md`). A given model only honours
/// the levels its menu advertises — the seven-value vocabulary is the
/// superset of every model's allowed set; the resolver drops anything the
/// active model doesn't accept.
pub const GROK_EFFORT_ALLOWED: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max",
];

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

    fn cursor_caps() -> HarnessCapabilities {
        capabilities_for(&crate::agent::provider::adapters::CURSOR)
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

    fn dsh_caps() -> HarnessCapabilities {
        capabilities_for(&crate::agent::provider::adapters::DSH)
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

        let cursor = cursor_caps();
        assert_eq!(cursor.harness_id, "cursor");
        assert!(cursor.supports_resume);
        assert!(cursor.auto_resume_on_startup);
        assert!(!cursor.requires_attention_hook);
        assert!(cursor.produces_readable_transcript);
        assert!(cursor.supports_model_override);
        assert!(!cursor.supports_effort_override);
        assert!(cursor.supports_prefill);
        assert_eq!(cursor.effort_control, EffortControlKind::None);

        let agy = agy_caps();
        assert_eq!(agy.harness_id, "agy");
        assert!(agy.supports_resume);
        assert!(agy.requires_attention_hook);
        // Issue #1283: AGY writes per-conversation JSONL, so the
        // transcript reader can hydrate the Node Digest / archive picker.
        assert!(agy.produces_readable_transcript);
        assert!(agy.supports_model_override);
        // Issue #1286: agy's CLI accepts `--effort <low|medium|high>`
        // (verified against `agy --help`). The closed vocabulary
        // mirrors Anthropic's; only the vocabulary differs from a
        // vocabulary-superset perspective (both use `low|medium|high`).
        assert!(agy.supports_effort_override);
        assert!(agy.supports_prefill);
        assert_eq!(
            agy.effort_control,
            EffortControlKind::Closed {
                allowed: vec!["low".into(), "medium".into(), "high".into()],
            }
        );

        let opencode = opencode_caps();
        assert_eq!(opencode.harness_id, "opencode");
        assert!(opencode.supports_resume);
        assert!(opencode.auto_resume_on_startup);
        assert!(!opencode.requires_attention_hook);
        assert!(opencode.supports_model_override);
        assert!(!opencode.supports_effort_override);
        assert!(opencode.supports_prefill);
        assert_eq!(opencode.effort_control, EffortControlKind::None);

        let terminal = terminal_caps();
        assert!(terminal.is_plain_terminal);
        assert!(!terminal.supports_resume);
        assert!(!terminal.requires_attention_hook);
        assert!(!terminal.supports_model_override);
        assert!(!terminal.supports_effort_override);
        assert!(!terminal.supports_prefill);
        assert_eq!(terminal.effort_control, EffortControlKind::None);

        // Kimi Code is the remaining interactive TUI without prefill
        // (positional prompt is a Grok / Cursor / mcode shape). No
        // effort, no attention hook (issue #886).
        let kimi = kimi_caps();
        assert!(kimi.supports_model_override);
        assert!(!kimi.supports_effort_override);
        assert!(!kimi.supports_prefill);
        assert_eq!(kimi.effort_control, EffortControlKind::None);

        let grok = grok_caps();
        assert!(grok.supports_resume);
        assert!(grok.auto_resume_on_startup);
        // Issue #1281: Grok Code writes per-session directories under
        // ~/.grok/sessions/<urlencoded-cwd>/<id>/{chat_history.jsonl,
        // updates.jsonl}. TranscriptFormat::Grok parses both into the shared
        // Turn/ToolCall wire shape, so the archived-node picker surfaces
        // Grok and the Coordinator Node Digest hydrates it.
        assert!(grok.produces_readable_transcript);
        assert!(grok.supports_model_override);
        // Issue #1280: Grok 1.0.5 accepts `--reasoning-effort` (alias
        // `--effort`) with the seven canonical levels — see
        // `docs/learning/grok-harness-capabilities.md` and the
        // `GROK_EFFORT_ALLOWED` constant above.
        assert!(grok.supports_effort_override);
        assert!(grok.supports_prefill);
        assert_eq!(
            grok.effort_control,
            EffortControlKind::Closed {
                allowed: GROK_EFFORT_ALLOWED.iter().map(|s| s.to_string()).collect(),
            }
        );

        let mcode = mcode_caps();
        // Issue #1179: mcode's interactive TUI rejects `--model`, so
        // the override is no longer advertised. The capability and
        // recipe now agree (see `adapters::mcode::tests`).
        assert!(!mcode.supports_model_override);
        assert!(!mcode.supports_effort_override);
        assert!(mcode.supports_prefill);
        assert_eq!(mcode.effort_control, EffortControlKind::None);

        let dsh = dsh_caps();
        assert_eq!(dsh.harness_id, "dsh");
        assert!(dsh.supports_resume);
        assert!(dsh.auto_resume_on_startup);
        assert!(!dsh.requires_attention_hook);
        assert!(!dsh.produces_readable_transcript);
        assert!(dsh.supports_model_override);
        assert!(!dsh.supports_effort_override);
        assert!(!dsh.supports_prefill);
        assert_eq!(dsh.effort_control, EffortControlKind::None);
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
            cursor_caps(),
            agy_caps(),
            opencode_caps(),
            terminal_caps(),
            kimi_caps(),
            grok_caps(),
            mcode_caps(),
            dsh_caps(),
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

    /// Issue #1179: `effort_control_for` was an adapter-id switch
    /// (`adapter.id() == "anthropic"`, `== "codex"`) living in
    /// `agent::capabilities`. The refactor moves the choice to the
    /// adapter itself via `AgentProvider::effort_control()`, and
    /// `capabilities_for` becomes a thin pass-through. This test pins
    /// the new contract: for every adapter, the descriptor's
    /// `effort_control` exactly matches the trait method's return.
    /// A future refactor that reintroduces an id switch in the
    /// descriptor (or drops a vocabulary the trait advertises) trips
    /// this test.
    #[test]
    fn effort_control_descriptor_delegates_to_adapter_no_id_switch() {
        for adapter in [
            &crate::agent::provider::adapters::ANTHROPIC as &dyn crate::agent::provider::AgentProvider,
            &crate::agent::provider::adapters::CODEX,
            &crate::agent::provider::adapters::CURSOR,
            &crate::agent::provider::adapters::AGY,
            &crate::agent::provider::adapters::OPENCODE,
            &crate::agent::provider::adapters::TERMINAL,
            &crate::agent::provider::adapters::KIMI,
            &crate::agent::provider::adapters::GROK,
            &crate::agent::provider::adapters::MCODE,
        ] {
            let from_trait = adapter.effort_control();
            let from_descriptor = capabilities_for(adapter).effort_control;
            assert_eq!(
                from_trait, from_descriptor,
                "effort_control must agree between trait method and descriptor for {}; \
                 trait = {:?}, descriptor = {:?}",
                adapter.id(), from_trait, from_descriptor
            );
        }
    }

    /// Cascade: explicit > mesh_override > mesh > application. Each layer
    /// wins over the next-lower one when non-empty. Pin: the per-Mesh
    /// override layer (issue #1151) sits between explicit and the legacy
    /// Mesh row in the cascade order — explicit wins, then mesh_override,
    /// then the legacy mesh columns, then application. A future ordering
    /// regression in `resolve_field` would shatter this pin.
    #[test]
    fn resolver_cascade_prefers_explicit_over_mesh_override_over_mesh_over_application() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: Some("explicit-model"),
                mesh_override: Some("override-model"),
                mesh: Some("mesh-model"),
                application: Some("app-model"),
            },
            effort: FieldInputs {
                explicit: Some("high"),
                mesh_override: Some("override-effort"),
                mesh: Some("medium"),
                application: Some("low"),
            },
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert_eq!(resolved.model.as_deref(), Some("explicit-model"));
        assert_eq!(resolved.effort.as_deref(), Some("high"));
    }

    /// Cascade fallthrough: an empty / whitespace-only layer lets the next
    /// layer win. Issue #1151 cascade order — whitespace `mesh_override`
    /// collapses to absent so the legacy `mesh` layer (or further cascade)
    /// surfaces. Mirrors the pre-#1151 fallthrough test, now extended
    /// through the new layer.
    #[test]
    fn resolver_cascade_falls_through_whitespace_layers() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: Some("   "),
                mesh_override: Some("  \t  "),
                mesh: Some("  \t  "),
                application: Some("opus-4"),
            },
            effort: FieldInputs {
                explicit: None,
                mesh_override: Some(""),
                mesh: Some(""),
                application: Some("medium"),
            },
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert_eq!(resolved.model.as_deref(), Some("opus-4"));
        assert_eq!(resolved.effort.as_deref(), Some("medium"));
    }

    /// Mask: a harness without model override never receives a model arg
    /// even when the Mesh layer supplied one. Terminal is the standing
    /// example (OpenCode now advertises `--model`). The capability mask
    /// is the contract that unsupported values never reach a harness
    /// process. Now extended through the new `mesh_override` layer
    /// (issue #1151).
    #[test]
    fn resolver_drops_model_for_harness_without_model_override() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: None,
                mesh_override: Some("some-model"),
                mesh: Some("some-model"),
                application: None,
            },
            effort: FieldInputs::default(),
        };
        let resolved = resolve_agent_config(&terminal_caps(), inputs);
        assert!(
            resolved.model.is_none(),
            "Terminal doesn't support model overrides; the mask must drop any value"
        );
    }

    /// Mask: a non-effort harness never receives an effort arg even when
    /// every layer supplied one. Issue #1149 acceptance criteria 7 — "a value
    /// must be omitted when the selected harness does not support that
    /// control, regardless of which precedence layer supplied it".
    #[test]
    fn resolver_drops_effort_for_harness_without_effort_control() {
        // agy_caps() dropped from this list in issue #1286: agy's CLI
        // accepts `--effort <low|medium|high>`, so the mask now keeps
        // matching values. The non-effort harnesses (opencode, terminal,
        // kimi) still drop every layer.
        for caps in [opencode_caps(), terminal_caps(), kimi_caps()] {
            let inputs = AgentConfigInputs {
                model: FieldInputs::default(),
                effort: FieldInputs {
                    explicit: Some("high"),
                    mesh_override: Some("medium"),
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
                mesh_override: None,
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
                mesh_override: None,
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
    /// no explicit or application value resolves to the Mesh value. The
    /// `mesh_override` slot is `None` here — the v33 migration is the path
    /// that fills it on a real DB; the legacy column stays inert on the
    /// resolver pass.
    #[test]
    fn resolver_mesh_slot_drives_supported_harness() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: None,
                mesh_override: None,
                mesh: Some("opus-4-1"),
                application: None,
            },
            effort: FieldInputs {
                explicit: None,
                mesh_override: None,
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
                mesh_override: None,
                mesh: Some("opus"),
                application: None,
            },
            effort: FieldInputs {
                explicit: None,
                mesh_override: None,
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
    /// and empty values treated as absent". Now extended through the new
    /// `mesh_override` layer (issue #1151).
    #[test]
    fn resolver_treats_all_whitespace_layers_as_absent() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: Some("   "),
                mesh_override: Some("\t\n  "),
                mesh: Some("\t\n  "),
                application: Some("  "),
            },
            effort: FieldInputs {
                explicit: Some(" "),
                mesh_override: Some("\n"),
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
                mesh_override: None,
                mesh: None,
                application: None,
            },
            effort: FieldInputs {
                explicit: Some(" high\t"),
                mesh_override: None,
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
    /// arguments"). Extended with the new `mesh_override` layer
    /// (issue #1151).
    #[test]
    fn terminal_harness_receives_no_synthetic_flags() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: Some("anything"),
                mesh_override: Some("whatever"),
                mesh: Some("whatever"),
                application: None,
            },
            effort: FieldInputs {
                explicit: Some("high"),
                mesh_override: Some("low"),
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

    // -----------------------------------------------------------------------
    // Application-default slot feed (issue #1150 / #1148)
    // -----------------------------------------------------------------------

    /// The application slot drives the resolved value when explicit + mesh_override
    /// + mesh are empty (issue #1148 cascade layer 4: `explicit > mesh_override > mesh > application > native`).
    ///
    /// The spawn path in `spawn_agent_inner` populates this slot from
    /// `preferences::harness_default_for(&node.provider)`; this test pins the resolver
    /// contract that the spawn path relies on.
    #[test]
    fn resolver_application_slot_drives_anthropic_when_mesh_and_explicit_empty() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: None,
                mesh_override: None,
                mesh: None,
                application: Some("opus-4-1"),
            },
            effort: FieldInputs {
                explicit: None,
                mesh_override: None,
                mesh: None,
                application: Some("high"),
            },
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert_eq!(resolved.model.as_deref(), Some("opus-4-1"));
        assert_eq!(resolved.effort.as_deref(), Some("high"));
    }

    /// Application slot is masked: a non-effort harness never receives an
    /// effort value, regardless of which layer supplied it. Pin: the spawn
    /// path passes the stored default's effort straight into the resolver,
    /// so a stale or hand-edited `preferences.json` entry can't bypass the
    /// capability mask. (Agy accepts model override but not effort, per the
    /// `inventory_matches_research_matrix` pin — the resolver model mask
    /// would otherwise pass the model through.)
    #[test]
    fn resolver_application_slot_dropped_when_capability_masks_it() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: None,
                mesh_override: None,
                mesh: None,
                application: Some("some-model"),
            },
            effort: FieldInputs {
                explicit: None,
                mesh_override: None,
                mesh: None,
                application: Some("high"),
            },
        };
        // Issue #1286: agy now accepts `--effort <low|medium|high>`, so
        // the application's `high` value passes the mask. Switch the
        // harness under test to `kimi` (advertises `supports_model_override`
        // but not effort), preserving the original "model passes,
        // effort drops" shape. `opencode` would also work but lacks
        // model override; kimi is the closest match.
        let resolved = resolve_agent_config(&kimi_caps(), inputs);
        assert_eq!(
            resolved.model.as_deref(),
            Some("some-model"),
            "Kimi accepts model override (per the capability inventory)"
        );
        assert!(resolved.effort.is_none(), "Kimi doesn't accept effort");
    }

    /// Application slot falls through when None (issue #1148 AC #11: "With
    /// no explicit or application value, Buildmesh omits the corresponding
    /// argument and preserves native harness behavior"). The resolver must
    /// return `None` for empty cascade layers — the harness binary's own
    /// defaults apply.
    #[test]
    fn resolver_application_slot_none_falls_through_to_native() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: None,
                mesh_override: None,
                mesh: None,
                application: None,
            },
            effort: FieldInputs::default(),
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert!(
            resolved.model.is_none(),
            "empty cascade must fall through to native behaviour"
        );
    }

    /// Whitespace-only application value collapses to absent (issue #1148
    /// AC #32). The validator at write time trims and removes the entry
    /// for an all-blank value; the resolver still defends in depth so a
    /// stale `preferences.json` (pre-migration hand-edit, or a future
    /// bypass of the validator) cannot forward whitespace to the harness.
    #[test]
    fn resolver_application_slot_trims_whitespace() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: None,
                mesh_override: None,
                mesh: None,
                application: Some("   "),
            },
            effort: FieldInputs {
                explicit: None,
                mesh_override: None,
                mesh: None,
                application: Some("  high  "),
            },
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert_eq!(resolved.model, None, "whitespace-only model collapses");
        assert_eq!(
            resolved.effort.as_deref(),
            Some("high"),
            "trimmed effort passes the mask"
        );
    }

    /// Application default wins over the explicit Agent Node argument's
    /// absence, but is overridden by an explicit Agent Node argument when
    /// both are present (issue #1148 cascade layer 1 > 4).
    #[test]
    fn resolver_explicit_argument_overrides_application_default() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: Some("sonnet-4"),
                mesh_override: None,
                mesh: None,
                application: Some("opus-4-1"),
            },
            effort: FieldInputs::default(),
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert_eq!(resolved.model.as_deref(), Some("sonnet-4"));
    }

    /// Pin the **Proxied Provider** path (issue #1148 AC #12: "Native and
    /// Proxied Provider Spawn Options consume the same application-default
    /// layer"). The spawn seam calls `parse_spawn_option_id` on
    /// `node.provider` so a composite id `"claude:minimax"` looks up the
    /// application default under its harness half `"claude"`. This test
    /// drives the resolver with inputs that mirror what
    /// `spawn_agent_inner` produces for a Proxied row: the harness's
    /// application default feeds through, and the harness's capability
    /// descriptor applies the same mask as the native row.
    #[test]
    fn proxied_provider_spawn_consumes_application_default() {
        // Simulate: the spawn seam split "claude:minimax" → harness_id="claude"
        // → looked up the application default → fed it into the resolver's
        // application slot. The Anthropic capability descriptor is shared
        // with native Claude (same adapter), so the resolved model +
        // effort flow through identically to a native Claude spawn.
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: None,
                mesh_override: None,
                mesh: None,
                application: Some("opus-4-1"),
            },
            effort: FieldInputs {
                explicit: None,
                mesh_override: None,
                mesh: None,
                application: Some("high"),
            },
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert_eq!(resolved.model.as_deref(), Some("opus-4-1"));
        assert_eq!(resolved.effort.as_deref(), Some("high"));
    }

    // -----------------------------------------------------------------------
    // Per-Mesh harness overrides layer (issue #1151 / slice 2 of #1148)
    // -----------------------------------------------------------------------

    /// Mesh override wins over the application default (issue #1151
    /// cascade order explicit > mesh_override > mesh > application > native).
    /// The legacy `mesh` column is intentionally absent here — the v33
    /// migration copied any non-empty legacy values into the new map.
    #[test]
    fn resolver_mesh_override_wins_over_application_default() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: None,
                mesh_override: Some("opus-4-1"),
                mesh: None,
                application: Some("app-model"),
            },
            effort: FieldInputs {
                explicit: None,
                mesh_override: Some("high"),
                mesh: None,
                application: Some("medium"),
            },
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert_eq!(resolved.model.as_deref(), Some("opus-4-1"));
        assert_eq!(resolved.effort.as_deref(), Some("high"));
    }

    /// Mesh override wins over the legacy `mesh` column (issue #1151 — the
    /// cascade order puts the new layer strictly above the legacy Mesh
    /// row). On a healthy v33+ DB the legacy columns are inert and the
    /// cascade resolves to the new map; pinning this protects against a
    /// future reorder that re-promotes the legacy columns.
    #[test]
    fn resolver_mesh_override_wins_over_legacy_mesh_row() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: None,
                mesh_override: Some("opus-4-1"),
                mesh: Some("legacy-model"),
                application: None,
            },
            effort: FieldInputs {
                explicit: None,
                mesh_override: Some("high"),
                mesh: Some("legacy-effort"),
                application: None,
            },
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert_eq!(resolved.model.as_deref(), Some("opus-4-1"));
        assert_eq!(resolved.effort.as_deref(), Some("high"));
    }

    /// Explicit Agent Node argument wins over the Mesh override (issue
    /// #1151 cascade layer 1 — the spawn path lets an ad-hoc Agent Node
    /// override pin the most-specific layer).
    #[test]
    fn resolver_explicit_argument_overrides_mesh_override() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: Some("ad-hoc-model"),
                mesh_override: Some("override-model"),
                mesh: None,
                application: Some("app-model"),
            },
            effort: FieldInputs::default(),
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert_eq!(resolved.model.as_deref(), Some("ad-hoc-model"));
    }

    /// Per-field independence: a partial Mesh override can override model
    /// while inheriting effort (issue #1148 acceptance criteria 16:
    /// "A partial Mesh override can override model while inheriting
    /// effort, or vice versa"). The cascade resolves per field
    /// independently, so a present mesh_override model falls through to
    /// the application default for effort.
    #[test]
    fn resolver_partial_mesh_override_inherits_for_other_field() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: None,
                mesh_override: Some("override-model"),
                mesh: None,
                application: Some("app-model"),
            },
            effort: FieldInputs {
                explicit: None,
                mesh_override: None,
                mesh: None,
                application: Some("high"),
            },
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert_eq!(resolved.model.as_deref(), Some("override-model"));
        assert_eq!(resolved.effort.as_deref(), Some("high"));
    }

    /// Mesh override is masked by the capability contract: a harness
    /// without `supports_model_override` (e.g. Terminal) drops the Mesh
    /// override layer at the resolver, even when the user (or an IPC
    /// write) supplied a value. Mirrors the application-default mask
    /// (`resolver_application_slot_dropped_when_capability_masks_it`).
    /// The spawn path supplies the stored override verbatim — the
    /// validator at the write boundary only rejects *unsupported effort*,
    /// not unsupported model — so the resolver's mask is the final gate.
    #[test]
    fn resolver_mesh_override_masked_for_harness_without_model_override() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: None,
                mesh_override: Some("some-model"),
                mesh: None,
                application: None,
            },
            effort: FieldInputs::default(),
        };
        let resolved = resolve_agent_config(&terminal_caps(), inputs);
        assert!(
            resolved.model.is_none(),
            "Terminal doesn't support model overrides; the mask must drop the mesh override layer"
        );
    }

    /// v33 migration pin: the legacy `mesh` column reading through the
    /// resolver still works on a DB that hasn't been migrated yet (a
    /// clean v32 install that never ran the v33 backfill). The legacy
    /// columns stay physically present and the resolver still consults
    /// them — only the spawn path later stops loading them as active
    /// config (issue #1151 acceptance criteria 6: "Legacy model and
    /// effort columns remain physically compatible but are no longer
    /// read as active spawn configuration"). The pin protects the
    /// read-side compatibility for any future safety-net path that
    /// needs to read the legacy shape.
    #[test]
    fn resolver_legacy_mesh_column_still_resolves_for_unsupported_harness_shape() {
        let inputs = AgentConfigInputs {
            model: FieldInputs {
                explicit: None,
                mesh_override: None,
                mesh: Some("opus-4-1"),
                application: None,
            },
            effort: FieldInputs {
                explicit: None,
                mesh_override: None,
                mesh: Some("high"),
                application: None,
            },
        };
        let resolved = resolve_agent_config(&anthropic_caps(), inputs);
        assert_eq!(resolved.model.as_deref(), Some("opus-4-1"));
        assert_eq!(resolved.effort.as_deref(), Some("high"));
    }
}
