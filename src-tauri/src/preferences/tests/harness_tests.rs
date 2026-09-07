//! Tests for the resolver::harness submodule.

use super::super::model::HarnessProfile;
use super::super::resolver::{
    default_harness_profiles, harness_profiles, is_known_harness_id, merge_detected_profiles,
    resolve_harness_provider,
};
use super::with_temp_dir;
use crate::preferences::AppPreferences;

#[test]
fn default_harness_profiles_include_terminal() {
    let profiles = default_harness_profiles();
    assert!(profiles.iter().any(|p| p.id == "terminal"));
}

#[test]
fn harness_profiles_always_contains_terminal_with_none_stored() {
    with_temp_dir(|_| {
        let profiles = harness_profiles();
        assert!(profiles.iter().any(|p| p.id == "terminal"));
    });
}

#[test]
fn harness_profiles_round_trips_a_stored_user_profile() {
    with_temp_dir(|_| {
        let mut prefs = AppPreferences::default();
        prefs.harness_profiles.push(HarnessProfile {
            id: "claude".to_string(),
            name: "Claude Code".to_string(),
            harness: "claude".to_string(),
        });
        super::super::storage::save(prefs).unwrap();
        let profiles = harness_profiles();
        assert!(profiles
            .iter()
            .any(|p| p.id == "claude" && p.name == "Claude Code"));
    });
}

#[test]
fn harness_profiles_user_overrides_default_by_id() {
    with_temp_dir(|_| {
        let mut prefs = AppPreferences::default();
        prefs.harness_profiles.push(HarnessProfile {
            id: "terminal".to_string(),
            name: "Shell".to_string(),
            harness: "terminal".to_string(),
        });
        super::super::storage::save(prefs).unwrap();
        let profiles = harness_profiles();
        let terminal = profiles.iter().find(|p| p.id == "terminal").unwrap();
        assert_eq!(terminal.name, "Shell");
    });
}

#[test]
fn harness_profiles_new_id_appends() {
    with_temp_dir(|_| {
        let mut prefs = AppPreferences::default();
        prefs.harness_profiles.push(HarnessProfile {
            id: "custom".to_string(),
            name: "Custom".to_string(),
            harness: "claude".to_string(),
        });
        super::super::storage::save(prefs).unwrap();
        let profiles = harness_profiles();
        assert!(profiles.iter().any(|p| p.id == "custom"));
        assert!(profiles.iter().any(|p| p.id == "terminal"));
    });
}

#[test]
fn resolve_harness_provider_maps_terminal_profile() {
    assert!(matches!(
        resolve_harness_provider("terminal"),
        crate::models::Provider::Terminal
    ));
}

#[test]
fn resolve_harness_provider_uses_profile_harness_field() {
    with_temp_dir(|_| {
        let mut prefs = AppPreferences::default();
        prefs.harness_profiles.push(HarnessProfile {
            id: "deepseek-via-claude".to_string(),
            name: "DeepSeek (via Claude)".to_string(),
            harness: "claude".to_string(),
        });
        super::super::storage::save(prefs).unwrap();
        assert!(matches!(
            resolve_harness_provider("deepseek-via-claude"),
            crate::models::Provider::Anthropic
        ));
    });
}

#[test]
fn resolve_harness_provider_falls_back_through_from_db_str() {
    // Unknown id → Anthropic default (preserves prior behaviour).
    assert!(matches!(
        resolve_harness_provider("totally-unknown"),
        crate::models::Provider::Anthropic
    ));
}

#[test]
fn merge_detected_profiles_appends_new_and_reports_count() {
    with_temp_dir(|_| {
        let added = merge_detected_profiles(vec![HarnessProfile {
            id: "claude".to_string(),
            name: "Claude Code".to_string(),
            harness: "claude".to_string(),
        }])
        .unwrap();
        assert_eq!(added, 1);
        let stored = super::super::storage::load().unwrap().harness_profiles;
        assert!(stored.iter().any(|p| p.id == "claude"));
    });
}

