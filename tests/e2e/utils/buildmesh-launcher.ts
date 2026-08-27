/**
 * Shared process & port helpers for Playwright e2e specs that need to
 * spawn the built `buildmesh.exe` (the test server on 1991 + the mobile
 * SPA on 1992-1994) and then shut it down deterministically.
 *
 * Design rules:
 *
 *   - We always own the lifecycle of the spawn we create. The caller
 *     gets back a `BuildmeshProcess` and must invoke `terminate()` in
 *     `afterEach`. We never kill by image name — an unscoped
 *     `taskkill /IM buildmesh.exe /F` will also murder the user's
 *     stable hub if it's running (per CLAUDE.local.md, the stable hub
 *     shares the base-identity ports 1991/1992-1994 and MUST be paused
 *     before this kind of spec runs).
 *
 *   - Termination is real, not fire-and-forget. `process.kill(pid,
 *     'SIGKILL')` on Windows maps to `TerminateProcess` and returns
 *     synchronously, but the kernel still has to release the bind.
 *     `waitForPortClosed` (from `./tauri-http`) is the deterministic
 *     handshake; a `setTimeout(500)` prayer is not.
 *
 *   - Mobile-port discovery retries until the server binds. The old
 *     single-shot `findMobilePort()` could race the test server's bind
 *     by a few ms and return `null` even when the server was about to
 *     come up.
 */

import { spawn, type ChildProcess } from 'child_process';
import path from 'path';
import { fileURLToPath } from 'url';
import { waitForPortClosed } from './tauri-http';

// The repo is ESM (`"type": "module"`), so `__dirname` is undefined at
// runtime — `import.meta.url` is the supported source of truth.
const __dirname = path.dirname(fileURLToPath(import.meta.url));

/**
 * Path to the built exe. Defaults to `src-tauri/target/release/buildmesh.exe`
 * two directories up from this file; override with `BUILDMESH_EXE` for a
 * sideloaded build or a dev-profile exe.
 */
export const EXE_PATH =
  process.env.BUILDMESH_EXE ??
  path.join(__dirname, '..', '..', 'src-tauri', 'target', 'release', 'buildmesh.exe');

export const TEST_SERVER_PORT = 1991;
export const MOBILE_PORTS = [1992, 1993, 1994] as const;

export interface BuildmeshProcess {
  readonly child: ChildProcess;
  /** Pre-captured so callers don't need to re-check `child.pid`. */
  readonly pid: number;
}

/**
 * Spawn the built buildmesh.exe without `detached`/`unref()` so we own
 * the lifecycle. The caller MUST invoke `terminate(process)` in
 * `afterEach` — never kill by image name.
 */
export function spawnBuildmesh(): BuildmeshProcess {
  const child = spawn(EXE_PATH, [], {
    stdio: 'ignore',
    windowsHide: true,
  });
  if (child.pid === undefined) {
    throw new Error('spawn() returned no PID for buildmesh.exe');
  }
  return { child, pid: child.pid };
}

/**
 * Terminate the spawned buildmesh by PID and wait for the kernel to
 * release the test-server socket so the next test can re-bind.
 * Deterministic — no `setTimeout` prayers.
 */
export async function terminate(
  proc: BuildmeshProcess,
  port: number = TEST_SERVER_PORT,
): Promise<void> {
  if (proc.child.exitCode !== null) return;
  try {
    process.kill(proc.pid, 'SIGKILL');
  } catch {
    // Process already exited — that's the success case.
    return;
  }
  await waitForPortClosed('127.0.0.1', port, 5000);
}

/**
 * Probe `127.0.0.1` for a TCP listener. Used as a pre-flight check to
 * surface a clear "pause the stable hub first" error instead of letting
 * the spawn fail with a vague bind error.
 */
export async function isPortBound(port: number, timeoutMs = 250): Promise<boolean> {
  try {
    const r = await fetch(`http://127.0.0.1:${port}/`, {
      signal: AbortSignal.timeout(timeoutMs),
    });
    return r.status > 0;
  } catch {
    return false;
  }
}

/**
 * Poll the mobile HTTP port range until one of 1992/1993/1994 serves the
 * SPA shell. Replaces the old single-shot loop that could race the test
 * server's bind by tens of milliseconds and bail.
 *
 * The mobile server doesn't expose `/health` (only the test server on
 * 1991 does — see commands/test.rs:195), so we hit `/` and look for any
 * HTTP status (including 401 for an unauth'd route) as evidence the
 * socket is bound.
 */
export async function findMobilePort(timeoutMs = 10000): Promise<number | null> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    for (const port of MOBILE_PORTS) {
      if (await isPortBound(port)) return port;
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  return null;
}
