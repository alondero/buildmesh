//! The native, blocking error surface for a fatal startup failure
//! (issue #1525).
//!
//! Why not the Boot Error Panel
//! ----------------------------
//! `<BootErrorPanel>` is a React component. It renders after the webview has
//! loaded, which requires the database to be open, which is precisely what
//! failed. The failures this module shows are the ones where there is no
//! window, no React, and no SQLite — so the surface has to be a native one,
//! and it has to be reachable from inside Tauri `setup`.
//!
//! Why a Win32 message box, and what it costs
//! -------------------------------------------
//! This runs on the main thread inside `setup`, before the event loop starts.
//! `tauri_plugin_dialog`'s `blocking_show` deadlocks there (it round-trips
//! through the event loop that has not started), which is the same reason
//! `instance_guard::show_fatal_startup_error` raises a raw `MessageBoxW`. A
//! message box is also the only surface a `windows_subsystem = "windows"`
//! release build (no console, `panic = "abort"`) can use to reach a user.
//!
//! The cost is that `MessageBoxW` captions its buttons Yes/No/Cancel and gives
//! no way to relabel them. So the *body* of the message names what each button
//! does ([`StartupFailure::user_text`] ends with an "Available: …" line), and
//! the binding is a pure function ([`bind_actions`]) that is unit-tested on
//! every platform rather than left to be eyeballed on one.
//!
//! The loop
//! --------
//! [`present_failure`] keeps re-showing the dialog until the user quits or a
//! retry succeeds: "copy details" and "open log folder" are things a user
//! wants to do *and then* act on the same information, and a dialog that
//! vanished on the first click would throw the message away. Each iteration
//! blocks in the modal box, so this is not a spin.

use crate::startup::failure::{StartupAction, StartupFailure};

/// Put a fatal startup failure in front of the user, blocking, until they quit
/// or a retry succeeds.
///
/// `retry` is only ever `Some` for a stage that failed before any
/// process-global state was installed, so calling it is a genuine second
/// attempt (see [`StartupStage::is_retry_safe`]). It is `None` on the paths
/// that failed at or after the database, where a retry could not do anything
/// real.
///
/// Returns `Ok(())` when a retry succeeded and startup may continue, or
/// `Err(failure)` when the user quit with the failure still outstanding. A
/// failure returned from here has already been recorded by the caller, so
/// there is no obligation to report it again.
pub(crate) fn present_failure(
    failure: &StartupFailure,
    // `mut` for `as_mut()` below: the loop comes back round to the retry arm
    // after a failed attempt, so the closure handle has to survive the match.
    mut retry: Option<&mut dyn FnMut() -> Result<(), StartupFailure>>,
) -> Result<(), StartupFailure> {
    let mut current = failure.clone();
    loop {
        let choice = ask(&current);
        match choice {
            StartupAction::CopyDetails => {
                copy_to_clipboard(&current.clipboard_text());
                // Stay up: the user has just taken the details, and is very
                // likely to want to read the message again.
            }
            StartupAction::OpenLogFolder => {
                if let Some(dir) = current.log_path().and_then(|log| log.parent()) {
                    open_in_file_manager(dir);
                }
            }
            StartupAction::Retry => {
                // Unreachable while `actions()` is the only source of the
                // offered set (Retry is only offered for retry-safe stages),
                // but degrade to "stay up" rather than quitting on a user.
                if let Some(retry) = retry.as_mut() {
                    match retry() {
                        Ok(()) => return Ok(()),
                        // A failed retry is new information, not a repeat:
                        // report it and let the user decide whether to try again.
                        Err(next) => {
                            crate::startup::record(&next);
                            current = next;
                        }
                    }
                }
            }
            StartupAction::Quit => return Err(current),
        }
    }
}

// ---------------------------------------------------------------------------
// Windows: the native surface
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
mod platform {
    use std::path::Path;