#[test]
fn merge_detected_profiles_is_idempotent() {
    with_temp_dir(|_| {
        merge_detected_profiles(vec![HarnessProfile {
            id: "claude".to_string(),
            name: "Claude Code".to_string(),
            harness: "claude".to_string(),
        }])
        .unwrap();
        let added = merge_detected_profiles(vec![HarnessProfile {
            id: "claude".to_string(),
            name: "Claude Code".to_string(),
            harness: "claude".to_string(),
        }])
        .unwrap();
        assert_eq!(added, 0);
    });
}

#[test]
fn merge_detected_profiles_never_overwrites_a_user_customized_entry() {
    with_temp_dir(|_| {
        // Pre-existing user rename
        let mut prefs = AppPreferences::default();
        prefs.harness_profiles.push(HarnessProfile {
            id: "claude".to_string(),
            name: "Renamed".to_string(),
            harness: "claude".to_string(),
        });
        super::super::storage::save(prefs).unwrap();
        // Detection sees the default name.
        let _ = merge_detected_profiles(vec![HarnessProfile {
            id: "claude".to_string(),
            name: "Claude Code".to_string(),
            harness: "claude".to_string(),
        }])
        .unwrap();
        let profiles = harness_profiles();
        let claude = profiles.iter().find(|p| p.id == "claude").unwrap();
        assert_eq!(claude.name, "Renamed");
    });
}

#[test]
fn set_harness_order_round_trips() {
    with_temp_dir(|_| {
        super::super::storage::update(|prefs| {
            prefs.harness_order = vec!["claude".to_string(), "codex".to_string()];
        })
        .unwrap();
        let stored = super::super::storage::load().unwrap().harness_order;
        assert_eq!(stored, vec!["claude".to_string(), "codex".to_string()]);
    });
}

#[test]
fn set_harness_order_preserves_uninstalled_harness_slot() {
    with_temp_dir(|_| {
        super::super::storage::update(|prefs| {
            prefs.harness_order = vec![
                "claude".to_string(),
                "codex".to_string(),
                "dormant".to_string(),
            ];
        })
        .unwrap();
        // "dormant" uninstalled; reorder installed ones.
        crate::preferences::set_harness_order(vec!["codex".to_string(), "claude".to_string()])
            .unwrap();
        let stored = super::super::storage::load().unwrap().harness_order;
        // "dormant" still in its slot.
        let dormant_idx = stored.iter().position(|s| s == "dormant").unwrap();
        assert_eq!(dormant_idx, 2);
    });
}

#[test]
fn set_harness_order_drops_duplicate_ids() {
    with_temp_dir(|_| {
        crate::preferences::set_harness_order(vec![
            "claude".to_string(),
            "codex".to_string(),
            "claude".to_string(),
        ])
        .unwrap();
        let stored = super::super::storage::load().unwrap().harness_order;
        let dedup: Vec<&String> = stored.iter().collect();
        let unique: std::collections::HashSet<&String> = stored.iter().collect();
        assert_eq!(dedup.len(), unique.len());
    });
}

#[test]
fn set_harness_order_filters_out_terminal() {
    with_temp_dir(|_| {
        crate::preferences::set_harness_order(vec![
            "terminal".to_string(),
            "claude".to_string(),
            "codex".to_string(),
        ])
        .unwrap();
        let stored = super::super::storage::load().unwrap().harness_order;
        assert!(!stored.contains(&"terminal".to_string()));
    });
}

#[test]
fn is_known_harness_id_accepts_every_builtin() {
    for id in crate::agent::provider::BUILTIN_HARNESS_IDS {
        assert!(is_known_harness_id(id), "builtin {id} should be known");
    }
}

#[test]
fn is_known_harness_id_rejects_unknown() {
    assert!(!is_known_harness_id("not-a-harness"));
    assert!(!is_known_harness_id(""));
    assert!(!is_known_harness_id("   "));
}
