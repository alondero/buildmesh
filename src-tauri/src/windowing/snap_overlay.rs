//! The native child window that makes Windows offer Snap Layouts.
//!
//! See the parent module for why this exists. The setup is copied from the
//! recipe in `Zbrooklyn/tauri-snap-layouts` (MIT), which verified it against a
//! live window on Windows 11 build 26200 at 100% and 150% scaling — five
//! independent Tauri plugins converged on the identical technique, so the
//! constraints below are load-bearing rather than stylistic:
//!
//! - **No extended styles.** `WS_EX_LAYERED` costs the hit test and
//!   `WS_EX_TRANSPARENT` makes the window hit-test-transparent — either one
//!   defeats the entire purpose.
//! - `WS_CLIPSIBLINGS` so the overlay is not painted over by its sibling, the
//!   WebView2 host HWND.
//! - Invisibility comes from *never painting* (`NULL_BRUSH`, no `WM_PAINT`
//!   handling), never from alpha.
//! - **`WM_CLOSE` is advisory, so the lifecycle hangs off destruction instead.**
//!   Buildmesh vetoes the close request whenever the exit-confirmation modal is
//!   up (`WindowCloseGuard` → `cancel_window_close`), so tearing the overlay down
//!   on `WM_CLOSE` would silently kill Snap Layouts for the rest of the session
//!   the moment a user cancels an exit — the exact silent-failure class this
//!   module exists to avoid. Teardown runs on `WM_NCDESTROY`, which only fires
//!   when the window really is going away.
//!
//! Subclassing the Tauri window to answer `WM_NCHITTEST` — Microsoft's
//! documented approach — cannot work here, and fails in a recognisable way: the
//! button keeps its CSS `:hover` and its HTML tooltip, which means the webview
//! received the mouse and our window's procedure was never consulted.

use std::sync::Once;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use tauri::{Emitter, WebviewWindow};
use windows_sys::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::{GetStockObject, NULL_BRUSH},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        HiDpi::GetDpiForWindow,
        Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT},
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, IsIconic, IsWindow,
            RegisterClassExW, SetWindowPos, ShowWindow, CS_HREDRAW, CS_VREDRAW, HTMAXBUTTON,
            HWND_TOP, SWP_ASYNCWINDOWPOS, SWP_SHOWWINDOW, SW_HIDE, WM_DPICHANGED, WM_NCDESTROY,
            WM_NCHITTEST, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE,
            WM_SIZE, WNDCLASSEXW, WS_CHILD, WS_CLIPSIBLINGS, WS_VISIBLE,
        },
    },
};

use super::{overlay_rect, MAXIMIZE_METRICS};

/// Overlay → frontend events.
///
/// The overlay owns the mouse over the maximise button, so these replace the
/// DOM's own hover and click signals *for that one button*. Everything else in
/// the title bar still behaves normally. The click event exists because the
/// overlay swallows the mouse click; `toggleMaximize` is then driven from here,
/// through the same call the DOM handler makes, so the single-writer
/// `isMaximized` contract (only the `onResized` re-query writes the glyph
/// state) is untouched.
pub const EVENT_HOVER: &str = "titlebar-overlay:hover";
pub const EVENT_LEAVE: &str = "titlebar-overlay:leave";
pub const EVENT_PRESS: &str = "titlebar-overlay:press";
pub const EVENT_RELEASE: &str = "titlebar-overlay:release";
pub const EVENT_CLICK: &str = "titlebar-overlay:click";

/// `SetWindowSubclass` identity for our hook on the parent window. Any unique
/// value works; this one spells `"tfsnap"` in ASCII so it is recognisable if
/// the message stream ever shows up in a debugger.
const SUBCLASS_ID: usize = 0x7466_736e_6170;

/// A raw window handle that is allowed to cross threads.
///
/// `HWND` is a raw pointer and so not `Send`, but the overlay state is touched
/// from two threads: the IPC command (which installs) and the wndprocs, which
/// only ever run on the thread owning the message loop. The pointer is
/// dereferenced exclusively on the main thread, and `teardown` destroys the
/// overlay before its entry leaves `STATE`, so a handle can never outlive the
/// window it names.
#[derive(Clone, Copy, PartialEq)]
struct SendHwnd(HWND);

unsafe impl Send for SendHwnd {}

impl SendHwnd {
    fn get(self) -> HWND {
        self.0
    }
}

struct OverlayState {
    parent: SendHwnd,
    overlay: SendHwnd,
    /// The window events are emitted through.
    window: WebviewWindow,
    hovering: bool,
    pressing: bool,
}

static STATE: Mutex<Option<OverlayState>> = Mutex::new(None);

