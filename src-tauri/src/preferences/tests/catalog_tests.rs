//! Tests for the resolver::catalog submodule — provider catalog, surface
//! mapping, first-class provider metadata.

use super::super::resolver::{
    default_provider_accounts, first_class_surfaces, is_claude_compatible_id,
    keyed_first_class_catalog,
};
use crate::preferences::{ApiSurface, BUILTIN_PROVIDER_ACCOUNTS};

#[test]
fn default_provider_accounts_are_self_auth_only() {
    for account in default_provider_accounts() {
        assert!(
            BUILTIN_PROVIDER_ACCOUNTS
                .iter()
                .find(|b| b.id == account.id)
                .is_some_and(|b| b.self_auth),
            "expected self_auth: {}",
            account.id
        );
    }
}

#[test]
fn default_provider_accounts_cover_the_builtin_providers() {
    let accounts = default_provider_accounts();
    let ids: Vec<&str> = accounts.iter().map(|a| a.id.as_str()).collect();
    for required in ["anthropic", "codex", "agy", "grok", "opencode"] {
        assert!(
            ids.contains(&required),
            "missing default account: {required}"
        );
    }
}

#[test]
fn builtin_minimax_pairing_reproduces_claude_code_model_routing() {
    let surfaces = first_class_surfaces("minimax");
    let anthropic = surfaces
        .iter()
        .find(|s| s.surface == ApiSurface::Anthropic)
        .expect("anthropic surface for minimax");
    let tiers = &anthropic.model_tiers;
    assert_eq!(tiers.default.as_deref(), Some("MiniMax-M3[1m]"));
    assert_eq!(tiers.opus.as_deref(), Some("MiniMax-M3[1m]"));
    assert_eq!(tiers.sonnet.as_deref(), Some("MiniMax-M3[1m]"));
    assert_eq!(tiers.haiku.as_deref(), Some("MiniMax-M2.7"));
    assert_eq!(tiers.small_fast.as_deref(), Some("MiniMax-M2.7"));
    assert_eq!(anthropic.base_url, "https://api.minimax.io/anthropic");
}

#[test]
fn builtin_deepseek_pairing_reproduces_claude_code_model_routing() {
    let surfaces = first_class_surfaces("deepseek");
    let anthropic = surfaces
        .iter()
        .find(|s| s.surface == ApiSurface::Anthropic)
        .expect("anthropic surface for deepseek");
    assert_eq!(
        anthropic.model_tiers.opus.as_deref(),
        Some("deepseek-reasoner")
    );
    assert_eq!(
        anthropic.model_tiers.fable.as_deref(),
        Some("deepseek-reasoner")
    );
    assert_eq!(
        anthropic.model_tiers.haiku.as_deref(),
        Some("deepseek-chat")
    );
}

#[test]
fn builtin_provider_accounts_have_no_via_substring_in_id() {
    for b in BUILTIN_PROVIDER_ACCOUNTS {
        assert!(
            !b.id.contains("via"),
            "legacy id slipped into catalog: {}",
            b.id
        );
    }
}

#[test]
fn kimi_via_claude_id_does_not_exist_in_default_provider_accounts() {
    let accounts = default_provider_accounts();
    let ids: Vec<&str> = accounts.iter().map(|a| a.id.as_str()).collect();
    assert!(!ids.contains(&"kimi-via-claude"));
}

#[test]
fn kimi_is_first_class_claude_compatible_with_moonshot_endpoint() {
    let catalog = keyed_first_class_catalog();
    let kimi = catalog.iter().find(|a| a.id == "kimi").unwrap();
    assert!(kimi.claude_compatible);
    assert!(kimi.api_key.is_none());
    let surfaces = first_class_surfaces("kimi");
    assert_eq!(surfaces.len(), 1);
    assert_eq!(surfaces[0].surface, ApiSurface::Anthropic);
    assert_eq!(surfaces[0].base_url, "https://api.moonshot.ai/anthropic");
}

#[test]
fn built_in_provider_accounts_table_is_consistent_with_default_provider_accounts() {
    for account in default_provider_accounts() {
        assert!(
            !is_claude_compatible_id(&account.id),
            "default account {} should be self-auth-only",
            account.id
        );
    }
}

#[test]
fn is_claude_compatible_id_matches_table_self_auth_flag() {
    assert!(!is_claude_compatible_id("anthropic"));
    assert!(!is_claude_compatible_id("codex"));
    assert!(is_claude_compatible_id("minimax"));
    assert!(is_claude_compatible_id("kimi"));
    assert!(is_claude_compatible_id("unknown-custom"));
}

#[test]
fn minimax_default_tiers_is_the_source_for_minimax_surface() {
    let surfaces = first_class_surfaces("minimax");
    let anthropic = surfaces
        .iter()
        .find(|s| s.surface == ApiSurface::Anthropic)
        .unwrap();
    assert_eq!(
        anthropic.model_tiers,
        crate::preferences::minimax_default_tiers()
    );
}
