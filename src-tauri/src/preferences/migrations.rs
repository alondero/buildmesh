//! Legacy `preferences.json` read migration (ADR-0025).
//!
//! Pure functions on `serde_json::Value` — runs at read time, before
//! deserialization, to bring an older JSON payload up to the current
//! schema in-place. The output is then round-tripped through the
//! [`super::model::AppPreferences`] struct; on success the migrated
//! payload is persisted by [`super::storage::read_from_disk`].
//!
//! See the [module-level docs](super) for what concerns each submodule owns.

use super::model::ApiSurface;
use super::resolver::{first_class_surfaces, is_claude_compatible_id};

/// One-shot ADR-0025 prefs JSON migration (pure on a `serde_json::Value`):
/// 1. Fold legacy `kimi-via-claude` companion key into a `kimi` account row.
/// 2. **Once** (`ad0025_account_pairings_migrated` flag): for each enabled
///    keyed Claude-compatible account with no Claude pairing yet, materialise
///    one from legacy account endpoint fields / first-class defaults (preserves
///    pre-ADR auto-derived Claude pairings). After the flag is set, saving a
///    key never auto-attaches.
/// 3. Strip legacy `base_url` / `model_tiers` / `models` from every account.
///
/// Returns whether anything changed (caller persists when true).
pub(crate) fn migrate_prefs_json(value: &mut serde_json::Value) -> bool {
    let Some(root) = value.as_object_mut() else {
        return false;
    };
    let mut changed = false;

    let claude_harness = claude_harness_id_from_json(root.get("harness_profiles"));
    let already_migrated = root
        .get("ad0025_account_pairings_migrated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !root.contains_key("provider_accounts") {
        root.insert("provider_accounts".into(), serde_json::json!([]));
    }
    if !root.contains_key("provider_pairings") {
        root.insert("provider_pairings".into(), serde_json::json!([]));
    }

    // --- 1. kimi-via-claude companion → first-class kimi (key only) ----------
    if migrate_kimi_companion_json(root) {
        changed = true;
    }

    // --- 2. one-shot: materialise Claude pairings for pre-ADR keyed accounts -
    let existing_pairings = root
        .get("provider_pairings")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut pairings_to_add: Vec<serde_json::Value> = Vec::new();

    if !already_migrated {
        if let Some(accounts) = root.get("provider_accounts").and_then(|v| v.as_array()) {
            for account in accounts {
                let Some(obj) = account.as_object() else {
                    continue;
                };
                let id = obj
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if id.is_empty() || !is_claude_compatible_id(&id) {
                    continue;
                }
                let enabled = obj.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                let api_key = obj
                    .get("api_key")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());
                if !enabled || api_key.is_none() {
                    continue;
                }
                let already = existing_pairings.iter().any(|p| {
                    p.get("harness_id").and_then(|v| v.as_str()) == Some(claude_harness.as_str())
                        && p.get("provider_id").and_then(|v| v.as_str()) == Some(id.as_str())
                });
                if already {
                    continue;
                }
                let legacy_base = obj
                    .get("base_url")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let legacy_tiers = obj.get("model_tiers").cloned();
                let published = first_class_surfaces(&id)
                    .into_iter()
                    .find(|e| e.surface == ApiSurface::Anthropic);
                let base_url = legacy_base
                    .or_else(|| published.as_ref().map(|e| e.base_url.clone()))
                    .unwrap_or_default();
                // ADR-0025: never synthesise a stored pairing whose
                // `base_url` is null — the attach command requires a non-empty
                // URL, and a sourced spawn env would route to nowhere.
                if base_url.is_empty() {
                    continue;
                }
                let model_tiers = legacy_tiers
                    .filter(|t| t.as_object().is_some_and(|o| !o.is_empty()))
                    .unwrap_or_else(|| {
                        published
                            .as_ref()
                            .map(|e| serde_json::to_value(&e.model_tiers).unwrap_or_default())
                            .unwrap_or_else(|| serde_json::json!({}))
                    });
                pairings_to_add.push(serde_json::json!({
                    "harness_id": claude_harness,
                    "provider_id": id,
                    "surface": "anthropic",
                    "base_url": if base_url.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String(base_url)
                    },
                    "model_tiers": model_tiers,
                }));
            }
        }
        root.insert(
            "ad0025_account_pairings_migrated".into(),
            serde_json::Value::Bool(true),
        );
        changed = true;
    }

    // --- 3. strip legacy endpoint fields from every account ------------------
    if let Some(accounts) = root
        .get_mut("provider_accounts")
        .and_then(|v| v.as_array_mut())
    {
        for account in accounts.iter_mut() {
            let Some(obj) = account.as_object_mut() else {
                continue;
            };
            let had_legacy = obj.contains_key("base_url")
                || obj.contains_key("model_tiers")
                || obj.contains_key("models");
            if had_legacy {
                obj.remove("base_url");
                obj.remove("model_tiers");
                obj.remove("models");
                changed = true;
            }
        }
    }

    if !pairings_to_add.is_empty() {
        if let Some(pairings) = root
            .get_mut("provider_pairings")
            .and_then(|v| v.as_array_mut())
        {
            pairings.extend(pairings_to_add);
            changed = true;
        }
    }

    changed
}

