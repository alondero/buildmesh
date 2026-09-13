//! Shared cascade helper for the per-harness defaults resolution (issue #1656).
//!
//! The cascade order `explicit > mesh_override > mesh_legacy > application >
//! native` is the single source of truth for how a spawn picks up model /
//! effort values. It is consumed by two callers that must agree:
//!
//! 1. The spawn pipeline (`agent::capabilities::resolve_agent_config`) — runs
//!    at process launch and produces the [`crate::agent::capabilities::ResolvedAgentConfig`]
//!    forwarded to `build_spawn_command`.
//! 2. The IPC seam `get_resolved_harness_view` (issue #1656) — runs from the
//!    Settings modal + spawn menu so the UI can display the cascade result
//!    without re-implementing the collapse client-side.
//!
//! Centralising the per-field collapse in this module means the helper-level
//! unit tests in `agent::capabilities::tests` (specifically
//! `resolver_cascade_prefers_explicit_over_mesh_override_over_mesh_over_application`
//! and `resolver_cascade_falls_through_whitespace_layers`) protect both
//! callers — any drift between the spawn path and the IPC path breaks them.
//!
//! Capability masking stays in `agent::capabilities::resolve_agent_config`:
//! the IPC returns the un-masked cascade layers so the UI can render
//! "inherited from application / overridden by mesh / no value" hints that
//! the masked resolver hides.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::super::model::HarnessConfigValue;
use crate::agent::capabilities::{EffortControlKind, FieldInputs};

