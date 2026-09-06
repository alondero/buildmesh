//! Always-on resource diagnostics.
//!
//! Why this exists
//! ---------------
//! Background node/mesh refresh (ADR 0020) plus a **per-node recursive file
//! watcher** (`commands::file_watcher`) are suspected of leaking memory,
//! handles, and git subprocesses as the open-node count grows — the app
//! grinds the whole machine down after a handful of agents are running, and
//! nothing previously recorded a resource *timeline*, so a post-lockup
//! investigation had no data to work from.
//!
//! This module closes that gap. A low-frequency sampler thread writes one
//! structured line every few seconds to a dedicated, size-bounded rolling log
//! (`logs/diagnostics.log`) the user can hand back after a bad session. Each
//! line pairs OS process vitals (working set, private bytes, handle count,
//! thread count, live direct-child-process count) with cheap in-process
//! counters bumped at existing emit sites (GIT_CHANGED fan-out, background
//! sync attempts/advances/failures, git-subprocess timeouts) and live gauges
//! (active file watchers, live PTY sessions).
//!
//! Design constraints:
//! * **No hot-path cost.** The counters are `Relaxed` atomic adds at sites
//!   that already do I/O; the sampler runs on its own thread. Nothing here
//!   touches the spawn / PTY / render path timing.
//! * **Can't fill the disk.** The writer self-rotates at a byte cap and keeps
//!   a bounded number of generations, so the diagnostics that exist to
//!   *diagnose* a disk-fill can never *cause* one.
//! * **Survives the lockup.** Every line is flushed to the OS immediately and
//!   `sync_all`'d periodically, so the timeline right up to a hard reboot is
//!   recoverable.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

mod watchdog;
mod writer;
pub(crate) use watchdog::{clear_expected_exit, mark_expected_exit, ExpectedExitReason};
#[cfg(not(target_os = "windows"))]
pub(crate) use watchdog::{relaunch_detached, AUTO_RELAUNCH_COOLDOWN_SECS};
pub use watchdog::{
    run_if_requested as run_crash_watchdog_if_requested, start as start_crash_watchdog,
};
pub(crate) use writer::RotatingWriter;

#[cfg(test)]
mod tests;

// ---- Main tracing-log writer -------------------------------------------

/// Byte cap / retention for the main `buildmesh.log`. Larger than the
/// diagnostics log because `debug`-level tracing is far chattier; 64 MiB × 4
/// generations bounds total on-disk size to 256 MiB.
const MAIN_LOG_MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAIN_LOG_KEEP: usize = 3;

/// Open the size-bounded, **fixed-name** `buildmesh.log` writer for the tracing
/// subscriber. Keeping the filename stable matters: the `/use`, `/verify`,
/// `/verify-ui` skills and `scripts/*log*.ps1` tail that exact path. Bounding
/// by bytes (not time) closes the unbounded-log disk-fill risk that a
/// `debug`-level build storm would otherwise reopen within a single hour.
pub(crate) fn main_log_writer(log_dir: &std::path::Path) -> std::io::Result<RotatingWriter> {
    RotatingWriter::with_limits(
        log_dir.join("buildmesh.log"),
        MAIN_LOG_MAX_BYTES,
        MAIN_LOG_KEEP,
    )
}

// ---- Per-subsystem counters --------------------------------------------
//
// Monotonic unless noted. Bumped at existing emit sites; read (and delta'd
// for rates) by the sampler. `Relaxed` is correct: we only need eventual
// visibility of an approximate count, never ordering against other memory.

/// GIT_CHANGED events emitted to the frontend by the file watcher. Every emit
/// fans out to diff/status/health/open-PR refetches app-wide, so a runaway
/// here is the "build-storm amplification" signature — a recursive watcher
/// over `target/` / `node_modules/` firing on every write of a build.
pub static GIT_CHANGED_EMITS: AtomicU64 = AtomicU64::new(0);

/// Background mesh-sync fetch attempts (`pool_worker`, ADR 0020).
pub static BG_SYNC_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
/// Background syncs that advanced the base ref (triggering a warm-pool reset).
pub static BG_SYNC_ADVANCED: AtomicU64 = AtomicU64::new(0);
/// Background syncs that failed (offline / auth / timeout). Self-healing, but
/// a climbing count while the machine grinds points at the network path.
pub static BG_SYNC_FAILURES: AtomicU64 = AtomicU64::new(0);
/// Wall-clock of the most recent background sync fetch, in milliseconds
/// (gauge, not monotonic — overwritten each sync).
pub static BG_SYNC_LAST_MS: AtomicU64 = AtomicU64::new(0);

