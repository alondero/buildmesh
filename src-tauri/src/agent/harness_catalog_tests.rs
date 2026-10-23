//! Tests for the built-in harness catalog (issue #2532 moved the suite out
//! of the owner file, per the sibling-test-file rule).

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

/// The alias table is the one declaration of the `claude` → `anthropic`
/// mapping (issue #2532). The lookup and its reverse resolve through it.
#[test]
fn the_alias_table_resolves_both_directions() {
    assert_eq!(
        harness_profile_alias("claude"),
        Some("anthropic"),
        "the Claude harness profile executes through the Anthropic adapter"
    );
    assert_eq!(
        harness_profile_id_for_adapter("anthropic"),
        Some("claude"),
        "the Anthropic adapter is reached through the Claude harness profile"
    );
    assert_eq!(
        harness_profile_alias("codex"),
        None,
        "a harness id that is already an adapter id has no alias"
    );
    assert_eq!(
        harness_adapter_id("claude"),
        "anthropic",
        "the adapter id falls back to the harness id when no alias applies"
    );
    assert_eq!(
        harness_adapter_id("codex"),
        "codex",
        "an unaliased harness id is its own adapter id"
    );
}

/// The harness facts own the Spawn Menu position, next to the alias table
/// (issue #2532) — Terminal is the plain-shell escape hatch, so it stays
/// last.
#[test]
fn terminal_declares_itself_last_in_the_spawn_menu() {
    assert!(
        harness_sorts_last("terminal"),
        "Terminal must declare itself as the last Spawn Menu row"
    );
    assert!(
        !harness_sorts_last("codex"),
        "an ordinary harness must not declare itself last"
    );
}

/// The `claude` → `anthropic` mapping is declared in **one** place
/// (issue #2532). A grep over the Rust sources must find the hand-rolled
/// shape nowhere: a comparison against the `"claude"` literal whose body
/// produces the `"anthropic"` literal, or the reverse.
///
/// The scan is the acceptance criterion itself — the issue asked for "one
/// mapping, not four" — so it is asserted over the whole source tree
/// rather than over the files this issue happened to touch.
#[test]
fn the_claude_anthropic_alias_is_declared_in_one_place() {
    let src_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders: Vec<String> = Vec::new();
    let mut queue = vec![src_root.clone()];
    while let Some(dir) = queue.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src tree") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                queue.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("read rust file");
            let lines: Vec<&str> = source.lines().collect();
            for (index, line) in lines.iter().enumerate() {
                let next = lines.get(index + 1).copied().unwrap_or_default();
                let after = lines.get(index + 2).copied().unwrap_or_default();
                let forward = line.contains("== \"claude\"") && next.contains("\"anthropic\"");
                let backward = line.contains("== \"anthropic\"")
                    && (next.contains("\"claude\"") || after.contains("\"claude\""));
                if forward || backward {
                    offenders.push(format!(
                        "{}:{} — the alias is re-derived here; resolve it through \
                         HARNESS_PROFILE_ALIASES",
                        path.display(),
                        index + 1
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the claude → anthropic alias must be declared once (issue #2532): {offenders:?}"
    );
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
    let alias_from: HashSet<&str> = HARNESS_PROFILE_ALIASES
        .iter()
        .map(|(from, _)| *from)
        .collect();

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
        builtin,
        covered,
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

    let parsed: serde_json::Value = serde_json::from_str(&json).expect("exported JSON must parse");
    let expected: serde_json::Value =
        serde_json::from_str(&catalog_json()).expect("in-memory JSON");
    assert_eq!(
        parsed, expected,
        "exported JSON must match the in-memory catalog"
    );

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
    assert_eq!(
        ids, &expected_ids,
        "exported ids must follow Provider::all()"
    );
    assert!(
        ts.contains("HARNESS_PROFILE_ALIASES"),
        "generated TS must export the claude → anthropic alias table"
    );
    assert!(
        ts.starts_with("// This file was generated by the harness catalog exporter"),
        "generated TS must carry the do-not-edit banner"
    );
}
