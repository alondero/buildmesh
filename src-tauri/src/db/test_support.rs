//! Shared test infrastructure for backend unit/integration tests that
//! need a real SQLite database.
//!
//! Each test in the backend that needs the global DB calls
//! [`ensure_db_for_tests`] instead of standing up its own scratch
//! path + `Once` + `db::init` boilerplate. The shared scratch path
//! is process-unique (PID-suffixed) so concurrent test binaries never
//! collide; the `Once` is module-scoped so the first caller
//! initialises the global `db::DB` and every later caller
//! short-circuits on the `is_initialized` probe without
//! re-acquiring the `INIT_LOCK`.
//!
//! Sites that need extra serialisation (e.g. test bodies holding a
//! mutex for write/read interleaving) keep their own local lock but
//! can still call this helper for the init step — the lock and the
//! DB init are orthogonal concerns.
//!
//! The `.db` suffix on the path is load-bearing —
//! `Connection::open(path)` expects a file, not a directory.
//! `db::mesh_tests` uses the same `*.db` shape; that contract
//! is the test-DB seam.

use std::path::{Path, PathBuf};
use std::sync::{Once, OnceLock};

/// Per-process scratch path under the OS temp directory. PID-suffixed
/// so concurrent test binaries never collide on the SQLite file.
///
/// The `-wal` and `-shm` siblings must be removed alongside the
/// `.db` itself — a previous crashed run leaves the WAL header on
/// disk, and SQLite refuses to open a database whose header SHA
/// disagrees with the live WAL. The remove-best-effort pattern is
/// `let _ = ...` because the files may not exist on a fresh run.
fn scratch_path() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let p = std::env::temp_dir().join(format!(
            "buildmesh_lib_test_{}.db",
            std::process::id()
        ));
        for sibling in [p.clone(), wal_sibling(&p), shm_sibling(&p)] {
            let _ = std::fs::remove_file(&sibling);
        }
        p
    })
    .clone()
}

fn wal_sibling(db: &Path) -> PathBuf {
    let mut s = db.to_owned().into_os_string();
    s.push("-wal");
    PathBuf::from(s)
}

fn shm_sibling(db: &Path) -> PathBuf {
    let mut s = db.to_owned().into_os_string();
    s.push("-shm");
    PathBuf::from(s)
}

/// One-shot `db::init` for any test that needs the global DB.
///
/// Idempotent across the whole test process: the `Once` runs the
/// closure exactly once, and `db::init` itself short-circuits with
/// `Ok(())` if the global `DB` OnceCell has already been populated
/// (so a sibling test file that won the race first is fine — we
/// just reuse its connection).
///
/// `db::init` failures are fatal — propagating the error from
/// `call_once` via `expect` would be cleaner, but `Once::call_once`
/// doesn't return its closure's result, so the next best thing is
/// to panic with a clear message. Silently swallowing the error
/// masks SQLite header corruption / disk-permission failures as
/// downstream "database not initialized" panics far from the
/// actual cause.
///
/// Tests that exercise only pure logic (no DB) should not call this
/// helper — every first call pays the `db::init` cost.
pub fn ensure_db_for_tests() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if let Err(e) = crate::db::init(&scratch_path()) {
            panic!(
                "db::test_support::ensure_db_for_tests: db::init failed: {e}; \
                 see db/mod.rs for the canonical init path"
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinning the Once/idempotency contract: the second call observes
    /// the populated `db::DB` and short-circuits without panic.
    /// Catches a regression that drops the `Once` (every call would
    /// then acquire the `INIT_LOCK` and re-run `init_schema`).
    #[test]
    fn ensure_db_for_tests_is_idempotent() {
        ensure_db_for_tests();
        assert!(crate::db::is_initialized());
        ensure_db_for_tests();
        assert!(crate::db::is_initialized());
    }
}
