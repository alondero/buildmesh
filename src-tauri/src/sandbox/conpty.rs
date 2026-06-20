//! Own the Windows ConPTY spawn so an agent PTY process can be launched *into*
//! an AppContainer (GH #498).
//!
//! `portable-pty` 0.8 builds its `STARTUPINFOEX` proc-thread attribute list with
//! capacity for exactly one attribute (`PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`)
//! and calls plain `CreateProcessW` with the caller's token — it exposes no seam
//! to add a second attribute or a sandbox token. An AppContainer must be applied
//! at process creation (it can't be attached afterwards like our Job Object), so
//! the sandboxed path re-implements the ~80 lines of ConPTY spawn here with a
//! **two-attribute** list: `PSEUDOCONSOLE` + `SECURITY_CAPABILITIES`.
//!
//! The returned [`SandboxChild`] / [`SandboxPty`] implement `portable_pty`'s
//! `Child` / `MasterPty` traits, so `spawn_agent_inner` consumes them exactly
//! like the unsandboxed `PtyPair` — the reader thread, resize, Job Object
//! containment, and kill path are all unchanged.
//!
//! FFI is inline `extern "system"` (no `windows-sys`/`winapi` dep), mirroring
//! [`crate::process_util`] and [`super::appcontainer`].

#![cfg(target_os = "windows")]

use std::io::{Error as IoError, Read, Result as IoResult, Write};
use std::sync::{Arc, Mutex};

use portable_pty::{Child, ChildKiller, ExitStatus, MasterPty, PtySize};

use super::appcontainer::AppContainerProfile;
use super::restricted_token::RestrictedToken;

#[allow(non_snake_case, non_camel_case_types)]
mod ffi {
    use std::os::raw::c_void;

    pub type HANDLE = *mut c_void;
    pub type BOOL = i32;
    pub type DWORD = u32;
    pub type HRESULT = i32;

    pub const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;
    pub const S_OK: HRESULT = 0;
    pub const INFINITE: DWORD = 0xFFFF_FFFF;
    pub const STILL_ACTIVE: DWORD = 259;

    pub const EXTENDED_STARTUPINFO_PRESENT: DWORD = 0x0008_0000;
    pub const CREATE_UNICODE_ENVIRONMENT: DWORD = 0x0000_0400;
    pub const STARTF_USESTDHANDLES: DWORD = 0x0000_0100;

    pub const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x0002_0016;
    pub const PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES: usize = 0x0002_0009;

    pub const HANDLE_FLAG_INHERIT: DWORD = 0x0000_0001;
    pub const DUPLICATE_SAME_ACCESS: DWORD = 0x0000_0002;

    // PseudoConsole resize/input-mode quirks (match portable-pty's defaults).
    pub const PSEUDOCONSOLE_RESIZE_QUIRK: DWORD = 0x2;
    pub const PSEUDOCONSOLE_WIN32_INPUT_MODE: DWORD = 0x4;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct COORD {
        pub X: i16,
        pub Y: i16,
    }

    #[repr(C)]
    pub struct SECURITY_ATTRIBUTES {
        pub nLength: DWORD,
        pub lpSecurityDescriptor: *mut c_void,
        pub bInheritHandle: BOOL,
    }

    #[repr(C)]
    pub struct STARTUPINFOW {
        pub cb: DWORD,
        pub lpReserved: *mut u16,
        pub lpDesktop: *mut u16,
        pub lpTitle: *mut u16,
        pub dwX: DWORD,
        pub dwY: DWORD,
        pub dwXSize: DWORD,
        pub dwYSize: DWORD,
        pub dwXCountChars: DWORD,
        pub dwYCountChars: DWORD,
        pub dwFillAttribute: DWORD,
        pub dwFlags: DWORD,
        pub wShowWindow: u16,
        pub cbReserved2: u16,
        pub lpReserved2: *mut u8,
        pub hStdInput: HANDLE,
        pub hStdOutput: HANDLE,
        pub hStdError: HANDLE,
    }

    #[repr(C)]
    pub struct STARTUPINFOEXW {
        pub StartupInfo: STARTUPINFOW,
        pub lpAttributeList: *mut c_void,
    }

