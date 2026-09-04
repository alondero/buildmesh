/**
 * useWorktreeEffectiveDir (issue #1519): resolves a mesh's effective
 * worktree container dir once per meshId via the backend-authoritative
 * `get_worktree_directory_config` (Mesh override → app default →
 * `.claude/worktrees`). Null while loading / without a mesh / on error
 * (callers treat null as "no extras" — legacy matching only).
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { useWorktreeEffectiveDir } from '../../src/hooks/useWorktreeEffectiveDir';

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
});
