//! Shared test infrastructure for backend tests that need a real SQLite
//! database.
//!
//! # Per-test isolation (issue #2048)
//!
//! [`isolated`] installs a fresh, fully-migrated, private in-memory database
//! for the *current test thread* and hands back a guard that uninstalls it
//! when the test's body ends. [`super::get`] prefers that thread's database
//! over the process-global `OnceCell`, so every `db::read_conn()` /
//! `db::write_conn()` in the test resolves to the test's own rows and two
//! tests on two threads can no longer see each other's rows. That shared
//! global row set is what forced `cargo test -- --test-threads=1` and the
//! module-level serialisation locks (`MESH_TESTS_LOCK`, `GFS_LOCK`,
//! `MESH_CLONE_TEST_LOCK`, `PR_TEST_LOCK`, `CREATE_PR_DB_LOCK`).
//!
//! The seam is the same shape [`crate::preferences::storage`] already uses for
//! its `APP_DATA_DIR` / `CACHE` statics (issue #1386): a `thread_local!` slot
//! behind `cfg(test)`, production untouched. This module is `#[cfg(test)]` in
//! its entirety, so a release build compiles the plain `OnceCell` path and
//! `db::init` still opens exactly one database.
//!
//! Use it as the first statement of a test body:
//!
//! ```ignore
//! #[test]
//! fn my_test() {
//!     let _db = db::test_support::isolated();
//!     db::create_mesh("only mine", "C:/only-mine").unwrap();
//!     // Counts here are this test's rows, whatever else runs in parallel.
//!     assert_eq!(db::list_meshes().unwrap().len(), 1);
//! }
//! ```
//!
//! Keep the guard: dropping it early falls back to the global database and
//! the test starts reading other tests' rows again.
//!
//! # Nesting
//!
//! `isolated()` is re-entrant. A shared helper called from a test body may
//! install one too; the inner call only bumps a depth counter and returns a
//! guard that decrements it. Without that, a nested call would swap the
//! database out from under the outer test and silently discard the rows it
//! had just written. The outermost call creates the database and the last
//! guard to drop uninstalls it, so the next test to run on a recycled libtest
//! worker thread starts from an empty schema.
//!
//! # Why the database is leaked, and what that costs
//!
//! `read_conn()` returns `ReadConnection<'static>` and `write_conn()` returns
//! `MutexGuard<'static, Connection>`: the public seam hands out `'static`
//! handles, so a per-thread database has to be reachable by a `'static`
//! reference. [`Box::leak`] is the sound way to get one — a `&'static` carved
//! out of the `thread_local!` cell would dangle when the thread exited, and
//! those handles are `Send`, so nothing stops a caller from outliving the
//! thread that installed the database.
//!
//! The cost is one unreachable [`super::Database`] per isolated test, holding
//! its writer and reader connections open until the process exits. Two
//! decisions bound that:
//!
//! * the database is in memory, so a run leaves nothing in the temp directory
//!   and a test database cannot inherit a stale file or WAL from a crashed
//!   previous run, and
//! * [`TEST_READER_POOL_SIZE`] gives an isolated database a small reader pool
//!   instead of the production [`super::READER_POOL_SIZE`], so a test costs a
//!   handful of connections rather than nine.
//!
//! A test that needs more concurrent checkouts than that pool holds should
//! take a `Connection` and thread it through the `_inner(&Connection)`
//! helpers, the way `db::circuit_tests` does — that path needs no pool.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use super::SqlResult;

/// Reader connections opened for an isolated test database. Production opens
/// [`super::READER_POOL_SIZE`]; an isolated database is used by one test
/// thread, so a smaller pool is enough for ordinary production code paths and
/// keeps the per-test cost of the leak above bounded.
const TEST_READER_POOL_SIZE: usize = 4;