    #[repr(C)]
    pub struct PROCESS_INFORMATION {
        pub hProcess: HANDLE,
        pub hThread: HANDLE,
        pub dwProcessId: DWORD,
        pub dwThreadId: DWORD,
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn CreatePipe(
            hReadPipe: *mut HANDLE,
            hWritePipe: *mut HANDLE,
            lpPipeAttributes: *const SECURITY_ATTRIBUTES,
            nSize: DWORD,
        ) -> BOOL;
        pub fn SetHandleInformation(hObject: HANDLE, dwMask: DWORD, dwFlags: DWORD) -> BOOL;
        pub fn CreatePseudoConsole(
            size: COORD,
            hInput: HANDLE,
            hOutput: HANDLE,
            dwFlags: DWORD,
            phPC: *mut HANDLE,
        ) -> HRESULT;
        pub fn ResizePseudoConsole(hPC: HANDLE, size: COORD) -> HRESULT;
        pub fn ClosePseudoConsole(hPC: HANDLE);
        pub fn InitializeProcThreadAttributeList(
            lpAttributeList: *mut c_void,
            dwAttributeCount: DWORD,
            dwFlags: DWORD,
            lpSize: *mut usize,
        ) -> BOOL;
        pub fn UpdateProcThreadAttribute(
            lpAttributeList: *mut c_void,
            dwFlags: DWORD,
            Attribute: usize,
            lpValue: *const c_void,
            cbSize: usize,
            lpPreviousValue: *mut c_void,
            lpReturnSize: *mut usize,
        ) -> BOOL;
        pub fn DeleteProcThreadAttributeList(lpAttributeList: *mut c_void);
        pub fn CreateProcessW(
            lpApplicationName: *const u16,
            lpCommandLine: *mut u16,
            lpProcessAttributes: *const SECURITY_ATTRIBUTES,
            lpThreadAttributes: *const SECURITY_ATTRIBUTES,
            bInheritHandles: BOOL,
            dwCreationFlags: DWORD,
            lpEnvironment: *const c_void,
            lpCurrentDirectory: *const u16,
            lpStartupInfo: *mut STARTUPINFOW,
            lpProcessInformation: *mut PROCESS_INFORMATION,
        ) -> BOOL;
        pub fn ReadFile(
            hFile: HANDLE,
            lpBuffer: *mut c_void,
            nNumberOfBytesToRead: DWORD,
            lpNumberOfBytesRead: *mut DWORD,
            lpOverlapped: *mut c_void,
        ) -> BOOL;
        pub fn WriteFile(
            hFile: HANDLE,
            lpBuffer: *const c_void,
            nNumberOfBytesToWrite: DWORD,
            lpNumberOfBytesWritten: *mut DWORD,
            lpOverlapped: *mut c_void,
        ) -> BOOL;
        pub fn GetExitCodeProcess(hProcess: HANDLE, lpExitCode: *mut DWORD) -> BOOL;
        pub fn TerminateProcess(hProcess: HANDLE, uExitCode: u32) -> BOOL;
        pub fn WaitForSingleObject(hHandle: HANDLE, dwMilliseconds: DWORD) -> DWORD;
        pub fn GetProcessId(Process: HANDLE) -> DWORD;
        pub fn GetCurrentProcess() -> HANDLE;
        pub fn DuplicateHandle(
            hSourceProcessHandle: HANDLE,
            hSourceHandle: HANDLE,
            hTargetProcessHandle: HANDLE,
            lpTargetHandle: *mut HANDLE,
            dwDesiredAccess: DWORD,
            bInheritHandle: BOOL,
            dwOptions: DWORD,
        ) -> BOOL;
        pub fn CloseHandle(hObject: HANDLE) -> BOOL;
    }

    #[link(name = "advapi32")]
    extern "system" {
        /// Like `CreateProcessW` but launches the child under `hToken` — the
        /// restricted/low-IL token that confines a sandboxed agent (ADR-0014).
        /// Identical parameter list to `CreateProcessW` with `hToken` prepended.
        pub fn CreateProcessAsUserW(
            hToken: HANDLE,
            lpApplicationName: *const u16,
            lpCommandLine: *mut u16,
            lpProcessAttributes: *const SECURITY_ATTRIBUTES,
            lpThreadAttributes: *const SECURITY_ATTRIBUTES,
            bInheritHandles: BOOL,
            dwCreationFlags: DWORD,
            lpEnvironment: *const c_void,
            lpCurrentDirectory: *const u16,
            lpStartupInfo: *mut STARTUPINFOW,
            lpProcessInformation: *mut PROCESS_INFORMATION,
        ) -> BOOL;
    }
}

