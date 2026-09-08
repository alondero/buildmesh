/**
 * `useUpdateCheck` state-machine transitions (issue #1526).
 *
 * The hook is the single source of truth that `UpdatePrompt` and
 * Settings > About both subscribe to. This test exercises the
 * transitions end-to-end: quiet vs manual check surfacing, install
 * progress, exit-readiness routing on restart, and the sanitize +
 * Retry cycle on failure.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import type { Update } from '@tauri-apps/plugin-updater';
import type { AgentNode } from '../../src/types/generated/AgentNode';

const {
  runUpdateCheckMock,
  downloadAndInstallUpdateMock,
  relaunchMock,
  showExitPromptMock,
  listProvidersMock,
  getAgentNodesMock,
} = vi.hoisted(() => ({
  runUpdateCheckMock: vi.fn(),
  downloadAndInstallUpdateMock: vi.fn(),
  relaunchMock: vi.fn().mockResolvedValue(undefined),
  showExitPromptMock: vi.fn(),
  listProvidersMock: vi.fn(),
  getAgentNodesMock: vi.fn(),
}));

vi.mock('../../src/lib/updater', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/lib/updater')>();
  return {
    ...actual,
    runUpdateCheck: runUpdateCheckMock,
    downloadAndInstallUpdate: downloadAndInstallUpdateMock,
  };
});

vi.mock('@tauri-apps/plugin-process', () => ({
  relaunch: relaunchMock,
}));

vi.mock('../../src/stores/exitPromptStore', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/stores/exitPromptStore')>();
  return {
    ...actual,
    useExitPromptStore: {
      getState: () => ({
        showExitPrompt: showExitPromptMock,
        // Default ON — mirrors the production default (issue #1501)
        // so tests exercise the prompt path. Individual tests can
        // override via `vi.mocked(...).getState.mockReturnValue(...)`.
        confirmBeforeQuit: true,
      }),
    },
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

const fakeUpdate = (): Update => ({
  version: '0.3.0',
  body: '',
} as unknown as Update);

describe('useUpdateCheck (issue #1526 — state machine)', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    // `vi.clearAllMocks()` clears call history but does NOT restore
    // spies. The `confirmBeforeQuit off` test below installs a spy
    // on `useExitPromptStore.getState`; without restoring it, every
    // later test would inherit the spy's `mockReturnValue` and the
    // `confirmBeforeQuit = true` default would never be re-applied.
    vi.restoreAllMocks();
    getAgentNodesMock.mockReturnValue([]);
    listProvidersMock.mockResolvedValue([]);
  });

  it('mounts with a quiet check that surfaces only `available`', async () => {
    // The quiet check suppresses `current` / `unreachable` / `disabled`
    // — those are the Settings > About surface, not a nag.
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
    // Wait a tick for the microtask to settle; the state should NOT
    // have transitioned away from `idle` because quiet checks ignore
    // `current`.
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

  it('install walks downloading → installing → ready_to_restart', async () => {
    downloadAndInstallUpdateMock.mockImplementation(
      async (
        _u: Update,
        onProgress?: (p: { downloaded: number; total: number | null }) => void,
      ) => {
        onProgress?.({ downloaded: 50, total: 100 });
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
    expect(result.current.state.kind).toBe('ready_to_restart');
  });

  it('install failure transitions to `failed` with retry button', async () => {
    downloadAndInstallUpdateMock.mockRejectedValue(new Error('disk full'));
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

  it('retry re-runs the failed install from the preserved handle', async () => {
    downloadAndInstallUpdateMock
      .mockRejectedValueOnce(new Error('disk full'))
      .mockResolvedValueOnce(undefined);
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
    expect(downloadAndInstallUpdateMock).toHaveBeenCalledTimes(2);
    expect(result.current.state.kind).toBe('ready_to_restart');
  });

  it('dismiss resets state to idle without dropping the staged update', async () => {
    downloadAndInstallUpdateMock.mockResolvedValue(undefined);
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

  it('restart with no active agents calls relaunch() directly', async () => {
    downloadAndInstallUpdateMock.mockResolvedValue(undefined);
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
    expect(showExitPromptMock).not.toHaveBeenCalled();
  });

  it('restart with confirmBeforeQuit off bypasses the prompt even with active nodes', async () => {
    // User preference: don't prompt. The updater must honor it (issue
    // #1501 — single preference, two flows). Without this, the user
    // can't get the original issue #826 "update silently and restart"
    // behavior even when they've opted out of the close prompt.
    const terminalNode = {
      id: 7,
      status: 'running',
      provider: 'terminal',
      cli_session_id: 'sess-7',
    } as unknown as AgentNode;
    getAgentNodesMock.mockReturnValue([terminalNode]);
    // Override the default `confirmBeforeQuit = true` from the mock
    // factory by replacing getState for this test.
    const { useExitPromptStore } = await import('../../src/stores/exitPromptStore');
    const originalGetState = useExitPromptStore.getState;
    vi.spyOn(useExitPromptStore, 'getState').mockReturnValue({
      ...originalGetState(),
      confirmBeforeQuit: false,
    } as ReturnType<typeof originalGetState>);
    downloadAndInstallUpdateMock.mockResolvedValue(undefined);
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
    expect(showExitPromptMock).not.toHaveBeenCalled();
    expect(relaunchMock).toHaveBeenCalledTimes(1);
  });

  it('restart with active non-resumable agents shows the exit prompt', async () => {
    // Issue #1526 — the updater must NOT own a divergent active-node
    // policy. With an active non-resumable harness (terminal), the
    // updater routes through the same exit-prompt store the window-
    // close flow uses.
    const terminalNode = {
      id: 7,
      status: 'running',
      provider: 'terminal',
      cli_session_id: 'sess-7',
    } as unknown as AgentNode;
    getAgentNodesMock.mockReturnValue([terminalNode]);
    listProvidersMock.mockResolvedValue([
      {
        id: 'terminal',
        harness_id: 'terminal',
        is_proxied: false,
        label: 'Terminal',
        capabilities: { supports_resume: false },
      },
    ]);
    downloadAndInstallUpdateMock.mockResolvedValue(undefined);
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
    expect(showExitPromptMock).toHaveBeenCalledTimes(1);
    expect(showExitPromptMock.mock.calls[0][0]).toBe('update-restart');
    // Relaunch is NOT called yet — the modal's confirm dispatches it.
    expect(relaunchMock).not.toHaveBeenCalled();
  });

  it('restart with active resumable agents still prompts (shared policy)', async () => {
    // The shared Exit Readiness policy (#1501): if any active node
    // exists AND `confirmBeforeQuit` is on, surface the confirmation
    // modal. The modal itself suppresses the warning block when
    // `nonResumable` is empty (handled in `ExitConfirmationModal`),
    // so the prompt here is a one-click confirm rather than a
    // blocking warning — but it still fires once so the user
    // explicitly acknowledges a restart with active work.
    const claudeNode = {
      id: 11,
      status: 'running',
      provider: 'claude',
      cli_session_id: 'sess-11',
    } as unknown as AgentNode;
    getAgentNodesMock.mockReturnValue([claudeNode]);
    listProvidersMock.mockResolvedValue([
      {
        id: 'claude',
        harness_id: 'claude',
        is_proxied: false,
        label: 'Claude Code',
        capabilities: { supports_resume: true },
      },
    ]);
    downloadAndInstallUpdateMock.mockResolvedValue(undefined);
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
    expect(showExitPromptMock).toHaveBeenCalledTimes(1);
    // Crucially, `nonResumable` is empty — the modal suppresses its
    // warning block; the prompt is informational, not blocking.
    expect(showExitPromptMock.mock.calls[0][2]).toEqual([]);
    // Relaunch is NOT called yet — the modal's confirm dispatches it.
    expect(relaunchMock).not.toHaveBeenCalled();
  });
});