/// Whitespace-normalised first-non-empty-layer picker for one field. Every
/// layer is trimmed; a layer that is empty or whitespace-only collapses to
/// absent so the cascade falls through to the next layer. Cascade order
/// mirrors the issue #1148 cascade (slice 2 settles the per-Mesh override
/// layer between explicit and the legacy Mesh row):
///   explicit > mesh_override > mesh (legacy) > application
///
/// This is the single source of truth for the cascade; the spawn pipeline
/// (`agent::capabilities::resolve_agent_config`) and the IPC resolver view
/// (`commands::preferences::get_resolved_harness_view`) both call it.
pub fn resolve_field(field: FieldInputs<'_>) -> Option<String> {
    field
        .explicit
        .and_then(normalize_non_empty)
        .or_else(|| field.mesh_override.and_then(normalize_non_empty))
        .or_else(|| field.mesh.and_then(normalize_non_empty))
        .or_else(|| field.application.and_then(normalize_non_empty))
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

/// A per-layer slot in the cascade. `None` means "this layer had no value"
/// — the cascade fell through. Surfaced on the IPC response so the UI can
/// render "inherited from application" / "overridden by mesh" hints without
/// re-implementing the collapse.
///
/// `Some(empty)` is **never** returned: every layer is normalised by
/// [`normalize_non_empty`] before being placed in this struct.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "ResolvedCascadeLayer.ts")]
pub struct ResolvedCascadeLayer {
    /// The explicit layer (Agent Node spawn argument).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit: Option<String>,
    /// The mesh-override layer (per-Mesh `harness_overrides` map).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_override: Option<String>,
    /// The mesh-legacy layer (`meshes.model` / `meshes.effort` columns).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh: Option<String>,
    /// The application-layer (per-harness defaults from App Settings).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application: Option<String>,
}

/// The full per-harness cascade — each layer surfaced for the UI plus the
/// collapsed value. Returned by the IPC `get_resolved_harness_view` command
/// (issue #1656) so the Settings modal and spawn menu can render the
/// effective value plus the source layer without re-implementing the
/// cascade client-side.
///
/// **Important:** this is the UN-MASKED cascade. The spawn pipeline applies
/// the capability mask after [`resolve_field`] (Terminal drops every model
/// value, harnesses with `EffortControlKind::None` drop every effort value).
/// The IPC returns the un-masked layer breakdown so the UI can render
/// "configured but harness doesn't accept it" hints; the renderer should
/// still gate display on the harness capability descriptor before claiming
/// the value applies.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "ResolvedCascadeView.ts")]
pub struct ResolvedCascadeView {
    /// Per-layer raw values, highest precedence first. Layers with no value
    /// are absent; the resolver falls through in the order documented on
    /// [`ResolvedCascadeLayer`].
    pub layers: ResolvedCascadeLayer,
    /// First non-empty layer after [`resolve_field`], or `None` if every
    /// layer was empty. This is the value the spawn pipeline forwards when
    /// the harness accepts it (capability mask applies downstream).
    pub resolved: Option<String>,
}

impl ResolvedCascadeView {
    /// Compute the cascade view for one field (`model` or `effort`) across
    /// the four precedence layers. Pure — same shape as
    /// [`resolve_field`] plus the per-layer breakdown the UI needs.
    pub fn for_field(field: FieldInputs<'_>) -> Self {
        let explicit = field.explicit.and_then(normalize_non_empty);
        let mesh_override = field.mesh_override.and_then(normalize_non_empty);
        let mesh = field.mesh.and_then(normalize_non_empty);
        let application = field.application.and_then(normalize_non_empty);
        let resolved = explicit
            .clone()
            .or_else(|| mesh_override.clone())
            .or_else(|| mesh.clone())
            .or_else(|| application.clone());
        Self {
            layers: ResolvedCascadeLayer {
                explicit,
                mesh_override,
                mesh,
                application,
            },
            resolved,
        }
    }
}

/// A harness capability mask applied AFTER the cascade collapses. The
/// capability mask is the same contract the spawn pipeline enforces (issue
/// #1148 acceptance criteria 5 + 7); the IPC result applies it so the UI
/// shows the value that will actually reach the harness.
pub fn apply_capability_mask(
    view: ResolvedCascadeView,
    field_name: &str,
    capabilities: &CapabilityMaskForResolver,
) -> ResolvedCascadeView {
    // Re-export of the mask gates — kept thin here so the IPC path doesn't
    // reach into `agent::capabilities::*` and accidentally introduce a
    // circular dep. The mask values come from
    // `preferences::resolver::harness::harness_capabilities_for`.
    let drop_value = match field_name {
        "model" => !capabilities.supports_model_override,
        "effort" => !effort_mask_allows(&capabilities.effort_control, view.resolved.as_deref()),
        // Unknown field name → don't drop (defensive: the IPC builder
        // always supplies one of the two known fields).
        _ => false,
    };
    if drop_value {
        ResolvedCascadeView {
            layers: view.layers,
            resolved: None,
        }
    } else {
        view
    }
}

/// Minimal capability descriptor the cascade IPC needs. A subset of
/// [`crate::agent::capabilities::HarnessCapabilities`] so the IPC command
/// doesn't have to serialize the full descriptor (which includes
/// platform-list + attention capability that the UI doesn't render).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "CapabilityMaskForResolver.ts")]
pub struct CapabilityMaskForResolver {
    pub supports_model_override: bool,
    pub effort_control: EffortControlKind,
}

/// True iff `value` is in the harness's allowed effort vocabulary, or the
/// harness accepts any effort (`EffortControlKind::None` always returns
/// false — those harnesses have no effort slot). Mirrors
/// `agent::capabilities::resolve_effort` semantics so the masked IPC value
/// matches the value the spawn pipeline forwards.
fn effort_mask_allows(control: &EffortControlKind, value: Option<&str>) -> bool {
    let allowed: &[String] = match control {
        EffortControlKind::None => return false,
        EffortControlKind::Closed { allowed } => allowed,
        EffortControlKind::InlineConfig { allowed, .. } => allowed,
    };
    let Some(value) = value else { return false };
    allowed.iter().any(|a| a == value)
}

/// Helper for building a `FieldInputs` from the four `&Option<String>`-shaped
/// sources the IPC command has on hand. Pure — no I/O, no allocation beyond
/// the borrowed references.
pub fn field_inputs<'a>(
    explicit: Option<&'a str>,
    mesh_override: Option<&'a str>,
    mesh: Option<&'a str>,
    application: Option<&'a str>,
) -> FieldInputs<'a> {
    FieldInputs {
        explicit,
        mesh_override,
        mesh,
        application,
    }
}

/// Extract the per-field string from a [`HarnessConfigValue`], normalising
/// whitespace-only values to `None`. Mirrors the
/// `normalize_harness_default` contract so a stored blank (`Some("")` or
/// `Some("   ")`) never wins the cascade over a populated lower layer.
pub fn harness_config_str(value: &HarnessConfigValue, field: HarnessConfigField) -> Option<String> {
    let raw = match field {
        HarnessConfigField::Model => value.model.as_deref(),
        HarnessConfigField::Effort => value.effort.as_deref(),
    };
    raw.and_then(normalize_non_empty)
}