use ffi::HANDLE;

/// A handle owned by us, closed on drop. `Send`+`Sync` because we serialise all
/// access behind the `Mutex`es in `SandboxPty`/`SandboxChild` (mirrors how
/// portable-pty wraps the process/pipe handles).
struct OwnedHandle(HANDLE);
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
    fn raw(&self) -> HANDLE {
        self.0
    }
    /// Duplicate into a second owned handle (e.g. for `try_clone_reader` /
    /// `clone_killer`), with the same access and non-inheritable.
    fn duplicate(&self) -> IoResult<OwnedHandle> {
        // SAFETY: `self.0` is a live handle; the duplicate is written on success.
        unsafe {
            let cur = ffi::GetCurrentProcess();
            let mut dup: HANDLE = std::ptr::null_mut();
            let ok = ffi::DuplicateHandle(
                cur,
                self.0,
                cur,
                &mut dup,
                0,
                0,
                ffi::DUPLICATE_SAME_ACCESS,
            );
            if ok == 0 {
                Err(IoError::last_os_error())
            } else {
                Ok(OwnedHandle(dup))
            }
        }
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != ffi::INVALID_HANDLE_VALUE {
            // SAFETY: handle owned by us, closed exactly once.
            unsafe {
                ffi::CloseHandle(self.0);
            }
        }
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Build a UTF-16, double-null-terminated environment block from key/value
/// pairs. Returns `None` for an empty set (inherit the parent's environment).
fn env_block(env: &[(String, String)]) -> Option<Vec<u16>> {
    if env.is_empty() {
        return None;
    }
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in env {
        block.extend(format!("{}={}", k, v).encode_utf16());
        block.push(0);
    }
    block.push(0);
    Some(block)
}

/// How a spawned process is confined. The owned ConPTY seam is identical for all
/// three; only the process-creation inputs differ (ADR-0014 §3):
///   * [`None`](Confinement::None) — plain `CreateProcessW`, 1-attribute list.
///   * [`AppContainer`](Confinement::AppContainer) — `CreateProcessW` with a
///     2-attribute list (`PSEUDOCONSOLE` + `SECURITY_CAPABILITIES`). The legacy
///     #498 primitive; superseded by `RestrictedToken` (kept for the existing
///     tests / cleanup story until the pivot fully lands).
///   * [`RestrictedToken`](Confinement::RestrictedToken) — `CreateProcessAsUserW`
///     under a restricted/low-IL token, 1-attribute list. The ADR-0014 primitive:
///     no AppContainer object namespace, so libuv named-pipe creation, MSYS
///     `bash`, and loopback all work (resolves #528 / #533 / Blocker #1).
pub enum Confinement<'a> {
    None,
    AppContainer(&'a AppContainerProfile),
    RestrictedToken(&'a RestrictedToken),
}

/// Spawn `command_line` inside the AppContainer described by `profile` (or
/// unconfined when `None`), attached to a fresh ConPTY of `rows`x`cols`.
/// `cwd`/`env` configure the child.
pub fn spawn_in_appcontainer(
    command_line: &str,
    cwd: Option<&str>,
    env: &[(String, String)],
    rows: u16,
    cols: u16,
    profile: Option<&AppContainerProfile>,
) -> Result<(SandboxChild, SandboxPty), String> {
    let confinement = match profile {
        Some(p) => Confinement::AppContainer(p),
        None => Confinement::None,
    };
    spawn_confined(command_line, cwd, env, rows, cols, confinement)
}

/// Spawn `command_line` under the restricted/low-integrity `token` via
/// `CreateProcessAsUserW`, attached to a fresh ConPTY (ADR-0014, #528).
pub fn spawn_with_restricted_token(
    command_line: &str,
    cwd: Option<&str>,
    env: &[(String, String)],
    rows: u16,
    cols: u16,
    token: &RestrictedToken,
) -> Result<(SandboxChild, SandboxPty), String> {
    spawn_confined(command_line, cwd, env, rows, cols, Confinement::RestrictedToken(token))
}

/// The shared owned-ConPTY spawn core. Returns the child (process handle wrapper)
/// and the master PTY (pipes + pseudoconsole) implementing portable-pty's traits.
fn spawn_confined(
    command_line: &str,
    cwd: Option<&str>,
    env: &[(String, String)],
    rows: u16,
    cols: u16,
    confinement: Confinement,
) -> Result<(SandboxChild, SandboxPty), String> {
    let appcontainer = match confinement {
        Confinement::AppContainer(p) => Some(p),
        _ => None,
    };
    let restricted_token = match confinement {
        Confinement::RestrictedToken(t) => Some(t),
        _ => None,
    };
    // SAFETY: every handle created is tracked and closed on its owning struct's
    // drop or explicitly before an early return; structs passed to Win32 are
    // zero-initialised POD with their exact sizes.
    unsafe {
        // --- inheritable pipes: child stdin (we write) + stdout (we read) ---
        let sa = ffi::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<ffi::SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let (stdin_read, stdin_write) = create_pipe(&sa)?;
        let (stdout_read, stdout_write) = create_pipe(&sa)?;
        // Our retained ends must NOT be inherited by the child.
        ffi::SetHandleInformation(stdin_write.raw(), ffi::HANDLE_FLAG_INHERIT, 0);
        ffi::SetHandleInformation(stdout_read.raw(), ffi::HANDLE_FLAG_INHERIT, 0);

        // --- pseudoconsole over the child's ends ---
        let mut hpc: HANDLE = std::ptr::null_mut();
        let hr = ffi::CreatePseudoConsole(
            ffi::COORD { X: cols as i16, Y: rows as i16 },
            stdin_read.raw(),
            stdout_write.raw(),
            ffi::PSEUDOCONSOLE_RESIZE_QUIRK | ffi::PSEUDOCONSOLE_WIN32_INPUT_MODE,
            &mut hpc,
        );
        if hr != ffi::S_OK {
            return Err(format!("CreatePseudoConsole failed: HRESULT 0x{:08X}", hr));
        }
        let pseudo = PseudoConsole(hpc);
        // The console duplicated the child's ends; drop our copies now.
        drop(stdin_read);
        drop(stdout_write);

        // --- proc-thread attribute list: PSEUDOCONSOLE (+ SECURITY_CAPABILITIES
        // for the AppContainer path only; the restricted-token path confines via
        // the token at CreateProcessAsUserW, not a proc-thread attribute) ---
        let attr_count: u32 = if appcontainer.is_some() { 2 } else { 1 };
        let mut size: usize = 0;
        ffi::InitializeProcThreadAttributeList(std::ptr::null_mut(), attr_count, 0, &mut size);
        let mut attr_buf = vec![0u8; size];
        let attr_list = attr_buf.as_mut_ptr() as *mut std::os::raw::c_void;
        if ffi::InitializeProcThreadAttributeList(attr_list, attr_count, 0, &mut size) == 0 {
            return Err(format!("InitializeProcThreadAttributeList failed: {}", IoError::last_os_error()));
        }
        let _attr_guard = AttrListGuard(attr_list);

        if ffi::UpdateProcThreadAttribute(
            attr_list,
            0,
            ffi::PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
            pseudo.0,
            std::mem::size_of::<HANDLE>(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(format!("UpdateProcThreadAttribute(PSEUDOCONSOLE) failed: {}", IoError::last_os_error()));
        }

        let sec_caps = appcontainer.map(|p| p.security_capabilities());
        if let Some(sec_caps) = sec_caps.as_ref() {
            if ffi::UpdateProcThreadAttribute(
                attr_list,
                0,
                ffi::PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
                sec_caps as *const _ as *const std::os::raw::c_void,
                std::mem::size_of_val(sec_caps),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(format!("UpdateProcThreadAttribute(SECURITY_CAPABILITIES) failed: {}", IoError::last_os_error()));
            }
        }

        // --- STARTUPINFOEX + CreateProcess ---
        let mut si: ffi::STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<ffi::STARTUPINFOEXW>() as u32;
        // ConPTY drives stdio via the pseudoconsole; explicitly mark the std
        // handles invalid so the child can't inherit our redirected handles.
        si.StartupInfo.dwFlags = ffi::STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = ffi::INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdOutput = ffi::INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdError = ffi::INVALID_HANDLE_VALUE;
        si.lpAttributeList = attr_list;

        let mut cmdline_w = to_wide(command_line);
        let cwd_w = cwd.map(to_wide);
        let env_w = env_block(env);

        let mut pi: ffi::PROCESS_INFORMATION = std::mem::zeroed();
        // bInheritHandles=FALSE — the ConPTY attaches via the proc-thread
        // attribute, not inherited handles (matches portable-pty).
        let creation_flags = ffi::EXTENDED_STARTUPINFO_PRESENT | ffi::CREATE_UNICODE_ENVIRONMENT;
        let env_ptr = env_w
            .as_ref()
            .map(|b| b.as_ptr() as *const std::os::raw::c_void)
            .unwrap_or(std::ptr::null());
        let cwd_ptr = cwd_w.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
        let (ok, create_fn) = if let Some(token) = restricted_token {
            (
                ffi::CreateProcessAsUserW(
                    token.raw(),
                    std::ptr::null(),
                    cmdline_w.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    creation_flags,
                    env_ptr,
                    cwd_ptr,
                    &mut si.StartupInfo,
                    &mut pi,
                ),
                "CreateProcessAsUserW",
            )
        } else {
            (
                ffi::CreateProcessW(
                    std::ptr::null(),
                    cmdline_w.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    creation_flags,
                    env_ptr,
                    cwd_ptr,
                    &mut si.StartupInfo,
                    &mut pi,
                ),
                "CreateProcessW",
            )
        };
        if ok == 0 {
            return Err(format!("{} failed: {}", create_fn, IoError::last_os_error()));
        }
        // The thread handle is unused; close it so it doesn't leak.
        ffi::CloseHandle(pi.hThread);

        let child = SandboxChild {
            proc: Arc::new(Mutex::new(OwnedHandle(pi.hProcess))),
        };
        let pty = SandboxPty {
            inner: Arc::new(Mutex::new(PtyInner {
                pseudo,
                reader: Some(stdout_read),
                writer: Some(stdin_write),
                rows,
                cols,
            })),
        };
        Ok((child, pty))
    }
}

fn create_pipe(sa: &ffi::SECURITY_ATTRIBUTES) -> Result<(OwnedHandle, OwnedHandle), String> {
    // SAFETY: both ends written on success; wrapped as OwnedHandle for cleanup.
    unsafe {
        let mut read: HANDLE = std::ptr::null_mut();
        let mut write: HANDLE = std::ptr::null_mut();
        if ffi::CreatePipe(&mut read, &mut write, sa, 0) == 0 {
            return Err(format!("CreatePipe failed: {}", IoError::last_os_error()));
        }
        Ok((OwnedHandle(read), OwnedHandle(write)))
    }
}

/// Owns an `HPCON`; closes it on drop.
struct PseudoConsole(HANDLE);
unsafe impl Send for PseudoConsole {}
impl Drop for PseudoConsole {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: created by CreatePseudoConsole, closed once.
            unsafe { ffi::ClosePseudoConsole(self.0) };
        }
    }
}

/// Frees a proc-thread attribute list when the spawn returns.
struct AttrListGuard(*mut std::os::raw::c_void);
impl Drop for AttrListGuard {
    fn drop(&mut self) {
        // SAFETY: initialised by InitializeProcThreadAttributeList, freed once.
        unsafe { ffi::DeleteProcThreadAttributeList(self.0) };
    }
}

// ---------------------------------------------------------------------------
// Child
// ---------------------------------------------------------------------------

/// The sandboxed agent process. Mirrors portable-pty's `WinChild`.
#[derive(Debug)]
pub struct SandboxChild {
    proc: Arc<Mutex<OwnedHandle>>,
}

impl std::fmt::Debug for OwnedHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OwnedHandle({:p})", self.0)
    }
}

