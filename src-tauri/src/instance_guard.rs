//! Profile ownership: exactly one Buildmesh process per app-data profile
//! (issue #1521).
//!
//! Why this exists
//! ---------------
//! Nothing stopped a second Buildmesh process from opening the *same*
//! app-data profile as a running one. That is not a harmless second window:
//! the loser opens the same SQLite database, its startup crash sweep
//! (`session_lifecycle::recover_from_crash`) cannot see the owner's live PTYs
//! — the process registry is process-local — so it marks the owner's running,
//! awaiting-input, pending, spawning, and ready Agent Nodes as suspended and
//! the frontend can auto-resume a *second* agent harness in the same Worktree
//! Node. The HTTP server's 1992→1994 fallback makes it worse, not better: the
//! loser happily binds a spare port, so both processes keep running.
//!
//! The fix is to make profile ownership a hard precondition of startup rather
//! than a best-effort convention: claim the profile, and only then run
//! anything that touches it. [`with_profile_ownership`] is that seam — it runs
//! the startup continuation *only* for the process that won the claim, so
//! "a losing process never reaches database initialization" is a property of
//! the production call order, not of every caller's discipline.
//!
//! # What the claim is
//!
//! One OS-level claim per **(bundle identifier, canonical app-data profile)**
//! pair. Both halves matter:
//!
//! * The identifier keeps the stable hub and the `.dev` profile apart, so a
//!   dev build still runs alongside a stable install (they already differ in
//!   data dir and port offset — `http::port_offset`).
//! * The canonicalised profile dir keeps two identifiers that happen to point
//!   at one directory apart, and makes `C:\Users\me\AppData\...` and
//!   `c:\users\me\appdata\...` the same profile. The path is canonicalised
//!   rather than used as written, so a caller that spells the profile
//!   differently cannot claim the same database twice.
//!
//! # Mechanism
//!
//! * Windows — a named mutex object (`CreateMutexW`) in the **`Global\`**
//!   namespace, created *without* initial ownership so the object's existence
//!   plus our open handle is the whole claim. There is no mutex to release and
//!   no thread to own it, which means the claim cannot be leaked by a
//!   panicking thread. `Global\` rather than `Local\` because a session-scoped
//!   object is invisible to other Windows sessions, and a second session
//!   resolving the same profile (an RDP reconnect, a scheduled task) must not
//!   get to be a second primary.
//! * macOS / Linux — an exclusive, non-blocking `flock` on
//!   `instance-<digest>.lock` inside the profile dir, where the digest is the
//!   same (identifier, profile) fingerprint the mutex name uses.
//!
//! Both are released by the OS when the process dies, which is the property
//! the crash-watchdog relaunch depends on: a hard crash must not leave a
//! profile unopenable until the user clears a lock file by hand.
//!
//! # Bringing the owner forward
//!
//! A second launch must not feel like a no-op, so the owner publishes its
//! process id at `instance-owner.pid` in the profile dir, and the loser finds
//! that process's main window by enumerating top-level windows, then restores
//! it (`SW_RESTORE`) and calls `SetForegroundWindow`.
//!
//! PID rather than a window handle, on purpose: `WebviewWindow::hwnd()` is not
//! available yet while `setup` runs on Windows (the native window is created
//! once the event loop starts, so it answers `RawHandleError(Unavailable)`), and
//! a protocol that depends on *when* a handle becomes available is a protocol
//! with a hole in it. The pid is known the moment the claim is won. A stale pid
//! left by a crashed owner is harmless — it names either no process or a
//! process with no window carrying Buildmesh's title — and Windows reclaims a
//! dead process's id only after that process is gone.
//!
//! Windows-only, as window enumeration is: elsewhere the loser exits quietly
//! and the user switches to the running window themselves.
//!
//! # Test seam
//!
//! Nothing here takes a `tauri::App` or a window. The child-process test at the
//! bottom of this file spawns a second OS process running this same module's
//! public API — with a real `db::init` as its "startup" continuation — to
//! prove the loser never reaches database initialization. That is the whole
//! reason the continuation is a callback instead of a line of code in `lib.rs`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use parking_lot::Mutex;
use sha2::{Digest, Sha256};

/// The stable app's `tauri.conf.json` identifier, as the tests spell it.
/// Production reads the identifier from the Tauri config, so this is not a
/// second source of truth — it only pins the value these tests reason about.
#[cfg(test)]
const STABLE_IDENTIFIER: &str = "com.alond.buildmesh";

/// Suffix for the profile-directory lock file on platforms without named
/// objects; the claim's digest completes the name.
#[cfg(not(target_os = "windows"))]
const CLAIM_FILE_SUFFIX: &str = ".lock";

/// How long a losing launch waits for a holder that may be on its way out.
///
/// Tauri's update and restart path spawns the successor from inside the
/// outgoing process's own `Exit` event, so a relaunch can reach this code
/// while the previous process is still alive and still holding the claim.
/// Without a pause the successor would conclude it was a second launch, hand
/// the activation to a process that is exiting, and quit — leaving the user
/// with no app after an update. A short wait turns that into an ordinary
/// handover. It costs a genuine second launch only a fraction of a second,
/// because the holder is not exiting and every attempt fails.
const RECLAIM_ATTEMPTS: u32 = 4;
const RECLAIM_RETRY: Duration = Duration::from_millis(100);

/// Where the profile owner publishes its process id, so a second launch can
/// bring that window forward instead of exiting silently.
const OWNER_PID_FILE: &str = "instance-owner.pid";

/// Title prefix the main window carries, used to pick Buildmesh's own window
/// out of everything the owner process owns. Shared with `lib.rs`, which sets
/// the title, so the two cannot drift apart.
pub const MAIN_WINDOW_TITLE_PREFIX: &str = "Buildmesh - ";

/// How many times the loser looks for the owner's window before giving up. The
/// owner publishes its pid the moment it wins the claim, but its window appears
/// only once the event loop starts, so a launch that loses the race can arrive
/// first. The bound is here so an owner that never shows a window (a crashed
/// process, a headless test) can never hang the second launch.
const OWNER_WINDOW_ATTEMPTS: u32 = 25;
const OWNER_WINDOW_RETRY: Duration = Duration::from_millis(100);

/// Ownership breadcrumb log: forwarded launches and fatal claim failures.
const PROFILE_OWNERSHIP_LOG: &str = "profile-ownership.log";
const PROFILE_OWNERSHIP_LOG_MAX_BYTES: u64 = 256 * 1024;
const PROFILE_OWNERSHIP_LOG_KEEP: usize = 1;