/// git subprocess deadline hits (`process_util::run_command_with_timeout`). A
/// wedged `git fetch`/`pull` leaks a thread until killed; a climbing count
/// means the network path is stalling and the pool worker is thrashing.
pub static GIT_SUBPROCESS_TIMEOUTS: AtomicU64 = AtomicU64::new(0);

/// Record one GIT_CHANGED emit (file-watcher fan-out driver).
#[inline]
pub fn record_git_changed_emit() {
    GIT_CHANGED_EMITS.fetch_add(1, Ordering::Relaxed);
}

/// Common to every background-sync outcome: it was an attempt, and it took
/// `elapsed`. Split out so `record_bg_sync` / `record_bg_sync_failure` differ
/// only in the outcome counter they bump.
#[inline]
fn stamp_bg_sync_attempt(elapsed: Duration) {
    BG_SYNC_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
    BG_SYNC_LAST_MS.store(elapsed.as_millis() as u64, Ordering::Relaxed);
}

/// Record a background-sync outcome. `advanced` ⇒ the fetch moved the base ref.
#[inline]
pub fn record_bg_sync(advanced: bool, elapsed: Duration) {
    stamp_bg_sync_attempt(elapsed);
    if advanced {
        BG_SYNC_ADVANCED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record a background-sync failure (the fetch errored — offline/auth/timeout).
#[inline]
pub fn record_bg_sync_failure(elapsed: Duration) {
    stamp_bg_sync_attempt(elapsed);
    BG_SYNC_FAILURES.fetch_add(1, Ordering::Relaxed);
}

/// Record a git-subprocess timeout (deadline hit in `run_command_with_timeout`).
#[inline]
pub fn record_git_subprocess_timeout() {
    GIT_SUBPROCESS_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
}

// ---- OS process vitals --------------------------------------------------

/// A point-in-time snapshot of this process's OS-level resource use. All
/// fields are 0 on non-Windows (the vitals FFI is Windows-only; the sampler
/// still runs and records counters/gauges).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ProcessVitals {
    /// Physical memory currently mapped (WorkingSetSize).
    pub working_set_bytes: u64,
    /// Committed private memory (PROCESS_MEMORY_COUNTERS_EX.PrivateUsage) —
    /// the number that climbs on a true leak even when the working set is
    /// trimmed by the OS.
    pub private_bytes: u64,
    /// Open kernel handles. Climbs on a watcher/child-process handle leak.
    pub handle_count: u32,
    /// OS threads owned by this process. Climbs on a thread leak (nodes closed
    /// but coalescer / exit-poller / reader threads never joined).
    pub thread_count: u32,
    /// Live processes in our whole descendant *tree* at sample time — not just
    /// direct children. Agents run as grandchildren (`buildmesh → powershell →
    /// claude.exe`, or under `wsl.exe`) and git spawns helpers, so a
    /// direct-child-only count would undercount exactly the tree a leak grows.
    /// Approximate: Windows recycles PIDs, so a reused parent PID can
    /// transiently mis-attribute a process — fine for a leak *signature*, not
    /// an exact census.
    pub child_process_count: u32,
}

#[cfg(target_os = "windows")]
pub fn sample_vitals() -> ProcessVitals {
    windows_vitals::sample()
}

#[cfg(not(target_os = "windows"))]
pub fn sample_vitals() -> ProcessVitals {
    ProcessVitals::default()
}

// ---- The formatted sample line (pure, unit-testable) --------------------

/// Everything one diagnostics line reports, gathered so `format_line` is a
/// pure function of its inputs (no FFI / clock / statics) and can be tested.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub uptime_secs: u64,
    pub vitals: ProcessVitals,
    pub watchers: usize,
    pub pty_sessions: usize,
    pub git_changed_total: u64,
    pub git_changed_delta: u64,
    /// **Measured** wall-clock seconds since the previous sample, used to turn
    /// the delta into a rate. Deliberately not the nominal interval: under the
    /// very pathology being diagnosed (machine grinding, scheduler starved) the
    /// sampler thread wakes late, so dividing by the nominal interval would
    /// *overstate* the rate exactly when it matters most.
    pub elapsed_secs: f64,
    pub bg_sync_attempts: u64,
    pub bg_sync_advanced: u64,
    pub bg_sync_failures: u64,
    pub bg_sync_last_ms: u64,
    pub git_timeouts: u64,
}

