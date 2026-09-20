//! Total built-in harness catalog — detection-independent capability + label
//! table (ADR-0037).
//!
//! [`available_providers`](crate::agent::provider_menu) only returns detected /
//! configured rows, so it cannot answer "what can harness X do?" for a harness
//! the user has not installed. The Circuits Inspector is an authoring form and
//! needs that answer for every selectable harness. This catalog enumerates
//! [`crate::models::Provider::all`] and records each adapter's
//! [`AgentProvider::capabilities`](crate::agent::provider::AgentProvider::capabilities)
//! plus its Inspector/docs label.
//!
//! `cargo test` writes the committed TypeScript/JSON artifacts under
//! `src/types/generated/` (same `TS_RS_EXPORT_DIR` as ts-rs). A hand-written
//! TypeScript mirror of these values is a defect.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::agent::capabilities::HarnessCapabilities;
use crate::models::Provider;

/// One row in the total built-in harness catalog.
#[derive(Debug, Clone)]
pub struct HarnessCatalogEntry {
    /// Adapter id — [`Provider`] Display / [`AgentProvider::id`](crate::agent::provider::AgentProvider::id).
    pub id: String,
    /// Inspector and docs display name. Distinct from [`UiMeta::label`](crate::agent::provider::UiMeta)
    /// (Spawn Menu adapter chip) when those names differ.
    pub label: String,
    /// Capability contract composed by the adapter.
    pub capabilities: HarnessCapabilities,
}

/// Profile ids that are not [`Provider`] variants. They execute through an
/// existing adapter and must be mapped deliberately rather than falling
/// through a default.
///
/// `claude` is a Harness Profile id (detection / Spawn Menu). Its executor is
/// the legacy `anthropic` adapter. [`crate::agent::provider::BUILTIN_HARNESS_IDS`]
/// lists both so the SQL whitelist covers stored profile rows.
pub const HARNESS_PROFILE_ALIASES: &[(&str, &str)] = &[("claude", "anthropic")];

/// Inspector / README / user-guide display name for a [`Provider`] variant.
///
/// Exhaustive so a new variant without a label is a compile error. These
/// names are the Circuits Inspector dropdown labels and the docs-gate
/// strings; they are not always identical to `UiMeta::label` or the
/// detection-profile `name`.
fn inspector_label(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "Claude Code",
        Provider::Agy => "Antigravity",
        Provider::OpenCode => "OpenCode",
        Provider::Codex => "Codex",
        Provider::Cursor => "Cursor",
        Provider::Grok => "Grok Code",
        Provider::Kimi => "Kimi Code",
        Provider::Mcode => "MiniMax Code",
        Provider::Dsh => "DeepSeek Harness",
        Provider::CommandCode => "Command Code",
        Provider::Freebuff => "Freebuff",
        Provider::Muse => "Meta Muse",
        Provider::Cline => "Cline",
        Provider::Terminal => "Terminal",
    }
}

/// Every built-in harness, independent of detection and configured accounts.
///
/// Order matches [`Provider::all`].
pub fn builtin_harness_catalog() -> Vec<HarnessCatalogEntry> {
    Provider::all()
        .iter()
        .copied()
        .map(|provider| {
            let adapter = provider.adapter();
            HarnessCatalogEntry {
                id: adapter.id().to_string(),
                label: inspector_label(provider).to_string(),
                capabilities: adapter.capabilities(),
            }
        })
        .collect()
}

#[derive(Serialize)]
struct CatalogFile {
    /// Adapter ids in [`Provider::all`] order. JSON objects do not have a
    /// guaranteed key order (serde_json's default `Map` is a BTreeMap), so
    /// the Inspector dropdown reads this array instead of `Object.keys`.
    ids: Vec<String>,
    labels: serde_json::Map<String, serde_json::Value>,
    capabilities: serde_json::Map<String, serde_json::Value>,
    aliases: serde_json::Map<String, serde_json::Value>,
}

fn catalog_file() -> CatalogFile {
    let catalog = builtin_harness_catalog();
    let ids: Vec<String> = catalog.iter().map(|entry| entry.id.clone()).collect();
    let mut labels = serde_json::Map::new();
    let mut capabilities = serde_json::Map::new();
    for entry in catalog {
        labels.insert(
            entry.id.clone(),
            serde_json::Value::String(entry.label.clone()),
        );
        capabilities.insert(
            entry.id,
            serde_json::to_value(&entry.capabilities)
                .expect("HarnessCapabilities is JSON-serialisable"),
        );
    }
    let mut aliases = serde_json::Map::new();
    for (from, to) in HARNESS_PROFILE_ALIASES {
        aliases.insert(
            (*from).to_string(),
            serde_json::Value::String((*to).to_string()),
        );
    }
    CatalogFile {
        ids,
        labels,
        capabilities,
        aliases,
    }
}

