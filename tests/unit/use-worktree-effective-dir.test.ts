/**
 * useWorktreeEffectiveDir (issue #1519): resolves a mesh's effective
 * worktree container dir once per meshId via the backend-authoritative
 * `get_worktree_directory_config` (Mesh override → app default →
 * `.claude/worktrees`). Null while loading / without a mesh / on error
 * (callers treat null as "no extras" — legacy matching only).
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, waitFor, act } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { useWorktreeEffectiveDir } from '../../src/hooks/useWorktreeEffectiveDir';
import {
  WORKTREE_DIR_CHANGED_EVENT,
} from '../../src/lib/events';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

describe('useWorktreeEffectiveDir', () => {
  beforeEach(() => vi.mocked(invoke).mockReset());

  it('resolves the effective directory from the backend config', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_worktree_directory_config') {
        return Promise.resolve({
          mesh_directory: 'custom-wt',
          app_directory: null,
          effective_directory: '/repo/mesh/custom-wt',
        });
      }
      return Promise.resolve({});
    });
    const { result } = renderHook(() => useWorktreeEffectiveDir(7));
    await waitFor(() => expect(result.current).toBe('/repo/mesh/custom-wt'));
    expect(invoke).toHaveBeenCalledWith('get_worktree_directory_config', { meshId: 7 });
  });

  it('returns null without a mesh and on backend failure', async () => {
    const { result, rerender } = renderHook(
      ({ id }: { id: number | null }) => useWorktreeEffectiveDir(id),
      { initialProps: { id: null as number | null } },
    );
    expect(result.current).toBeNull();
    expect(invoke).not.toHaveBeenCalled();

    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_worktree_directory_config') {
        return Promise.reject(new Error('gone'));
      }
      return Promise.resolve({});
    });
    rerender({ id: 9 });
    await waitFor(() => expect(invoke).toHaveBeenCalled());
    await waitFor(() => expect(result.current).toBeNull());
  });

  it('exposes the event name as a single source of truth (stringly-typed drift guard)', () => {
    // Symmetric to the Rust `worktree_dir_changed_event_name_matches_frontend_constant`
    // test — keeps the two halves of the IPC contract from drifting.
    expect(WORKTREE_DIR_CHANGED_EVENT).toBe('worktree-directory-changed');
  });

  it('re-resolves on worktree-directory-changed for this mesh or all meshes', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_worktree_directory_config') {
        return Promise.resolve({
          mesh_directory: null,
          app_directory: null,
          effective_directory: '/repo/mesh/.claude/worktrees',
        });
      }
      return Promise.resolve({});
    });
    const { result } = renderHook(() => useWorktreeEffectiveDir(7));
    await waitFor(() => expect(result.current).toBe('/repo/mesh/.claude/worktrees'));
    const callsAfterMount = vi.mocked(invoke).mock.calls.length;

    // Matching mesh id refetches.
    await act(async () => {
      await emit(WORKTREE_DIR_CHANGED_EVENT, 7);
    });
    await waitFor(() =>
      expect(vi.mocked(invoke).mock.calls.length).toBeGreaterThan(callsAfterMount),
    );

    // Unrelated mesh id does not.
    const callsAfterMatch = vi.mocked(invoke).mock.calls.length;
    await act(async () => {
      await emit(WORKTREE_DIR_CHANGED_EVENT, 8);
    });
    await new Promise((r) => setTimeout(r, 50));
    expect(vi.mocked(invoke).mock.calls.length).toBe(callsAfterMatch);

    // Null payload (app default moved) refetches.
    await act(async () => {
      await emit(WORKTREE_DIR_CHANGED_EVENT, null);
    });
    await waitFor(() =>
      expect(vi.mocked(invoke).mock.calls.length).toBeGreaterThan(callsAfterMatch),
    );
  });
});
