//! Which processes are holding a worktree directory, so a blocked cleanup can
//! be made actionable (issue #2139).
//!
//! The failure the user actually sees is an OS error — error 32 ("being used by
//! another process") for a root handle, error 5 ("access is denied") for a
//! child-directory handle — which names *no* process. Without a diagnosis the
//! only recovery is "close things until it works".
//!
//! This module answers the question the error can't: which processes are
//! pinning this tree. Two signals are checked per process:
//!
//! * its executable lives inside the worktree (an agent-spawned helper, a
//!   tool copied into the tree), and
//! * its working directory is inside the worktree (the classic case: a shell,
//!   a PowerShell sleeper, or Explorer parked on a folder).
//!
//! Both are read from public OS state — on Windows the toolhelp snapshot plus
//! the target's process environment block; on Unix `/proc/<pid>/cwd` and
//! `/proc/<pid>/exe`. Sysinternals `handle.exe` would name the process holding
//! a specific *handle*, but it cannot be assumed installed, so the diagnosis is
//! deliberately process-level and approximate: it names candidates the user can
//! act on, while the removal error itself says which permission failed.
//!
//! Nothing here terminates a process on its own. [`terminate_process`] is a
//! separate, user-driven action: the app never closes applications by itself
//! (least of all Explorer), it only names them and offers the choice.

use serde::Serialize;
use ts_rs::TS;

/// A process that may be holding a worktree directory, with the evidence that
/// flagged it. `reason` is the signal that matched (`working-directory`, or
/// `executable-inside-worktree`); `detail` is the path that matched.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "BlockingProcess.ts")]
pub struct BlockingProcess {
    pub pid: u32,
    /// Best-effort process name (executable file name).
    pub name: Option<String>,
    /// Full executable path when readable.
    pub executable_path: Option<String>,
    /// Which signal matched: `working-directory` or `executable-inside-worktree`.
    pub reason: String,
    /// The path that matched, so the user can see exactly which folder is held.
    pub detail: String,
}

impl BlockingProcess {
    fn new(
        pid: u32,
        name: Option<String>,
        executable_path: Option<String>,
        reason: &str,
        detail: &str,
    ) -> Self {
        Self {
            pid,
            name,
            executable_path,
            reason: reason.to_string(),
            detail: detail.to_string(),
        }
    }

    /// A one-line summary for a toast/diagnostics block.
    pub fn describe(&self) -> String {
        let who = self.name.as_deref().unwrap_or("unknown process");
        format!("{who} (pid {}) — {}", self.pid, self.reason)
    }
}

/// Every process (other than ourselves) whose executable or working directory
/// lies inside `path`'s tree.
///
/// Failing to read a process is not an error, it simply isn't reported: a
/// protected or just-exited process contributes no diagnosis rather than a
/// wrong one.
pub fn diagnose_blocking_processes(path: &str) -> Vec<BlockingProcess> {
    let Some(root) = normalize(path) else {
        return Vec::new();
    };
    let our_pid = current_pid();

    #[cfg(target_os = "windows")]
    {
        windows::blocking_processes(&root, our_pid)
    }
    #[cfg(not(target_os = "windows"))]
    {
        unix::blocking_processes(&root, our_pid)
    }
}

/// Explicitly terminate a process that the diagnosis named, re-checked here.
///
/// Never called automatically: the only caller is a user acting on one diagnosed
/// blocker. The pid is re-checked against a *fresh* diagnosis of the same
/// worktree before anything is terminated, because process IDs are reused on
/// Windows — the pid a user saw a minute ago may now belong to an unrelated
/// program, and the kill reaches its children too (issue #2139 review round 1).
pub fn release_blocker(worktree_path: &str, pid: u32) -> Result<(), String> {
    let blockers = diagnose_blocking_processes(worktree_path);
    if !blockers.iter().any(|blocker| blocker.pid == pid) {
        return Err(format!(
            "process {pid} is no longer holding {worktree_path} — refusing to end it \
             (its id may have been reused)"
        ));
    }
    terminate_process(pid)
}

