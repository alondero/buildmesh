//! The mobile-route fixture gate (issue #2318).
//!
//! The sibling of `mobile_wire.rs`: the canonical samples live there, the
//! assertions that compare them with the committed fixtures live here.

use super::*;

/// The committed fixture must still be the body the Rust wire type
/// produces. A wire change that is not regenerated fails here, and the
/// Android test that reads the fixture fails next.
#[test]
fn committed_mobile_wire_fixtures_match_the_rust_wire_types() {
    let dir = fixture_dir();
    let mut stale = Vec::new();
    for fixture in fixtures() {
        let path = dir.join(format!("{}.json", fixture.name));
        let committed = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                stale.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        let parsed: serde_json::Value = match serde_json::from_str(&committed) {
            Ok(value) => value,
            Err(error) => {
                stale.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        if parsed != fixture.body {
            stale.push(format!(
                "{}: the committed body no longer matches the Rust wire type",
                path.display()
            ));
        }
    }
    assert!(
        stale.is_empty(),
        "The mobile-route fixtures are stale ({} of them):\n{}\n\n\
         Regenerate them with:\n  cd src-tauri && cargo test --lib http::mobile_wire -- --ignored",
        stale.len(),
        stale.join("\n"),
    );
}

/// The fixture directory holds one file per fixture the Rust wire types
/// produce, and no other file.
///
/// A file left behind by a rename is drift too: the comparison above would
/// keep passing while an Android test kept reading a body no route sends.
#[test]
fn the_fixture_directory_holds_only_derived_fixtures() {
    let expected: Vec<String> = fixtures()
        .iter()
        .map(|fixture| format!("{}.json", fixture.name))
        .collect();
    let mut orphans = Vec::new();
    for entry in std::fs::read_dir(fixture_dir()).expect("read the mobile wire fixture directory") {
        let entry = entry.expect("read a mobile wire fixture entry");
        let name = entry.file_name().to_string_lossy().to_string();
        if !expected.contains(&name) {
            orphans.push(name);
        }
    }
    orphans.sort();
    assert!(
        orphans.is_empty(),
        "The mobile wire fixture directory holds {} file(s) no Rust wire type produces:\n{}\n\n\
         Delete them, or add them to `mobile_wire.rs`.",
        orphans.len(),
        orphans.join("\n"),
    );
}

/// Rewrite the committed fixtures from the current wire types. Ignored by
/// default: it is the deliberate step after an intentional wire change,
/// not something a test run does behind the author's back.
#[test]
#[ignore = "regenerates the committed mobile-route fixtures"]
fn write_the_committed_mobile_wire_fixtures() {
    let dir = fixture_dir();
    std::fs::create_dir_all(&dir).expect("create the mobile wire fixture directory");
    for fixture in fixtures() {
        let path = dir.join(format!("{}.json", fixture.name));
        let mut contents =
            serde_json::to_string_pretty(&fixture.body).expect("serialise mobile wire fixture");
        contents.push('\n');
        std::fs::write(&path, contents).expect("write the mobile wire fixture");
    }
}