/// Identifier for which field of [`HarnessConfigValue`] a cascade input
/// reads. Used by [`harness_config_str`] and the IPC builder so the four
/// layers don't have to repeat `match field { Model => ..., Effort => ... }`
/// at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessConfigField {
    Model,
    Effort,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cascade_view_for_field_collapses_in_order() {
        let view = ResolvedCascadeView::for_field(field_inputs(
            Some("explicit-model"),
            Some("override-model"),
            Some("mesh-model"),
            Some("app-model"),
        ));
        assert_eq!(view.resolved.as_deref(), Some("explicit-model"));
        assert_eq!(view.layers.explicit.as_deref(), Some("explicit-model"));
        assert_eq!(view.layers.mesh_override.as_deref(), Some("override-model"));
        assert_eq!(view.layers.mesh.as_deref(), Some("mesh-model"));
        assert_eq!(view.layers.application.as_deref(), Some("app-model"));
    }

    #[test]
    fn cascade_view_falls_through_whitespace_layers() {
        let view = ResolvedCascadeView::for_field(field_inputs(
            Some("   "),
            Some("  \t  "),
            Some(""),
            Some("opus-4"),
        ));
        assert_eq!(view.resolved.as_deref(), Some("opus-4"));
        assert!(view.layers.explicit.is_none());
        assert!(view.layers.mesh_override.is_none());
        assert!(view.layers.mesh.is_none());
        assert_eq!(view.layers.application.as_deref(), Some("opus-4"));
    }

    #[test]
    fn cascade_view_with_all_layers_absent_is_none() {
        let view = ResolvedCascadeView::for_field(field_inputs(None, None, None, None));
        assert_eq!(view.resolved, None);
        assert_eq!(view.layers, ResolvedCascadeLayer::default());
    }

    #[test]
    fn capability_mask_drops_model_when_unsupported() {
        let view = ResolvedCascadeView::for_field(field_inputs(None, None, None, Some("opus-4")));
        let caps = CapabilityMaskForResolver {
            supports_model_override: false,
            effort_control: EffortControlKind::None,
        };
        let masked = apply_capability_mask(view, "model", &caps);
        assert_eq!(
            masked.resolved, None,
            "harness without model_override must drop the cascaded value"
        );
        assert_eq!(
            masked.layers.application.as_deref(),
            Some("opus-4"),
            "the layer breakdown is preserved so the UI can still show the source"
        );
    }

    #[test]
    fn capability_mask_keeps_effort_when_in_vocabulary() {
        let view = ResolvedCascadeView::for_field(field_inputs(None, None, None, Some("high")));
        let caps = CapabilityMaskForResolver {
            supports_model_override: true,
            effort_control: EffortControlKind::Closed {
                allowed: vec!["low".into(), "medium".into(), "high".into()],
            },
        };
        let masked = apply_capability_mask(view, "effort", &caps);
        assert_eq!(masked.resolved.as_deref(), Some("high"));
    }

    #[test]
    fn capability_mask_drops_effort_when_not_in_vocabulary() {
        let view = ResolvedCascadeView::for_field(field_inputs(None, None, None, Some("ultra")));
        let caps = CapabilityMaskForResolver {
            supports_model_override: true,
            effort_control: EffortControlKind::Closed {
                allowed: vec!["low".into(), "medium".into(), "high".into()],
            },
        };
        let masked = apply_capability_mask(view, "effort", &caps);
        assert_eq!(
            masked.resolved, None,
            "an out-of-vocabulary value must be dropped even when the harness accepts effort"
        );
    }

    #[test]
    fn capability_mask_drops_effort_for_none_kind() {
        let view = ResolvedCascadeView::for_field(field_inputs(None, None, None, Some("high")));
        let caps = CapabilityMaskForResolver {
            supports_model_override: true,
            effort_control: EffortControlKind::None,
        };
        let masked = apply_capability_mask(view, "effort", &caps);
        assert_eq!(masked.resolved, None);
    }
}