fn catalog_json() -> String {
    let mut body = serde_json::to_string_pretty(&catalog_file()).expect("catalog JSON");
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body
}

fn catalog_ids() -> Vec<String> {
    builtin_harness_catalog()
        .into_iter()
        .map(|entry| entry.id)
        .collect()
}

fn render_ts(json: &str, ids: &[String]) -> String {
    let union = ids
        .iter()
        .map(|id| format!("  | \"{id}\""))
        .collect::<Vec<_>>()
        .join("\n");
    // JSON is a valid TypeScript object literal (quoted keys, null, arrays).
    format!(
        r#"// This file was generated by the harness catalog exporter (src-tauri/src/agent/harness_catalog.rs). Do not edit this file manually.
import type {{ HarnessCapabilities }} from "./HarnessCapabilities";

/**
 * Adapter ids the Circuits Inspector can select. Equal to the `Provider`
 * enum variant set. The Harness Profile id `claude` is not a variant; it
 * maps to `anthropic` via `HARNESS_PROFILE_ALIASES`.
 */
export type InspectorHarnessId =
{union};

type Catalog = {{
  ids: InspectorHarnessId[],
  labels: Record<InspectorHarnessId, string>,
  capabilities: Record<InspectorHarnessId, HarnessCapabilities>,
  aliases: Record<string, InspectorHarnessId>,
}};

export const HARNESS_CATALOG = {json} as Catalog;

export const HARNESS_IDS: InspectorHarnessId[] = HARNESS_CATALOG.ids;
export const HARNESS_LABEL: Record<InspectorHarnessId, string> = HARNESS_CATALOG.labels;
export const HARNESS_CAPABILITIES: Record<InspectorHarnessId, HarnessCapabilities> = HARNESS_CATALOG.capabilities;
/**
 * Profile ids that are not `Provider` variants. `claude` is a Harness
 * Profile id whose executor is the `anthropic` adapter (legacy executor
 * id). Lookups must map through this table rather than a default fallback.
 */
export const HARNESS_PROFILE_ALIASES: Record<string, InspectorHarnessId> = HARNESS_CATALOG.aliases;
"#
    ) + "\n"
}

/// Directory ts-rs (and this exporter) write into. Cwd-sensitive: run
/// `cargo test` from `src-tauri/` so `TS_RS_EXPORT_DIR` lands in
/// `src/types/generated/` rather than `src-tauri/bindings/`.
pub fn generated_export_dir() -> PathBuf {
    match std::env::var("TS_RS_EXPORT_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => PathBuf::from("bindings"),
    }
}

