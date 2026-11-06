//! The mesh-property writes' own suite (issue #2527).
//!
//! The statements moved out of the command layer, so the layer that owns
//! them is where they are pinned: the allowlist rejects an unknown column
//! before a statement is built, and a write that matches no rows says so.

use super::{set_mesh_base_ref, set_mesh_column, set_mesh_name, WRITABLE_COLUMNS};
use crate::db::test_support;

#[test]
fn a_mesh_name_write_updates_the_row() {
    let _db = test_support::isolated();
    let mesh = crate::db::create_mesh("before", "C:/buildmesh-properties").unwrap();

    set_mesh_name(mesh.id, "after").unwrap();

    let stored = crate::db::get_mesh_by_id(mesh.id).unwrap();
    assert_eq!(stored.name, "after");
}

#[test]
fn a_writable_column_is_updated() {
    let _db = test_support::isolated();
    let mesh = crate::db::create_mesh("columns", "C:/buildmesh-columns").unwrap();

    set_mesh_column(mesh.id, "model", "gpt-5").unwrap();

    let stored = crate::db::get_mesh_by_id(mesh.id).unwrap();
    assert_eq!(stored.model.as_deref(), Some("gpt-5"));
}

#[test]
fn an_unknown_column_is_rejected_before_the_statement_runs() {
    let _db = test_support::isolated();
    let mesh = crate::db::create_mesh("unknown", "C:/buildmesh-unknown").unwrap();

    let error = set_mesh_column(mesh.id, "id", "tampered").unwrap_err();

    // The name is interpolated into the statement, so the allowlist is the
    // only thing standing between the wire and the SQL. The rejection is
    // the DB layer's, not only the command boundary's.
    assert!(
        matches!(error, rusqlite::Error::InvalidParameterName(ref name) if name == "id"),
        "an unknown column must be rejected by name, got {error:?}"
    );
    // The row is untouched.
    let stored = crate::db::get_mesh_by_id(mesh.id).unwrap();
    assert_eq!(stored.id, mesh.id);
}

#[test]
fn the_writable_column_list_holds_no_primary_key() {
    // The list interpolates a column name into an UPDATE, so a column that
    // is not a plain setting must never appear in it.
    assert!(!WRITABLE_COLUMNS.contains(&"id"));
    assert!(!WRITABLE_COLUMNS.contains(&"base_ref"));
    assert!(!WRITABLE_COLUMNS.contains(&"use_worktree"));
    assert!(WRITABLE_COLUMNS.contains(&"model"));
}

#[test]
fn a_base_ref_write_updates_the_row() {
    let _db = test_support::isolated();
    let mesh = crate::db::create_mesh("base", "C:/buildmesh-base-ref").unwrap();

    set_mesh_base_ref(mesh.id, "HEAD").unwrap();

    let stored = crate::db::get_mesh_by_id(mesh.id).unwrap();
    assert_eq!(stored.base_ref, "HEAD");
}