/// Hex characters of the claim digest kept in the lock name. 16 nibbles is
/// 64 bits: this only has to separate the handful of profiles one machine can
/// plausibly run, and the object name has to stay well inside Win32's
/// `MAX_PATH`-sized name limit.
const CLAIM_DIGEST_LEN: usize = 16;

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// The (identifier, canonical profile dir) pair a claim is taken on.
///
/// Construction canonicalises the directory, so two spellings of the same
/// profile collapse to one claim. A profile dir that cannot be canonicalised
/// is an error rather than a best-effort pass-through: continuing without
/// knowing which profile we own is exactly the failure this module exists to
/// prevent (issue #1521, requirement 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileIdentity {
    identifier: String,
    profile_dir: PathBuf,
}

impl ProfileIdentity {
    /// Resolve `identifier` + `profile_dir` into a claimable identity.
    pub fn new(identifier: &str, profile_dir: &Path) -> Result<Self, OwnershipError> {
        let canonical = std::fs::canonicalize(profile_dir).map_err(|source| {
            OwnershipError::ProfileUnresolved {
                identifier: identifier.to_string(),
                profile_dir: profile_dir.display().to_string(),
                source,
            }
        })?;
        Ok(Self {
            identifier: identifier.to_string(),
            profile_dir: canonical,
        })
    }

    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    /// The canonicalised app-data directory this identity owns.
    pub fn profile_dir(&self) -> &Path {
        &self.profile_dir
    }

    /// The platform object the claim is taken on: a Win32 mutex name on
    /// Windows, the claim file's path elsewhere.
    pub fn claim_target(&self) -> String {
        let digest = claim_digest(self);
        #[cfg(target_os = "windows")]
        {
            // `Global\`, not `Local\`: a `Local\` object is invisible to other
            // Windows sessions, so an RDP reconnect, a scheduled task, or a
            // `runas` launch resolving the same profile would each become a
            // second primary and reach the database — the exact bug this
            // module exists to stop. Global mutexes need no special privilege
            // (that applies to file mappings and symlinks), and two users still
            // never collide because their app-data paths differ, and the path
            // is what the digest covers.
            format!("Global\\{}-{}", sanitize_object_name(&self.identifier), digest)
        }
        #[cfg(not(target_os = "windows"))]
        {
            // The digest carries the identifier, so two identifiers pointed at
            // one directory stay two claims here as well — the file name is the
            // claim, and the directory alone would not say which identity it
            // belongs to.
            self.profile_dir
                .join(format!("instance-{digest}{}", super::CLAIM_FILE_SUFFIX))
                .display()
                .to_string()
        }
    }
}

/// Reduce an identifier to characters Win32 accepts in an object name. The
/// digest carries the uniqueness, so this only exists to keep a hostile or
/// merely unusual identifier (`com.acme/buildmesh`) from producing an invalid
/// object name.
fn sanitize_object_name(identifier: &str) -> String {
    identifier
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Hash of *both* halves of the identity. Including the profile dir means two
/// identifiers pointed at one directory still get separate claims, and
/// including the identifier means two profiles for one build never collide.
fn claim_digest(identity: &ProfileIdentity) -> String {
    let mut hasher = Sha256::new();
    hasher.update(identity.identifier.as_bytes());
    // NUL separator: without it "ab" + "c" and "a" + "bc" would hash alike.
    hasher.update([0u8]);
    hasher.update(identity.profile_dir.to_string_lossy().as_bytes());
    let full = hex::encode(hasher.finalize());
    full[..CLAIM_DIGEST_LEN].to_string()
}

// ---------------------------------------------------------------------------
// Errors and outcomes
// ---------------------------------------------------------------------------

/// Why a profile could not be claimed. Every variant is fatal: Buildmesh
/// refuses to start rather than run as an undeclared second owner.
#[derive(Debug, thiserror::Error)]
pub enum OwnershipError {
    #[error("could not resolve the {identifier} app-data profile at {profile_dir}: {source}")]
    ProfileUnresolved {
        identifier: String,
        profile_dir: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not claim the {identifier} app-data profile: {source}")]
    ClaimFailed {
        identifier: String,
        #[source]
        source: std::io::Error,
    },
}

/// What happened to a second launch. The two failure modes are kept apart
/// because they mean different things to whoever reads the breadcrumb: a
/// missing window is our lookup being wrong, a refused foreground is Windows
/// policy doing exactly what it is supposed to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ForwardReport {
    /// The owner's main window was found.
    pub window_found: bool,
    /// That window was restored and accepted as the foreground window.
    pub focused: bool,
    /// The owner process the lookup targeted, when one was published. Kept for
    /// the breadcrumb so a failure says *which* process it tried.
    pub owner_pid: Option<u32>,
}

/// Outcome of a startup that was gated on profile ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Startup {
    /// This process owns the profile; the startup continuation has run.
    Owned,
    /// A live process already owns it. The continuation did **not** run and
    /// the caller must exit without touching the profile.
    Forwarded(ForwardReport),
}

/// Failure of a gated startup: either ownership could not be established
/// (fatal, and must be surfaced to the user) or the startup continuation
/// itself failed (whatever it would have failed with ungated).
#[derive(Debug, thiserror::Error)]
pub enum StartupError<E> {
    #[error(transparent)]
    Ownership(#[from] OwnershipError),
    #[error("startup failed: {0}")]
    Body(E),
}

// ---------------------------------------------------------------------------
// The claim
// ---------------------------------------------------------------------------

/// Proof that this process owns one profile, held for as long as the value
/// lives. Dropping it releases the profile; the gate below parks it for the
/// process lifetime so nothing can drop it by accident.
pub struct InstanceGuard {
    /// Kept for diagnostics: which OS object a claim is held on is the first
    /// thing worth knowing when diagnosing a profile that will not open.
    target: String,
    _platform: platform::PlatformGuard,
}

// A Win32 `HANDLE` is a raw pointer, so it is neither `Send` nor `Sync` by
// default. The handle is only ever passed to `CloseHandle` from `Drop`, which
// is thread-agnostic — no other state is shared behind the guard.
#[cfg(target_os = "windows")]
unsafe impl Send for InstanceGuard {}
#[cfg(target_os = "windows")]
unsafe impl Sync for InstanceGuard {}

impl std::fmt::Debug for InstanceGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstanceGuard")
            .field("target", &self.target)
            .finish()
    }
}

/// Result of taking the claim.
#[derive(Debug)]
enum Claim {
    Primary(InstanceGuard),
    Secondary,
}