/// Format one diagnostics line. Stable, greppable `DIAG` prefix and
/// `key=value` fields so a handed-back log can be scanned with plain `grep`.
/// The `{ts}` timestamp is prepended by the writer, not here.
pub fn format_line(s: &Sample) -> String {
    let rate = if s.elapsed_secs > 0.0 {
        s.git_changed_delta as f64 / s.elapsed_secs
    } else {
        0.0
    };
    format!(
        "DIAG uptime_s={} rss_mb={} priv_mb={} handles={} threads={} child_procs={} \
         watchers={} pty={} git_changed={}(+{}/{:.1}ps) \
         bg_sync=att:{}/adv:{}/fail:{}/last_ms:{} git_timeouts={}",
        s.uptime_secs,
        s.vitals.working_set_bytes / (1024 * 1024),
        s.vitals.private_bytes / (1024 * 1024),
        s.vitals.handle_count,
        s.vitals.thread_count,
        s.vitals.child_process_count,
        s.watchers,
        s.pty_sessions,
        s.git_changed_total,
        s.git_changed_delta,
        rate,
        s.bg_sync_attempts,
        s.bg_sync_advanced,
        s.bg_sync_failures,
        s.bg_sync_last_ms,
        s.git_timeouts,
    )
}

// ---- The sampler thread -------------------------------------------------

/// Default seconds between samples. Overridable via `BUILDMESH_DIAG_INTERVAL_MS`
/// so a user chasing a fast grind can crank the resolution for one session.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);

/// Env kill-switch. Diagnostics are on by default (the user opted into
/// always-on); set `BUILDMESH_DIAG=0` to disable the sampler entirely.
fn diagnostics_enabled() -> bool {
    !matches!(std::env::var("BUILDMESH_DIAG").as_deref(), Ok("0"))
}

fn sample_interval() -> Duration {
    std::env::var("BUILDMESH_DIAG_INTERVAL_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|ms| *ms >= 250) // floor: never busier than 4 Hz
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_INTERVAL)
}

/// Start the diagnostics sampler on a dedicated OS thread. Best-effort: a
/// failure to open the log is logged and the sampler simply doesn't run — it
/// must never take down the app it exists to observe. `log_dir` is the same
/// `logs/` directory the tracing appender uses.
pub fn start_sampler(log_dir: std::path::PathBuf) {
    if !diagnostics_enabled() {
        tracing::info!("diagnostics sampler disabled via BUILDMESH_DIAG=0");
        return;
    }
    let interval = sample_interval();
    let started = std::thread::Builder::new()
        .name("diagnostics-sampler".to_string())
        .spawn(move || run_sampler(log_dir, interval));
    if let Err(e) = started {
        tracing::warn!("failed to spawn diagnostics sampler: {e}");
    } else {
        tracing::info!(
            "diagnostics sampler started (interval {}ms) → logs/diagnostics.log",
            interval.as_millis()
        );
    }
}

fn run_sampler(log_dir: std::path::PathBuf, interval: Duration) {
    let path = log_dir.join("diagnostics.log");
    let mut writer = match RotatingWriter::open(path) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("diagnostics sampler: cannot open log ({e}); not sampling");
            return;
        }
    };
    // A session-boundary marker so multiple runs are distinguishable in one file.
    let _ = writer.write_line(&format!(
        "DIAG session_start git_sha={} interval_ms={}",
        env!("GIT_SHA"),
        interval.as_millis()
    ));

    let start = std::time::Instant::now();
    let mut prev_git_changed: u64 = GIT_CHANGED_EMITS.load(Ordering::Relaxed);
    let mut prev_sample_at = start;

    loop {
        std::thread::sleep(interval);

        // Measured, not nominal: the sampler wakes late when the machine is
        // grinding, so the rate must divide by real elapsed time.
        let now = std::time::Instant::now();
        let elapsed_secs = now.saturating_duration_since(prev_sample_at).as_secs_f64();
        prev_sample_at = now;

        let git_changed_total = GIT_CHANGED_EMITS.load(Ordering::Relaxed);
        let sample = Sample {
            uptime_secs: start.elapsed().as_secs(),
            vitals: sample_vitals(),
            // Live gauges, read directly at sample time (no counter plumbing).
            watchers: crate::commands::file_watcher::active_watcher_count(),
            pty_sessions: crate::agent::process::PROCESS_REGISTRY.len(),
            git_changed_total,
            git_changed_delta: git_changed_total.saturating_sub(prev_git_changed),
            elapsed_secs,
            bg_sync_attempts: BG_SYNC_ATTEMPTS.load(Ordering::Relaxed),
            bg_sync_advanced: BG_SYNC_ADVANCED.load(Ordering::Relaxed),
            bg_sync_failures: BG_SYNC_FAILURES.load(Ordering::Relaxed),
            bg_sync_last_ms: BG_SYNC_LAST_MS.load(Ordering::Relaxed),
            git_timeouts: GIT_SUBPROCESS_TIMEOUTS.load(Ordering::Relaxed),
        };
        prev_git_changed = git_changed_total;

        if let Err(e) = writer.write_line(&format_line(&sample)) {
            // Don't spin logging on a failing disk; one warn then keep trying.
            tracing::warn!("diagnostics sampler: write failed: {e}");
        }
    }
}

