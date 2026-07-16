import { describe, it, expect, vi, beforeEach } from 'vitest';
import {
  describeUpdate,
  isDevProfile,
  decideUpdateEnabled,
  runUpdateCheck,
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

describe('runUpdateCheck (integration with mocked IPC)', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    __resetIdentifierCacheForTests();
    // jsdom does not have __TAURI_INTERNALS__ by default; tests that need
    // the Tauri-present path add it before each case.
  });

  it('skips the check when not in a Tauri window', async () => {
    // No __TAURI_INTERNALS__ on window → the function returns null without
    // calling the IPC or the plugin.
    const result = await runUpdateCheck();
    expect(result).toBeNull();
    expect(getAppIdentifierMock).not.toHaveBeenCalled();
    expect(checkMock).not.toHaveBeenCalled();
  });

  it('skips the check for the dev profile (does not poll the stable feed)', async () => {
    (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {};
    getAppIdentifierMock.mockResolvedValue('com.alond.buildmesh.dev');
    const result = await runUpdateCheck();
    expect(result).toBeNull();
    expect(getAppIdentifierMock).toHaveBeenCalled();
    // Critical: the dev profile must NEVER call `check()`.
    expect(checkMock).not.toHaveBeenCalled();
  });
});