/// One isolated database installed on a test thread.
struct Installed {
    /// `&'static` handle to the isolated database, obtained by leaking the
    /// owned [`super::Database`]. See the module docs for why leaking is the
    /// sound choice; this field is the only reference to it.
    db: &'static super::Database,
    /// Files to best-effort remove once the test is done with this database.
    ///
    /// Only a file-backed install populates it. The removal is allowed to
    /// fail: the leaked connections still hold the file open on Windows, so
    /// the directory is left to the OS temp cleaner there — see
    /// [`scratch_dir`], which is deliberately not reclaimed by the next run.
    files: Arc<Vec<PathBuf>>,
    /// This install's private preferences directory. `preferences::storage`
    /// keeps `APP_DATA_DIR` / `CACHE` in per-thread slots, so without a fresh
    /// directory per install a test would inherit whatever the previous test
    /// on this recycled libtest worker thread left behind. Dropping this
    /// `TempDir` removes the files.
    prefs: Option<tempfile::TempDir>,
    /// How many live guards point at this database on this thread. The
    /// outermost [`isolated`] call sets it to 1; a nested call inside a
    /// shared helper bumps it so the helper cannot replace the caller's rows.
    depth: usize,
    /// Whether this thread is the one that *created* the database. Only the
    /// creator clears `files`, so a thread that merely [`adopt`]ed the
    /// database does not delete a file its creator is still using.
    owner: bool,
}

thread_local! {
    static INSTALLED: RefCell<Option<Installed>> = const { RefCell::new(None) };
}

/// Hands out a distinct name to every install in this process, whether it is
/// an in-memory URI or a scratch file. See [`next_database_uri`].
static NEXT_ORDINAL: AtomicU64 = AtomicU64::new(0);

/// The database [`isolated`] installed for this thread, if any.
///
/// `db::get` calls this to resolve a connection. `None` means the thread has
/// not installed one and callers fall back to the process-global `OnceCell`,
/// which is always the case in production.
pub(super) fn installed_db() -> Option<&'static super::Database> {
    INSTALLED.with(|slot| slot.borrow().as_ref().map(|installed| installed.db))
}

/// A private, in-memory database URI unique to this process and install.
///
/// The shared-cache form is what [`super::init`] uses for `:memory:`: SQLite
/// gives each plain `:memory:` connection its own private database, so the
/// writer and the reader pool would not see one schema. The counter is what
/// keeps two test threads from landing on the same in-memory database — a
/// shared name would reintroduce exactly the cross-test row sharing this seam
/// exists to remove.
fn next_database_uri() -> PathBuf {
    let ordinal = NEXT_ORDINAL.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!(
        "file:buildmesh-test-{}-{ordinal}?mode=memory&cache=shared",
        std::process::id()
    ))
}

/// Open a fresh in-memory database with the full Buildmesh schema applied.
///
/// This mirrors [`super::init`] — same pragmas, same full migration pipeline,
/// same writer + reader pool shape — so a test sees the same schema and the
/// same connection semantics it would see in production, minus the file.
fn open_isolated_at(db_path: &Path) -> SqlResult<&'static super::Database> {
    let conn = super::open_writer(db_path)?;
    super::apply_connection_pragmas(&conn, false)?;
    super::init_schema(&conn)?;
    let readers = super::ReaderPool::open_sized(db_path, TEST_READER_POOL_SIZE)?;
    Ok(Box::leak(Box::new(super::Database {
        writer: Mutex::new(conn),
        readers,
    })))
}

/// Per-process scratch directory for the file-backed installs of
/// [`isolated_file`].
///
/// The PID keeps concurrent runs apart: `scripts/rust-test-shards.mjs` runs
/// several test binaries at once, and they all share one temp directory.
///
/// This directory is **not** reclaimed by the next run, and that is deliberate.
/// The `remove_dir_all` here can only ever target the *current* PID's name,
/// which does not exist yet, so pretending otherwise would document a cleanup
/// that never happens. What does clean up is [`IsolatedDbGuard`]'s drop, which
/// removes the files it created: on Unix an unlinked open file is fine, while
/// on Windows the leaked connections still hold them open and the removal is a
/// no-op. A Windows run therefore leaves one small directory behind for the OS
/// temp cleaner. Only the handful of tests that need real fsync costs or a
/// reopenable path reach this path at all; everything else is in memory and
/// writes nothing.
fn scratch_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| std::env::temp_dir().join(format!("buildmesh_lib_test_{}", std::process::id()))).clone()
}

