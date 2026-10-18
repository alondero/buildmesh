import { _invoke } from './_invoke';
import type { DiagnosticPaths } from '../../types/generated/DiagnosticPaths';

/** The absolute log/profile locations this process resolved at startup
 *  (issue #1525). The backend opened these files before the database, so this
 *  is the only source that can tell a user where their log actually is. */
export const getDiagnosticPaths = () =>
  _invoke<DiagnosticPaths>('get_diagnostic_paths');

/** Retract a vetoed window close (issue #1501).
 *  Clears the backend's eager expected-exit marking (`USER_CLOSE_REQUESTED`
 *  + watchdog marker) when the user backs out of the exit-confirmation
 *  modal, so a later real crash still auto-relaunches. */
export const cancelWindowClose = () =>
  _invoke('cancel_window_close');

/** Confirmed exit (issue #1501): backend lifecycle shutdown for the
 *  exit-confirmation modal. A custom command, not a window IPC — window
 *  commands are ACL-gated and the modal must not depend on the
 *  compiled-in capability set. */
export const exitApplication = () =>
  _invoke('exit_application');

/** Native Windows Snap Layouts for the bespoke title bar (ADR-0035).
 *
 *  The window controls can't get the Windows 11 snap flyout from CSS or JS:
 *  the shell only offers it to a window whose `WM_NCHITTEST` answers
 *  `HTMAXBUTTON`, and our page lives in a WebView2 child HWND that answers the
 *  hit test first. A native child window is parked over the maximise button
 *  instead, and this reports that button's real box so it lands exactly on top
 *  of it.
 *
 *  Logical (CSS) pixels — the backend applies the window DPI scale, which the
 *  frontend cannot see. Reported on mount and again on resize; the backend is
 *  idempotent. No-op off Windows. */
export const setTitlebarMaximizeMetrics = (metrics: {
  rightInset: number;
  top: number;
  width: number;
  height: number;
}) => _invoke('set_titlebar_maximize_metrics', { ...metrics });

/** App-level metadata (issue #826). The updater guard in `lib/updater.ts`
 *  uses this to reject the dev profile (`*.dev` bundle id) from polling
 *  the stable release feed — `tauri:build:dev` is a production-mode Vite
 *  build, so an `import.meta.env.PROD` check alone can't tell them apart. */
export const getAppIdentifier = () =>
  _invoke<string>('get_app_identifier');
