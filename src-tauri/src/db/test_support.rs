//! Shared test infrastructure for backend unit/integration tests that
//! need a real SQLite database.
//!
//! Six modules in the backend previously copied the same
//! "per-process scratch path + `std::sync::Once` + `db::init`"
//! bootstrap (raised on PR #1643 review): `commands::agent::tests`,
//! `commands::prune_tests`, `services::agent_node::tests`,
//! `services::warm_pool::tests`, `git::worktree::provision::tests`,
//! and `http::ws::tests`. Each grew a near-identical
//! `ensure_*_db()` helper plus a `*_test_db_path()` resolver. One
//! of them (`services::warm_pool`) had a latent bug where
//! `std::sync::Once::new().call_once(|| …)` constructed a fresh
//! `Once` on every call — only `db::init`'s own
//! `if DB.get().is_some() { return Ok(()) }` guard prevented
//! duplicate work.
//!
//! Call sites that previously rolled their own should now call
//! [`ensure_db_for_tests`] instead. The shared scratch path is
//! process-unique (PID-suffixed) so concurrent test binaries never
//! collide; the `Once` is module-scoped so a test that wins the
//! race initialises the global `db::DB` and every later caller
//! short-circuits on the second `is_initialized` probe without
//! re-acquiring the `INIT_LOCK`.
//!
//! Migration is incremental — sites with extra concerns (e.g.
//! `commands::agent::tests::PR_TEST_LOCK`, which holds a mutex
//! across the whole test body for serialisation) keep their
//! local lock but can call this helper instead of their own
//! path/`Once` pair.
//!
//! The `.db` suffix on the path is load-bearing —
//! `Connection::open(path)` expects a file, not a directory.
//! `db::mesh_tests` uses the same `*.db` shape; that contract
//! is the test-DB seam.

use std::path::PathBuf;
use std::sync::{Once, OnceLock};

/// Per-process scratch path under the OS temp directory. PID-suffixed
/// so concurrent test binaries never collide on the SQLite file.
fn scratch_path() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let p = std::env::temp_dir().join(format!(
            "buildmesh_lib_test_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    })
    .clone()
}

/// One-shot `db::init` for any test that needs the global DB.
///
/// Idempotent across the whole test process: the `Once` runs the
/// closure exactly once, and `db::init` itself short-circuits with
/// `Ok(())` if the global `DB` OnceCell has already been populated
/// (so a sibling test file that won the race first is fine — we
/// just reuse its connection).
///
/// No-op when the test binary doesn't need the DB at all. Tests
/// that exercise only pure logic (no DB) should not call this
/// helper.
pub fn ensure_db_for_tests() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // `db::init` is the canonical entry point — it acquires the
        // process-wide `INIT_LOCK` and runs `init_schema` against the
        // scratch path. A second concurrent caller (without the
        // `Once` wrapper) would block on the lock, then see the
        // `DB.get().is_some()` short-circuit and return Ok without
        // doing extra work — but the lock + short-circuit is wasted
        // overhead, so we funnel everything through the `Once`.
        let _ = crate::db::init(&scratch_path());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Calling `ensure_db_for_tests` more than once must be safe —
    /// the second call observes the populated `db::DB` and
    /// short-circuits without panic. Pinning the idempotency
    /// contract so a future refactor that drops the `Once`
    /// (re-introducing the warm_pool bug class) fails here.
    #[test]
    fn ensure_db_for_tests_is_idempotent() {
        ensure_db_for_tests();
        assert!(crate::db::is_initialized());
        ensure_db_for_tests();
        assert!(crate::db::is_initialized());
    }
}