/// The SQLite sidecar files a database path can leave behind, in the order
/// they must go: the `-wal` and `-shm` siblings plus the database itself.
fn database_files(db_path: &Path) -> Vec<PathBuf> {
    let mut siblings = vec![db_path.to_path_buf()];
    for suffix in ["-wal", "-shm"] {
        let mut name = db_path.as_os_str().to_os_string();
        name.push(suffix);
        siblings.push(PathBuf::from(name));
    }
    siblings
}

/// Node creation resolves a launch recipe from preferences, so a test that
/// creates nodes still needs an app-data directory. Preferences already
/// isolate `APP_DATA_DIR` per test thread (issue #1386), so each thread gets
/// its own temp directory here, exactly as the old process-global helper gave
/// each test binary one.
/// Give this thread a private preferences directory and return it so the
/// install can drop it.
///
/// `preferences::storage` keeps `APP_DATA_DIR` / `CACHE` in per-thread slots,
/// and libtest reuses worker threads. A directory created once per *thread* is
/// not enough: the next test to land here would inherit the previous one's
/// files and in-memory cache. So the outermost install claims a fresh one —
/// but only when the thread has none, because a test is allowed to install its
/// own directory *before* calling `isolated()` (several do, to seed a specific
/// preferences fixture) and that must not be silently replaced. What actually
/// stops the bleed is [`crate::preferences::reset_for_tests`] on the outermost
/// drop, which clears the slots before the next test sees them.
fn install_preferences_dir() -> Option<tempfile::TempDir> {
    if crate::preferences::app_data_dir().is_some() {
        // The test owns this directory; do not adopt it as ours to delete.
        return None;
    }
    let dir = tempfile::tempdir().expect("test preferences directory");
    crate::preferences::init_for_tests(dir.path().to_path_buf());
    Some(dir)
}

/// Serialises the tests that have to use the process-global database.
///
/// Exactly one lock and one initialisation live here on purpose. `db::init`
/// fills a first-wins `OnceCell`, so every caller shares whichever database
/// won the race; two module-local locks would happily hand two tests their own
/// "serialised" sections while they still wrote the same device-session tables
/// concurrently. One lock, one database.
///
/// Prefer [`isolated`]. Reach for this only when the code under test reaches the
/// database from a thread the test does not own — a `spawn_blocking` offload or
/// a Tauri-managed runtime, where a `thread_local!` install is invisible and
/// [`adopt`] cannot reach.
static GLOBAL_DB_LOCK: Mutex<()> = Mutex::new(());

/// RAII handle serialising access to the process-global database.
#[must_use = "this handle is what serialises the test; bind it (`let _serial = ...`) and drop it when the test ends"]
pub struct SharedGlobalDb {
    /// Not read: holding the lock for the guard's lifetime *is* the effect.
    _guard: std::sync::MutexGuard<'static, ()>,
}

/// Serialised access to the process-global database — see [`GLOBAL_DB_LOCK`] for
/// when (and why) this exists at all.
pub fn shared_global_db() -> SharedGlobalDb {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = scratch_dir();
        let _ = std::fs::create_dir_all(scratch_dir());
        let _ = super::init(&scratch_dir().join("global.sqlite"));
    });
    SharedGlobalDb {
        _guard: GLOBAL_DB_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    }
}

/// Install a private database for the current test thread and return the guard
/// that uninstalls it.
///
/// The first call on a thread creates a fresh, fully-migrated in-memory
/// database. Later calls on the same thread — a shared helper invoked from a
/// test body — return a guard for the same database rather than a new one, so
/// a helper cannot discard the caller's rows.
///
/// Failures are fatal. Propagating the error from the install needs a closure
/// that returns it, and silently swallowing it would mask a real schema or
/// open failure as a downstream "database not initialized" panic far from the
/// cause.
#[must_use = "the guard keeps the test's database installed; bind it (`let _db = ...`) and drop it when the test ends"]
pub fn isolated() -> IsolatedDbGuard {
    install(next_database_uri(), Vec::new())
}