impl SandboxChild {
    fn exit_status(&self) -> IoResult<Option<ExitStatus>> {
        let guard = self.proc.lock().unwrap();
        // SAFETY: live process handle.
        unsafe {
            let mut code: ffi::DWORD = 0;
            if ffi::GetExitCodeProcess(guard.raw(), &mut code) == 0 {
                return Ok(None);
            }
            if code == ffi::STILL_ACTIVE {
                Ok(None)
            } else {
                Ok(Some(ExitStatus::with_exit_code(code)))
            }
        }
    }
}

impl ChildKiller for SandboxChild {
    fn kill(&mut self) -> IoResult<()> {
        let guard = self.proc.lock().unwrap();
        // SAFETY: live process handle. Best-effort, like portable-pty's WinChild.
        unsafe {
            ffi::TerminateProcess(guard.raw(), 1);
        }
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(SandboxChildKiller {
            proc: Arc::clone(&self.proc),
        })
    }
}

#[derive(Debug)]
struct SandboxChildKiller {
    proc: Arc<Mutex<OwnedHandle>>,
}

impl ChildKiller for SandboxChildKiller {
    fn kill(&mut self) -> IoResult<()> {
        let guard = self.proc.lock().unwrap();
        // SAFETY: live process handle.
        unsafe {
            ffi::TerminateProcess(guard.raw(), 1);
        }
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(SandboxChildKiller {
            proc: Arc::clone(&self.proc),
        })
    }
}

