/**
 * Shared process / port / log-path helpers for Playwright e2e specs that need
 * to spawn the built `buildmesh.exe` (test server on 1991 + mobile SPA on
 * 1992-1994) and shut it down deterministically.
 *
 * Owns the lifecycle of the spawn it creates — we track the PID and kill by
 * PID, never by image name (an unscoped `taskkill /IM buildmesh.exe /F`
 * would also murder the user's stable hub, which CLAUDE.local.md says must
 * be left running).
 *
 * Per the caveat in CLAUDE.local.md the stable hub must be paused before
 * calling `spawnBuildmesh()` — the spawned exe binds the phone/Android
 * ports the hub owns (1992–1994). The hub does not bind the test bridge.
 * `stableHubIsRunning()` reads the hub profile's `instance-owner.pid`.
 */

import { spawn, type ChildProcess } from 'child_process';
import { randomBytes } from 'crypto';
import { mkdtempSync, readFileSync, rmSync } from 'fs';
import os from 'os';
import path from 'path';
import { fileURLToPath } from 'url';
import { BRIDGE_ENABLE_ENV, BRIDGE_TOKEN_ENV, PROFILE_DIR_ENV } from '../../../scripts/test-bridge-auth.mjs';
import { waitForPortClosed } from './tauri-http';

// The repo is ESM (`"type": "module"`), so `__dirname` is undefined at
// runtime — `import.meta.url` is the supported source of truth.
const __dirname = path.dirname(fileURLToPath(import.meta.url));

/**
 * Path to the built exe. `tests/e2e/utils/buildmesh-launcher.ts` is THREE
 * levels deep from the repo root, so we need three `..` segments to land
 * at the root before joining `src-tauri/...`. Earlier revisions used two
 * segments and produced `tests/src-tauri/...` (no such directory) — silent
 * ENOENT at spawn() that was masked by the spec's pre-flight gate.
 *
 * Override with `BUILDMESH_EXE` for a sideloaded build. The stable-profile
 * ports are 1991 (test bridge) and 1992–1994 (phone/Android). The release
 * entry points do not pass `dev-tools`, which is the feature that compiles
 * the bridge. E2E needs `npx tauri build --features dev-tools` with the
 * stable identifier so those ports stay put. A dev-profile exe (`*.dev`)
 * binds 2991/2992–2994 and this helper will not find it.
 */
export const EXE_PATH =
  process.env.BUILDMESH_EXE ??
  path.join(
    __dirname,
    '..', '..', '..',
    'src-tauri', 'target', 'release', 'buildmesh.exe',
  );

export const TEST_SERVER_PORT = 1991;
export const MOBILE_PORTS = [1992, 1993, 1994] as const;

// ─── Log path resolution ──────────────────────────────────────────────
//
// The base-identity app data directory mirrors `src-tauri/src/lib.rs:98`
// (which is the source of truth for Windows / Darwin / Linux):
//
//   Windows : %APPDATA%\com.alond.buildmesh                → logs/buildmesh.log
//   macOS   : ~/Library/Application Support/com.alond.buildmesh
//   Linux   : $XDG_DATA_HOME or ~/.local/share/com.alond.buildmesh
//
// The dev-profile suffix `.dev` is appended to the dir-name when the
// identifier ends in `.dev`; the spec doesn't need that here — the base
// profile is what `npm run tauri build` produces — but expose the
// override so a reviewer running a dev-profile build can point at the
// correct log location without forking the helper.

function platformLogDir(appName: string): string | null {
  if (process.platform === 'win32') {
    const appData = process.env.APPDATA;
    if (!appData) return null;
    return path.join(appData, appName, 'logs');
  }
  if (process.platform === 'darwin') {
    return path.join(os.homedir(), 'Library', 'Application Support', appName, 'logs');
  }
  // Linux + other XDG-conforming systems.
  const xdg = process.env.XDG_DATA_HOME ?? path.join(os.homedir(), '.local', 'share');
  return path.join(xdg, appName, 'logs');
}