/// Register the overlay window class exactly once per process.
fn register_class() {
    static REGISTERED: Once = Once::new();
    REGISTERED.call_once(|| {
        // SAFETY: the class name outlives the call (it lives in a `OnceLock`),
        // which is all `RegisterClassExW` requires — it copies the name.
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(overlay_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: module_instance(),
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: GetStockObject(NULL_BRUSH),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name().as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            RegisterClassExW(&class);
        }
    });
}

fn class_name() -> &'static [u16] {
    static NAME: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
    NAME.get_or_init(|| "BuildmeshSnapOverlay\0".encode_utf16().collect())
}

fn module_instance() -> HINSTANCE {
    // SAFETY: a null module name asks for the handle of the running executable.
    unsafe { GetModuleHandleW(std::ptr::null()) }
}

/// Create the overlay, or reposition it if it already exists for this window.
///
/// Idempotent by design: the frontend reports metrics on mount and again on
/// resize, and every report lands here.
pub fn install_or_update(window: &WebviewWindow) -> Result<()> {
    // Wrapped before the closure: `run_on_main_thread` requires a `Send`
    // closure, and a bare `HWND` is a raw pointer.
    let parent = SendHwnd(
        window
            .hwnd()
            .context("snap overlay needs a Win32 window handle")?
            .0,
    );
    // Owned clone for the closure — the call itself borrows `window`.
    let owned = window.clone();

    window.run_on_main_thread(move || {
        // SAFETY: `run_on_main_thread` guarantees we are on the thread that
        // owns the window's message loop — the only thread permitted to create
        // or subclass that window's children.
        unsafe { install_on_main(parent, owned) };
    })?;

    Ok(())
}

unsafe fn install_on_main(parent: SendHwnd, window: WebviewWindow) {
    // Hold the lock in a scope so no guard is alive while we call into Win32.
    let already_installed = {
        let guard = STATE.lock();
        guard
            .as_ref()
            .is_some_and(|state| state.parent == parent)
    };
    if already_installed {
        reposition(parent.get());
        return;
    }

    teardown();
    register_class();

    let class = class_name();
    // The style set is deliberately minimal. `WS_CLIPSIBLINGS` keeps the overlay
    // from being painted over by its sibling, the WebView2 host. There are no
    // extended styles: `WS_EX_LAYERED` costs the hit test and
    // `WS_EX_TRANSPARENT` makes the window hit-test-transparent. Invisibility
    // comes from never painting, not from alpha. (`WS_OVERLAPPED` is
    // `0x00000000` and contradicts `WS_CHILD`, so it is not listed.)
    let overlay = CreateWindowExW(
        0,
        class.as_ptr(),
        class.as_ptr(),
        WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
        0,
        0,
        0,
        0,
        parent.get(),
        std::ptr::null_mut(),
        module_instance(),
        std::ptr::null(),
    );

    if overlay.is_null() {
        tracing::warn!("snap overlay: CreateWindowExW failed; Snap Layouts unavailable");
        return;
    }

    *STATE.lock() = Some(OverlayState {
        parent,
        overlay: SendHwnd(overlay),
        window,
        hovering: false,
        pressing: false,
    });

    SetWindowSubclass(parent.get(), Some(parent_subclass_proc), SUBCLASS_ID, 0);
    reposition(parent.get());
    tracing::debug!("snap overlay installed over the maximise button");
}

/// Destroy the overlay and drop its state.
///
/// Called from `WM_NCDESTROY` — the parent is genuinely going away — and from
/// `install_on_main` when it needs to replace a stale overlay. Deliberately NOT
/// from `WM_CLOSE`: that message is advisory while the exit-confirmation modal
/// can still veto the close (see the module doc), so hanging teardown off it
/// would tear the overlay down on a cancelled exit.
fn teardown() {
    let Some(state) = STATE.lock().take() else {
        return;
    };

    // SAFETY: both handles came from the main thread and are torn down on it.
    unsafe {
        RemoveWindowSubclass(state.parent.get(), Some(parent_subclass_proc), SUBCLASS_ID);
        // By `WM_NCDESTROY` the parent's children have already been destroyed, so
        // the handle is normally stale — and a stale handle may since have been
        // recycled, so ask before destroying rather than firing blind.
        if IsWindow(state.overlay.get()) != 0 {
            DestroyWindow(state.overlay.get());
        }
    }
}

