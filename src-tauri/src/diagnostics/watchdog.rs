//! Out-of-process crash forensics and recovery for Windows.
//!
//! An in-process panic hook or Tauri run-event callback cannot observe the
//! whole-process termination seen when WebView2/the native host disappears.
//! The main process therefore starts a detached copy of this executable in a
//! tiny supervisor mode. The supervisor holds a process handle, waits for the
//! parent to terminate, records its OS exit code, and relaunches unless the
//! parent wrote its per-run expected-exit marker first.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const WATCHDOG_ARG: &str = "--buildmesh-crash-watchdog";
const RELAUNCH_COMMIT_PREFIX: &str = ".auto_relaunch_commit_";
pub(crate) const AUTO_RELAUNCH_COOLDOWN_SECS: u64 = 60;

static EXPECTED_EXIT_MARKER: OnceLock<PathBuf> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExpectedExitReason {
    CloseRequested,
    ExitRequested,
}

impl ExpectedExitReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::CloseRequested => "close_requested",
            Self::ExitRequested => "exit_requested",
        }
    }
}

#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelaunchDecision {
    Relaunch,
    ExpectedExit,
    Cooldown,
}

#[cfg(any(target_os = "windows", test))]
fn decide_relaunch(expected_exit: bool, cooldown_active: bool) -> RelaunchDecision {
    if expected_exit {
        RelaunchDecision::ExpectedExit
    } else if cooldown_active {
        RelaunchDecision::Cooldown
    } else {
        RelaunchDecision::Relaunch
    }
}

/// Intercept the private supervisor CLI before Tauri initialises.
///
/// Returns `true` whenever the watchdog flag was present, including malformed
/// invocations: a broken private invocation must exit rather than accidentally
/// opening a second full Buildmesh instance.
pub fn run_if_requested() -> bool {
    let mut args = std::env::args_os();
    let _exe = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new(WATCHDOG_ARG)) {
        return false;
    }

    #[cfg(target_os = "windows")]
    {
        let parent_pid = args
            .next()
            .and_then(|arg| arg.to_str().and_then(|value| value.parse::<u32>().ok()));
        let marker = args.next().map(PathBuf::from);
        if let (Some(parent_pid), Some(marker)) = (parent_pid, marker) {
            run_supervisor(parent_pid, &marker);
        }
    }
    true
}

