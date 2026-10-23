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

/// The adapter id a Harness Profile id executes through, when the alias table
/// names one. `None` for a profile id that already *is* an adapter id.
///
/// This is the one place the `claude` → `anthropic` mapping is declared
/// (issue #2532). Callers resolve through here instead of re-deriving the
/// mapping, so the next harness alias is one table edit rather than a grep
/// across the tree.
pub fn harness_profile_alias(id: &str) -> Option<&'static str> {
    HARNESS_PROFILE_ALIASES
        .iter()
        .find(|(from, _)| *from == id)
        .map(|(_, to)| *to)
}

/// The Harness Profile id that executes through `adapter_id` — the reverse
/// lookup over [`HARNESS_PROFILE_ALIASES`].
///
/// `None` when no profile id is aliased onto that adapter.
pub fn harness_profile_id_for_adapter(adapter_id: &str) -> Option<&'static str> {
    HARNESS_PROFILE_ALIASES
        .iter()
        .find(|(_, to)| *to == adapter_id)
        .map(|(from, _)| *from)
}

/// The adapter id a Harness Profile id executes through, defaulting to the id
/// itself when no alias applies.
///
/// The one-call form of [`harness_profile_alias`] for the callers that want
/// the effective adapter id rather than "is there an alias?" — building a
/// fallback [`crate::preferences::HarnessProfile`] from a bare harness id,
/// where an unaliased id is its own adapter.
pub fn harness_adapter_id(id: &str) -> &str {
    harness_profile_alias(id).unwrap_or(id)
}

/// Harness ids whose Spawn Menu row is pinned to the bottom of the menu.
///
/// A harness declares its own menu position here rather than the comparator
/// special-casing one id (issue #2532). Terminal is the plain-shell escape
/// hatch, so it stays last.
pub const HARNESS_MENU_LAST: &[&str] = &["terminal"];

/// True when `harness_id` declares itself as the last row of the Spawn Menu.
pub fn harness_sorts_last(harness_id: &str) -> bool {
    HARNESS_MENU_LAST.contains(&harness_id)
}

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

/// Inspector/docs label for an adapter id ([`AgentProvider::id`](crate::agent::provider::AgentProvider::id)),
/// e.g. `"dsh"` → `"DeepSeek Harness"`. `None` for unknown ids. The
/// compatibility gate uses this for reviewer-refusal copy so user-facing
/// harness names come from the one label table, not a hand-rolled
/// capitaliser (issue #1816 review).
pub fn inspector_label_for_adapter(adapter_id: &str) -> Option<&'static str> {
    Provider::all()
        .iter()
        .copied()
        .find(|p| p.adapter().id() == adapter_id)
        .map(inspector_label)
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
    let ts = render_ts(json.trim_end(), &ids);
    write_if_changed(&dir.join("HarnessCapabilitiesTable.json"), &json)?;
    write_if_changed(&dir.join("HarnessCapabilitiesTable.ts"), &ts)?;
    Ok(())
}

#[cfg(test)]
#[path = "harness_catalog_tests.rs"]
mod tests;
