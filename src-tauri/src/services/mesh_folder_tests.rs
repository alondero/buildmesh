//! The mesh-folder-missing marker's own contract (issue #2602).
//!
//! The marker's job is to be unambiguous: a failure that might fix itself
//! must not carry it, and a folder that is gone must.

use super::*;

#[test]
fn a_folder_that_is_there_is_not_missing() {
    let dir = std::env::temp_dir();
    assert!(
        !folder_is_missing(&dir.to_string_lossy()),
        "the temp directory exists"
    );
    assert!(
        !folder_is_missing(""),
        "an empty path names nothing, so it is not a missing folder"
    );
}

#[test]
fn a_folder_that_is_gone_is_missing() {
    let gone = std::env::temp_dir().join("buildmesh-mesh-folder-that-is-gone-2602");
    let gone = gone.to_string_lossy().to_string();
    assert!(folder_is_missing(&gone));
}

#[test]
fn only_a_missing_folder_is_marked() {
    let dir = std::env::temp_dir();
    let there = dir.to_string_lossy().to_string();
    assert_eq!(
        mark_if_missing(&there, "no working repository".to_string()),
        "no working repository",
        "a folder that exists keeps the plain error — it might fix itself"
    );

    let gone = std::env::temp_dir().join("buildmesh-mesh-folder-that-is-gone-2602");
    let gone = gone.to_string_lossy().to_string();
    let marked = mark_if_missing(&gone, "no working repository".to_string());
    assert!(
        marked.starts_with(MESH_FOLDER_MISSING),
        "a missing folder must carry the marker, got: {marked}"
    );
    assert!(
        marked.contains("no working repository"),
        "the marker must not swallow the reason, got: {marked}"
    );
}

/// The marker is an IPC contract with `src/lib/meshFolder.ts` — the same
/// shape as the `pool-count-changed` event name. Pin it on both sides so a
/// rename cannot silently break the other half.
#[test]
fn the_marker_matches_the_frontend_constant() {
    assert_eq!(
        MESH_FOLDER_MISSING, "MESH_FOLDER_MISSING:",
        "the marker must match `MESH_FOLDER_MISSING` in src/lib/meshFolder.ts — \
         rename in lockstep on both sides"
    );
}