/// Terminate a process outright. Reachable only through [`release_blocker`],
/// which exists to prove the process is still holding the worktree first — the
/// app never closes an application by itself (issue #2139).
pub fn terminate_process(pid: u32) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        // `taskkill /F /T` also takes the process's children down, which is what
        // the process-tree kill on the close path uses. It is user-requested
        // here, one pid at a time, after the diagnosis named it.
        crate::process_util::kill_process_tree(pid);
        if wait_for_exit(pid, std::time::Duration::from_millis(500)) {
            Ok(())
        } else {
            Err(format!(
                "process {pid} did not exit after the termination request"
            ))
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        unix::kill(pid);
        if wait_for_exit(pid, std::time::Duration::from_millis(500)) {
            Ok(())
        } else {
            Err(format!(
                "process {pid} did not exit after the termination request"
            ))
        }
    }
}

fn current_pid() -> u32 {
    std::process::id()
}

/// The path to test membership of, normalized the same way on both platforms:
/// trailing separators dropped and separators unified to `/`. Lowercasing is
/// Windows-only, where paths are case-insensitive; on Unix two paths that
/// differ in case are different files.
fn normalize(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return None;
    }
    let unified = trimmed.replace('\\', "/");
    Some(if cfg!(target_os = "windows") {
        unified.to_lowercase()
    } else {
        unified
    })
}

/// Whether `candidate` is `root` itself or lives inside it. Both must be
/// normalized first.
fn is_inside(candidate: &str, root: &str) -> bool {
    let Some(candidate) = normalize(candidate) else {
        return false;
    };
    candidate == root || candidate.starts_with(&format!("{root}/"))
}

