use super::*;

#[test]
fn cascade_view_for_field_collapses_in_order() {
    let view = ResolvedCascadeView::for_field(field_inputs(
        Some("explicit-model"),
        Some("mesh-model"),
        Some("app-model"),
    ));
    assert_eq!(view.resolved.as_deref(), Some("explicit-model"));
    assert_eq!(view.layers.explicit.as_deref(), Some("explicit-model"));
    assert_eq!(view.layers.mesh.as_deref(), Some("mesh-model"));
    assert_eq!(view.layers.application.as_deref(), Some("app-model"));
}

#[test]
fn cascade_view_falls_through_whitespace_layers() {
    let view = ResolvedCascadeView::for_field(field_inputs(Some("   "), Some(""), Some("opus-4")));
    assert_eq!(view.resolved.as_deref(), Some("opus-4"));
    assert!(view.layers.explicit.is_none());
    assert!(view.layers.mesh.is_none());
    assert_eq!(view.layers.application.as_deref(), Some("opus-4"));
}

#[test]
fn cascade_view_with_all_layers_absent_is_none() {
    let view = ResolvedCascadeView::for_field(field_inputs(None, None, None));
    assert_eq!(view.resolved, None);
    assert_eq!(view.layers, ResolvedCascadeLayer::default());
}

#[test]
fn capability_mask_drops_model_when_unsupported() {
    let view = ResolvedCascadeView::for_field(field_inputs(None, None, Some("opus-4")));
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
    let view = ResolvedCascadeView::for_field(field_inputs(None, None, Some("high")));
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
    let view = ResolvedCascadeView::for_field(field_inputs(None, None, Some("ultra")));
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
fn capability_mask_keeps_a_codex_model_defined_effort() {
    let view = ResolvedCascadeView::for_field(field_inputs(None, None, Some("experimental")));
    let caps = CapabilityMaskForResolver {
        supports_model_override: true,
        effort_control: EffortControlKind::InlineConfig {
            key: "model_reasoning_effort".into(),
            allowed: vec!["low".into(), "medium".into(), "high".into()],
        },
    };
    let masked = apply_capability_mask(view, "effort", &caps);
    assert_eq!(masked.resolved.as_deref(), Some("experimental"));
}

#[test]
fn capability_mask_drops_effort_for_none_kind() {
    let view = ResolvedCascadeView::for_field(field_inputs(None, None, Some("high")));
    let caps = CapabilityMaskForResolver {
        supports_model_override: true,
        effort_control: EffortControlKind::None,
    };
    let masked = apply_capability_mask(view, "effort", &caps);
    assert_eq!(masked.resolved, None);
}
