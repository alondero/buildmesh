//! Tests for the resolver::default_provider submodule — precedence resolver.

use super::super::resolver::resolve_default_provider;

#[test]
fn resolve_precedence_explicit_wins() {
    assert_eq!(
        resolve_default_provider(Some("e".into()), Some("p".into()), Some("a".into())),
        "e"
    );
}

#[test]
fn resolve_precedence_falls_through_to_per_mesh() {
    assert_eq!(
        resolve_default_provider(None, Some("p".into()), Some("a".into())),
        "p"
    );
}

#[test]
fn resolve_precedence_falls_through_to_app_wide() {
    assert_eq!(resolve_default_provider(None, None, Some("a".into())), "a");
}

#[test]
fn resolve_precedence_falls_through_to_claude() {
    assert_eq!(resolve_default_provider(None, None, None), "claude");
}

#[test]
fn resolve_precedence_treats_empty_strings_as_absent() {
    assert_eq!(
        resolve_default_provider(Some("".into()), Some("".into()), Some("".into())),
        "claude"
    );
    assert_eq!(
        resolve_default_provider(Some("".into()), Some("p".into()), Some("a".into())),
        "p"
    );
}