/// Move the overlay onto the measured button box. Called on install, on each
/// metrics update, and on every parent resize or DPI change.
///
/// A minimized window has an empty client area, so the arithmetic would place
/// the overlay at a negative x for no benefit — and it would still be a live
/// hit-test target if anything asked. Park it hidden instead; the restore's
/// `WM_SIZE` brings it back, because the normal path passes `SWP_SHOWWINDOW`.
unsafe fn reposition(parent: HWND) {
    let Some(metrics) = *MAXIMIZE_METRICS.lock() else {
        return;
    };

    let mut client: RECT = std::mem::zeroed();
    if GetClientRect(parent, &mut client) == 0 {
        return;
    }

    // Read the handle, then release the lock: no Win32 call below should run
    // with `STATE` held, because a synchronous message could re-enter
    // `with_state` and deadlock on the non-reentrant mutex. Handles only ever
    // change on this thread (`install_on_main` and `teardown` both run here), so
    // there is nothing to race with between dropping the lock and using it.
    let overlay = {
        let guard = STATE.lock();
        match guard.as_ref() {
            Some(state) => state.overlay.get(),
            None => return,
        }
    };

    if IsIconic(parent) != 0 || client.right <= 0 || client.bottom <= 0 {
        ShowWindow(overlay, SW_HIDE);
        return;
    }

    let (x, y, width, height) = overlay_rect(&metrics, client.right, GetDpiForWindow(parent));

    SetWindowPos(
        overlay,
        HWND_TOP,
        x,
        y,
        width,
        height,
        SWP_ASYNCWINDOWPOS | SWP_SHOWWINDOW,
    );
}

/// Run `f` against the state owning `overlay`, if there is one.
fn with_state<R>(overlay: HWND, f: impl FnOnce(&mut OverlayState) -> R) -> Option<R> {
    let mut guard = STATE.lock();
    let state = guard.as_mut()?;
    if state.overlay.get() != overlay {
        return None;
    }
    Some(f(state))
}

/// Emit `event` to the frontend through the window the overlay belongs to.
fn emit_event(overlay: HWND, event: &str) {
    let window = {
        let guard = STATE.lock();
        guard
            .as_ref()
            .filter(|state| state.overlay.get() == overlay)
            .map(|state| state.window.clone())
    };

    if let Some(window) = window {
        let _ = window.emit(event, ());
    }
}

unsafe extern "system" fn overlay_proc(
    overlay: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // The whole point: Windows offers Snap Layouts only to a window that
        // answers this way for the maximise button's rectangle.
        WM_NCHITTEST => return HTMAXBUTTON as LRESULT,

        WM_NCMOUSEMOVE => {
            // The event *and* the tracking are edge-triggered. WM_NCMOUSEMOVE
            // fires per pixel, the coordinates are not needed because the button
            // does not move, and once armed, non-client tracking stays armed
            // until WM_NCMOUSELEAVE — so re-arming on every pixel would be a
            // syscall per pixel for nothing.
            let entered = with_state(overlay, |state| {
                !std::mem::replace(&mut state.hovering, true)
            })
            .unwrap_or(false);

            if entered {
                // Ask for WM_NCMOUSELEAVE, or we never learn the pointer left.
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE | TME_NONCLIENT,
                    hwndTrack: overlay,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut track);
                emit_event(overlay, EVENT_HOVER);
            }
            return 0;
        }

        WM_NCMOUSELEAVE => {
            with_state(overlay, |state| {
                state.hovering = false;
                state.pressing = false;
            });
            emit_event(overlay, EVENT_LEAVE);
            return 0;
        }

        WM_NCLBUTTONDOWN => {
            with_state(overlay, |state| state.pressing = true);
            emit_event(overlay, EVENT_PRESS);
            return 0;
        }

        WM_NCLBUTTONUP => {
            // Only a press that ended on this window counts as a click, so a
            // press-and-drag-away does not maximise.
            let pressed =
                with_state(overlay, |state| std::mem::take(&mut state.pressing)).unwrap_or(false);

            emit_event(overlay, EVENT_RELEASE);
            if pressed {
                emit_event(overlay, EVENT_CLICK);
            }
            return 0;
        }

        _ => {}
    }

    DefWindowProcW(overlay, msg, wparam, lparam)
}

unsafe extern "system" fn parent_subclass_proc(
    parent: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    _ref_data: usize,
) -> LRESULT {
    match msg {
        // Keep the overlay pinned to the button across resizes and DPI moves.
        // Repositioning reads the stored right-inset, so it tracks a live drag
        // without waiting for the frontend to re-measure. This also covers the
        // minimize → restore round trip, which re-shows the overlay.
        WM_SIZE | WM_DPICHANGED => reposition(parent),
        // Real destruction, not the advisory `WM_CLOSE` — see `teardown`.
        WM_NCDESTROY => teardown(),
        _ => {}
    }

    DefSubclassProc(parent, msg, wparam, lparam)
}