// ---- Windows vitals FFI -------------------------------------------------
//
// Raw `extern "system"` blocks in the style of `process_util::job_ffi` rather
// than pulling new `windows-sys` feature flags — the surface is five kernel32
// functions and three POD structs, and this keeps the dependency graph flat.

#[cfg(target_os = "windows")]
mod windows_vitals {
    use super::ProcessVitals;
    use std::os::raw::c_void;

    type Handle = *mut c_void;
    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const INVALID_HANDLE_VALUE: isize = -1;

    #[repr(C)]
    struct ProcessMemoryCountersEx {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }

    #[repr(C)]
    struct ProcessEntry32W {
        dw_size: u32,
        cnt_usage: u32,
        th32_process_id: u32,
        th32_default_heap_id: usize,
        th32_module_id: u32,
        cnt_threads: u32,
        th32_parent_process_id: u32,
        pc_pri_class_base: i32,
        dw_flags: u32,
        sz_exe_file: [u16; 260],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> Handle;
        fn GetCurrentProcessId() -> u32;
        fn GetProcessHandleCount(process: Handle, count: *mut u32) -> i32;
        // Exported from kernel32 as `K32GetProcessMemoryInfo` (psapi forwarder),
        // so no separate psapi link is needed.
        fn K32GetProcessMemoryInfo(
            process: Handle,
            counters: *mut ProcessMemoryCountersEx,
            cb: u32,
        ) -> i32;
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> Handle;
        fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    pub fn sample() -> ProcessVitals {
        let mut vitals = ProcessVitals::default();
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle (never freed);
        // the memory/handle calls read into stack-owned POD with the exact
        // `cb` size; the toolhelp snapshot is closed on every path.
        unsafe {
            let process = GetCurrentProcess();

            let mut handle_count: u32 = 0;
            if GetProcessHandleCount(process, &mut handle_count) != 0 {
                vitals.handle_count = handle_count;
            }

            let mut mem: ProcessMemoryCountersEx = std::mem::zeroed();
            mem.cb = std::mem::size_of::<ProcessMemoryCountersEx>() as u32;
            if K32GetProcessMemoryInfo(process, &mut mem, mem.cb) != 0 {
                vitals.working_set_bytes = mem.working_set_size as u64;
                vitals.private_bytes = mem.private_usage as u64;
            }

            let (threads, children) = snapshot_threads_and_children();
            vitals.thread_count = threads;
            vitals.child_process_count = children;
        }
        vitals
    }

    /// One process-snapshot pass yields both our own thread count (from our
    /// own `PROCESSENTRY32W.cnt_threads`) and the size of our whole descendant
    /// process *tree* — no second (thread) snapshot required.
    ///
    /// The tree (not just direct children) matters because agents run as
    /// grandchildren (`buildmesh → shell → agent CLI`) and git spawns helper
    /// processes; a direct-child count would miss exactly the tree a leak
    /// grows. We collect every `(pid, parent_pid)` pair, then count all
    /// processes transitively reachable from our pid via a small BFS. `seen`
    /// guards against a PID-reuse cycle so the walk always terminates.
    ///
    /// # Safety
    /// Caller ensures this runs on Windows; the snapshot handle is validated
    /// and always closed.
    unsafe fn snapshot_threads_and_children() -> (u32, u32) {
        use std::collections::HashSet;

        let our_pid = GetCurrentProcessId();
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot as isize == INVALID_HANDLE_VALUE || snapshot.is_null() {
            return (0, 0);
        }

        let mut entry: ProcessEntry32W = std::mem::zeroed();
        entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;

        let mut thread_count = 0u32;
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                if entry.th32_process_id == our_pid {
                    thread_count = entry.cnt_threads;
                }
                pairs.push((entry.th32_process_id, entry.th32_parent_process_id));
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);

        // BFS the descendant tree from our pid.
        let mut seen: HashSet<u32> = HashSet::new();
        let mut frontier: Vec<u32> = vec![our_pid];
        while let Some(pid) = frontier.pop() {
            for (child, parent) in &pairs {
                if *parent == pid && *child != pid && seen.insert(*child) {
                    frontier.push(*child);
                }
            }
        }
        (thread_count, seen.len() as u32)
    }
}