/// Take the claim for `identity`, or report that a live process already holds
/// it. Split out from [`with_profile_ownership`] so tests can hold and release
/// a claim explicitly (a process boundary is the only thing that releases one
/// in production).
fn claim_profile(identity: &ProfileIdentity) -> Result<Claim, OwnershipError> {
    let target = identity.claim_target();
    match platform::acquire(&target) {
        Ok(platform::Acquired::Primary(guard)) => Ok(Claim::Primary(InstanceGuard {
            target,
            _platform: guard,
        })),
        Ok(platform::Acquired::Secondary) => Ok(Claim::Secondary),
        Err(source) => Err(OwnershipError::ClaimFailed {
            identifier: identity.identifier().to_string(),
            source,
        }),
    }
}

/// Claims held for this process's lifetime. `OnceLock<Vec<..>>` rather than a
/// single `OnceLock<InstanceGuard>` because tests claim several distinct
/// profiles in one process; a single slot would drop the losers' guards and
/// hand their profiles straight back.
static HELD_CLAIMS: OnceLock<Mutex<Vec<InstanceGuard>>> = OnceLock::new();

fn held_claims() -> &'static Mutex<Vec<InstanceGuard>> {
    HELD_CLAIMS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Take the claim, or wait briefly for a holder that is on its way out.
///
/// A second launch that meets a live owner is forwarded and exits. A *relaunch*
/// meets a holder that is already exiting, and mistaking that for a live owner
/// would hand the activation to a dying process and leave the user with no app
/// at all — so the loss of a race is retried for a moment before it is treated
/// as a genuine second launch.
fn claim_or_wait_for_exit(identity: &ProfileIdentity) -> Result<Claim, OwnershipError> {
    let mut claim = claim_profile(identity)?;
    for _ in 0..RECLAIM_ATTEMPTS {
        if !matches!(claim, Claim::Secondary) {
            break;
        }
        std::thread::sleep(RECLAIM_RETRY);
        claim = claim_profile(identity)?;
    }
    Ok(claim)
}

/// Claim `identity`, then run `startup` only if this process won it.
///
/// This is the startup gate. Everything that touches the profile — opening the
/// database, starting pool / Autopilot / Circuit / ledger / diagnostics
/// workers — must live inside `startup`, because for a losing process that
/// closure is never called and the caller is expected to exit.
///
/// `on_forward` runs instead, on the losing path only, with what happened to
/// the owner's window.
pub fn with_profile_ownership<E>(
    identity: &ProfileIdentity,
    on_forward: impl FnOnce(&ForwardReport),
    startup: impl FnOnce() -> Result<(), E>,
) -> Result<Startup, StartupError<E>> {
    match claim_or_wait_for_exit(identity)? {
        Claim::Secondary => {
            let report = forward_activation(identity.profile_dir());
            on_forward(&report);
            Ok(Startup::Forwarded(report))
        }
        Claim::Primary(guard) => {
            // Publish before anything else can fail: a second launch that
            // arrives during our own startup still has to be able to find us.
            // Best-effort — losing the breadcrumb costs a quiet exit, not
            // ownership, which is already secured.
            if let Err(e) = publish_owner_pid(identity.profile_dir()) {
                eprintln!("could not publish the profile owner's pid: {e}");
            }
            // Park before running: a panic inside `startup` unwinds out of
            // `run()`, and a dropped guard would free the profile for a
            // half-initialised app.
            held_claims().lock().push(guard);
            startup().map_err(StartupError::Body)?;
            Ok(Startup::Owned)
        }
    }
}

// ---------------------------------------------------------------------------
// Bringing the owner's window forward
// ---------------------------------------------------------------------------

/// Publish this process id so a later second launch of this profile can find
/// this process's window and bring it forward. Written by the gate the moment
/// the claim is won, which is why it needs neither a window nor a Tauri handle.
fn publish_owner_pid(profile_dir: &Path) -> std::io::Result<()> {
    std::fs::write(
        profile_dir.join(OWNER_PID_FILE),
        std::process::id().to_string(),
    )
}

/// Ask the profile owner to come forward. Bounded, and never fatal: a failure
/// to focus only means the second launch exits quietly.
fn forward_activation(profile_dir: &Path) -> ForwardReport {
    platform::forward_activation(profile_dir, OWNER_WINDOW_ATTEMPTS, OWNER_WINDOW_RETRY)
}

/// Parse a published owner pid. Pid 0 is never a real process, so neither "0"
/// nor a half-written value left by a crash mid-publish is treated as one.
fn parse_owner_pid(text: &str) -> Option<u32> {
    match text.trim().parse::<u32>() {
        Ok(pid) if pid > 0 => Some(pid),
        _ => None,
    }
}

/// Whether a window title belongs to Buildmesh's main window. Kept separate
/// from the Win32 calls so the rule is testable off Windows.
fn title_is_main_window(title: &str) -> bool {
    title.starts_with(MAIN_WINDOW_TITLE_PREFIX)
}

/// Log that a launch was forwarded to a live owner, in the profile's
/// `logs/profile-ownership.log`.
///
/// A forwarded launch is deliberately invisible — no window, no dialog, no
/// console in a release build — so this breadcrumb is the only way a user can
/// tell "my click did nothing" apart from "the app never started".
pub fn record_forwarded(
    profile_dir: &Path,
    identifier: &str,
    report: &ForwardReport,
) -> std::io::Result<()> {
    append_ownership_line(
        profile_dir,
        &format!(
            "forwarded to the running instance \
             (identifier={identifier} pid={} window_found={} focused={} owner_pid={:?})",
            std::process::id(),
            report.window_found,
            report.focused,
            report.owner_pid
        ),
    )
}

/// Log that ownership could not be established at all. Startup is fatal in
/// that case, and it happens before the tracing subscriber exists, so this is
/// the only durable record of why.
pub fn record_claim_failure(
    profile_dir: &Path,
    identifier: &str,
    error: &OwnershipError,
) -> std::io::Result<()> {
    append_ownership_line(
        profile_dir,
        &format!(
            "could not claim the profile (identifier={identifier} pid={} error={error})",
            std::process::id()
        ),
    )
}

/// Bounded by the same rotating writer as the diagnostics log, because the one
/// way to write many lines here is a user repeatedly double-clicking the
/// shortcut — and because a startup failure that silently filled the disk
/// would be a worse failure than the one it replaced.
fn append_ownership_line(profile_dir: &Path, line: &str) -> std::io::Result<()> {
    let log_dir = profile_dir.join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let mut writer = crate::diagnostics::RotatingWriter::with_limits(
        log_dir.join(PROFILE_OWNERSHIP_LOG),
        PROFILE_OWNERSHIP_LOG_MAX_BYTES,
        PROFILE_OWNERSHIP_LOG_KEEP,
    )?;
    // `write_line` adds the RFC3339 stamp, so the file is readable on its own.
    writer.write_line(line)
}