impl Child for SandboxChild {
    fn try_wait(&mut self) -> IoResult<Option<ExitStatus>> {
        self.exit_status()
    }

    fn wait(&mut self) -> IoResult<ExitStatus> {
        if let Ok(Some(status)) = self.try_wait() {
            return Ok(status);
        }
        let raw = self.proc.lock().unwrap().raw();
        // SAFETY: live process handle; INFINITE wait until exit.
        unsafe {
            ffi::WaitForSingleObject(raw, ffi::INFINITE);
            let mut code: ffi::DWORD = 0;
            if ffi::GetExitCodeProcess(raw, &mut code) == 0 {
                return Err(IoError::last_os_error());
            }
            Ok(ExitStatus::with_exit_code(code))
        }
    }

    fn process_id(&self) -> Option<u32> {
        let raw = self.proc.lock().unwrap().raw();
        // SAFETY: live process handle.
        let id = unsafe { ffi::GetProcessId(raw) };
        if id == 0 {
            None
        } else {
            Some(id)
        }
    }

    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        Some(self.proc.lock().unwrap().raw() as std::os::windows::io::RawHandle)
    }
}

// ---------------------------------------------------------------------------
// MasterPty
// ---------------------------------------------------------------------------

struct PtyInner {
    pseudo: PseudoConsole,
    reader: Option<OwnedHandle>,
    writer: Option<OwnedHandle>,
    rows: u16,
    cols: u16,
}

