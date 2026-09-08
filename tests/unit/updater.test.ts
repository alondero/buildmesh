import { describe, it, expect, vi, beforeEach } from 'vitest';
import {
  describeUpdate,
  isDevProfile,
  decideUpdateEnabled,
  runUpdateCheck,
  downloadAndInstallUpdate,
  sanitizeUpdaterError,
  __resetIdentifierCacheForTests,
} from '../../src/lib/updater';

const { getAppIdentifierMock, checkMock } = vi.hoisted(() => ({
  getAppIdentifierMock: vi.fn(),
  checkMock: vi.fn(),
}));

// `getAppIdentifier` is a thin IPC wrapper around the new
// `get_app_identifier` command; `check` is the updater plugin's feed probe.
vi.mock('../../src/lib/tauri', () => ({ getAppIdentifier: getAppIdentifierMock }));
vi.mock('@tauri-apps/plugin-updater', () => ({ check: checkMock }));

describe('describeUpdate', () => {
  it('builds a headline message from the version', () => {
    const { version, message } = describeUpdate({ version: '0.2.0', body: '' });
    expect(version).toBe('0.2.0');
    expect(message).toBe('Buildmesh 0.2.0 is available.');
  });

  it('trims the release notes body', () => {
    const { notes } = describeUpdate({ version: '1.0.0', body: '  Fixes and polish.\n\n' });
    expect(notes).toBe('Fixes and polish.');
  });

  it('treats a null/undefined body as empty notes', () => {
    // The updater feed omits `body` when a release has no notes.
    expect(describeUpdate({ version: '1.0.0', body: null as unknown as string }).notes).toBe('');
    expect(describeUpdate({ version: '1.0.0', body: undefined }).notes).toBe('');
  });
});

describe('isDevProfile', () => {
  it('identifies the dev profile by .dev suffix', () => {
    expect(isDevProfile('com.alond.buildmesh.dev')).toBe(true);
  });

  it('identifies the stable profile', () => {
    expect(isDevProfile('com.alond.buildmesh')).toBe(false);
  });

  it('rejects any other identifier', () => {
    expect(isDevProfile('com.example.other')).toBe(false);
  });
});

// Pure decision matrix — every guard branch is a single test. `runUpdateCheck`
// (the integration path) is covered below for the cases that actually
// exercise the IPC seam.
describe('decideUpdateEnabled (issue #826 — dev profile bug)', () => {
  it('rejects non-production builds', () => {
    expect(decideUpdateEnabled(false, true, 'com.alond.buildmesh')).toBe(false);
  });

  it('rejects non-Tauri page loads', () => {
    expect(decideUpdateEnabled(true, false, 'com.alond.buildmesh')).toBe(false);
  });

  it('rejects when the identifier fetch failed (null)', () => {
    expect(decideUpdateEnabled(true, true, null)).toBe(false);
  });

  it('rejects the dev profile (does not poll the stable feed)', () => {
    // Critical: the dev profile would otherwise see the stable release and
    // offer to replace itself. See ADR 0021.
    expect(decideUpdateEnabled(true, true, 'com.alond.buildmesh.dev')).toBe(false);
  });

  it('accepts the stable profile', () => {
    expect(decideUpdateEnabled(true, true, 'com.alond.buildmesh')).toBe(true);
  });
});