/// Resolve the Claude harness id from a prefs JSON `harness_profiles` value —
/// first profile with `harness == "anthropic"`, else `"claude"`.
fn claude_harness_id_from_json(profiles: Option<&serde_json::Value>) -> String {
    profiles
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|p| {
                if p.get("harness").and_then(|h| h.as_str()) == Some("anthropic") {
                    p.get("id").and_then(|id| id.as_str()).map(str::to_string)
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| "claude".to_string())
}

/// Fold a stored `kimi-via-claude` companion into the first-class `kimi` row
/// (key only — endpoint fields are handled by the pairing migration). Mutates
/// the prefs root object; returns whether anything changed.
fn migrate_kimi_companion_json(root: &mut serde_json::Map<String, serde_json::Value>) -> bool {
    let Some(accounts) = root
        .get_mut("provider_accounts")
        .and_then(|v| v.as_array_mut())
    else {
        return false;
    };
    let Some(companion_idx) = accounts
        .iter()
        .position(|a| a.get("id").and_then(|v| v.as_str()) == Some("kimi-via-claude"))
    else {
        return false;
    };
    let companion = accounts[companion_idx].clone();
    let companion_key = companion
        .get("api_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if let Some(kimi) = accounts
        .iter_mut()
        .find(|a| a.get("id").and_then(|v| v.as_str()) == Some("kimi"))
    {
        if let Some(obj) = kimi.as_object_mut() {
            let empty = obj
                .get("api_key")
                .and_then(|v| v.as_str())
                .is_none_or(|s| s.is_empty());
            if empty {
                if let Some(key) = companion_key {
                    obj.insert("api_key".into(), serde_json::Value::String(key));
                }
            }
        }
    } else {
        // Materialise a first-class kimi row from the catalog template.
        let mut kimi = serde_json::json!({
            "id": "kimi",
            "name": "Moonshot / Kimi",
            "enabled": true,
            "billing_mode": "pay_as_you_go",
            "claude_compatible": true,
            "api_key": null,
        });
        if let Some(key) = companion_key {
            if let Some(obj) = kimi.as_object_mut() {
                obj.insert("api_key".into(), serde_json::Value::String(key));
            }
        }
        // Carry companion endpoint fields so step 2 can turn them into a pairing.
        if let (Some(src), Some(dst)) = (companion.as_object(), kimi.as_object_mut()) {
            for field in ["base_url", "model_tiers", "models", "enabled"] {
                if let Some(v) = src.get(field) {
                    dst.insert(field.to_string(), v.clone());
                }
            }
        }
        accounts.push(kimi);
    }
    accounts.remove(companion_idx);
    true
}