/// The sandboxed master PTY. Mirrors portable-pty's `ConPtyMasterPty`.
#[derive(Clone)]
pub struct SandboxPty {
    inner: Arc<Mutex<PtyInner>>,
}

impl MasterPty for SandboxPty {
    fn resize(&self, size: PtySize) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        // SAFETY: live HPCON.
        let hr = unsafe {
            ffi::ResizePseudoConsole(
                inner.pseudo.0,
                ffi::COORD { X: size.cols as i16, Y: size.rows as i16 },
            )
        };
        if hr != ffi::S_OK {
            anyhow::bail!("ResizePseudoConsole failed: HRESULT 0x{:08X}", hr);
        }
        inner.rows = size.rows;
        inner.cols = size.cols;
        Ok(())
    }

    fn get_size(&self) -> anyhow::Result<PtySize> {
        let inner = self.inner.lock().unwrap();
        Ok(PtySize {
            rows: inner.rows,
            cols: inner.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
    }

    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn Read + Send>> {
        let inner = self.inner.lock().unwrap();
        let handle = inner
            .reader
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("reader already taken"))?
            .duplicate()?;
        Ok(Box::new(HandleReader(handle)))
    }

    fn take_writer(&self) -> anyhow::Result<Box<dyn Write + Send>> {
        let mut inner = self.inner.lock().unwrap();
        let handle = inner
            .writer
            .take()
            .ok_or_else(|| anyhow::anyhow!("writer already taken"))?;
        Ok(Box::new(HandleWriter(handle)))
    }

    #[cfg(unix)]
    fn process_group_leader(&self) -> Option<i32> {
        None
    }
}