fn write_if_changed(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        if existing == contents {
            return Ok(());
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}

/// Write `HarnessCapabilitiesTable.ts` and `.json` next to the ts-rs bindings.
pub fn export_harness_capabilities_table() -> std::io::Result<()> {
    let dir = generated_export_dir();
    let ids = catalog_ids();
    let json = catalog_json();
    let ts = render_ts(&json.trim_end().to_string(), &ids);
    write_if_changed(&dir.join("HarnessCapabilitiesTable.json"), &json)?;
    write_if_changed(&dir.join("HarnessCapabilitiesTable.ts"), &ts)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::provider::BUILTIN_HARNESS_IDS;
    use std::collections::HashSet;

    #[test]
    fn catalog_keys_match_provider_enum() {
        let catalog = builtin_harness_catalog();
        let catalog_ids: Vec<String> = catalog.iter().map(|e| e.id.clone()).collect();
        let provider_ids: Vec<String> = Provider::all().iter().map(|p| p.to_string()).collect();
        assert_eq!(
            catalog_ids, provider_ids,
            "builtin_harness_catalog() must enumerate Provider::all() in the same order"
        );
        assert_eq!(
            catalog_ids.len(),
            catalog_ids.iter().collect::<HashSet<_>>().len(),
            "catalog ids must be unique"
        );
    }

    #[test]
    fn catalog_id_matches_adapter_id() {
        for provider in Provider::all() {
            let adapter = provider.adapter();
            assert_eq!(
                provider.to_string(),
                adapter.id(),
                "Provider Display ({provider}) must equal adapter.id() ({})",
                adapter.id()
            );
        }
    }

    #[test]
    fn catalog_capabilities_match_adapter() {
        for entry in builtin_harness_catalog() {
            let provider = Provider::from_db_str(&entry.id);
            assert_eq!(
                provider.to_string(),
                entry.id,
                "from_db_str({:?}) must round-trip the catalog id (not fall back to anthropic)",
                entry.id
            );
            let expected = provider.adapter().capabilities();
            let actual_json = serde_json::to_value(&entry.capabilities).unwrap();
            let expected_json = serde_json::to_value(&expected).unwrap();
            assert_eq!(
                actual_json, expected_json,
                "catalog capabilities for {} drifted from adapter.capabilities()",
                entry.id
            );
            assert_eq!(entry.capabilities.harness_id, entry.id);
            assert_eq!(entry.label, inspector_label(provider));
        }
    }

    #[test]
    fn builtin_harness_ids_are_catalog_keys_or_aliases() {
        let catalog_ids: HashSet<String> = builtin_harness_catalog()
            .into_iter()
            .map(|e| e.id)
            .collect();
        let alias_from: HashSet<&str> = HARNESS_PROFILE_ALIASES.iter().map(|(from, _)| *from).collect();

        for (from, to) in HARNESS_PROFILE_ALIASES {
            assert!(
                !catalog_ids.contains(*from),
                "alias source {from:?} collides with a catalog key"
            );
            assert!(
                catalog_ids.contains(*to),
                "alias target {to:?} is not a catalog key"
            );
        }

        let mut covered: HashSet<&str> = catalog_ids.iter().map(String::as_str).collect();
        covered.extend(alias_from.iter().copied());

        let builtin: HashSet<&str> = BUILTIN_HARNESS_IDS.iter().copied().collect();
        assert_eq!(
            builtin, covered,
            "BUILTIN_HARNESS_IDS must equal catalog keys ∪ HARNESS_PROFILE_ALIASES.\n\
             only in BUILTIN: {:?}\nonly in catalog/aliases: {:?}",
            builtin.difference(&covered).collect::<Vec<_>>(),
            covered.difference(&builtin).collect::<Vec<_>>()
        );
    }

    #[test]
    fn claude_alias_targets_anthropic() {
        assert_eq!(HARNESS_PROFILE_ALIASES, &[("claude", "anthropic")]);
        assert!(
            builtin_harness_catalog()
                .iter()
                .any(|e| e.id == "anthropic"),
            "anthropic adapter must remain in the catalog so the claude alias resolves"
        );
    }

    /// ts-rs-style export: `cargo test` (cwd `src-tauri/`) refreshes the
    /// committed artifact. CI's `git diff --exit-code src/types/generated`
    /// fails when the committed file lags the adapters.
    #[test]
    fn export_bindings_harness_capabilities_table() {
        export_harness_capabilities_table().expect("export HarnessCapabilitiesTable");

        let dir = generated_export_dir();
        let json_path = dir.join("HarnessCapabilitiesTable.json");
        let ts_path = dir.join("HarnessCapabilitiesTable.ts");
        let json = std::fs::read_to_string(&json_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", json_path.display()));
        let ts = std::fs::read_to_string(&ts_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", ts_path.display()));

        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("exported JSON must parse");
        let expected: serde_json::Value =
            serde_json::from_str(&catalog_json()).expect("in-memory JSON");
        assert_eq!(parsed, expected, "exported JSON must match the in-memory catalog");

        for entry in builtin_harness_catalog() {
            assert!(
                ts.contains(&format!("\"{}\"", entry.id)),
                "generated TS missing catalog id {}",
                entry.id
            );
            assert!(
                ts.contains(&entry.label),
                "generated TS missing label {}",
                entry.label
            );
        }
        let ids = parsed
            .get("ids")
            .and_then(|v| v.as_array())
            .expect("catalog JSON must include ids in Provider::all() order");
        let expected_ids: Vec<serde_json::Value> = catalog_ids()
            .into_iter()
            .map(serde_json::Value::String)
            .collect();
        assert_eq!(ids, &expected_ids, "exported ids must follow Provider::all()");
        assert!(
            ts.contains("HARNESS_PROFILE_ALIASES"),
            "generated TS must export the claude → anthropic alias table"
        );
        assert!(
            ts.starts_with("// This file was generated by the harness catalog exporter"),
            "generated TS must carry the do-not-edit banner"
        );
    }
}