/// Poll until the process is gone, or the bounded wait expires.
fn wait_for_exit(pid: u32, budget: std::time::Duration) -> bool {
    #[cfg(target_os = "windows")]
    {
        windows::wait_for_exit(pid, budget)
    }
    #[cfg(not(target_os = "windows"))]
    {
        unix::wait_for_exit(pid, budget)
    }
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
mod windows {
    use super::{is_inside, BlockingProcess};
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    const STATUS_SUCCESS: i32 = 0;
    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    /// A Win32 handle. Kept as a pointer rather than an integer so the kernel32
    /// declarations below have the same signature as the ones the crate
    /// already declares in `diagnostics`: Rust requires two `extern` blocks in
    /// one crate to agree on an imported symbol's signature, or it warns.
    type Handle = *mut std::ffi::c_void;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const PROCESS_VM_READ: u32 = 0x0010;
    const PROCESS_BASIC_INFORMATION: u32 = 0;
    /// `ProcessWow64Information` — for a WOW64 process this is *its* 32-bit PEB
    /// address, which is what the 32-bit layout has to be read from.
    const PROCESS_WOW64_INFORMATION: u32 = 26;
    /// `PROCESS_NAME_WIN32` — the path in `C:\…` form rather than the raw
    /// `\Device\HarddiskVolumeN\…` device path.
    const PROCESS_NAME_WIN32: u32 = 0;

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

    /// Matches winternl's `PROCESS_BASIC_INFORMATION` field order, so the
    /// struct's own padding puts `peb_base_address` where the API writes it.
    #[repr(C)]
    struct ProcessBasicInformation {
        exit_status: i32,
        _padding: u32,
        peb_base_address: usize,
        affinity_mask: usize,
        base_priority: isize,
        unique_process_id: usize,
        inherited_from_unique_process_id: usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> Handle;
        fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn QueryFullProcessImageNameW(
            process: Handle,
            flags: u32,
            name: *mut u16,
            size: *mut u32,
        ) -> i32;
        fn IsWow64Process(process: Handle, wow64: *mut i32) -> i32;
        fn GetLastError() -> u32;
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn NtQueryInformationProcess(
            process: Handle,
            information_class: u32,
            info: *mut std::ffi::c_void,
            length: u32,
            returned: *mut u32,
        ) -> i32;
        fn NtReadVirtualMemory(
            process: Handle,
            base: *const std::ffi::c_void,
            buffer: *mut std::ffi::c_void,
            size: usize,
            read: *mut usize,
        ) -> i32;
    }

    pub fn blocking_processes(root: &str, our_pid: u32) -> Vec<BlockingProcess> {
        let mut found: Vec<BlockingProcess> = Vec::new();
        let mut entries = snapshot_processes();
        for (pid, name) in entries.drain(..) {
            if pid == our_pid || pid == 0 || pid == 4 {
                // Our own process (we can't block ourselves), and the idle and
                // system processes, which have no executable inside any repo.
                continue;
            }
            let Some(process) = open_process(pid) else {
                continue; // just exited, or protected: nothing to report
            };
            let executable = read_image_name(process);
            if let Some(exe) = &executable {
                if is_inside(exe, root) {
                    found.push(BlockingProcess::new(
                        pid,
                        Some(name.clone()),
                        executable.clone(),
                        "executable-inside-worktree",
                        exe,
                    ));
                }
            }
            if let Some(cwd) = read_working_directory(process) {
                if is_inside(&cwd, root) {
                    found.push(BlockingProcess::new(
                        pid,
                        Some(name),
                        executable,
                        "working-directory",
                        &cwd,
                    ));
                }
            }
            unsafe { CloseHandle(process) };
        }
        found
    }

    fn snapshot_processes() -> Vec<(u32, String)> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if invalid(snapshot) {
            return Vec::new();
        }
        let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
        entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
        let mut list = Vec::new();
        if unsafe { Process32FirstW(snapshot, &mut entry) } != 0 {
            loop {
                list.push((entry.th32_process_id, utf16_entry(&entry.sz_exe_file)));
                if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
                    break;
                }
            }
        }
        unsafe { CloseHandle(snapshot) };
        list
    }

    /// Both `INVALID_HANDLE_VALUE` (a null-ish `-1`) and a null return mean
    /// "no handle".
    fn invalid(handle: Handle) -> bool {
        handle.is_null() || handle as isize == -1
    }

    fn open_process(pid: u32) -> Option<Handle> {
        let handle =
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid) };
        if invalid(handle) {
            None
        } else {
            Some(handle)
        }
    }

    fn read_image_name(process: Handle) -> Option<String> {
        let mut buffer = vec![0u16; 1024];
        let mut size = buffer.len() as u32;
        if unsafe {
            QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size)
        } == 0
        {
            return None;
        }
        buffer.truncate(size as usize);
        Some(OsString::from_wide(&buffer).to_string_lossy().to_string())
    }

    /// Read the process's current directory out of its process environment
    /// block: `PEB.ProcessParameters.CurrentDirectory.DosPath`.
    ///
    /// The layouts differ between a 64-bit and a 32-bit target, and a 64-bit
    /// reader that assumes the 64-bit layout on a 32-bit process reads garbage:
    /// `ProcessBasicInformation.PebBaseAddress` is the *64-bit* PEB even for a
    /// WOW64 process, and the 32-bit `UNICODE_STRING.Buffer` pointer sits at a
    /// different offset. So a 32-bit target is re-queried with
    /// [`PROCESS_WOW64_INFORMATION`] (class 26), which returns its own 32-bit
    /// PEB, and the whole walk then uses 32-bit offsets and a 4-byte pointer
    /// width (issue #2139 review round 1). Returns `None` for anything that
    /// can't be read (a different user's elevated process, a just-exited one),
    /// which is why the diagnosis is best-effort.
    fn read_working_directory(process: Handle) -> Option<String> {
        let mut info: ProcessBasicInformation = unsafe { std::mem::zeroed() };
        let status = unsafe {
            NtQueryInformationProcess(
                process,
                PROCESS_BASIC_INFORMATION,
                &mut info as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<ProcessBasicInformation>() as u32,
                std::ptr::null_mut(),
            )
        };
        if status != STATUS_SUCCESS {
            return None;
        }

        let mut wow64: i32 = 0;
        let target_is_32bit = unsafe { IsWow64Process(process, &mut wow64) } != 0 && wow64 != 0;
        // For a 32-bit target, the *own* PEB address (the 32-bit one) comes from
        // the Wow64 information class, not from the basic information above.
        let peb_base_address = if target_is_32bit {
            let mut wow64_peb: usize = 0;
            let status = unsafe {
                NtQueryInformationProcess(
                    process,
                    PROCESS_WOW64_INFORMATION,
                    std::ptr::addr_of_mut!(wow64_peb).cast(),
                    std::mem::size_of::<usize>() as u32,
                    std::ptr::null_mut(),
                )
            };
            if status != STATUS_SUCCESS || wow64_peb == 0 {
                return None;
            }
            wow64_peb
        } else if info.peb_base_address == 0 {
            return None;
        } else {
            info.peb_base_address
        };

        let (peb_params_offset, cwd_offset, pointer_width) = if target_is_32bit {
            (0x10usize, 0x24usize, 4usize)
        } else {
            (0x20usize, 0x38usize, 8usize)
        };

        // `PEB.ProcessParameters` is a pointer-sized field.
        let mut process_parameters: usize = 0;
        read_memory(
            process,
            (peb_base_address + peb_params_offset) as *const std::ffi::c_void,
            std::ptr::addr_of_mut!(process_parameters).cast(),
            pointer_width,
        )?;
        if process_parameters == 0 {
            return None;
        }

        // `CurrentDirectory` is a `CURDIR`: a UNICODE_STRING (length, capacity,
        // pointer) then a handle pointer. Read the raw bytes and interpret them
        // with the target's pointer width. The pointer's offset differs: the
        // 64-bit layout pads the two `u16` fields out to eight bytes, so
        // `Buffer` sits at offset 8; the 32-bit layout has no padding, so it
        // sits at offset 4 — reading offset 8 there yields the `Handle` field
        // instead, which is why 32-bit holders used to be missed (issue #2139
        // review round 1).
        let mut raw = [0u8; 24];
        read_memory(
            process,
            (process_parameters + cwd_offset) as *const std::ffi::c_void,
            raw.as_mut_ptr().cast(),
            8 + pointer_width,
        )?;
        let length = u16::from_ne_bytes([raw[0], raw[1]]) as usize;
        if length < 2 {
            return None;
        }
        let (buffer_offset, buffer_width) = if target_is_32bit {
            (4usize, 4usize)
        } else {
            (8usize, 8usize)
        };
        let buffer_address = if target_is_32bit {
            u32::from_ne_bytes([
                raw[buffer_offset],
                raw[buffer_offset + 1],
                raw[buffer_offset + 2],
                raw[buffer_offset + 3],
            ]) as usize
        } else {
            u64::from_ne_bytes(
                raw[buffer_offset..buffer_offset + buffer_width]
                    .try_into()
                    .ok()?,
            ) as usize
        };
        if buffer_address == 0 {
            return None;
        }

        let mut utf16 = vec![0u16; length / 2];
        read_memory(
            process,
            buffer_address as *const std::ffi::c_void,
            utf16.as_mut_ptr().cast(),
            utf16.len() * 2,
        )?;
        Some(
            OsString::from_wide(&utf16)
                .to_string_lossy()
                .trim_end_matches('\0')
                .to_string(),
        )
    }

    /// Read exactly `size` bytes from `base` in `process` into `destination`.
    ///
    /// # Safety
    /// Callers pass an address obtained from the target's own structures and a
    /// destination of at least `size` bytes. A failure returns `None` rather
    /// than a partial or guessed read.
    fn read_memory(
        process: Handle,
        base: *const std::ffi::c_void,
        destination: *mut std::ffi::c_void,
        size: usize,
    ) -> Option<()> {
        let mut read: usize = 0;
        let status = unsafe { NtReadVirtualMemory(process, base, destination, size, &mut read) };
        if status != STATUS_SUCCESS || read != size {
            None
        } else {
            Some(())
        }
    }

    fn utf16_entry(raw: &[u16; 260]) -> String {
        let end = raw.iter().position(|c| *c == 0).unwrap_or(raw.len());
        OsString::from_wide(&raw[..end])
            .to_string_lossy()
            .to_string()
    }

    /// Wait for a pid to disappear under a bounded budget. A pid the OS no
    /// longer recognises (`ERROR_INVALID_PARAMETER`) counts as gone.
    pub fn wait_for_exit(pid: u32, budget: std::time::Duration) -> bool {
        const ERROR_INVALID_PARAMETER: u32 = 87;
        let deadline = std::time::Instant::now() + budget;
        loop {
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
            if invalid(handle) {
                if unsafe { GetLastError() } == ERROR_INVALID_PARAMETER {
                    return true;
                }
            } else {
                unsafe { CloseHandle(handle) };
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}

// ---------------------------------------------------------------------------
// Unix
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "windows"))]
mod unix {
    use super::{is_inside, BlockingProcess};