/// Start one detached supervisor for this app process.
///
/// This call does not return successfully until the supervisor has opened a
/// handle to the parent. That handshake removes the PID-reuse/startup race:
/// once setup continues, the supervisor owns a reference to this exact process
/// even if the PID is recycled after termination.
pub fn start(log_dir: &Path) -> Result<(), String> {
    #[cfg(not(target_os = "windows"))]
    {
        let _ = log_dir;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        if std::env::var("BUILDMESH_DISABLE_CRASH_WATCHDOG").as_deref() == Ok("1") {
            tracing::info!("crash watchdog disabled via BUILDMESH_DISABLE_CRASH_WATCHDOG=1");
            return Ok(());
        }

        let parent_pid = std::process::id();
        let marker = log_dir.join(format!(
            ".watchdog-expected-{}-{}",
            parent_pid,
            uuid::Uuid::new_v4()
        ));
        EXPECTED_EXIT_MARKER
            .set(marker.clone())
            .map_err(|_| "crash watchdog already started".to_string())?;
        let ready = ready_path(&marker);
        let _ = std::fs::remove_file(&ready);

        let mut command = detached_self_command()?;
        command
            .arg(WATCHDOG_ARG)
            .arg(parent_pid.to_string())
            .arg(&marker);
        let child = command
            .spawn()
            .map_err(|e| format!("spawn crash watchdog: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() {
            if Instant::now() >= deadline {
                return Err(format!(
                    "crash watchdog {} did not acquire parent handle within 2s",
                    child.id()
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_file(&ready);
        tracing::info!(
            watchdog_pid = child.id(),
            parent_pid,
            "external crash watchdog started"
        );
        Ok(())
    }
}

/// Tell the external supervisor that the current shutdown/relaunch is known.
pub fn mark_expected_exit(reason: ExpectedExitReason) {
    let Some(marker) = EXPECTED_EXIT_MARKER.get() else {
        return;
    };
    if let Ok(mut file) = std::fs::File::create(marker) {
        let _ = writeln!(file, "{}", reason.as_str());
        let _ = file.sync_all();
    }
}

/// Retract a previously recorded expected exit (issue #1501).
///
/// The frontend vetoes a window close *after* the backend `CloseRequested`
/// handler has already marked the exit expected. When the user cancels out
/// of the exit-confirmation modal ("Keep Working"), the stale marker would
/// otherwise make a later real crash look like an expected exit and suppress
/// the supervisor's auto-relaunch. Removing the file restores the
/// crash-means-relaunch invariant. No-op when the watchdog never started.
/// Best-effort like `mark_expected_exit`: a removal failure is ignored —
/// the worst case is the pre-existing stale-marker behaviour, never a
/// failed user action.
pub fn clear_expected_exit() {
    let Some(marker) = EXPECTED_EXIT_MARKER.get() else {
        return;
    };
    let _ = std::fs::remove_file(marker);
}

/// Relaunch the current executable under the shared crash-loop cooldown.
pub fn relaunch_detached(log_dir: &Path) -> Result<bool, String> {
    let mut command = detached_self_command()?;
    let now_ms = unix_time_ms()?;
    if !reserve_relaunch(log_dir, now_ms)? {
        return Ok(false);
    }

    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = std::fs::remove_file(log_dir.join("auto_relaunched_at"));
            return Err(format!("spawn: {error}"));
        }
    };
    if let Err(error) = mark_relaunch_committed(log_dir, now_ms, child.id()) {
        append_forensic_line(
            log_dir,
            &format!("WATCHDOG relaunch_commit=failed error={error}"),
        );
    }
    Ok(true)
}

#[cfg(target_os = "windows")]
fn run_supervisor(parent_pid: u32, marker: &Path) {
    let Some(log_dir) = marker.parent() else {
        return;
    };
    let exit_result = match windows::ProcessWait::open(parent_pid) {
        Ok(process) => {
            if write_ready(marker).is_err() {
                append_forensic_line(log_dir, "WATCHDOG readiness_marker=failed");
            }
            process.wait()
        }
        Err(error) => Err(error),
    };
    let decision = classify_and_record(log_dir, marker, parent_pid, &exit_result);

    if decision == RelaunchDecision::Relaunch {
        clear_relaunch_lock_owned_by(log_dir, parent_pid);
        clear_uncommitted_relaunch_owned_by(log_dir, parent_pid);
        match relaunch_detached(log_dir) {
            Ok(true) => append_forensic_line(log_dir, "WATCHDOG relaunch=spawned"),
            Ok(false) => append_forensic_line(log_dir, "WATCHDOG relaunch=cooldown"),
            Err(error) => {
                append_forensic_line(log_dir, &format!("WATCHDOG relaunch=failed error={error}"))
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn write_ready(marker: &Path) -> std::io::Result<()> {
    let mut file = std::fs::File::create(ready_path(marker))?;
    writeln!(file, "ready")?;
    file.sync_all()
}

fn ready_path(marker: &Path) -> PathBuf {
    let mut value = marker.as_os_str().to_os_string();
    value.push(".ready");
    PathBuf::from(value)
}

#[cfg(any(target_os = "windows", test))]
fn classify_and_record(
    log_dir: &Path,
    marker: &Path,
    parent_pid: u32,
    exit_result: &Result<u32, String>,
) -> RelaunchDecision {
    let expected_exit = marker.exists();
    let cooldown = cooldown_active(log_dir, unix_time_ms().unwrap_or(0))
        && !uncommitted_relaunch_owned_by(log_dir, parent_pid);
    let decision = decide_relaunch(expected_exit, cooldown);
    let exit_code = match exit_result {
        Ok(code) => format!("0x{code:08X}"),
        Err(error) => format!("unavailable({error})"),
    };
    let marker_reason = std::fs::read_to_string(marker)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "none".to_string());
    append_forensic_line(
        log_dir,
        &format!(
            "WATCHDOG parent_pid={parent_pid} exit_code={exit_code} expected_marker={marker_reason} decision={decision:?} git_sha={}",
            env!("GIT_SHA")
        ),
    );
    let _ = std::fs::remove_file(marker);
    decision
}

fn reserve_relaunch(log_dir: &Path, now_ms: u128) -> Result<bool, String> {
    std::fs::create_dir_all(log_dir).map_err(|e| format!("create log dir: {e}"))?;
    let Some(_lock) = RelaunchReservationLock::acquire(log_dir, now_ms)? else {
        return Ok(false);
    };
    let stamp = log_dir.join("auto_relaunched_at");
    if cooldown_active(log_dir, now_ms) {
        return Ok(false);
    }
    match std::fs::remove_file(&stamp) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("remove expired relaunch stamp: {error}")),
    }
    remove_old_relaunch_commit_markers(log_dir);

    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stamp)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(format!("reserve relaunch stamp: {error}")),
    };
    writeln!(file, "{now_ms} reserved {}", std::process::id())
        .map_err(|e| format!("write relaunch stamp: {e}"))?;
    file.sync_all()
        .map_err(|e| format!("sync relaunch stamp: {e}"))?;
    Ok(true)
}

fn mark_relaunch_committed(log_dir: &Path, now_ms: u128, child_pid: u32) -> Result<(), String> {
    let path = relaunch_commit_path(log_dir, now_ms, std::process::id(), child_pid);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("create relaunch commit marker: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("sync relaunch commit marker: {error}"))
}

fn relaunch_commit_path(log_dir: &Path, now_ms: u128, owner_pid: u32, child_pid: u32) -> PathBuf {
    log_dir.join(format!(
        "{RELAUNCH_COMMIT_PREFIX}{now_ms}_{owner_pid}_{child_pid}"
    ))
}

fn remove_old_relaunch_commit_markers(log_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(RELAUNCH_COMMIT_PREFIX)
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn uncommitted_relaunch_owned_by(log_dir: &Path, owner_pid: u32) -> bool {
    let Ok(value) = std::fs::read_to_string(log_dir.join("auto_relaunched_at")) else {
        return false;
    };
    let mut fields = value.split_whitespace();
    let Some(timestamp) = fields.next().and_then(|value| value.parse::<u128>().ok()) else {
        return false;
    };
    if fields.next() != Some("reserved")
        || fields.next().and_then(|pid| pid.parse::<u32>().ok()) != Some(owner_pid)
    {
        return false;
    }
    !relaunch_commit_exists(log_dir, timestamp, owner_pid)
}

fn relaunch_commit_exists(log_dir: &Path, now_ms: u128, owner_pid: u32) -> bool {
    let prefix = format!("{RELAUNCH_COMMIT_PREFIX}{now_ms}_{owner_pid}_");
    std::fs::read_dir(log_dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
    })
}

fn clear_uncommitted_relaunch_owned_by(log_dir: &Path, owner_pid: u32) -> bool {
    if !uncommitted_relaunch_owned_by(log_dir, owner_pid) {
        return false;
    }
    std::fs::remove_file(log_dir.join("auto_relaunched_at")).is_ok()
}

struct RelaunchReservationLock {
    path: PathBuf,
}

impl RelaunchReservationLock {
    fn acquire(log_dir: &Path, now_ms: u128) -> Result<Option<Self>, String> {
        let path = log_dir.join(".auto_relaunch_lock");
        loop {
            match std::fs::create_dir(&path) {
                Ok(()) => {
                    let owner = path.join("acquired_at");
                    if let Err(error) =
                        std::fs::write(&owner, format!("{} {now_ms}", std::process::id()))
                    {
                        let _ = std::fs::remove_dir(&path);
                        return Err(format!("write relaunch lock: {error}"));
                    }
                    return Ok(Some(Self { path }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if !relaunch_lock_is_stale(&path, now_ms) {
                        return Ok(None);
                    }

                    // Rename first so a stale-lock cleanup can never delete a
                    // fresh lock created by another contender.
                    let quarantine = log_dir.join(format!(
                        ".auto_relaunch_lock_stale_{}",
                        uuid::Uuid::new_v4()
                    ));
                    match std::fs::rename(&path, &quarantine) {
                        Ok(()) => remove_quarantined_lock(&quarantine),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => return Ok(None),
                    }
                }
                Err(error) => return Err(format!("create relaunch lock: {error}")),
            }
        }
    }
}

impl Drop for RelaunchReservationLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.path.join("acquired_at"));
        let _ = std::fs::remove_dir(&self.path);
    }
}

fn clear_relaunch_lock_owned_by(log_dir: &Path, owner_pid: u32) -> bool {
    let path = log_dir.join(".auto_relaunch_lock");
    let owned_by_parent = std::fs::read_to_string(path.join("acquired_at"))
        .ok()
        .and_then(|value| {
            value
                .split_whitespace()
                .next()
                .and_then(|pid| pid.parse::<u32>().ok())
        })
        == Some(owner_pid);
    if !owned_by_parent {
        return false;
    }

    let quarantine = log_dir.join(format!(
        ".auto_relaunch_lock_orphan_{}",
        uuid::Uuid::new_v4()
    ));
    if std::fs::rename(&path, &quarantine).is_err() {
        return false;
    }
    remove_quarantined_lock(&quarantine);
    true
}

fn remove_quarantined_lock(path: &Path) {
    let _ = std::fs::remove_file(path.join("acquired_at"));
    let _ = std::fs::remove_dir(path);
}

fn relaunch_lock_is_stale(lock_path: &Path, now_ms: u128) -> bool {
    let created_ms = std::fs::read_to_string(lock_path.join("acquired_at"))
        .ok()
        .and_then(|value| {
            value
                .split_whitespace()
                .next_back()
                .and_then(|timestamp| timestamp.parse::<u128>().ok())
        })
        .or_else(|| {
            std::fs::metadata(lock_path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis())
        });
    created_ms.is_some_and(|created_ms| {
        now_ms.saturating_sub(created_ms) >= u128::from(AUTO_RELAUNCH_COOLDOWN_SECS) * 1000
    })
}

fn detached_self_command() -> Result<std::process::Command, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let mut command = crate::process_util::command_detached(exe);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    Ok(command)
}

fn cooldown_active(log_dir: &Path, now_ms: u128) -> bool {
    let mut value = String::new();
    if std::fs::File::open(log_dir.join("auto_relaunched_at"))
        .and_then(|mut file| file.read_to_string(&mut value))
        .is_err()
    {
        return false;
    }
    value
        .split_whitespace()
        .next()
        .and_then(|timestamp| timestamp.parse::<u128>().ok())
        .is_some_and(|last_ms| {
            now_ms.saturating_sub(last_ms) < u128::from(AUTO_RELAUNCH_COOLDOWN_SECS) * 1000
        })
}

fn unix_time_ms() -> Result<u128, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|e| format!("system clock: {e}"))
}