    use windows_sys::Win32::Foundation::{GlobalFree, HANDLE};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDNO, IDYES, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_YESNO,
        MB_YESNOCANCEL, SW_SHOWNORMAL,
    };

    use super::{bind_actions, DialogKind, StartupAction, StartupFailure};

    /// `CF_UNICODETEXT` from the Win32 clipboard-format registry. windows-sys
    /// does not export the `CF_*` clipboard-format constants, so the header
    /// value is restated here rather than invented.
    const CF_UNICODETEXT: u32 = 13;

    /// `OpenClipboard` fails while *another* application holds the clipboard,
    /// which is transient and common. A copy button that gives up instantly
    /// would look broken, so it retries briefly before reporting failure.
    const CLIPBOARD_OPEN_ATTEMPTS: u32 = 5;
    const CLIPBOARD_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(60);

    /// NUL-terminated UTF-16 for the `*W` Win32 entry points.
    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// NUL-terminated UTF-16 for a path, which may be non-UTF-8 on disk.
    fn wide_path(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    use std::os::windows::ffi::OsStrExt;

    /// Show the modal box and return what the user chose.
    ///
    /// `MB_SETFOREGROUND` because this runs on the main thread before the
    /// event loop has started, so nothing else will raise the window for us.
    pub(super) fn ask(failure: &StartupFailure) -> StartupAction {
        let dialog = bind_actions(&failure.actions());
        // The legend is appended from the same `dialog` that maps the answer
        // below, so the text can never disagree with the buttons.
        let text = wide(&format!(
            "{}{}",
            failure.user_text(),
            dialog.legend()
        ));
        let caption = wide("Buildmesh could not start");
        let style = match dialog.kind {
            DialogKind::Ok => MB_OK,
            DialogKind::YesNo => MB_YESNO,
            DialogKind::YesNoCancel => MB_YESNOCANCEL,
        } | MB_ICONERROR
            | MB_SETFOREGROUND;

        // SAFETY: both strings are NUL-terminated by `wide`, and a null owner
        // HWND asks for an application-modal box rather than one tied to a
        // window that does not exist yet.
        let answer = unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                caption.as_ptr(),
                style,
            )
        };

        match answer {
            IDYES => dialog.yes.unwrap_or(dialog.dismiss),
            IDNO => dialog.no.unwrap_or(dialog.dismiss),
            // `IDCANCEL`, Escape, and closing the box are all the same intent:
            // the user wants out. Treating any unknown answer as "quit" is the
            // safe default on a path where the app is already ending.
            _ => dialog.dismiss,
        }
    }

    /// Put `text` on the clipboard as UTF-16.
    ///
    /// The global memory block is allocated and filled *before* the clipboard
    /// is opened: `EmptyClipboard` destroys whatever the user had copied, so
    /// it must not run until the replacement is ready to go in.
    pub(super) fn copy_to_clipboard(text: &str) {
        let mut units: Vec<u16> = text.encode_utf16().collect();
        units.push(0);
        let byte_len = units.len() * std::mem::size_of::<u16>();

        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE, byte_len);
            if handle.is_null() {
                return;
            }
            let locked = GlobalLock(handle);
            if locked.is_null() {
                GlobalFree(handle);
                return;
            }
            std::ptr::copy_nonoverlapping(units.as_ptr().cast::<u8>(), locked.cast::<u8>(), byte_len);
            // A `GMEM_MOVEABLE` block that is still locked has a lock count, and
            // the return value reports that; the count is not an error, so the
            // return is deliberately ignored.
            let _ = GlobalUnlock(handle);

            let mut opened = false;
            for _ in 0..CLIPBOARD_OPEN_ATTEMPTS {
                if OpenClipboard(std::ptr::null_mut()) != 0 {
                    opened = true;
                    break;
                }
                std::thread::sleep(CLIPBOARD_RETRY_DELAY);
            }
            if !opened {
                GlobalFree(handle);
                return;
            }

            EmptyClipboard();
            // On success the system takes ownership of the block. On failure it
            // is still ours, so it must be freed rather than leaked.
            if SetClipboardData(CF_UNICODETEXT, handle as HANDLE).is_null() {
                GlobalFree(handle);
            }
            CloseClipboard();
        }
    }

    /// Open the profile's `logs` directory in the OS file manager.
    pub(super) fn open_in_file_manager(dir: &Path) {
        let verb = wide("open");
        let target = wide_path(dir);
        // SAFETY: both strings are NUL-terminated; the two unused parameters
        // are null, which the shell documents as "no parameters / no working
        // directory". The return value is an `HINSTANCE`-shaped status where
        // anything below 32 is a shell error code, and there is nothing useful
        // to do about it here — the log path is already on screen.
        unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            );
        }
    }
}

#[cfg(target_os = "windows")]
use platform::{ask, copy_to_clipboard, open_in_file_manager};

/// Off Windows there is no modal surface to raise from a
/// `windows_subsystem`-style build, so the reason goes to stderr — a
/// developer or CI launch sees it there.
///
/// The button legend is printed too: it is the platform-independent half of
/// the surface, and it tells whoever reads a CI log exactly what the same
/// failure would have offered a user on Windows.
///
/// A `retry` closure is deliberately not offered: with no interactive surface
/// there is nobody to press it, and silently calling it in a loop would
/// re-run the failing step for no reason.
#[cfg(not(target_os = "windows"))]
fn ask(failure: &StartupFailure) -> StartupAction {
    let dialog = bind_actions(&failure.actions());
    eprintln!("{}{}", failure.user_text(), dialog.legend());
    StartupAction::Quit
}