/// `std::io::Read` over a pipe handle (the ConPTY output).
struct HandleReader(OwnedHandle);

impl Read for HandleReader {
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        let mut read: ffi::DWORD = 0;
        // SAFETY: live readable pipe handle; buf is valid for `buf.len()` bytes.
        let ok = unsafe {
            ffi::ReadFile(
                self.0.raw(),
                buf.as_mut_ptr() as *mut std::os::raw::c_void,
                buf.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            // Broken pipe (child exited) reads as EOF, matching PTY semantics.
            let err = IoError::last_os_error();
            const ERROR_BROKEN_PIPE: i32 = 109;
            if err.raw_os_error() == Some(ERROR_BROKEN_PIPE) {
                return Ok(0);
            }
            return Err(err);
        }
        Ok(read as usize)
    }
}

/// `std::io::Write` over a pipe handle (the ConPTY input).
struct HandleWriter(OwnedHandle);

impl Write for HandleWriter {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        let mut written: ffi::DWORD = 0;
        // SAFETY: live writable pipe handle; buf is valid for `buf.len()` bytes.
        let ok = unsafe {
            ffi::WriteFile(
                self.0.raw(),
                buf.as_ptr() as *const std::os::raw::c_void,
                buf.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(IoError::last_os_error());
        }
        Ok(written as usize)
    }

