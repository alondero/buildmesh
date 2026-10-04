//! State recovery IPC facet (issue #1537).
//!
//! Typed wrappers around the Settings > Data & Diagnostics commands. Every
//! call routes through the `_invoke` chokepoint (ADR-0010) — never raw
//! `@tauri-apps/api/core`, which `tests/unit/tauri-ipc-seam.test.ts` pins as
//! a single allowed site.
//!
//! ## Two export/restore entry points each
//!
//! The dialog-backed commands (`exportState`, `inspectStateBundle`,
//! `stageStateRestore`) open a native picker in Rust, so the frontend never
//! handles a filesystem path it did not choose. The `_to` /
//! `_from`-path variants exist for the mobile HTTP surface and for tests,
//! where there is no native dialog.
//!
//! ## `null` means "the user cancelled"
//!
//! The dialog commands return `null` rather than throwing when the picker is
//! dismissed. That is a normal outcome, not an error, and the UI treats it as
//! a no-op — a cancelled picker must not render a failure banner.

import { _invoke } from './_invoke';
import type { StateRecoveryInfo } from '../../types/generated/StateRecoveryInfo';
import type { StateSnapshot } from '../../types/generated/StateSnapshot';
import type { StateIntegrityReport } from '../../types/generated/StateIntegrityReport';
import type { StateExportResult } from '../../types/generated/StateExportResult';
import type { StateRestorePlan } from '../../types/generated/StateRestorePlan';

/** Everything the Data & Diagnostics pane needs for its first render. */
export const getStateRecoveryInfo = () =>
  _invoke<StateRecoveryInfo>('get_state_recovery_info');

export const listStateSnapshots = () =>
  _invoke<StateSnapshot[]>('list_state_snapshots');

export const createStateSnapshot = () =>
  _invoke<StateSnapshot>('create_state_snapshot');

/** `full = false` runs the fast `quick_check`; `true` runs `integrity_check`. */
export const checkStateIntegrity = (full: boolean) =>
  _invoke<StateIntegrityReport>('check_state_integrity', { full });

/** Opens a native save dialog. Resolves to `null` if the user cancels. */
export const exportState = (redacted: boolean) =>
  _invoke<StateExportResult | null>('export_state', { redacted });

/** Export to a caller-supplied path (mobile HTTP + tests). */
export const exportStateTo = (path: string, redacted: boolean) =>
  _invoke<StateExportResult>('export_state_to', { path, redacted });

/** Report a bundle's contents without staging it. `null` if cancelled. */
export const inspectStateBundle = () =>
  _invoke<StateRestorePlan | null>('inspect_state_bundle');

/** Verify + stage a restore for the next launch. `null` if cancelled. */
export const stageStateRestore = () =>
  _invoke<StateRestorePlan | null>('stage_state_restore');

export const cancelStateRestore = () =>
  _invoke<null>('cancel_state_restore');

export const getStateDataFolder = () =>
  _invoke<string>('get_state_data_folder');

export type {
  StateRecoveryInfo,
  StateSnapshot,
  StateIntegrityReport,
  StateExportResult,
  StateRestorePlan,
};
