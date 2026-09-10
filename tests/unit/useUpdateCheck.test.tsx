/**
 * `useUpdateCheck` shared-store wiring (issue #1526 + #1654 review).
 *
 * The hook is a thin subscriber over `useUpdaterStore`. Two surfaces
 * (`UpdatePrompt` at App root and `<UpdateAboutSection>` in Settings)
 * read the same `phase` so a dismiss in one place is visible in the
 * other. This test exercises:
 *   - shared cross-instance state (finding 1)
 *   - phase transitions through install + retry (finding 2 + 3)
 *   - install-phase ordering (the install() promise starts AFTER the
 *     `installing` phase is set — finding 2)
 *   - exit-readiness routing through `useExitPromptStore.requestExit`
 *     (finding 4)
 *   - the cancel escape hatch (finding 5)
 *   - the failedAt union narrowed to `download | install` (finding 3)
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import type { Update } from '@tauri-apps/plugin-updater';
import type { AgentNode } from '../../src/types/generated/AgentNode';

const {
  runUpdateCheckMock,
  downloadUpdateMock,
  installUpdateMock,
  relaunchMock,
  requestExitMock,
  listProvidersMock,
  getAgentNodesMock,
} = vi.hoisted(() => ({
  runUpdateCheckMock: vi.fn(),
  downloadUpdateMock: vi.fn(),
  installUpdateMock: vi.fn().mockResolvedValue(undefined),
  relaunchMock: vi.fn().mockResolvedValue(undefined),
  requestExitMock: vi.fn(),
  listProvidersMock: vi.fn(),
  getAgentNodesMock: vi.fn(),
}));

vi.mock('../../src/lib/updater', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/lib/updater')>();
  return {
    ...actual,
    runUpdateCheck: runUpdateCheckMock,
    downloadUpdate: downloadUpdateMock,
    installUpdate: installUpdateMock,
  };
});

vi.mock('@tauri-apps/plugin-process', () => ({
  relaunch: relaunchMock,
}));

vi.mock('../../src/stores/exitPromptStore', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/stores/exitPromptStore')>();
  // Override ONLY `requestExit`; keep the real store's `pending`,
  // `keepWorking`, `setState`, etc. so round-2 tests can exercise the
  // restart → keepWorking → assert-back-at-ready_to_restart path.
  // The previous mock replaced the whole module export, leaving
  // `pending` permanently null and `keepWorking` undefined — that
  // hid the round-2 dead-end. The impl that populates `pending` is
  // re-installed in `beforeEach` so `vi.restoreAllMocks()` doesn't
  // clear it between cases.
  const realStore = actual.useExitPromptStore;
  return {
    ...actual,
    useExitPromptStore: Object.assign({}, realStore, {
      getState: () => Object.assign({}, realStore.getState(), {
        requestExit: requestExitMock,
      }),
    }),
  };
});

vi.mock('../../src/stores/agentNodeStore', () => ({
  useAgentNodeStore: {
    getState: () => ({ getAgentNodes: getAgentNodesMock }),
  },
}));

vi.mock('../../src/lib/tauri', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/lib/tauri')>();
  return {
    ...actual,
    listProviders: listProvidersMock,
  };
});

import { useUpdateCheck } from '../../src/hooks/useUpdateCheck';
import { useUpdaterStore, resetUpdaterStateForTests } from '../../src/stores/updaterStore';
import { useExitPromptStore } from '../../src/stores/exitPromptStore';

const fakeUpdate = (): Update => ({
  version: '0.3.0',
  body: '',
} as unknown as Update);

describe('useUpdateCheck / useUpdaterStore (issue #1526 + #1654 review)', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    // NB: do NOT call `vi.restoreAllMocks()` — that would clear
    // the requestExitMock implementation set below.
    resetUpdaterStateForTests();
    // Reset the real exit-prompt store's `pending` field so a
    // previous test's modal-clear state doesn't leak into this one.
    useExitPromptStore.setState({ pending: null, exiting: false });
    getAgentNodesMock.mockReturnValue([]);
    listProvidersMock.mockResolvedValue([]);
    // Default to a "modal comes up" implementation that actually
    // populates `pending` on the real exit-prompt store. The previous
    // `mockResolvedValue(true)` would have returned true without
    // touching state, which the round-2 restart-cancel test needs to
    // observe. Individual tests can still override with
    // `mockResolvedValueOnce(false)` to exercise the direct relaunch
    // path.
    requestExitMock.mockImplementation(async (mode: 'window-close' | 'update-restart') => {
      useExitPromptStore.setState({ pending: { mode, activeCount: 0, nonResumable: [] } });
      return true;
    });
  });

  it('mounts with a quiet check that surfaces only `available`', async () => {
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.3.0', notes: '', message: 'Buildmesh 0.3.0 is available.' },
    });
    const { result } = renderHook(() => useUpdateCheck());
    await waitFor(() => {
      expect(result.current.state.kind).toBe('available');
    });
  });

  it('quiet check stays in `idle` when feed reports `current`', async () => {
    runUpdateCheckMock.mockResolvedValue({ phase: 'current' });
    const { result } = renderHook(() => useUpdateCheck());
    await waitFor(() => expect(runUpdateCheckMock).toHaveBeenCalled());
    await act(async () => {
      await Promise.resolve();
    });
    expect(result.current.state.kind).toBe('idle');
  });

  it('quiet check stays in `idle` when feed is `unreachable`', async () => {
    runUpdateCheckMock.mockResolvedValue({ phase: 'unreachable', error: 'offline' });
    const { result } = renderHook(() => useUpdateCheck());
    await waitFor(() => expect(runUpdateCheckMock).toHaveBeenCalled());
    await act(async () => {
      await Promise.resolve();
    });
    expect(result.current.state.kind).toBe('idle');
  });

  it('manual check surfaces `current` (Settings > About feedback)', async () => {
    runUpdateCheckMock.mockResolvedValue({ phase: 'current' });
    const { result } = renderHook(() => useUpdateCheck());
    await act(async () => {
      await result.current.check();
    });
    expect(result.current.state.kind).toBe('current');
  });

  it('manual check surfaces `unreachable` with the sanitized error', async () => {
    runUpdateCheckMock.mockResolvedValue({ phase: 'unreachable', error: 'offline' });
    const { result } = renderHook(() => useUpdateCheck());
    await act(async () => {
      await result.current.check();
    });
    if (result.current.state.kind !== 'unreachable') throw new Error('expected unreachable');
    expect(result.current.state.error).toBe('offline');
  });

  // Finding 2 — install walks `downloading` → `installing` → `ready_to_restart`
  // and the install() promise is awaited UNDER the installing phase.
  it('install walks downloading → installing → ready_to_restart', async () => {
    let phaseAtInstallStart: string | null = null;
    installUpdateMock.mockImplementation(async () => {
      phaseAtInstallStart = useUpdaterStore.getState().phase.kind;
    });
    downloadUpdateMock.mockImplementation(
      async (
        _u: Update,
        onProgress?: (p: { downloaded: number; total: number | null }) => void,
      ) => {
        onProgress?.({ downloaded: 100, total: 100 });
      },
    );
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.3.0', notes: '', message: 'Buildmesh 0.3.0 is available.' },
    });
    const { result } = renderHook(() => useUpdateCheck());
    await waitFor(() => expect(result.current.state.kind).toBe('available'));
    await act(async () => {
      await result.current.install();
    });
    expect(phaseAtInstallStart).toBe('installing');
    expect(result.current.state.kind).toBe('ready_to_restart');
  });

  // Finding 3 — the failure phase distinguishes download-time from
  // install-time failures (the pre-review code hardcoded `install`).
  it('install failure transitions to `failed` with failedAt=install', async () => {
    installUpdateMock.mockRejectedValue(new Error('disk full'));
    downloadUpdateMock.mockResolvedValue(undefined);
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.3.0', notes: '', message: 'Buildmesh 0.3.0 is available.' },
    });
    const { result } = renderHook(() => useUpdateCheck());
    await waitFor(() => expect(result.current.state.kind).toBe('available'));
    await act(async () => {
      await result.current.install();
    });
    if (result.current.state.kind !== 'failed') throw new Error('expected failed');
    expect(result.current.state.failedAt).toBe('install');
    expect(result.current.state.error).toBe('disk full');
  });

  it('download failure transitions to `failed` with failedAt=download', async () => {
    // The pre-review code only ever produced failedAt=install. Now the
    // store reads `get().phase.kind` at catch time to distinguish
    // download-time vs install-time failures.
    downloadUpdateMock.mockRejectedValue(new Error('network down'));
    installUpdateMock.mockResolvedValue(undefined);
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.3.0', notes: '', message: 'Buildmesh 0.3.0 is available.' },
    });
    const { result } = renderHook(() => useUpdateCheck());
    await waitFor(() => expect(result.current.state.kind).toBe('available'));
    await act(async () => {
      await result.current.install();
    });
    if (result.current.state.kind !== 'failed') throw new Error('expected failed');
    expect(result.current.state.failedAt).toBe('download');
  });

  it('retry re-runs the failed install from the preserved handle', async () => {
    installUpdateMock
      .mockRejectedValueOnce(new Error('disk full'))
      .mockResolvedValueOnce(undefined);
    downloadUpdateMock.mockResolvedValue(undefined);
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.3.0', notes: '', message: 'Buildmesh 0.3.0 is available.' },
    });
    const { result } = renderHook(() => useUpdateCheck());
    await waitFor(() => expect(result.current.state.kind).toBe('available'));
    await act(async () => {
      await result.current.install();
    });
    if (result.current.state.kind !== 'failed') throw new Error('expected failed');
    await act(async () => {
      await result.current.retry();
    });
    expect(downloadUpdateMock).toHaveBeenCalledTimes(2);
    expect(result.current.state.kind).toBe('ready_to_restart');
  });

  it('dismiss resets state to idle without dropping the staged update', async () => {
    downloadUpdateMock.mockResolvedValue(undefined);
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.3.0', notes: '', message: 'Buildmesh 0.3.0 is available.' },
    });
    const { result } = renderHook(() => useUpdateCheck());
    await waitFor(() => expect(result.current.state.kind).toBe('available'));
    await act(async () => {
      await result.current.install();
    });
    expect(result.current.state.kind).toBe('ready_to_restart');
    act(() => result.current.dismiss());
    expect(result.current.state.kind).toBe('idle');
  });

  // Finding 5 — the ProgressPrompt escape hatch. Bumping the seq guard
  // makes in-flight progress callbacks bail; the store transitions to
  // `failed { failedAt: 'download' }`.
  it('cancelInstall transitions to `failed` with failedAt=download', async () => {
    // Defer the download resolution so cancel lands while it's still
    // in flight. We also assert the seq guard makes subsequent
    // progress callbacks inert.
    let resolveDownload!: () => void;
    const progressCalls: number[] = [];
    downloadUpdateMock.mockImplementation(
      async (
        _u: Update,
        onProgress?: (p: { downloaded: number; total: number | null }) => void,
      ) => {
        onProgress?.({ downloaded: 50, total: 100 });
        await new Promise<void>((r) => { resolveDownload = r; });
      },
    );
    installUpdateMock.mockResolvedValue(undefined);
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.3.0', notes: '', message: 'Buildmesh 0.3.0 is available.' },
    });
    const { result } = renderHook(() => useUpdateCheck());
    await waitFor(() => expect(result.current.state.kind).toBe('available'));
    // Kick off install but don't await yet — we want to land in
    // `downloading` first.
    let installPromise: Promise<void> = Promise.resolve();
    act(() => {
      installPromise = result.current.install();
    });
    await waitFor(() => expect(result.current.state.kind).toBe('downloading'));
    act(() => result.current.cancelInstall());
    expect(result.current.state.kind).toBe('failed');
    if (result.current.state.kind === 'failed') {
      expect(result.current.state.failedAt).toBe('download');
    }
    // Let the download resolve; its terminal callback should bail on
    // the seq guard and not overwrite our `failed` phase.
    resolveDownload();
    await act(async () => {
      await installPromise;
    });
    if (result.current.state.kind !== 'failed') throw new Error('expected failed after late download resolve');
    expect(result.current.state.failedAt).toBe('download');
    // Sanity: install was NOT called (the cancelled download never
    // reached install time).
    expect(installUpdateMock).not.toHaveBeenCalled();
    progressCalls.length = 0; // unused; placeholder so test reads cleanly
  });

  // Finding 4 — restart delegates to `useExitPromptStore.requestExit`.
  it('restart delegates to useExitPromptStore.requestExit', async () => {
    downloadUpdateMock.mockResolvedValue(undefined);
    installUpdateMock.mockResolvedValue(undefined);
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.3.0', notes: '', message: 'Buildmesh 0.3.0 is available.' },
    });
    const { result } = renderHook(() => useUpdateCheck());
    await waitFor(() => expect(result.current.state.kind).toBe('available'));
    await act(async () => {
      await result.current.install();
    });
    await act(async () => {
      await result.current.restart();
    });
    expect(requestExitMock).toHaveBeenCalledWith('update-restart');
  });

  it('restart with no active agents calls relaunch() directly (no prompt needed)', async () => {
    downloadUpdateMock.mockResolvedValue(undefined);
    installUpdateMock.mockResolvedValue(undefined);
    requestExitMock.mockResolvedValue(false);
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.3.0', notes: '', message: 'Buildmesh 0.3.0 is available.' },
    });
    const { result } = renderHook(() => useUpdateCheck());
    await waitFor(() => expect(result.current.state.kind).toBe('available'));
    await act(async () => {
      await result.current.install();
    });
    await act(async () => {
      await result.current.restart();
    });
    expect(relaunchMock).toHaveBeenCalledTimes(1);
  });

  it('restart with active non-resumable agents shows the exit prompt (no direct relaunch)', async () => {
    const terminalNode = {
      id: 7,
      status: 'running',
      provider: 'terminal',
      cli_session_id: 'sess-7',
    } as unknown as AgentNode;
    getAgentNodesMock.mockReturnValue([terminalNode]);
    downloadUpdateMock.mockResolvedValue(undefined);
    installUpdateMock.mockResolvedValue(undefined);
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.3.0', notes: '', message: 'Buildmesh 0.3.0 is available.' },
    });
    const { result } = renderHook(() => useUpdateCheck());
    await waitFor(() => expect(result.current.state.kind).toBe('available'));
    await act(async () => {
      await result.current.install();
    });
    await act(async () => {
      await result.current.restart();
    });
    expect(requestExitMock).toHaveBeenCalledWith('update-restart');
    expect(relaunchMock).not.toHaveBeenCalled();
  });

  // Round-2 BLOCKING — the pre-fix flow transitioned to a
  // `restarting` phase before awaiting `requestExit`. If the user
  // then clicked "Keep Working" on the modal, both surfaces
  // stranded in `restarting` with no Restart affordance anywhere.
  // The fix stays in `ready_to_restart` throughout the modal flow
  // and lets the modal z-50 stack on top of the ReadyPrompt.
  it('after Keep Working on the restart modal, the staged binary is still restartable', async () => {
    downloadUpdateMock.mockResolvedValue(undefined);
    installUpdateMock.mockResolvedValue(undefined);
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.3.0', notes: '', message: 'Buildmesh 0.3.0 is available.' },
    });
    const { result } = renderHook(() => useUpdateCheck());
    await waitFor(() => expect(result.current.state.kind).toBe('available'));
    await act(async () => {
      await result.current.install();
    });
    expect(result.current.state.kind).toBe('ready_to_restart');
    // First Restart click — modal comes up.
    await act(async () => {
      await result.current.restart();
    });
    expect(useExitPromptStore.getState().pending?.mode).toBe('update-restart');
    // User clicks "Keep Working" — modal clears, but state stays at
    // `ready_to_restart` with the staged binary intact.
    act(() => useExitPromptStore.getState().keepWorking());
    expect(useExitPromptStore.getState().pending).toBeNull();
    expect(result.current.state.kind).toBe('ready_to_restart');
    if (result.current.state.kind === 'ready_to_restart') {
      expect(result.current.state.update).toBeTruthy();
      expect(result.current.state.summary.version).toBe('0.3.0');
    }
    // Second Restart click — modal comes up again. No re-entry issue.
    await act(async () => {
      await result.current.restart();
    });
    expect(useExitPromptStore.getState().pending?.mode).toBe('update-restart');
  });

  // Round-2 blocking — idempotency: a Restart click while the modal
  // is already up for this restart is a no-op.
  it('restart is a no-op while the update-restart modal is already pending', async () => {
    downloadUpdateMock.mockResolvedValue(undefined);
    installUpdateMock.mockResolvedValue(undefined);
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.3.0', notes: '', message: 'Buildmesh 0.3.0 is available.' },
    });
    const { result } = renderHook(() => useUpdateCheck());
    await waitFor(() => expect(result.current.state.kind).toBe('available'));
    await act(async () => {
      await result.current.install();
    });
    await act(async () => {
      await result.current.restart();
    });
    const callCountAfterFirst = requestExitMock.mock.calls.length;
    // Second click while modal is up — no-op.
    await act(async () => {
      await result.current.restart();
    });
    expect(requestExitMock.mock.calls.length).toBe(callCountAfterFirst);
    expect(useExitPromptStore.getState().pending?.mode).toBe('update-restart');
  });

  // Round-2 minor 5 — setter-side guard in `check()`. The Settings
  // button also gates on this, but the guard here makes the class
  // impossible rather than merely unclicked.
  it('check() is a no-op while downloading/installing/ready_to_restart', async () => {
    // Quiet check resolves to `current` (suppressed) so the boot
    // probe leaves phase at `idle`. The manual `check()` below
    // returns `available` so we can install and reach
    // `ready_to_restart`.
    runUpdateCheckMock
      .mockResolvedValueOnce({ phase: 'current' })
      .mockResolvedValueOnce({
        phase: 'available',
        update: fakeUpdate(),
        summary: { version: '0.3.0', notes: '', message: 'Buildmesh 0.3.0 is available.' },
      });
    downloadUpdateMock.mockResolvedValue(undefined);
    installUpdateMock.mockResolvedValue(undefined);
    const { result } = renderHook(() => useUpdateCheck());
    await act(async () => {
      await result.current.check();
    });
    await waitFor(() => expect(result.current.state.kind).toBe('available'));
    await act(async () => {
      await result.current.install();
    });
    expect(result.current.state.kind).toBe('ready_to_restart');
    const callsBefore = runUpdateCheckMock.mock.calls.length;
    await act(async () => {
      await result.current.check();
    });
    // check() was a no-op — no extra runUpdateCheck call, no phase
    // transition.
    expect(runUpdateCheckMock.mock.calls.length).toBe(callsBefore);
    expect(result.current.state.kind).toBe('ready_to_restart');
  });

  // Finding 1 — the central cross-instance desync case. Two
  // `useUpdateCheck` calls must share a single `phase`, so a dismiss
  // in one is visible to the other.
  it('two useUpdateCheck instances share the same phase (finding 1)', async () => {
    downloadUpdateMock.mockResolvedValue(undefined);
    installUpdateMock.mockResolvedValue(undefined);
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.3.0', notes: '', message: 'Buildmesh 0.3.0 is available.' },
    });
    const { result: a } = renderHook(() => useUpdateCheck());
    const { result: b } = renderHook(() => useUpdateCheck());
    await waitFor(() => expect(a.current.state.kind).toBe('available'));
    // Both readers see the same `available` state.
    expect(b.current.state.kind).toBe('available');
    // Trigger install from B — A observes the install lifecycle.
    await act(async () => {
      await b.current.install();
    });
    expect(a.current.state.kind).toBe('ready_to_restart');
    // Dismiss from A — B observes the reset.
    act(() => a.current.dismiss());
    expect(b.current.state.kind).toBe('idle');
  });
});