const LOG_APP_NAME =
  process.env.BUILDMESH_LOG_APP_NAME ?? 'com.alond.buildmesh';

export const LOG_DIR =
  process.env.BUILDMESH_LOG_DIR ?? platformLogDir(LOG_APP_NAME);

export const LOG_PATH = LOG_DIR ? path.join(LOG_DIR, 'buildmesh.log') : null;

/** True iff we resolved a usable log path; specs that assert log content
 *  should `test.skip(!LOG_PATH, ...)` rather than silently lying about
 *  conditional assertions. */
export function hasLogPath(): boolean {
  return LOG_PATH !== null;
}

// ─── Process lifecycle ────────────────────────────────────────────────

export interface BuildmeshProcess {
  readonly child: ChildProcess;
  /** Pre-captured so callers don't need to re-check `child.pid`. */
  readonly pid: number;
  /** Bearer for this child. Also copied onto `process.env` for `invokeViaHttp`. */
  readonly bridgeToken: string;
  /** Log file inside the isolated profile, which may not exist until the app writes it. */
  readonly logPath: string;
  /** Temp root that contains the isolated profile directory. */
  readonly dataRoot: string;
  /** Absolute directory passed as `BUILDMESH_PROFILE_DIR`. Holds `buildmesh.db` and `logs/`. */
  readonly profileDir: string;
}

const STABLE_APP_NAME = 'com.alond.buildmesh';

function isolatedProfileDir(dataRoot: string): string {
  return path.join(dataRoot, STABLE_APP_NAME);
}

function isolatedLogPath(profileDir: string): string {
  return path.join(profileDir, 'logs', 'buildmesh.log');
}

/** The stable hub's real profile. This is not the child's override directory. */
export function stableProfileDir(): string | null {
  if (process.platform === 'win32') {
    const appData = process.env.APPDATA;
    if (!appData) return null;
    return path.join(appData, STABLE_APP_NAME);
  }
  if (process.platform === 'darwin') {
    return path.join(os.homedir(), 'Library', 'Application Support', STABLE_APP_NAME);
  }
  const xdg = process.env.XDG_DATA_HOME ?? path.join(os.homedir(), '.local', 'share');
  return path.join(xdg, STABLE_APP_NAME);
}

/**
 * True when the profile directory's `instance-owner.pid` names a live process.
 * A stale pid from a dead hub does not count. `EPERM` means the process exists
 * but this caller cannot signal it.
 */
export function profileOwnerIsAlive(profileDir: string): boolean {
  let text: string;
  try {
    text = readFileSync(path.join(profileDir, 'instance-owner.pid'), 'utf8');
  } catch {
    return false;
  }
  const pid = Number(text.trim());
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'EPERM';
  }
}

/** The stable hub publishes its pid in the real profile, not on port 1991. */
export function stableHubIsRunning(): boolean {
  const profile = stableProfileDir();
  if (!profile) return false;
  return profileOwnerIsAlive(profile);
}

/**
 * Delete a spawn's temp root. Only a directory created under the OS temp
 * folder, named `buildmesh-e2e-…`, is removed. Anything else is left in place.
 */
export function removeSpawnDataRoot(dataRoot: string): void {
  const root = path.resolve(dataRoot);
  const temp = path.resolve(os.tmpdir());
  const relative = path.relative(temp, root);
  if (relative === '' || relative.startsWith('..') || path.isAbsolute(relative)) return;
  if (!path.basename(root).startsWith('buildmesh-e2e-')) return;
  rmSync(root, { recursive: true, force: true });
}

/**
 * Spawn the built buildmesh.exe. Attach an 'error' listener BEFORE we hand
 * the child to the caller — without one, Node treats `spawn` failures
 * (ENOENT from a misconfigured `EXE_PATH`, permission denied, missing
 * DLLs) as uncaught exceptions and tears down the Playwright worker.
 * With one, the failure surfaces through the returned handle and the
 * caller can decide to wait it out or bail.
 */
