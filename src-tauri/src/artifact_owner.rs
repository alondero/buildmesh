//! Keeps this worktree's Cargo artifact stamp in the library test binary.
//!
//! `scripts/rust-test-shards.mjs` searches that executable for the stamp before
//! running it. A shared target directory can otherwise hand back another
//! worktree's binary (issue #2339). A plain reference is not enough: the
//! Windows linker drops the bytes unless the running test keeps them alive.

fn artifact_owner() -> &'static str {
    option_env!("BUILDMESH_ARTIFACT_OWNER").unwrap_or("")
}

#[test]
fn artifact_owner_stamp_is_linked() {
    let owner = std::hint::black_box(artifact_owner());
    assert!(
        owner.starts_with("buildmesh-artifact-owner-"),
        "the shard runner searches the test binary for this stamp"
    );
}
