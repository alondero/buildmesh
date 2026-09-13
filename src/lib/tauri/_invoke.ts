//! Central IPC chokepoint (ADR-0010 / issue #386 / issue #1656).
//!
//! Every typed wrapper in the `tauri/` facet modules (`mesh`, `provider`,
//! `circuit`, etc.) routes through this `_invoke`. The chokepoint is the
//! only file that imports `_rawInvoke` from `@tauri-apps/api/core`; the
//! `tests/unit/tauri-ipc-seam.test.ts` drift ratchet enforces that rule.
//!
//! On rejection, the chokepoint logs a sanitized arg-shape via
//! `frontendLog` then re-throws the original `Error` so every consumer
//! sees the same shape they did before the wrapper existed. The arg-shape
//! serializer (`ipcShape.shapeArgs`) ensures PII (API keys) and unbounded
//! payloads (terminal scrollback) are never written to `buildmesh.log`.
//!
//! Test strategy (mirrors the existing wrapper):
//! - `tests/unit/ipc-error-logging.test.ts` stubs `_rawInvoke` via
//!   `vi.mock('@tauri-apps/api/core', …)` and asserts on the formatted
//!   shape.
//! - The seam test (`tauri-ipc-seam.test.ts`) hard-pins this file as the
//!   sole allowed raw-invoke site alongside `frontendLog.ts`.

import { invoke as _rawInvoke } from '@tauri-apps/api/core';
import { logFrontend } from '../frontendLog';
import { shapeArgs } from '../ipcShape';

/** Truncate the error text so a chatty backend reply doesn't flood the log. */
const ERROR_TEXT_CAP = 200;

/**
 * Invoke a Tauri command through the central chokepoint.
 *
 * Pass `args = undefined` (the default) when the command takes no
 * arguments — keeping the call shape `invoke('cmd')` rather than
 * `invoke('cmd', {})` matters because some tests assert on the exact
 * call form.
 */
export async function _invoke<T>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  try {
    return args === undefined
      ? await _rawInvoke<T>(cmd)
      : await _rawInvoke<T>(cmd, args);
  } catch (err) {
    const shape = JSON.stringify(shapeArgs(args));
    const raw = String(err);
    const truncated =
      raw.length > ERROR_TEXT_CAP ? raw.slice(0, ERROR_TEXT_CAP) + '…' : raw;
    logFrontend('error', `[IPC:${cmd}] args=${shape} — ${truncated}`);
    throw err;
  }
}