/// Show a modal error to the user when ownership could not be established.
///
/// A Win32 message box rather than the dialog plugin: this runs inside Tauri's
/// `setup`, on the main thread, where the plugin's `blocking_show` deadlocks.
/// `panic = "abort"` builds have no console either, so a message box is the only
/// thing left that can actually reach the user before the process exits.
#[cfg(target_os = "windows")]
pub fn show_fatal_startup_error(reason: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    let text = wide(reason);
    let caption = wide("Buildmesh");
    // SAFETY: both strings are NUL-terminated by `wide`, and a null owner HWND
    // asks for an application-modal box rather than one tied to a window that
    // has not finished loading.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// Off Windows there is no message box to raise from a `windows_subsystem`
/// build, so the reason goes to stderr (and to the profile's ownership log
/// written by the caller). A dev/console launch shows it there.
#[cfg(not(target_os = "windows"))]
pub fn show_fatal_startup_error(reason: &str) {
    eprintln!("{reason}");
}

// ---------------------------------------------------------------------------
// Platform layer
// ---------------------------------------------------------------------------
#[cfg(target_os = "windows")]
mod platform {
    use super::ForwardReport;
    use std::io;
    use std::path::Path;
    use std::time::Duration;
    use windows_sys::core::BOOL;
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, GetLastError, SetLastError, ERROR_ALREADY_EXISTS, FALSE, HANDLE, HWND,
            LPARAM, TRUE,
        },
        System::Threading::{AttachThreadInput, CreateMutexW, GetCurrentThreadId},
        UI::WindowsAndMessaging::{
            EnumWindows, GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW,
            GetWindowThreadProcessId, IsWindowVisible, SetForegroundWindow, ShowWindow, SW_RESTORE,
        },
    };

    pub enum Acquired {
        Primary(PlatformGuard),
        Secondary,
    }

    /// An open handle to the claim object — a kernel handle, not a window
    /// handle. The object exists exactly as long as some process holds it open,
    /// so an open handle *is* the claim: there is no separate "release" step
    /// to get wrong.
    pub struct PlatformGuard(HANDLE);

    impl Drop for PlatformGuard {
        fn drop(&mut self) {
            // SAFETY: `CloseHandle` accepts any live handle, on any thread; a
            // null handle is never constructed because `acquire` checks it
            // before building the guard.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    pub fn acquire(name: &str) -> io::Result<Acquired> {
        let wide = super::wide(name);
        // `SetLastError(0)` first: `CreateMutexW` reports "it already existed"
        // through the thread's last-error value, and a stale 183 left over from
        // an unrelated call would otherwise read as a lost race.
        // SAFETY: null SECURITY_ATTRIBUTES (we want default, non-inheritable
        // security), `bInitialOwner = FALSE` (the object's existence is the
        // claim; we never take or release mutex ownership), and a NUL-
        // terminated name we just built.
        unsafe {
            SetLastError(0);
            let handle = CreateMutexW(std::ptr::null(), 0, wide.as_ptr());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            // Must be the very next call: anything else may clobber it.
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(handle);
                return Ok(Acquired::Secondary);
            }
            Ok(Acquired::Primary(PlatformGuard(handle)))
        }
    }

    /// Find the owner's main window and bring it forward.
    ///
    /// Retries while no window is found, not just while the pid file is
    /// missing: the owner publishes its pid the moment it wins the claim, but
    /// its window only appears once the event loop starts, so a second launch
    /// can easily arrive inside that gap. Once a window is found the answer is
    /// final — a later one cannot be better, and a crashed owner will never
    /// produce one, so the bound is what stops this from spinning.
    pub fn forward_activation(
        profile_dir: &Path,
        attempts: u32,
        retry: Duration,
    ) -> ForwardReport {
        let path = profile_dir.join(super::OWNER_PID_FILE);
        let mut owner_pid = None;
        for attempt in 0..attempts.max(1) {
            // A missing file is the "owner hasn't published yet" case and is
            // worth another look; any other read error is not.
            let pid = match std::fs::read_to_string(&path) {
                Ok(text) => match super::parse_owner_pid(&text) {
                    Some(pid) => pid,
                    // A half-written file is the owner mid-publish.
                    None if attempt + 1 < attempts => {
                        std::thread::sleep(retry);
                        continue;
                    }
                    None => return ForwardReport::default(),
                },
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    if attempt + 1 < attempts {
                        std::thread::sleep(retry);
                        continue;
                    }
                    return ForwardReport::default();
                }
                Err(_) => return ForwardReport::default(),
            };
            owner_pid = Some(pid);

            match owner_window(pid) {
                Some(hwnd) => {
                    return ForwardReport {
                        window_found: true,
                        focused: bring_forward(hwnd),
                        owner_pid: Some(pid),
                    }
                }
                None if attempt + 1 < attempts => std::thread::sleep(retry),
                None => break,
            }
        }
        ForwardReport {
            window_found: false,
            focused: false,
            owner_pid,
        }
    }

    /// The owner's visible main window, if it has one yet.
    fn owner_window(pid: u32) -> Option<HWND> {
        // `EnumWindows` hands each candidate to a callback; the collected
        // handles travel back through LPARAM as a pointer to this stack Vec,
        // which outlives the call because `EnumWindows` is synchronous.
        let mut found: Vec<HWND> = Vec::new();
        // SAFETY: `collect_owner_window` only appends to the `Vec` behind the
        // pointer, and `EnumWindows` returns before this frame ends, so the
        // borrow and the pointer both stay valid for the whole enumeration.
        unsafe {
            let ok = EnumWindows(
                Some(collect_owner_window),
                (&mut found as *mut Vec<HWND>) as LPARAM,
            );
            if ok == 0 {
                return None;
            }
        }
        found.into_iter().find(|hwnd| belongs_to(*hwnd, pid) && is_main_window(*hwnd))
    }

    unsafe extern "system" fn collect_owner_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the pointer `owner_window` passed in, valid for
        // the duration of this synchronous enumeration, and every window
        // `EnumWindows` offers is a live top-level window.
        let found = unsafe { &mut *(lparam as *mut Vec<HWND>) };
        found.push(hwnd);
        TRUE
    }

    fn belongs_to(hwnd: HWND, pid: u32) -> bool {
        let mut owner: u32 = 0;
        // SAFETY: `hwnd` came from `EnumWindows` in this process's lifetime
        // and is passed straight back to the API that produced it.
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut owner);
        }
        owner == pid
    }

    fn is_main_window(hwnd: HWND) -> bool {
        // SAFETY: as above — a live top-level window handle.
        unsafe {
            if IsWindowVisible(hwnd) == 0 {
                return false;
            }
            window_title(hwnd)
                .as_deref()
                .is_some_and(super::title_is_main_window)
        }
    }

    fn window_title(hwnd: HWND) -> Option<String> {
        // SAFETY: as above. The length probe and the read are both bounded by
        // a buffer we own, and a title that grows between the two is truncated
        // by `GetWindowTextW` rather than overrunning anything.
        unsafe {
            let len = GetWindowTextLengthW(hwnd);
            if len <= 0 {
                return None;
            }
            let mut buf = vec![0u16; len as usize + 1];
            let copied = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
            if copied <= 0 {
                return None;
            }
            buf.truncate(copied as usize);
            Some(String::from_utf16_lossy(&buf))
        }
    }

    /// Restore and foreground the owner's window. Returns whether Win32
    /// accepted the foreground request.
    fn bring_forward(hwnd: HWND) -> bool {
        // SAFETY: `hwnd` was just validated as the owner's live, visible main
        // window by `owner_window`, and this runs synchronously before anything
        // else can destroy it.
        unsafe {
            // A minimized window must be restored first, or the foreground
            // call "succeeds" and the user is still looking at their desktop.
            ShowWindow(hwnd, SW_RESTORE);

            // Windows grants the right to set the foreground to a process that
            // currently owns it, or that the foreground process started — and
            // refuses everyone else. A second Buildmesh launched from a shell,
            // a shortcut, or Task Scheduler is exactly "everyone else", so the
            // plain call would routinely report failure. Borrowing the
            // foreground window's input queue lifts that; the attach is undone
            // before returning, and only taken when it succeeded.
            let mut foreground_thread = 0;
            let foreground = GetForegroundWindow();
            if !foreground.is_null() {
                foreground_thread =
                    GetWindowThreadProcessId(foreground, std::ptr::null_mut());
            }
            let our_thread = GetCurrentThreadId();
            let attached = foreground_thread != 0
                && foreground_thread != our_thread
                && AttachThreadInput(our_thread, foreground_thread, TRUE) != 0;

            let foregrounded = SetForegroundWindow(hwnd) != 0;

            if attached {
                AttachThreadInput(our_thread, foreground_thread, FALSE);
            }
            foregrounded
        }
    }
}