// Issue #1526: the previous `runUpdateCheck` silently swallowed check
// failures as `null` ("no update"). The new contract is a typed result
// that distinguishes `current` (up to date) from `unreachable` (feed
// failed) — manual checks surface both, quiet checks suppress them.
describe('runUpdateCheck (issue #1526 — typed result)', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    __resetIdentifierCacheForTests();
    // vitest runs in `test` mode so `import.meta.env.PROD` is false by
    // default. Stub it so the integration tests can exercise the
    // stable-profile path without rebuilding. `stubEnv` updates both
    // `process.env` and `import.meta.env` in vitest 0.30+.
    vi.stubEnv('PROD', 'true');
    // jsdom does not have __TAURI_INTERNALS__ by default; tests that need
    // the Tauri-present path add it before each case.
  });

  afterEach(() => {
    vi.unstubAllEnvs();
  });

  it('returns phase=disabled when not in a Tauri window', async () => {
    const result = await runUpdateCheck();
    expect(result).toEqual({ phase: 'disabled' });
    expect(getAppIdentifierMock).not.toHaveBeenCalled();
    expect(checkMock).not.toHaveBeenCalled();
  });

  it('returns phase=disabled for the dev profile (does not poll the stable feed)', async () => {
    (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {};
    getAppIdentifierMock.mockResolvedValue('com.alond.buildmesh.dev');
    const result = await runUpdateCheck();
    expect(result).toEqual({ phase: 'disabled' });
    expect(getAppIdentifierMock).toHaveBeenCalled();
    // Critical: the dev profile must NEVER call `check()`.
    expect(checkMock).not.toHaveBeenCalled();
  });

  it('returns phase=current when the feed reports no newer version', async () => {
    (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {};
    getAppIdentifierMock.mockResolvedValue('com.alond.buildmesh');
    checkMock.mockResolvedValue(null);
    const result = await runUpdateCheck();
    expect(result.phase).toBe('current');
  });

  it('returns phase=available with summary when the feed reports a newer version', async () => {
    (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {};
    getAppIdentifierMock.mockResolvedValue('com.alond.buildmesh');
    const fakeUpdate = { version: '1.4.0', body: '  New things.\n' };
    checkMock.mockResolvedValue(fakeUpdate);
    const result = await runUpdateCheck();
    expect(result.phase).toBe('available');
    if (result.phase !== 'available') throw new Error('unreachable');
    expect(result.update).toBe(fakeUpdate);
    expect(result.summary.version).toBe('1.4.0');
    expect(result.summary.notes).toBe('New things.');
    expect(result.summary.message).toBe('Buildmesh 1.4.0 is available.');
  });

  it('returns phase=unreachable (NOT null) when the feed check throws', async () => {
    // The old behaviour swallowed this as `null`, which the UI mapped to
    // "no update available" — the bug #1526 catches. Manual checks must
    // be able to distinguish "up to date" from "feed down".
    (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {};
    getAppIdentifierMock.mockResolvedValue('com.alond.buildmesh');
    checkMock.mockRejectedValue(new Error('boom'));
    const result = await runUpdateCheck();
    expect(result.phase).toBe('unreachable');
    if (result.phase !== 'unreachable') throw new Error('unreachable');
    expect(result.error).toContain('boom');
  });

  it('sanitizes feed URLs from the unreachable message', async () => {
    (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {};
    getAppIdentifierMock.mockResolvedValue('com.alond.buildmesh');
    checkMock.mockRejectedValue(
      new Error('GET https://api.github.com/repos/foo/bar/releases failed'),
    );
    const result = await runUpdateCheck();
    if (result.phase !== 'unreachable') throw new Error('unreachable');
    expect(result.error).not.toContain('api.github.com');
    expect(result.error).toContain('<feed>');
  });
});

// Issue #1526: errors reaching the UI must not leak the feed URL,
// authorization tokens, or absolute paths from the host machine.
describe('sanitizeUpdaterError (issue #1526 — no token / URL / path leaks)', () => {
  it('strips URLs', () => {
    expect(sanitizeUpdaterError(new Error('Failed at https://api.github.com/foo'))).not.toContain(
      'api.github.com',
    );
  });

  it('strips bearer tokens', () => {
    const result = sanitizeUpdaterError(new Error('Authorization: Bearer abc.def.ghi'));
    expect(result).not.toContain('abc.def.ghi');
    expect(result).toContain('<redacted>');
  });

  it('strips absolute Windows paths', () => {
    expect(
      sanitizeUpdaterError(new Error('Read C:\\Users\\alice\\AppData\\buildmesh\\foo failed')),
    ).not.toContain('alice');
  });

  it('strips absolute POSIX paths under sensitive roots', () => {
    expect(
      sanitizeUpdaterError(new Error('Write to /home/alice/.config/buildmesh/foo failed')),
    ).not.toContain('alice');
  });

  it('truncates very long messages to keep the modal readable', () => {
    const long = 'A'.repeat(1000);
    const result = sanitizeUpdaterError(new Error(long));
    expect(result.length).toBeLessThanOrEqual(240);
  });

  it('returns a stable fallback for non-Error throws', () => {
    expect(sanitizeUpdaterError(undefined)).toBe('Unknown error.');
    expect(sanitizeUpdaterError(42)).toBe('Unknown error.');
    expect(sanitizeUpdaterError({ weird: true })).toBe('Unknown error.');
  });
});

// Issue #1526: `downloadAndInstallUpdate` translates the plugin's
// per-chunk events into a cumulative `DownloadProgress`. The hook
// surfaces this so the modal can show a real progress bar.
describe('downloadAndInstallUpdate (issue #1526 — progress + sanitized errors)', () => {
  it('reports cumulative progress across Progress events', async () => {
    const installMock = vi.fn().mockResolvedValue(undefined);
    const update = {
      version: '1.0.0',
      body: null,
      install: installMock,
      download: vi.fn(async (cb: (e: unknown) => void) => {
        cb({ event: 'Started', data: { contentLength: 1000 } });
        cb({ event: 'Progress', data: { chunkLength: 200 } });
        cb({ event: 'Progress', data: { chunkLength: 300 } });
        cb({ event: 'Progress', data: { chunkLength: 500 } });
        cb({ event: 'Finished' });
      }),
    };
    const progress: Array<{ downloaded: number; total: number | null }> = [];
    await downloadAndInstallUpdate(update, (p) => progress.push(p));
    // Each event should have produced a tick: started with total 1000,
    // then three cumulative steps (200, 500, 1000).
    expect(progress.at(-1)?.downloaded).toBe(1000);
    expect(progress.at(-1)?.total).toBe(1000);
    expect(progress.map((p) => p.downloaded)).toEqual([0, 200, 500, 1000, 1000]);
    expect(installMock).toHaveBeenCalledTimes(1);
  });

  it('handles chunked responses (contentLength undefined)', async () => {
    const update = {
      version: '1.0.0',
      body: null,
      install: vi.fn().mockResolvedValue(undefined),
      download: vi.fn(async (cb: (e: unknown) => void) => {
        cb({ event: 'Started', data: {} });
        cb({ event: 'Progress', data: { chunkLength: 100 } });
      }),
    };
    const progress: Array<{ downloaded: number; total: number | null }> = [];
    await downloadAndInstallUpdate(update, (p) => progress.push(p));
    expect(progress.at(-1)?.downloaded).toBe(100);
    expect(progress.at(-1)?.total).toBeNull();
  });

  it('sanitizes errors from the download path', async () => {
    const update = {
      version: '1.0.0',
      body: null,
      install: vi.fn().mockResolvedValue(undefined),
      download: vi.fn(async () => {
        throw new Error('GET https://api.github.com/x failed');
      }),
    };
    await expect(downloadAndInstallUpdate(update)).rejects.toThrow(/<feed>/);
  });

  it('sanitizes errors from the install path', async () => {
    const update = {
      version: '1.0.0',
      body: null,
      install: vi.fn().mockRejectedValue(new Error('Write C:\\Users\\victim\\foo failed')),
      download: vi.fn().mockResolvedValue(undefined),
    };
    await expect(downloadAndInstallUpdate(update)).rejects.toThrow(/<path>/);
  });
});