/// Like [`isolated`], but the database is a real file, so every commit pays
/// the fsync a production commit pays.
///
/// Most tests want [`isolated`]: an in-memory database is faster and leaves
/// nothing behind. A test that *measures* commit cost needs the file, because
/// an in-memory commit skips the fsync entirely and would make a
/// before/after comparison meaningless. `db::mesh_tests`' batch bench is the
/// case this exists for.
///
/// The file lives in a per-process scratch directory; see [`scratch_dir`] for
/// what is cleaned up when (and what is not).
#[must_use = "the guard keeps the test's database installed; bind it (`let _db = ...`) and drop it when the test ends"]
pub fn isolated_file() -> IsolatedDbGuard {
    let ordinal = NEXT_ORDINAL.fetch_add(1, Ordering::Relaxed);
    let scratch = scratch_dir();
    if let Err(error) = std::fs::create_dir_all(&scratch) {
        panic!("db::test_support::isolated_file: creating {} failed: {error}", scratch.display());
    }
    let db_path = scratch.join(format!("db-{ordinal}.sqlite"));
    install(db_path.clone(), database_files(&db_path))
}

/// Shared install path for [`isolated`] and [`isolated_file`].
fn install(db_path: PathBuf, files: Vec<PathBuf>) -> IsolatedDbGuard {
    match claim_depth() {
        // A nested call inside a shared helper: keep the caller's database.
        Some(current) => IsolatedDbGuard::from(current),
        None => {
            let db = open_isolated_at(&db_path).unwrap_or_else(|error| {
                panic!(
                    "db::test_support: opening the isolated test database at {} failed: {error}; \
                     see db/mod.rs for the canonical init path",
                    db_path.display()
                )
            });
            let handle = IsolatedDbHandle {
                db,
                files: Arc::new(files),
            };
            INSTALLED.with(|slot| {
                *slot.borrow_mut() = Some(Installed {
                    db,
                    files: Arc::clone(&handle.files),
                    prefs: install_preferences_dir(),
                    depth: 1,
                    owner: true,
                });
            });
            IsolatedDbGuard::from(handle)
        }
    }
}

/// Install `handle`'s database on the *current* thread, returning a guard that
/// uninstalls it when dropped.
///
/// A `thread_local!` install does not follow the thread: code a test hands to
/// `std::thread::spawn` (or to a multi-threaded tokio runtime) resolves the
/// process-global `OnceCell` instead of the test's database, which is a
/// "database not initialized" panic at best and another test's rows at worst.
/// Production code reaches the database from worker threads, so a test that
/// exercises such a path has to say so explicitly:
///
/// ```ignore
/// let _db = db::test_support::isolated();
/// let on_worker = _db.handle();
/// let handle = std::thread::spawn(move || {
///     let _adopted = db::test_support::adopt(&on_worker);
///     db::delete_orphaned_claimed_warm_worktrees();
/// });
/// ```
///
/// Adopting a *different* database than the thread already has installed is a
/// bug in the test rather than something to paper over, so this panics instead
/// of silently resolving to the thread's existing database.
///
/// The adopting thread is not the creator, so it does not delete a
/// file-backed database's files when its guard drops — the creating test's
/// guard still owns that.
pub fn adopt(handle: &IsolatedDbHandle) -> IsolatedDbGuard {
    // Checked *before* claiming depth, so a panic leaves the thread's depth
    // balanced. A libtest worker thread is reused by later tests, and an
    // unbalanced count would keep the install alive into them.
    if let Some(current) = installed_db() {
        assert!(
            std::ptr::eq(current, handle.db),
            "db::test_support::adopt: this thread already has a different isolated database \
             installed; adopting another one in the same thread cannot work"
        );
    }
    match claim_depth() {
        Some(current) => IsolatedDbGuard::from(current),
        None => {
            let handle = handle.clone();
            INSTALLED.with(|slot| {
                *slot.borrow_mut() = Some(Installed {
                    db: handle.db,
                    files: Arc::clone(&handle.files),
                    // The adopting thread has preferences slots of its own, and
                    // nothing seeded them.
                    prefs: install_preferences_dir(),
                    depth: 1,
                    owner: false,
                });
            });
            IsolatedDbGuard::from(handle)
        }
    }
}