fn append_forensic_line(log_dir: &Path, message: &str) {
    let _ = std::fs::create_dir_all(log_dir);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("watchdog.log"))
    {
        let _ = writeln!(file, "{} {message}", chrono::Utc::now().to_rfc3339());
        let _ = file.sync_all();
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use std::os::raw::c_void;

    type Handle = *mut c_void;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const INFINITE: u32 = 0xFFFF_FFFF;
    const WAIT_OBJECT_0: u32 = 0;

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, process_id: u32) -> Handle;
        fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
        fn GetExitCodeProcess(process: Handle, exit_code: *mut u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
        fn GetLastError() -> u32;
    }

    pub struct ProcessWait {
        handle: Handle,
    }

    impl ProcessWait {
        pub fn open(process_id: u32) -> Result<Self, String> {
            // SAFETY: `OpenProcess` returns either null or a process handle
            // owned by the returned guard and closed exactly once in `Drop`.
            let process = unsafe {
                OpenProcess(
                    SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    process_id,
                )
            };
            if process.is_null() {
                return Err(format!("OpenProcess({process_id}) failed: {}", unsafe {
                    GetLastError()
                }));
            }
            Ok(Self { handle: process })
        }

        pub fn wait(self) -> Result<u32, String> {
            // SAFETY: the guard owns a live process handle; the APIs only wait
            // on it and write an exit code into stack-owned storage.
            unsafe {
                let wait_result = WaitForSingleObject(self.handle, INFINITE);
                if wait_result != WAIT_OBJECT_0 {
                    return Err(format!("WaitForSingleObject failed: {}", GetLastError()));
                }
                let mut exit_code = 0u32;
                if GetExitCodeProcess(self.handle, &mut exit_code) == 0 {
                    return Err(format!("GetExitCodeProcess failed: {}", GetLastError()));
                }
                Ok(exit_code)
            }
        }
    }

    impl Drop for ProcessWait {
        fn drop(&mut self) {
            // SAFETY: `handle` came from `OpenProcess` and this guard is its
            // sole owner.
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unexpected_parent_exit_relaunches() {
        assert_eq!(decide_relaunch(false, false), RelaunchDecision::Relaunch);
    }

    #[test]
    fn expected_parent_exit_does_not_relaunch() {
        assert_eq!(decide_relaunch(true, false), RelaunchDecision::ExpectedExit);
    }

    #[test]
    fn recent_relaunch_suppresses_a_crash_loop() {
        assert_eq!(decide_relaunch(false, true), RelaunchDecision::Cooldown);
    }

    #[test]
    fn cooldown_stamp_is_time_bounded() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("auto_relaunched_at"), "100000").unwrap();
        assert!(cooldown_active(dir.path(), 159_999));
        assert!(!cooldown_active(dir.path(), 160_000));
    }

    #[test]
    fn relaunch_reservation_is_atomic_and_suppresses_a_second_caller() {
        let dir = tempfile::tempdir().unwrap();
        assert!(reserve_relaunch(dir.path(), 100_000).unwrap());
        assert!(!reserve_relaunch(dir.path(), 100_001).unwrap());
    }

    #[test]
    fn concurrent_relaunch_reservations_have_exactly_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("auto_relaunched_at"), "1").unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let winners = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let barrier = std::sync::Arc::clone(&barrier);
                    let path = dir.path();
                    scope.spawn(move || {
                        barrier.wait();
                        reserve_relaunch(path, 100_000).unwrap()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .filter(|won| *won)
                .count()
        });
        assert_eq!(winners, 1);
    }

    #[test]
    fn supervisor_can_reclaim_a_fresh_lock_owned_by_its_dead_parent() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join(".auto_relaunch_lock");
        std::fs::create_dir(&lock).unwrap();
        std::fs::write(lock.join("acquired_at"), "42 100000").unwrap();

        assert!(!reserve_relaunch(dir.path(), 100_001).unwrap());
        assert!(clear_relaunch_lock_owned_by(dir.path(), 42));
        assert!(reserve_relaunch(dir.path(), 100_001).unwrap());
    }

    #[test]
    fn supervisor_reclaims_a_dead_parents_uncommitted_spawn_reservation() {
        let dir = tempfile::tempdir().unwrap();
        let now_ms = unix_time_ms().unwrap();
        std::fs::write(
            dir.path().join("auto_relaunched_at"),
            format!("{now_ms} reserved 42"),
        )
        .unwrap();

        let marker = dir.path().join("missing.marker");
        let decision = classify_and_record(dir.path(), &marker, 42, &Ok(1));
        assert_eq!(decision, RelaunchDecision::Relaunch);
        assert!(clear_uncommitted_relaunch_owned_by(dir.path(), 42));
        assert!(reserve_relaunch(dir.path(), now_ms).unwrap());
    }

    #[test]
    fn supervisor_respects_a_committed_relaunch_cooldown() {
        let dir = tempfile::tempdir().unwrap();
        let now_ms = unix_time_ms().unwrap();
        std::fs::write(
            dir.path().join("auto_relaunched_at"),
            format!("{now_ms} reserved 42"),
        )
        .unwrap();
        std::fs::File::create(relaunch_commit_path(dir.path(), now_ms, 42, 84)).unwrap();

        let marker = dir.path().join("missing.marker");
        let decision = classify_and_record(dir.path(), &marker, 42, &Ok(1));
        assert_eq!(decision, RelaunchDecision::Cooldown);
    }

    #[test]
    fn torn_commit_marker_does_not_hide_an_uncommitted_reservation() {
        let dir = tempfile::tempdir().unwrap();
        let now_ms = unix_time_ms().unwrap();
        std::fs::write(
            dir.path().join("auto_relaunched_at"),
            format!("{now_ms} reserved 42"),
        )
        .unwrap();
        std::fs::write(dir.path().join(".auto_relaunch_commit_incomplete"), "42").unwrap();

        let marker = dir.path().join("missing.marker");
        let decision = classify_and_record(dir.path(), &marker, 42, &Ok(1));
        assert_eq!(decision, RelaunchDecision::Relaunch);
    }

    #[test]
    fn expected_supervisor_flow_consumes_marker_and_persists_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("expected.marker");
        std::fs::write(&marker, "test_expected").unwrap();
        let decision = classify_and_record(dir.path(), &marker, 42, &Ok(23));
        assert_eq!(decision, RelaunchDecision::ExpectedExit);
        assert!(!marker.exists());
        let log = std::fs::read_to_string(dir.path().join("watchdog.log")).unwrap();
        assert!(log.contains("parent_pid=42"));
        assert!(log.contains("exit_code=0x00000017"));
        assert!(log.contains("expected_marker=test_expected"));
        assert!(log.contains("decision=ExpectedExit"));
    }

    #[test]
    fn unexpected_supervisor_flow_persists_relaunch_decision() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("missing.marker");
        let decision = classify_and_record(dir.path(), &marker, 84, &Err("terminated".into()));
        assert_eq!(decision, RelaunchDecision::Relaunch);
        let log = std::fs::read_to_string(dir.path().join("watchdog.log")).unwrap();
        assert!(log.contains("parent_pid=84"));
        assert!(log.contains("exit_code=unavailable(terminated)"));
        assert!(log.contains("decision=Relaunch"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn supervisor_observes_a_real_child_exit_code() {
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "ping 127.0.0.1 -n 2 > nul & exit /b 23"])
            .spawn()
            .unwrap();
        let process = windows::ProcessWait::open(child.id()).unwrap();
        let exit_code = process.wait().unwrap();
        let _ = child.wait();
        assert_eq!(exit_code, 23);
    }
}