export function spawnBuildmesh(): BuildmeshProcess {
  const dataRoot = mkdtempSync(path.join(os.tmpdir(), `buildmesh-e2e-${process.pid}-`));
  const profileDir = isolatedProfileDir(dataRoot);
  const bridgeToken = randomBytes(32).toString('hex');
  // Same-process callers (`invokeViaHttp`) read this. The child gets its own
  // copy below. Two overlapping spawns would clobber it; specs spawn one.
  process.env[BRIDGE_TOKEN_ENV] = bridgeToken;
  // `BUILDMESH_PROFILE_DIR` is the profile itself. Setting APPDATA, HOME, or
  // XDG_DATA_HOME does not move a Windows Tauri profile.
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    [BRIDGE_ENABLE_ENV]: '1',
    [BRIDGE_TOKEN_ENV]: bridgeToken,
    [PROFILE_DIR_ENV]: profileDir,
  };

  const child = spawn(EXE_PATH, [], {
    stdio: 'ignore',
    windowsHide: true,
    env,
  });
  if (child.pid === undefined) {
    throw new Error(`spawn() returned no PID for ${EXE_PATH}`);
  }
  // Capture the first 'error' so a later awaiter sees why the spawn
  // flopped (most commonly ENOENT — see the EXE_PATH comment above).
  let spawnError: Error | null = null;
  child.once('error', (err) => {
    spawnError = err;
  });
  // Stash on the child so callers can read it after wait/terminate.
  (child as ChildProcess & { __spawnError?: Error | null }).__spawnError =
    spawnError;
  return {
    child,
    pid: child.pid,
    bridgeToken,
    logPath: isolatedLogPath(profileDir),
    dataRoot,
    profileDir,
  };
}

/**
 * Read the spawn error if one fired. Returns `null` if the child is still
 * alive and `Error` if `spawn()` itself failed (ENOENT, EACCES, etc.).
 *
 * Use after `terminate()` to distinguish a kill that landed from a kill
 * that was superfluous because the child never started.
 */
export function spawnError(proc: BuildmeshProcess): Error | null {
  return (
    (proc.child as ChildProcess & { __spawnError?: Error | null })
      .__spawnError ?? null
  );
}

/**
 * Terminate the spawned buildmesh by PID and wait for the kernel to
 * release the test-server socket so the next test can re-bind.
 *
 * The wait runs unconditionally — even when the process already exited
 * (via crash or natural) the socket may still be in TIME_WAIT. Discarding
 * the wait's return value would let a stale bind leak through silently,
 * so we surface a hard error if the port fails to free within the
 * timeout.
 */
export async function terminate(
  proc: BuildmeshProcess,
  port: number = TEST_SERVER_PORT,
): Promise<void> {
  if (proc.child.exitCode === null) {
    try {
      process.kill(proc.pid, 'SIGKILL');
    } catch {
      // Process already gone — fall through and still drain the socket.
    }
  }
  try {
    const closed = await waitForPortClosed('127.0.0.1', port, 5000);
    if (!closed) {
      throw new Error(
        `Port ${port} remained bound 5000ms after terminating PID ${proc.pid}`,
      );
    }
  } finally {
    removeSpawnDataRoot(proc.dataRoot);
  }
}

// ─── Port helpers ─────────────────────────────────────────────────────

/**
 * Probe `127.0.0.1` for a TCP listener. Used as a pre-flight check to
 * surface "pause the stable hub first" rather than letting spawn fail
 * with a vague bind error.
 */
export async function isPortBound(
  port: number,
  timeoutMs = 250,
): Promise<boolean> {
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
 * SPA shell. Replaces the old single-pass loop that could race the test
 * server's bind by tens of milliseconds and bail.
 *
 * The phone/Android server doesn't expose `/health` (only the development
 * test bridge does), so we hit `/` and look for any HTTP status, including
 * 401, as evidence the socket is bound.
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