/// Claim one unit of depth on this thread's existing install, if it has one.
///
/// Returns a handle to the installed database so the caller can reuse it, or
/// `None` when the caller must install one.
fn claim_depth() -> Option<IsolatedDbHandle> {
    INSTALLED.with(|slot| {
        let mut slot = slot.borrow_mut();
        let installed = slot.as_mut()?;
        installed.depth += 1;
        Some(IsolatedDbHandle {
            db: installed.db,
            files: Arc::clone(&installed.files),
        })
    })
}

/// A name for an isolated database that can be moved to another thread.
///
/// This is what [`adopt`] consumes. It is `Clone` + `Send` and deliberately
/// **not** an RAII guard: dropping a handle must not uninstall anything, or
/// `let handle = guard.handle();` followed by an unrelated drop would tear the
/// database out from under the test. Use [`IsolatedDbGuard`] to keep an install
/// alive and [`IsolatedDbHandle`] only to reach it from a spawned thread.
#[derive(Clone)]
pub struct IsolatedDbHandle {
    db: &'static super::Database,
    files: Arc<Vec<PathBuf>>,
}

// `adopt` moves a handle into a spawned closure, so this has to hold.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<IsolatedDbHandle>();
};

/// RAII guard keeping this thread's isolated database installed.
///
/// The guard is single-owner and is *not* `Clone`, so it cannot be dropped by
/// accident from a copied value; to reach the database from a spawned thread,
/// take an [`IsolatedDbHandle`] with [`IsolatedDbGuard::handle`] instead.
///
/// It is also `!Send` and `!Sync` (`PhantomData<*const ()>`). A guard owns a
/// *thread-local* install, so moving one to another thread would make its
/// `Drop` decrement whatever that thread happened to have installed. The
/// handle is the type that crosses threads.
///
/// Dropping the last guard on a thread uninstalls that thread's install and
/// clears its preferences slots, so the next test to run on the same libtest
/// worker thread starts from an empty schema and an empty preferences
/// directory. The `Database` itself is leaked rather than freed — see the
/// module docs — so a guard governs *which* database the thread resolves, not
/// the lifetime of the allocation.
#[must_use = "the guard keeps the test's database installed; bind it (`let _db = ...`) and drop it when the test ends"]
pub struct IsolatedDbGuard {
    handle: IsolatedDbHandle,
    /// Pins the guard to its thread; see the type docs.
    _not_send: PhantomData<*const ()>,
}

impl IsolatedDbGuard {
    /// A movable name for this database, for use with [`adopt`] on another
    /// thread. Dropping the returned handle does not uninstall anything.
    pub fn handle(&self) -> IsolatedDbHandle {
        self.handle.clone()
    }
}

/// Single construction site for the guard, so the thread pin cannot be
/// forgotten at one of the install paths.
impl From<IsolatedDbHandle> for IsolatedDbGuard {
    fn from(handle: IsolatedDbHandle) -> Self {
        IsolatedDbGuard { handle, _not_send: PhantomData }
    }
}