    pub fn blocking_processes(root: &str, our_pid: u32) -> Vec<BlockingProcess> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return found;
        };
        for entry in entries.flatten() {
            let Some(pid) = entry.file_name().to_string_lossy().parse::<u32>().ok() else {
                continue;
            };
            if pid == our_pid || pid == 0 {
                continue;
            }
            let proc = format!("/proc/{pid}");
            let name = std::fs::read(format!("{proc}/comm"))
                .ok()
                .map(|raw| String::from_utf8_lossy(&raw).trim().to_string());
            let executable = read_link(&format!("{proc}/exe"));
            if let Some(exe) = &executable {
                if is_inside(exe, root) {
                    found.push(BlockingProcess::new(
                        pid,
                        name.clone(),
                        executable.clone(),
                        "executable-inside-worktree",
                        exe,
                    ));
                }
            }
            if let Some(cwd) = read_link(&format!("{proc}/cwd")) {
                if is_inside(&cwd, root) {
                    found.push(BlockingProcess::new(
                        pid,
                        name,
                        executable,
                        "working-directory",
                        &cwd,
                    ));
                }
            }
        }
        found
    }

    fn read_link(path: &str) -> Option<String> {
        std::fs::canonicalize(path)
            .ok()
            .map(|p| p.to_string_lossy().to_string())
    }

    pub fn kill(pid: u32) {
        let _ = std::process::Command::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .output();
    }

    pub fn wait_for_exit(pid: u32, budget: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + budget;
        loop {
            if std::fs::canonicalize(format!("/proc/{pid}")).is_err() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Membership is the whole question this module answers, so pin the
    /// normalisation: a descendant must match, while a sibling that merely
    /// shares a prefix (the classic `wt-a` vs `wt-abc`) must not.
    #[test]
    fn is_inside_distinguishes_descendants_from_prefix_siblings() {
        let root = normalize("/repo/m/.claude/worktrees/wt-a").unwrap();

        assert!(is_inside("/repo/m/.claude/worktrees/wt-a", &root));
        assert!(is_inside("/repo/m/.claude/worktrees/wt-a/src", &root));
        assert!(is_inside(
            "/repo/m/.claude/worktrees/wt-a/src/nested/deep",
            &root
        ));
        // Windows separators and a trailing slash must not defeat the match.
        assert!(is_inside(
            "\\repo\\m\\.claude\\worktrees\\wt-a\\src\\",
            &root
        ));

        assert!(!is_inside("/repo/m/.claude/worktrees/wt-abc", &root));
        assert!(!is_inside("/repo/m/.claude/worktrees", &root));
        assert!(!is_inside(
            "/repo/m/.claude/worktrees/wt-a-other/src",
            &root
        ));
        assert!(!is_inside("", &root));
    }

    /// A live process parked in a directory inside the tree must be named, with
    /// the pid and the signal that matched. This is the reported failure mode
    /// — a sleeper whose working directory pins the worktree, invisible in the
    /// OS error.
    #[test]
    fn diagnosis_finds_a_process_working_inside_the_tree() {
        use crate::env::test_helpers::ScopedChild;
        let td = crate::env::test_helpers::TestDir::new("blockers_diag");
        let held = td.path().join("nested").join("held");
        std::fs::create_dir_all(&held).unwrap();

        #[cfg(target_os = "windows")]
        let child = {
            // `ping -n N` is the portable "stay alive" shell-out that never
            // writes to a console (see process_util's hang helper).
            let mut command = crate::process_util::command_no_window("cmd");
            command
                .args(["/c", "ping -n 30 127.0.0.1"])
                .current_dir(&held)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            ScopedChild::spawn(command)
        };
        #[cfg(not(target_os = "windows"))]
        let child = {
            let mut command = crate::process_util::command_no_window("sleep");
            command
                .arg("30")
                .current_dir(&held)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            ScopedChild::spawn(command)
        };
        // Give the child a moment to start so its working directory is set
        // before we read it.
        std::thread::sleep(std::time::Duration::from_millis(500));

        let root = normalize(&td.path().to_string_lossy()).unwrap();
        let found = diagnose_blocking_processes(&root);

        let match_row = found.iter().find(|process| process.pid == child.id());
        let Some(process) = match_row else {
            let _ = &child;
            panic!(
                "the child parked in {} must be diagnosed (found {found:?})",
                held.display()
            );
        };
        assert_eq!(
            process.reason, "working-directory",
            "the matched signal is the pinned working directory"
        );
        assert!(
            process
                .detail
                .to_lowercase()
                .replace('\\', "/")
                .contains("held"),
            "the reported path is the directory actually held, got {}",
            process.detail
        );
    }

    /// A 32-bit process holds its folder too. Windows runs 32-bit processes
    /// under WOW64, whose process environment block has different offsets and a
    /// different PEB address than the 64-bit one the basic information class
    /// reports — the pre-#2139-fix code read the 64-bit block with 32-bit
    /// offsets and found nothing (issue #2139 review round 1). Skipped where
    /// SysWOW64 does not exist (32-bit Windows), since there is no 32-bit
    /// process to spawn.
    #[test]
    #[cfg(target_os = "windows")]
    fn diagnosis_finds_a_32_bit_process_working_inside_the_tree() {
        use crate::env::test_helpers::ScopedChild;
        let syswow = std::path::Path::new(r"C:\Windows\SysWOW64\cmd.exe");
        if !syswow.exists() {
            eprintln!("SKIP: no SysWOW64 on this Windows install");
            return;
        }
        let td = crate::env::test_helpers::TestDir::new("blockers_wow64");
        let held = td.path().join("wow64").join("held");
        std::fs::create_dir_all(&held).unwrap();

        let mut command = crate::process_util::command_no_window(syswow);
        command
            .args(["/c", "ping -n 30 127.0.0.1"])
            .current_dir(&held)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = ScopedChild::spawn(command);
        std::thread::sleep(std::time::Duration::from_millis(500));

        let root = normalize(&td.path().to_string_lossy()).unwrap();
        let found = diagnose_blocking_processes(&root);

        let process = found.iter().find(|process| process.pid == child.id());
        let Some(process) = process else {
            let _ = &child;
            panic!("the 32-bit child parked in {held:?} must be diagnosed (found {found:?})");
        };
        assert_eq!(
            process.reason, "working-directory",
            "the matched signal is the pinned working directory"
        );
        assert!(
            process
                .detail
                .to_lowercase()
                .replace('\\', "/")
                .contains("held"),
            "the reported path is the directory actually held, got {}",
            process.detail
        );
    }

    /// A directory nothing references yields an empty answer rather than an
    /// error.
    #[test]
    fn diagnosis_of_an_unheld_directory_is_empty() {
        let found = diagnose_blocking_processes("buildmesh-no-such-directory-2139");
        assert!(
            found.is_empty(),
            "a path nothing references has no blockers (got {found:?})"
        );
    }

    /// The summary line a user reads names the process and the signal.
    #[test]
    fn blocking_process_describes_itself() {
        let process = BlockingProcess::new(
            4242,
            Some("powershell.exe".to_string()),
            Some("C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe".to_string()),
            "working-directory",
            "C:/repo/.claude/worktrees/wt-a",
        );
        assert!(
            process.describe().contains("powershell.exe")
                && process.describe().contains("4242")
                && process.describe().contains("working-directory"),
            "got: {}",
            process.describe()
        );
    }

    /// Ending a process is only allowed for a pid that *currently* holds the
    /// worktree. A pid that never held it — the shape an id-reuse accident takes
    /// — must be refused rather than terminated (issue #2139 review round 1).
    #[test]
    fn terminating_a_process_that_is_not_a_blocker_is_refused() {
        let td = crate::env::test_helpers::TestDir::new("blockers_refuse");
        let _held = td.path().join("refuse");
        std::fs::create_dir_all(&_held).unwrap();
        let root = td.path().to_string_lossy().to_string();

        // This process holds nothing inside that tree, and the pid certainly
        // exists — so the only thing that can save the test runner is the check.
        let our_pid = current_pid();
        let result = release_blocker(&root, our_pid);
        assert!(
            result.is_err(),
            "a pid the diagnosis does not name must not be terminated"
        );
        assert!(
            result.unwrap_err().contains("is no longer holding"),
            "the refusal says why"
        );
        assert!(
            !diagnose_blocking_processes(&root)
                .iter()
                .any(|process| process.pid == our_pid),
            "the pid really is not a blocker of this tree"
        );
        assert_eq!(
            std::process::id(),
            our_pid,
            "the test process is still alive"
        );
    }
}