/// NUL-terminated UTF-16 for the `*W` Win32 entry points.
#[cfg(target_os = "windows")]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::ForwardReport;
    use std::io;
    use std::os::fd::AsRawFd;
    use std::path::Path;
    use std::time::Duration;

    /// `LOCK_EX | LOCK_NB` from `<sys/file.h>`. The values are 2 and 4 on both
    /// Linux and macOS, and declaring them here keeps `libc` out of the dep
    /// tree for two constants.
    const LOCK_EXCLUSIVE_NONBLOCKING: i32 = 2 | 4;

    extern "C" {
        /// `flock(2)`. The kernel releases the lock when the last descriptor
        /// for the file closes — including on process death — so a crashed
        /// owner cannot strand the profile.
        fn flock(fd: i32, operation: i32) -> i32;
    }

    pub enum Acquired {
        Primary(PlatformGuard),
        Secondary,
    }

    pub struct PlatformGuard(std::fs::File);

    pub fn acquire(path: &str) -> io::Result<Acquired> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        // SAFETY: `flock` only inspects and records the open file descriptor,
        // which `file` keeps alive for the duration of the call.
        let locked = unsafe { flock(file.as_raw_fd(), LOCK_EXCLUSIVE_NONBLOCKING) };
        if locked == 0 {
            return Ok(Acquired::Primary(PlatformGuard(file)));
        }
        let error = io::Error::last_os_error();
        // EWOULDBLOCK/EAGAIN is "somebody else holds it", which is a normal
        // outcome here rather than a failure. std maps both to `WouldBlock`.
        if error.kind() == io::ErrorKind::WouldBlock {
            Ok(Acquired::Secondary)
        } else {
            Err(error)
        }
    }

    /// No window enumeration off Windows; see the module docs. The pid is
    /// still published, so a future platform implementation only has to add
    /// the lookup.
    pub fn forward_activation(
        _profile_dir: &Path,
        _attempts: u32,
        _retry: Duration,
    ) -> ForwardReport {
        ForwardReport::default()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A unique profile dir per call. The claim is OS-global, so a leftover
    /// directory from an earlier run — or a second test running in parallel —
    /// must not be able to collide.
    fn temp_profile() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp profile dir")
    }

    fn identity(identifier: &str, dir: &Path) -> ProfileIdentity {
        ProfileIdentity::new(identifier, dir).expect("resolve profile identity")
    }

    /// The headline in-process guarantee: the second caller is forwarded and
    /// its startup continuation never runs. Cross-process proof is the
    /// child-process test at the bottom of this module.
    #[test]
    fn a_second_claim_on_one_profile_forwards_and_skips_startup() {
        let dir = temp_profile();
        let identity = identity(STABLE_IDENTIFIER, dir.path());
        let body_runs = AtomicUsize::new(0);
        let mut forwarded: Option<ForwardReport> = None;

        let first = with_profile_ownership(
            &identity,
            |r| forwarded = Some(*r),
            || -> Result<(), std::io::Error> {
                body_runs.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .expect("first claim");
        assert!(matches!(first, Startup::Owned), "first launch must own");
        assert_eq!(body_runs.load(Ordering::SeqCst), 1);
        assert_eq!(forwarded, None, "the owner forwards nothing");

        let second = with_profile_ownership(
            &identity,
            |r| forwarded = Some(*r),
            || -> Result<(), std::io::Error> {
                body_runs.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .expect("second claim");
        assert!(
            matches!(second, Startup::Forwarded(_)),
            "a second claim on a live profile must forward, got {second:?}"
        );
        assert_eq!(
            body_runs.load(Ordering::SeqCst),
            1,
            "the forwarded process must not run the startup continuation"
        );
        assert!(
            forwarded.is_some(),
            "on_forward must run on the losing path so the caller can log it"
        );
    }

    /// The stable hub and the dev profile are different profiles and must both
    /// be able to own theirs, or the dev build would refuse to start beside a
    /// stable install (issue #1521: "stable and dev identifiers still run
    /// simultaneously").
    ///
    /// Both claims are held in named bindings on purpose: a claim written as a
    /// temporary inside an `assert!` is dropped at the end of that statement,
    /// which would release the lock and make the test pass even if the two
    /// profiles collided.
    #[test]
    fn distinct_profiles_can_both_be_owned() {
        let stable_dir = temp_profile();
        let dev_dir = temp_profile();
        let stable = identity(STABLE_IDENTIFIER, stable_dir.path());
        let dev = identity("com.alond.buildmesh.dev", dev_dir.path());

        let stable_claim = claim_profile(&stable).expect("stable claim");
        let dev_claim = claim_profile(&dev).expect("dev claim");
        assert!(matches!(stable_claim, Claim::Primary(_)));
        assert!(
            matches!(dev_claim, Claim::Primary(_)),
            "the .dev profile must be claimable alongside the stable hub"
        );
        assert_ne!(
            stable.claim_target(),
            dev.claim_target(),
            "the two profiles must not share a claim target"
        );
    }

    /// The profile directory is half the key, so one identifier pointed at two
    /// directories is two claims — the case a bare identifier-keyed lock would
    /// get wrong.
    #[test]
    fn one_identifier_over_two_profiles_is_two_claims() {
        let first_dir = temp_profile();
        let second_dir = temp_profile();
        let first = identity(STABLE_IDENTIFIER, first_dir.path());
        let second = identity(STABLE_IDENTIFIER, second_dir.path());

        let first_claim = claim_profile(&first).expect("first claim");
        let second_claim = claim_profile(&second).expect("second claim");
        assert!(matches!(first_claim, Claim::Primary(_)));
        assert!(matches!(second_claim, Claim::Primary(_)));
        assert_ne!(first.claim_target(), second.claim_target());
    }

    /// The same profile resolves to the same claim however it is spelled, so a
    /// caller using a different-but-equivalent path cannot slip past the lock.
    #[test]
    fn the_same_profile_spelled_two_ways_is_one_claim() {
        let dir = temp_profile();
        let canonical = identity(STABLE_IDENTIFIER, dir.path());
        // A `.` component is the same directory spelled differently.
        let roundabout = identity(STABLE_IDENTIFIER, &dir.path().join("."));

        assert_eq!(canonical.claim_target(), roundabout.claim_target());

        let held = claim_profile(&canonical).expect("canonical claim");
        assert!(matches!(held, Claim::Primary(_)));
        assert!(
            matches!(claim_profile(&roundabout).expect("roundabout claim"), Claim::Secondary),
            "an equivalent path must land on the same claim"
        );
    }

    /// Releasing the claim hands the profile back. In production only process
    /// exit does this, which the child-process test covers end to end; this is
    /// the primitive underneath it.
    #[test]
    fn dropping_the_claim_releases_the_profile() {
        let dir = temp_profile();
        let identity = identity(STABLE_IDENTIFIER, dir.path());

        let claim = claim_profile(&identity).expect("first claim");
        assert!(matches!(claim, Claim::Primary(_)));
        drop(claim);

        assert!(
            matches!(claim_profile(&identity).expect("re-claim"), Claim::Primary(_)),
            "a released profile must be claimable again"
        );
    }

    /// An unresolvable profile is fatal, not a free pass. Two ways to spell
    /// "the same directory" that disagree would otherwise mean two owners.
    #[test]
    fn an_unresolvable_profile_is_an_error() {
        let dir = temp_profile();
        let missing = dir.path().join("no-such-profile-dir");
        let error = ProfileIdentity::new(STABLE_IDENTIFIER, &missing)
            .expect_err("a missing profile dir must not resolve");
        assert!(
            matches!(error, OwnershipError::ProfileUnresolved { .. }),
            "got {error:?}"
        );
        assert!(error.to_string().contains(STABLE_IDENTIFIER));
    }

    /// The claim name must survive an identifier that is not a valid Win32
    /// object name, and must stay inside the namespace prefix.
    #[test]
    fn claim_targets_stay_inside_the_object_name_syntax() {
        assert_eq!(sanitize_object_name("com.alond.buildmesh"), "com.alond.buildmesh");
        assert_eq!(sanitize_object_name("com.acme/buildmesh"), "com.acme_buildmesh");
        assert_eq!(sanitize_object_name("a\\b"), "a_b");
        assert_eq!(sanitize_object_name(""), "");

        let dir = temp_profile();
        let target = identity("com.acme/buildmesh", dir.path()).claim_target();
        assert_eq!(target.matches('\\').count(), 1, "only the namespace prefix");
        // `Global\`, not `Local\`: a session-scoped object would let a second
        // Windows session open the same profile (issue #1521).
        assert!(target.starts_with("Global\\"), "{target}");
        assert!(target.len() < 200, "object name must fit MAX_PATH: {target}");
    }

    /// Different identifiers, and different profiles, must never share a claim
    /// target — that is the whole collision test for the digest.
    #[test]
    fn claim_targets_separate_both_halves_of_the_identity() {
        let dir = temp_profile();
        let stable = identity(STABLE_IDENTIFIER, dir.path());
        let dev = identity("com.alond.buildmesh.dev", dir.path());
        let other_dir = temp_profile();
        let stable_elsewhere = identity(STABLE_IDENTIFIER, other_dir.path());

        assert_ne!(stable.claim_target(), dev.claim_target(), "identifier");
        assert_ne!(
            stable.claim_target(),
            stable_elsewhere.claim_target(),
            "profile dir"
        );
        // Two independent constructions of the same identity must agree, or a
        // relaunch would never recognise the profile it already owns.
        assert_eq!(stable.claim_target(), identity(STABLE_IDENTIFIER, dir.path()).claim_target());
    }

    /// A relaunch must be able to take over from the process it replaces.
    /// Tauri spawns the successor from inside the outgoing process's `Exit`
    /// event, so the successor really can arrive while the old claim is still
    /// held — and a successor that gave up would leave the user with no app
    /// after an update. Claiming alone is what's exercised here; the gate wraps
    /// it and adds the (separately bounded) activation lookup on the losing
    /// path, which would otherwise dominate this test's wall clock.
    #[test]
    fn a_losing_claim_waits_for_a_holder_that_is_exiting() {
        let dir = temp_profile();
        let identity = identity(STABLE_IDENTIFIER, dir.path());

        // Hold the profile the way the outgoing process would, then release it
        // a moment later — a holder on its way out, not a second launch.
        let holder = std::thread::spawn({
            let identity = identity.clone();
            move || {
                let guard = claim_profile(&identity).expect("holder claim");
                std::thread::sleep(Duration::from_millis(120));
                drop(guard);
            }
        });

        let claim = claim_or_wait_for_exit(&identity).expect("successor claim");
        holder.join().expect("holder thread");

        assert!(
            matches!(claim, Claim::Primary(_)),
            "a successor must take over from an exiting holder, got {claim:?}"
        );
    }

    /// The counterpart: a holder that is *not* exiting is not waited on beyond
    /// the bounded window, so a second launch is forwarded promptly.
    #[test]
    fn a_live_holder_is_not_waited_on_forever() {
        let dir = temp_profile();
        let identity = identity(STABLE_IDENTIFIER, dir.path());
        let held = claim_profile(&identity).expect("first claim");
        assert!(matches!(held, Claim::Primary(_)));

        let started = std::time::Instant::now();
        let claim = claim_or_wait_for_exit(&identity).expect("second claim");
        let waited = started.elapsed();

        assert!(
            matches!(claim, Claim::Secondary),
            "a live owner must not be displaced, got {claim:?}"
        );
        // Generous margin over the nominal 4 x 100 ms, because this asserts a
        // bound rather than a duration: the point is "does not spin", not
        // "finishes in exactly 400 ms".
        assert!(
            waited < RECLAIM_ATTEMPTS * RECLAIM_RETRY * 3,
            "a live holder must be given up on promptly, waited {waited:?}"
        );
    }

    #[test]
    fn owner_pids_are_parsed_not_guessed() {
        assert_eq!(parse_owner_pid("517204\n"), Some(517_204));
        assert_eq!(parse_owner_pid("  517204  \n"), Some(517_204));
        assert_eq!(parse_owner_pid("0"), None);
        assert_eq!(parse_owner_pid("-1"), None);
        assert_eq!(parse_owner_pid(""), None);
        assert_eq!(parse_owner_pid("   \n"), None);
        assert_eq!(parse_owner_pid("not-a-pid"), None);
        assert_eq!(parse_owner_pid("517204 517206"), None);
        // A half-written publish is not a pid to act on.
        assert_eq!(parse_owner_pid("517"), Some(517));
        assert_eq!(parse_owner_pid("517204."), None);
    }

    /// The gate publishes this process's pid when it wins the claim, so a
    /// second launch has something to look up before the window exists.
    #[test]
    fn winning_the_claim_publishes_this_process() {
        let dir = temp_profile();
        let identity = identity(STABLE_IDENTIFIER, dir.path());
        with_profile_ownership(&identity, |_| {}, || Ok::<_, std::io::Error>(()))
            .expect("claim");

        let published = std::fs::read_to_string(dir.path().join(OWNER_PID_FILE))
            .expect("owner pid published");
        assert_eq!(parse_owner_pid(&published), Some(std::process::id()));
    }

    /// A pid left behind by a crashed owner must not produce a focus: the
    /// process is gone, so no window of ours can match it. Deterministic
    /// because `u32::MAX` is not a live process on any sane machine.
    #[test]
    fn a_stale_owner_pid_is_reported_not_focused() {
        let dir = temp_profile();
        std::fs::write(dir.path().join(OWNER_PID_FILE), u32::MAX.to_string()).unwrap();

        let report = platform::forward_activation(dir.path(), 1, Duration::from_millis(0));
        assert!(
            !report.window_found,
            "a pid with no window must not report a window"
        );
        assert!(!report.focused);
        assert_eq!(report.owner_pid, Some(u32::MAX));
    }

    /// The pid round-trips: a second launch reads exactly what the owner wrote.
    #[test]
    fn a_published_owner_pid_is_read_back() {
        let dir = temp_profile();
        publish_owner_pid(dir.path()).expect("publish pid");

        let report = platform::forward_activation(dir.path(), 1, Duration::from_millis(0));
        // This test process has no Buildmesh window, so there is nothing to
        // focus — but the lookup must have targeted the right pid.
        assert!(!report.window_found, "no live main window in this test process");
        assert!(!report.focused);
        assert_eq!(report.owner_pid, Some(std::process::id()));
    }

    /// A missing pid file must return "did not focus" rather than blocking
    /// startup forever — the owner may be a crashed process.
    #[test]
    fn forwarding_with_no_published_owner_gives_up() {
        let dir = temp_profile();
        let report = platform::forward_activation(dir.path(), 2, Duration::from_millis(0));
        assert!(!report.window_found);
        assert!(!report.focused);
        assert_eq!(report.owner_pid, None);
    }

    /// The window lookup only accepts a window carrying Buildmesh's own title,
    /// so a pid cannot make us foreground one of the owner's unrelated windows
    /// (a splash screen, a devtools popup).
    #[test]
    fn only_a_titled_window_counts_as_the_main_window() {
        assert!(title_is_main_window("Buildmesh - abc1234"));
        assert!(title_is_main_window("Buildmesh - abc1234-dirty"));
        // A second Buildmesh of a *different* profile is a different pid, so
        // it is never a candidate; the prefix only has to reject everything
        // that is not our main window.
        assert!(!title_is_main_window(""));
        assert!(!title_is_main_window("Program Manager"));
        assert!(!title_is_main_window("buildmesh - abc1234"));
        assert!(!title_is_main_window("Agent - buildmesh - abc"));
    }

    /// A forwarded launch is invisible, so the breadcrumb is the only record
    /// that it happened — and it has to name the profile and the outcome.
    #[test]
    fn a_forwarded_launch_leaves_a_breadcrumb() {
        let dir = temp_profile();
        record_forwarded(
            dir.path(),
            "com.alond.buildmesh.dev",
            &ForwardReport {
                window_found: true,
                focused: true,
                owner_pid: Some(517_204),
            },
        )
        .expect("record forwarded launch");

        let log = ownership_log(dir.path());
        assert_eq!(log.lines().count(), 1, "{log}");
        let line = log.lines().next().unwrap();
        assert!(line.contains("identifier=com.alond.buildmesh.dev"), "{line}");
        assert!(line.contains(&format!("pid={}", std::process::id())), "{line}");
        assert!(line.contains("window_found=true"), "{line}");
        assert!(line.contains("focused=true"), "{line}");
        assert!(line.contains("owner_pid=Some(517204)"), "{line}");
        // RotatingWriter stamps the line itself, so it is self-describing.
        assert!(line.starts_with(char::is_numeric), "{line}");
    }

    /// A fatal claim failure happens before the tracing subscriber exists, so
    /// the same log is the only place the reason can be read back from.
    #[test]
    fn a_claim_failure_leaves_a_breadcrumb() {
        let dir = temp_profile();
        let identity = ProfileIdentity::new(STABLE_IDENTIFIER, &dir.path().join("missing"))
            .expect_err("missing profile dir does not resolve");
        record_claim_failure(dir.path(), STABLE_IDENTIFIER, &identity).expect("record failure");

        let log = ownership_log(dir.path());
        assert!(log.contains("could not claim the profile"), "{log}");
        assert!(log.contains(STABLE_IDENTIFIER), "{log}");
        assert!(log.contains("could not resolve"), "{log}");
    }

    fn ownership_log(profile_dir: &Path) -> String {
        std::fs::read_to_string(profile_dir.join("logs").join(PROFILE_OWNERSHIP_LOG))
            .expect("ownership log")
    }

    // -- child process -----------------------------------------------------
    //
    // The in-process tests above share one OS process with their subject, so
    // they cannot catch the actual bug: a *second OS process* reaching
    // `db::init` and rewriting the owner's rows. This pair re-executes the
    // test binary as a real second process.

    const CHILD_PROFILE_ENV: &str = "BUILDMESH_TEST_INSTANCE_PROFILE";
    const CHILD_NAME_ENV: &str = "BUILDMESH_TEST_INSTANCE_NAME";
    const CHILD_HOLD_ENV: &str = "BUILDMESH_TEST_INSTANCE_HOLD";

    /// The startup continuation the child runs: open the profile database,
    /// exactly as `run_profile_startup` does, and journal that it got there.
    fn child_startup(profile: &Path) -> Result<(), String> {
        crate::db::init(&profile.join("buildmesh.db")).map_err(|e| format!("db::init: {e}"))?;
        let journal = profile.join("reached-database-init.log");
        let name = std::env::var(CHILD_NAME_ENV).unwrap_or_default();
        let mut existing = std::fs::read_to_string(&journal).unwrap_or_default();
        existing.push_str(&format!("{name}\n"));
        std::fs::write(&journal, existing).map_err(|e| format!("journal: {e}"))
    }

    /// Child half. Selected explicitly by the parent via
    /// `--ignored --exact instance_guard::tests::child_process_claim`, so it
    /// never runs as part of an ordinary `cargo test` pass.
    #[test]
    #[ignore = "spawned as a second OS process by loser_never_reaches_database_initialization"]
    fn child_process_claim() {
        let profile = PathBuf::from(
            std::env::var(CHILD_PROFILE_ENV).expect("child must be spawned with a profile"),
        );
        let name = std::env::var(CHILD_NAME_ENV).expect("child must be spawned with a name");
        let identity = identity(STABLE_IDENTIFIER, &profile);
        let mut report = ForwardReport::default();

        let outcome = with_profile_ownership(&identity, |r| report = *r, || {
            child_startup(&profile)
        })
        .expect("claim");

        let summary = match outcome {
            Startup::Owned => format!("owned:{name}"),
            Startup::Forwarded(r) => format!("forwarded:{name}:{}", r.focused),
        };
        std::fs::write(profile.join(format!("outcome-{name}.txt")), summary)
            .expect("write child outcome");

        // The holder stays alive until the parent releases it, so the loser
        // really is racing a live owner rather than a finished one.
        if std::env::var(CHILD_HOLD_ENV).is_ok() {
            let release = profile.join("release-holder");
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            while !release.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "parent never released the holder"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    fn spawn_child(profile: &Path, name: &str, hold: bool) -> std::process::Child {
        let mut command = std::process::Command::new(
            std::env::current_exe().expect("current test binary"),
        );
        command
            .arg("--ignored")
            .arg("--exact")
            .arg("instance_guard::tests::child_process_claim")
            .arg("--nocapture")
            .env(CHILD_PROFILE_ENV, profile)
            .env(CHILD_NAME_ENV, name)
            .stdin(std::process::Stdio::null());
        if hold {
            command.env(CHILD_HOLD_ENV, "1");
        }
        command.spawn().expect("spawn child process")
    }

    fn wait_for(path: &Path, what: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what} at {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn child_output(child: std::process::Child) -> (std::process::ExitStatus, String) {
        let output = child.wait_with_output().expect("wait for child");
        (
            output.status,
            String::from_utf8_lossy(&output.stderr).to_string(),
        )
    }

    /// The acceptance test for issue #1521: a second OS process against a live
    /// owner's profile is forwarded and exits without opening the database,
    /// and the profile becomes claimable again as soon as the owner exits.
    #[test]
    fn loser_never_reaches_database_initialization() {
        let dir = temp_profile();
        let profile = dir.path().to_path_buf();

        // Owner: claims the profile and opens the database, then holds.
        let holder = spawn_child(&profile, "holder", true);
        wait_for(&profile.join("outcome-holder.txt"), "the owner to claim");
        assert_eq!(
            std::fs::read_to_string(profile.join("outcome-holder.txt")).unwrap(),
            "owned:holder",
            "the first process must own the profile"
        );
        assert!(
            profile.join("buildmesh.db").exists(),
            "control: the owner's startup continuation really does open the database"
        );

        // Loser: a genuine second process, same profile, same identifier.
        let loser = spawn_child(&profile, "loser", false);
        let (status, stderr) = child_output(loser);
        assert!(status.success(), "the loser must exit cleanly, got {status}: {stderr}");
        assert_eq!(
            std::fs::read_to_string(profile.join("outcome-loser.txt")).unwrap(),
            "forwarded:loser:false",
            "the second process must be forwarded to the owner"
        );
        assert_eq!(
            std::fs::read_to_string(profile.join("reached-database-init.log")).unwrap(),
            "holder\n",
            "only the owner may reach database initialization"
        );

        // Release the owner; its exit must free the profile (the crash
        // watchdog relaunches the app, which would otherwise find a profile
        // it could never claim).
        std::fs::write(profile.join("release-holder"), "go").unwrap();
        let (status, stderr) = child_output(holder);
        assert!(status.success(), "the owner must exit cleanly, got {status}: {stderr}");

        let next = spawn_child(&profile, "next", false);
        let (status, stderr) = child_output(next);
        assert!(status.success(), "the third process must run, got {status}: {stderr}");
        assert_eq!(
            std::fs::read_to_string(profile.join("outcome-next.txt")).unwrap(),
            "owned:next",
            "a profile whose owner exited must be claimable again"
        );
        assert_eq!(
            std::fs::read_to_string(profile.join("reached-database-init.log")).unwrap(),
            "holder\nnext\n",
            "the reclaiming process is the only other one to reach the database"
        );
    }
}