    fn flush(&mut self) -> IoResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// End-to-end: spawn `cmd /c` inside an AppContainer, attached to a ConPTY,
    /// and read its output. Proves the whole owned-spawn path — pseudoconsole +
    /// two-attribute list + SECURITY_CAPABILITIES + CreateProcessW — actually
    /// launches a process into the container and streams its output back.
    ///
    /// ConPTY does NOT signal EOF on the output pipe when the child exits (the
    /// pseudoconsole keeps the write end open) — the same behaviour production
    /// works around in `watch_child_exit` (issue #287). So the read runs on a
    /// thread, and after the child exits we drop the PTY to close the
    /// pseudoconsole, which finally breaks the pipe and ends the read. Without
    /// this the reader would block forever.
    #[test]
    fn spawns_command_in_container_and_reads_output() {
        let profile = AppContainerProfile::create_or_derive(
            "com.alond.buildmesh.test-conpty",
            false,
        )
        .expect("create profile");

        // Keep the child alive with `pause` so ConPTY actually repaints its
        // screen buffer (the echoed line) — `cmd /c echo` alone exits before the
        // async repaint, so the frame with the marker is never emitted. Real
        // agents are long-lived and stream continuously, so they don't hit this.
        let (mut child, pty) = spawn_in_appcontainer(
            "C:\\Windows\\System32\\cmd.exe /c \"echo SANDBOX_OK & pause\"",
            None,
            &[],
            24,
            80,
            Some(&profile),
        )
        .expect("spawn in appcontainer");

        // Stream chunks off the reader thread through a channel. ConPTY renders
        // and flushes the child's output to the pipe asynchronously, so we can't
        // race the child's exit — instead we wait (bounded) for the marker to
        // actually arrive, then close the pseudoconsole to end the reader.
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let mut reader = pty.try_clone_reader().expect("reader");
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(chunk) => {
                    out.extend_from_slice(&chunk);
                    if String::from_utf8_lossy(&out).contains("SANDBOX_OK") {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        // `pause` keeps the child alive, so kill it; then close the
        // pseudoconsole so the reader thread sees EOF, then join.
        child.kill().ok();
        drop(pty);
        let _ = child.wait();
        reader_thread.join().ok();

        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("SANDBOX_OK"),
            "expected child output to contain SANDBOX_OK, got: {:?}",
            text
        );

        profile.delete().ok();
    }

    /// Control: the same owned ConPTY spawn with NO sandbox (1-attribute list).
    /// Isolates "is my ConPTY impl correct?" from "does AppContainer interfere
    /// with the console connection?". If this passes but the sandboxed test
    /// above fails, the blocker is the AppContainer, not the ConPTY plumbing.
    #[test]
    fn plain_conpty_streams_child_output() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let (mut child, pty) = spawn_in_appcontainer(
            "C:\\Windows\\System32\\cmd.exe /c \"echo SANDBOX_OK & pause\"",
            None,
            &[],
            24,
            80,
            None,
        )
        .expect("spawn (no sandbox)");

        let mut reader = pty.try_clone_reader().expect("reader");
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(chunk) => {
                    out.extend_from_slice(&chunk);
                    if String::from_utf8_lossy(&out).contains("SANDBOX_OK") {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        child.kill().ok();
        drop(pty);
        let _ = child.wait();
        reader_thread.join().ok();

        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("SANDBOX_OK"),
            "plain ConPTY (no sandbox) should stream child output, got: {:?}",
            text
        );
    }

    /// End-to-end for the ADR-0014 production path: spawn `cmd /c` under a real
    /// restricted token via `CreateProcessAsUserW` and read its output. This is
    /// the CI cover for the #528 fix's spawn wiring — it exercises the
    /// `Confinement::RestrictedToken` branch (1-attribute list, `CreateProcessAsUserW`)
    /// without needing the live `claude.exe`/auth the `spike_*` tests require. Uses
    /// the strict token (`new(false)`) so it also proves a maximally-restricted
    /// token can still stream ConPTY I/O. Mirrors `plain_conpty_streams_child_output`.
    #[test]
    fn spawns_command_under_restricted_token_and_reads_output() {
        use crate::sandbox::restricted_token::RestrictedToken;
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let token = RestrictedToken::new(false).expect("build restricted token");
        let (mut child, pty) = spawn_with_restricted_token(
            "C:\\Windows\\System32\\cmd.exe /c \"echo SANDBOX_OK & pause\"",
            None,
            &[],
            24,
            80,
            &token,
        )
        .expect("spawn under restricted token");

        let mut reader = pty.try_clone_reader().expect("reader");
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(chunk) => {
                    out.extend_from_slice(&chunk);
                    if String::from_utf8_lossy(&out).contains("SANDBOX_OK") {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        child.kill().ok();
        drop(pty);
        let _ = child.wait();
        reader_thread.join().ok();

        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("SANDBOX_OK"),
            "restricted-token ConPTY (CreateProcessAsUserW) should stream child output, got: {:?}",
            text
        );
    }

    /// Integration: a sandboxed process can read a file in a directory that was
    /// granted to its container SID via `acl::grant_dir` — the mechanism that
    /// confines a real agent to (but lets it use) its worktree. Without the
    /// grant the read would be denied (proven by the Phase 0 spike); here we
    /// prove the grant *opens* exactly that access.
    #[test]
    fn sandboxed_process_reads_granted_directory() {
        use crate::sandbox::acl;
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let dir = std::env::temp_dir().join(format!("bm-acl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("marker.txt"), "GRANTED_READ_OK\n").unwrap();

        let profile =
            AppContainerProfile::create_or_derive("com.alond.buildmesh.test-acl", false)
                .expect("create profile");
        let sid = profile.sid_string().expect("sid string");
        acl::grant_dir(&dir, &sid, acl::Access::Full).expect("grant dir");

        let cmdline = format!(
            "C:\\Windows\\System32\\cmd.exe /c \"type {}\\marker.txt & pause\"",
            dir.display()
        );
        let (mut child, pty) =
            spawn_in_appcontainer(&cmdline, None, &[], 24, 80, Some(&profile)).expect("spawn");

        let mut reader = pty.try_clone_reader().expect("reader");
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(chunk) => {
                    out.extend_from_slice(&chunk);
                    if String::from_utf8_lossy(&out).contains("GRANTED_READ_OK") {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        child.kill().ok();
        drop(pty);
        let _ = child.wait();
        reader_thread.join().ok();

        acl::revoke_dir(&dir, &sid).ok();
        profile.delete().ok();
        let _ = std::fs::remove_dir_all(&dir);

        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("GRANTED_READ_OK"),
            "sandboxed process should read the granted file, got: {:?}",
            text
        );
    }
}