impl Drop for IsolatedDbGuard {
    fn drop(&mut self) {
        let mine = self.handle.db;
        INSTALLED.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(installed) = slot.as_mut() else {
                return;
            };
            // Only ever release *our own* install. The guard is `!Send` today,
            // so this cannot fire — it is the invariant stated in one place, so
            // a future change that relaxes the pin fails loudly here instead of
            // silently stealing another install's depth.
            assert!(
                std::ptr::eq(installed.db, mine),
                "db::test_support: dropping a guard released a different install"
            );
            installed.depth = installed.depth.saturating_sub(1);
            if installed.depth == 0 {
                // Only the creating thread removes the files: an adopting
                // thread is working with a database its creator may still be
                // using, and on Windows the creator's leaked connections are
                // what hold them open anyway.
                if installed.owner {
                    for file in installed.files.iter() {
                        // Best effort: on Unix the open file can be unlinked,
                        // on Windows the leaked connections keep it locked and
                        // the directory is left to the OS temp cleaner.
                        let _ = std::fs::remove_file(file);
                    }
                }
                // The preferences slots are per-thread, so leaving them set
                // would hand this test's preferences to the next test that runs
                // on this recycled worker thread. Dropping the `TempDir` here
                // removes the files as well.
                crate::preferences::reset_for_tests();
                installed.prefs = None;
                *slot = None;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test that installs an isolated database owns its own rows: the row
    /// it writes is visible to its own connection, and a fresh install after
    /// the guard drops starts empty. This is the property the whole seam
    /// exists for, pinned at the seam rather than at one call site.
    #[test]
    fn isolated_database_is_private_per_install() {
        {
            let _db = isolated();
            assert!(crate::db::is_initialized());
            crate::db::create_mesh("private", "C:/isolated-private").unwrap();
            assert_eq!(crate::db::list_meshes().unwrap().len(), 1);
        }

        let _db = isolated();
        assert_eq!(
            crate::db::list_meshes().unwrap().len(),
            0,
            "a reinstalled database must not inherit the previous test's rows"
        );
    }

    /// Pinning the nesting contract: a helper that calls `isolated()` must
    /// reuse the caller's database, not swap a fresh one in and discard the
    /// rows the caller already wrote.
    #[test]
    fn nested_install_reuses_the_outer_database() {
        let _db = isolated();
        crate::db::create_mesh("before", "C:/isolated-nested").unwrap();

        {
            let _nested = isolated();
            assert_eq!(
                crate::db::list_meshes().unwrap().len(),
                1,
                "a nested install must not replace the caller's database"
            );
        }

        assert_eq!(
            crate::db::list_meshes().unwrap().len(),
            1,
            "the database must survive its nested guard"
        );
    }

    /// Two isolated databases never share a row set, even when they live in
    /// the same process. This is the parallelism guarantee the leaked
    /// in-memory URIs have to keep: a collision here would hand two test
    /// threads the same schema and reintroduce the ordering dependence
    /// `--test-threads=1` used to paper over.
    #[test]
    fn distinct_installs_do_not_share_rows() {
        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                let _db = isolated();
                crate::db::create_mesh("first", "C:/isolated-first").unwrap();
                crate::db::list_meshes().unwrap().len()
            });
            let second = scope.spawn(|| {
                let _db = isolated();
                crate::db::create_mesh("second", "C:/isolated-second").unwrap();
                crate::db::list_meshes().unwrap().len()
            });
            (first.join().unwrap(), second.join().unwrap())
        });

        assert_eq!((first, second), (1, 1));
    }

    /// A thread a test hands work to has no install of its own, so without
    /// [`adopt`] it resolves the process-global database instead of the test's
    /// — a "database not initialized" panic, or another test's rows. Pin the
    /// contract on both sides: the worker sees the test's rows, and a worker
    /// that never adopts does not.
    #[test]
    fn adopted_thread_resolves_the_tests_database() {
        let _db = isolated();
        crate::db::create_mesh("owned", "C:/adopt-owned").unwrap();
        let on_worker = _db.handle();

        let seen = std::thread::spawn(move || {
            let _adopted = adopt(&on_worker);
            crate::db::list_meshes()
                .unwrap()
                .into_iter()
                .map(|mesh| mesh.name)
                .collect::<Vec<_>>()
        })
        .join()
        .unwrap();

        assert_eq!(
            seen,
            vec!["owned".to_string()],
            "an adopted thread must resolve the test's database, not an empty or shared one"
        );
    }

    /// A handle is a *name* for a database, not a claim on it. Dropping one —
    /// or dropping a clone of one — must leave the install standing, which is
    /// what lets a caller move a handle into a spawned closure without the move
    /// itself uninstalling anything.
    #[test]
    fn dropping_a_handle_does_not_uninstall_the_database() {
        let _db = isolated();
        crate::db::create_mesh("kept", "C:/handle-drop").unwrap();

        let handle = _db.handle();
        let clone = handle.clone();
        drop(handle);
        drop(clone);

        assert_eq!(
            crate::db::list_meshes().unwrap().len(),
            1,
            "dropping a handle must not uninstall the database its guard installed"
        );
    }

    /// Adopting a database the thread does not already have is a test bug, not
    /// something to absorb silently. Pinned here because the failure mode is
    /// otherwise invisible: the thread would quietly keep resolving its
    /// *existing* database and the assertions would pass against the wrong rows.
    #[test]
    #[should_panic(expected = "already has a different isolated database")]
    fn adopting_a_foreign_database_is_rejected() {
        // Take a handle to one database, then let its guard drop so this thread
        // stops resolving it. The `Database` itself is leaked, so the handle
        // still names a live (if no longer installed) database.
        let foreign = {
            let guard = isolated();
            guard.handle()
        };

        // A different install now owns this thread.
        let _current = isolated();
        crate::db::create_mesh("current", "C:/adopt-foreign-current").unwrap();

        let _adopted = adopt(&foreign);
    }

    /// Preferences are per-thread slots and libtest reuses worker threads, so
    /// an install that does not reset them hands the next test on that thread
    /// the previous test's preferences. Pin both halves: the directory is
    /// fresh per install, and the outermost drop leaves the thread clean.
    #[test]
    fn preferences_are_reset_for_the_next_test_on_the_thread() {
        // libtest reuses worker threads, so pin the precondition: this test
        // owns the thread's preferences slots from here on.
        crate::preferences::reset_for_tests();

        let first_dir;
        {
            let _db = isolated();
            first_dir = crate::preferences::app_data_dir().unwrap();
            assert!(first_dir.exists(), "the install seeds a real directory");
        }
        assert_eq!(
            crate::preferences::app_data_dir(),
            None,
            "the outermost drop must clear the thread's preferences slot, or the \
             next test on this recycled worker thread inherits them"
        );

        let _db = isolated();
        let second_dir = crate::preferences::app_data_dir().unwrap();
        assert_ne!(
            first_dir, second_dir,
            "each install gets its own preferences directory"
        );
        assert!(!first_dir.exists(), "the previous directory is torn down");
    }

    /// One lock for the process-global database. Two module-local locks would
    /// each look serialised while both callers still wrote the same tables,
    /// because `db::init` fills a first-wins `OnceCell` they both share.
    #[test]
    fn shared_global_db_is_serialised_against_itself() {
        let _serial = shared_global_db();
        // A second holder on another thread must not be able to take it, which
        // is what "one lock" buys over the two module locks this replaced.
        let contended = std::thread::spawn(|| {
            let _contended = shared_global_db();
            true
        });

        assert!(
            !contended.is_finished(),
            "a second shared_global_db() must block while the first is held"
        );
        drop(_serial);
        assert!(contended.join().unwrap());
    }

    /// `db::init` must still open exactly one process-global database, and a
    /// second `init` must still be a no-op rather than replacing it. The
    /// per-test seam must not change production's single-connection
    /// contract.
    #[test]
    fn init_still_opens_one_process_global_database() {
        let _db = isolated();
        let scratch = tempfile::tempdir().unwrap();
        crate::db::init(&scratch.path().join("global.sqlite")).unwrap();
        crate::db::init(&scratch.path().join("second.sqlite")).unwrap();

        // The installed test database still wins for this thread.
        assert_eq!(crate::db::list_meshes().unwrap().len(), 0);
    }
}