#[cfg(not(target_os = "windows"))]
fn copy_to_clipboard(_text: &str) {}

#[cfg(not(target_os = "windows"))]
fn open_in_file_manager(_dir: &std::path::Path) {}

// ---------------------------------------------------------------------------
// Button binding (pure — unit-tested on every platform)
// ---------------------------------------------------------------------------

/// The `MessageBoxW` style, and what each of its buttons does.
///
/// The native surface has fixed captions, so the mapping is: the first offered
/// action is Yes, the second is No, and closing the box (Cancel, Escape, or
/// the window's X) is `dismiss`, which is always [`StartupAction::Quit`].
/// Nothing else is a way out.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Dialog {
    /// `MB_OK` / `MB_YESNO` / `MB_YESNOCANCEL`.
    kind: DialogKind,
    yes: Option<StartupAction>,
    no: Option<StartupAction>,
    dismiss: StartupAction,
}

impl Dialog {
    /// One line per button, naming the button's real caption and what it does.
    ///
    /// Keyed on `kind`, **not** on which actions happen to be bound: the three
    /// styles have different captions. An earlier version derived the lines
    /// from `yes`/`no` being present, which printed "Cancel" for an `MB_OK` box
    /// (whose only button is OK) and printed a phantom "Cancel" alongside a
    /// duplicate quit line for an `MB_YESNO` box (which has no Cancel at all).
    ///
    /// Built from the very same [`Dialog`] that maps the answer in `ask`, so the
    /// legend cannot describe a button the code does not have. `MessageBoxW`
    /// will not relabel its captions, so without this the user faces buttons
    /// whose meaning is guesswork.
    fn legend(&self) -> String {
        // `bind_actions` is the only constructor and it keeps `kind` and the
        // bound actions in step, so each arm's `expect` is unreachable rather
        // than a guess. `unreachable!` would panic on a fatal-error path, so a
        // missing binding degrades to the quit line instead.
        let line = |action: Option<StartupAction>| {
            action.map(|action| action.to_string()).unwrap_or_default()
        };
        let lines: Vec<String> = match self.kind {
            DialogKind::Ok => vec![format!("OK: {}", line(self.yes.or(self.no)))],
            DialogKind::YesNo => vec![
                format!("Yes: {}", line(self.yes)),
                format!("No: {}", line(self.no)),
            ],
            DialogKind::YesNoCancel => vec![
                format!("Yes: {}", line(self.yes)),
                format!("No: {}", line(self.no)),
                // Closing the box (Cancel, Escape, or the window's X) all land
                // on the same answer, so the caption covers all three.
                format!("Cancel, Esc, or the window's X: {}", self.dismiss),
            ],
        };
        format!("\n{}", lines.join("\n"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DialogKind {
    /// A single acknowledgement. Only used when the failure has nothing but
    /// "quit" to offer, which the action list never produces — `CopyDetails`
    /// is always there — so this exists to keep the mapping total.
    Ok,
    YesNo,
    YesNoCancel,
}

/// Bind a failure's action list onto the message box's fixed buttons.
///
/// `Quit` is required to be last (it is how the user leaves), so everything
/// before it fills Yes then No, and the dismiss path is `Quit`. Returns `None`
/// only for an empty list, which cannot be produced by
/// [`StartupFailure::actions`] — the fallback is a bare acknowledgement rather
/// than a panic on the error path.
fn bind_actions(actions: &[StartupAction]) -> Dialog {
    let offered: Vec<StartupAction> = actions
        .iter()
        .copied()
        .filter(|action| *action != StartupAction::Quit)
        .collect();
    let dismiss = actions
        .iter()
        .rev()
        .find(|action| **action == StartupAction::Quit)
        .copied()
        .unwrap_or(StartupAction::Quit);

    match offered.as_slice() {
        [] => Dialog {
            kind: DialogKind::Ok,
            yes: None,
            no: None,
            dismiss,
        },
        [only] => Dialog {
            kind: DialogKind::YesNo,
            yes: Some(*only),
            no: Some(dismiss),
            dismiss,
        },
        [first, second, ..] => Dialog {
            kind: DialogKind::YesNoCancel,
            yes: Some(*first),
            no: Some(*second),
            dismiss,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::startup::failure::StartupStage;
    use std::path::{Path, PathBuf};

    /// SQLite's documented primary result codes, restated because
    /// `libsqlite3-sys` keeps its own copies private. `ffi::Error::new` takes
    /// the raw code and maps it to an `ErrorCode` itself.
    mod code {
        pub const CANTOPEN: i32 = 14;
        pub const NOTADB: i32 = 26;
    }

    fn database_failure(corrupt: bool) -> StartupFailure {
        let raw_code = if corrupt { code::NOTADB } else { code::CANTOPEN };
        StartupFailure::database(
            Path::new("/profiles/stable/buildmesh.db"),
            &rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(raw_code), None),
        )
        .with_paths(
            Some(PathBuf::from("/profiles/stable/logs/buildmesh.log")),
            Some(PathBuf::from("/profiles/stable")),
        )
    }

    /// Every real failure must render as a dialog whose buttons are all wired
    /// to actions, and whose dismiss path quits. This is the "no dead end"
    /// invariant of the native surface, checked on every platform.
    #[test]
    fn every_real_failure_binds_to_a_usable_dialog() {
        let cases = [
            StartupFailure::app_data("no dir", "denied", Some(PathBuf::from("/p"))),
            StartupFailure::app_data("no dir", "denied", None),
            StartupFailure::log_directory(
                "no logs",
                "denied",
                Some(PathBuf::from("/p/logs")),
                Some(PathBuf::from("/p")),
            ),
            database_failure(true),
            database_failure(false),
            StartupFailure::services("the recovery sweep", "no such table"),
        ];
        for failure in &cases {
            let actions = failure.actions();
            let dialog = bind_actions(&actions);
            assert_eq!(
                dialog.dismiss,
                StartupAction::Quit,
                "closing the dialog must quit: {failure:?}"
            );
            match dialog.kind {
                DialogKind::Ok => assert!(
                    actions.is_empty() || actions == [StartupAction::Quit],
                    "a bare acknowledgement must have nothing else to offer: {failure:?}"
                ),
                DialogKind::YesNo => {
                    assert_eq!(dialog.yes, Some(StartupAction::CopyDetails));
                    assert_eq!(dialog.no, Some(StartupAction::Quit));
                }
                DialogKind::YesNoCancel => {
                    assert_eq!(dialog.yes, Some(StartupAction::CopyDetails));
                    assert!(matches!(
                        dialog.no,
                        Some(StartupAction::OpenLogFolder) | Some(StartupAction::Retry)
                    ));
                }
            }
        }
    }

    /// A retry-safe stage gets its own button; a database failure must not,
    /// because `db::init` would short-circuit to `Ok(())` on the second call
    /// and the user would be told the retry worked.
    #[test]
    fn retry_button_appears_only_for_retry_safe_stages() {
        let retryable = bind_actions(
            &StartupFailure::app_data("no dir", "denied", Some(PathBuf::from("/p"))).actions(),
        );
        assert_eq!(retryable.kind, DialogKind::YesNoCancel);
        assert_eq!(retryable.no, Some(StartupAction::Retry));

        let not_retryable = bind_actions(&database_failure(true).actions());
        assert_eq!(not_retryable.kind, DialogKind::YesNoCancel);
        assert_eq!(not_retryable.no, Some(StartupAction::OpenLogFolder));
        assert_ne!(not_retryable.no, Some(StartupAction::Retry));
    }

    /// The log-folder button is offered only when the log holds this
    /// failure's record — a `LogDirectory` failure falls back to copy + retry.
    #[test]
    fn log_folder_button_requires_a_log_that_holds_the_failure() {
        let log_failure = bind_actions(
            &StartupFailure::log_directory(
                "could not open the log",
                "denied",
                Some(PathBuf::from("/p/logs")),
                Some(PathBuf::from("/p")),
            )
            .actions(),
        );
        assert_eq!(log_failure.no, Some(StartupAction::Retry));
        assert_ne!(log_failure.no, Some(StartupAction::OpenLogFolder));
    }

    /// A defensive fallback: a malformed action list must still produce a
    /// dialog that quits, not a panic on the error path (where a panic would
    /// replace the real message with a truncated one).
    #[test]
    fn an_empty_action_list_still_quits() {
        let dialog = bind_actions(&[]);
        assert_eq!(dialog.kind, DialogKind::Ok);
        assert_eq!(dialog.dismiss, StartupAction::Quit);
    }

    /// `MessageBoxW` will not relabel its captions, so the legend is the only
    /// thing telling the user what those buttons do. It must name exactly the
    /// captions the chosen style renders — and nothing else. A legend that
    /// mentions a button the box does not have is worse than none, because the
    /// user will hunt for it.
    #[test]
    fn the_legend_names_exactly_the_captions_the_dialog_renders() {
        let cases = [
            StartupFailure::app_data("no dir", "denied", Some(PathBuf::from("/p"))),
            StartupFailure::app_data("no dir", "denied", None),
            StartupFailure::log_directory(
                "no logs",
                "denied",
                Some(PathBuf::from("/p/logs")),
                Some(PathBuf::from("/p")),
            ),
            database_failure(true),
            database_failure(false),
        ];
        for failure in &cases {
            let dialog = bind_actions(&failure.actions());
            let legend = dialog.legend();

            // An MB_OK box has no Yes, No, or Cancel.
            if dialog.kind == DialogKind::Ok {
                assert!(legend.contains("OK:"), "{legend}");
                for absent in ["Yes:", "No:", "Cancel"] {
                    assert!(
                        !legend.contains(absent),
                        "MB_OK has no {absent:?} button, but the legend says: {legend}"
                    );
                }
                continue;
            }

            // An MB_YESNO box has no Cancel.
            if dialog.kind == DialogKind::YesNo {
                assert!(
                    !legend.contains("Cancel"),
                    "MB_YESNO has no Cancel button, but the legend says: {legend}"
                );
            }

            // Whatever the style, every bound action's own words appear.
            let mut bound: Vec<StartupAction> = dialog.yes.into_iter().chain(dialog.no).collect();
            if dialog.kind == DialogKind::YesNoCancel {
                bound.push(dialog.dismiss);
            }
            for action in bound {
                assert!(
                    legend.contains(&action.to_string()),
                    "the legend omits {action} for {failure:?}: {legend}"
                );
            }
            // And the caption lines are exactly the ones the style renders.
            let expected_lines = match dialog.kind {
                DialogKind::Ok => 1,
                DialogKind::YesNo => 2,
                DialogKind::YesNoCancel => 3,
            };
            assert_eq!(
                legend.trim().lines().count(),
                expected_lines,
                "wrong number of button lines for {failure:?}: {legend}"
            );
        }
    }

    /// A retry-safe failure is capped at three actions even when a caller also
    /// hands it a log path — the fourth would be silently dropped by the
    /// presenter, and `Retry` is the one worth keeping.
    #[test]
    fn a_retry_safe_failure_with_a_log_path_still_fits_three_actions() {
        let failure = StartupFailure::log_directory(
            "Buildmesh could not open its log file.",
            "Access is denied",
            Some(PathBuf::from("/p/logs")),
            Some(PathBuf::from("/p")),
        )
        // The collision the invariant exists for: a retry-safe stage that also
        // carries a log path.
        .with_paths(
            Some(PathBuf::from("/p/logs/buildmesh.log")),
            Some(PathBuf::from("/p")),
        );
        let actions = failure.actions();
        assert!(
            actions.len() <= 3,
            "the presenter has three buttons: {actions:?}"
        );
        assert!(
            actions.contains(&StartupAction::Retry),
            "retry is the actionable one to keep: {actions:?}"
        );
        assert!(
            !actions.contains(&StartupAction::OpenLogFolder),
            "a stage with no log of its own must not offer the folder: {actions:?}"
        );

        let legend = bind_actions(&actions).legend();
        assert!(
            legend.contains(&StartupAction::Retry.to_string()),
            "the surviving action must be named: {legend}"
        );
        assert!(
            !legend.contains("folder"),
            "the dropped action must not be named: {legend}"
        );
    }

    /// A bare action list is not a legend: the whole point is that the user can
    /// tell which caption does what, so the two must not be confused.
    #[test]
    fn the_failure_text_itself_names_no_buttons() {
        let failure = database_failure(true).with_paths(
            Some(PathBuf::from("/p/logs/buildmesh.log")),
            Some(PathBuf::from("/p")),
        );
        let text = failure.user_text();
        for caption in ["Yes:", "No:", "Cancel"] {
            assert!(
                !text.contains(caption),
                "button captions belong to the presenter, not the failure text: {text}"
            );
        }
        // The clipboard payload still records what was on offer, for a report.
        assert!(failure.clipboard_text().contains("Available actions:"));
    }

    /// The retry loop must be driven by the *stage* rule, not by which
    /// platform we happen to be on: this documents that a retryable failure
    /// is one whose stage permits it.
    #[test]
    fn retry_is_gated_on_the_stage_not_the_platform() {
        assert!(StartupStage::AppData.is_retry_safe());
        assert!(!StartupStage::Database.is_retry_safe());
    }
}
