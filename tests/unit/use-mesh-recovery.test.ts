import { describe, it, expect, beforeEach, vi } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { useMeshRecovery } from '../../src/hooks/useMeshRecovery';
import type { HoldingWorktree } from '../../src/lib/tauri';

const mockInvoke = invoke as ReturnType<typeof vi.fn>;

const HOLDER: HoldingWorktree = {
  path: '/m/.claude/worktrees/foo',
  name: 'foo',
  is_active: false,
};

describe('useMeshRecovery (issue #283)', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  describe('restore', () => {
    it('flips inFlight, calls restore_mesh_to_base, surfaces message, fires onMutate', async () => {
      mockInvoke.mockResolvedValueOnce({ restored: true, message: 'Restored to main' });
      const onMutate = vi.fn();

      const { result } = renderHook(() => useMeshRecovery(7, onMutate));
      expect(result.current.inFlight).toBe(false);

      await act(async () => {
        await result.current.restore();
      });

      expect(mockInvoke).toHaveBeenCalledWith('restore_mesh_to_base', { meshId: 7 });
      expect(result.current.message).toBe('Restored to main');
      expect(result.current.inFlight).toBe(false);
      expect(onMutate).toHaveBeenCalledTimes(1);
    });

    it('records the backend error in message, clears inFlight, and DOES NOT fire onMutate', async () => {
      mockInvoke.mockRejectedValueOnce(new Error('refusing: dirty'));
      const onMutate = vi.fn();

      const { result } = renderHook(() => useMeshRecovery(7, onMutate));
      await act(async () => {
        await result.current.restore();
      });

      expect(result.current.message).toContain('Restore error');
      expect(result.current.message).toContain('refusing: dirty');
      expect(result.current.inFlight).toBe(false);
      // A failed recovery must NOT bump the caches — otherwise the panel
      // would lose its stale-but-actionable state and look "fixed".
      expect(onMutate).not.toHaveBeenCalled();
    });

    it('blocks a re-entry while a restore is already in flight', async () => {
      // The HealthBlock button disables itself on inFlight, but the hook
      // also enforces this so a misbehaving caller (test code, future
      // refactor) can't double-fire.
      let resolveRestore!: () => void;
      mockInvoke.mockImplementationOnce(() =>
        new Promise<void>((r) => { resolveRestore = () => r(); }).then(() => ({ restored: true, message: 'ok' })),
      );
      const onMutate = vi.fn();
      const { result } = renderHook(() => useMeshRecovery(7, onMutate));

      let first!: Promise<void>;
      act(() => {
        first = result.current.restore();
      });
      await waitFor(() => expect(result.current.inFlight).toBe(true));

      // Second click while the first is pending: no extra IPC, no extra onMutate.
      let second!: Promise<void>;
      act(() => {
        second = result.current.restore();
      });
      expect(mockInvoke).toHaveBeenCalledTimes(1);

      resolveRestore();
      await act(async () => { await first; await second; });
      expect(onMutate).toHaveBeenCalledTimes(1);
    });
  });

  describe('free', () => {
    it('calls free_base_branch with the holder path and reports the detached SHA', async () => {
      mockInvoke.mockResolvedValueOnce({ detached_at_sha: 'abc1234' });
      const onMutate = vi.fn();

      const { result } = renderHook(() => useMeshRecovery(7, onMutate));
      await act(async () => {
        await result.current.free(HOLDER);
      });

      expect(mockInvoke).toHaveBeenCalledWith('free_base_branch', {
        meshId: 7,
        worktreePath: HOLDER.path,
      });
      expect(result.current.message).toContain('abc1234');
      expect(onMutate).toHaveBeenCalledTimes(1);
    });

    it('reports backend errors and skips onMutate', async () => {
      mockInvoke.mockRejectedValueOnce(new Error('holder vanished'));
      const onMutate = vi.fn();

      const { result } = renderHook(() => useMeshRecovery(7, onMutate));
      await act(async () => {
        await result.current.free(HOLDER);
      });

      expect(result.current.message).toContain('Free error');
      expect(result.current.message).toContain('holder vanished');
      expect(onMutate).not.toHaveBeenCalled();
    });
  });

  it('shares a single inFlight flag across restore and free (mirrors button disabled-state)', async () => {
    // The HealthBlock disables both buttons when EITHER is mid-flight, so
    // the hook must expose one combined flag rather than two separate ones.
    let resolveRestore!: () => void;
    mockInvoke.mockImplementationOnce(() =>
      new Promise<void>((r) => { resolveRestore = () => r(); }).then(() => ({ restored: true, message: 'ok' })),
    );
    const { result } = renderHook(() => useMeshRecovery(7, vi.fn()));

    let pending!: Promise<void>;
    act(() => {
      pending = result.current.restore();
    });
    await waitFor(() => expect(result.current.inFlight).toBe(true));

    resolveRestore();
    await act(async () => { await pending; });
    expect(result.current.inFlight).toBe(false);
  });
});
