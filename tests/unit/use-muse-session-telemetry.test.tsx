import { describe, it, expect, vi, beforeEach } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit, listen } from '@tauri-apps/api/event';
import { useMuseSessionTelemetry } from '../../src/hooks/useMuseSessionTelemetry';
import { MUSE_SESSION_TELEMETRY_EVENT } from '../../src/lib/tauri';
import type { ObservedMuseSessionTelemetry } from '../../src/types/generated/ObservedMuseSessionTelemetry';

function snapshot(nodeId: number, total: number): ObservedMuseSessionTelemetry {
  return {
    kind: 'observed_session_telemetry',
    node_id: nodeId,
    session_id: `sess-${nodeId}`,
    model_id: null,
    last_turn: null,
    cumulative: { prompt_tokens: total - 5, output_tokens: 5, total_tokens: total },
    context: null,
  };
}

describe('useMuseSessionTelemetry (issue #1680)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(listen).mockClear();
  });

  it('does not fetch telemetry for a non-Muse node', async () => {
    const { result } = renderHook(() => useMuseSessionTelemetry(9, 'anthropic'));
    await Promise.resolve();
    expect(result.current).toBeNull();
    expect(vi.mocked(invoke).mock.calls.some(([cmd]) => cmd === 'get_muse_session_telemetry')).toBe(false);
  });

  it('fetches and then follows live updates for a Muse node', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_muse_session_telemetry') return Promise.resolve(snapshot(9, 16));
      return Promise.resolve(null);
    });
    const { result } = renderHook(() => useMuseSessionTelemetry(9, 'muse'));
    await waitFor(() => expect(result.current?.cumulative.total_tokens).toBe(16));
    expect(vi.mocked(invoke)).toHaveBeenCalledWith('get_muse_session_telemetry', { nodeId: 9 });

    await emit(MUSE_SESSION_TELEMETRY_EVENT, snapshot(9, 30));
    await waitFor(() => expect(result.current?.cumulative.total_tokens).toBe(30));
  });

  it('ignores live events for a different node', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_muse_session_telemetry') return Promise.resolve(snapshot(9, 16));
      return Promise.resolve(null);
    });
    const { result } = renderHook(() => useMuseSessionTelemetry(9, 'muse'));
    await waitFor(() => expect(result.current?.cumulative.total_tokens).toBe(16));
    await emit(MUSE_SESSION_TELEMETRY_EVENT, snapshot(10, 950));
    expect(result.current?.node_id).toBe(9);
    expect(result.current?.cumulative.total_tokens).toBe(16);
  });

  it('does not commit an older fetch after nodeId changes', async () => {
    const pending = new Map<number, { resolve: (value: ObservedMuseSessionTelemetry) => void; reject: (reason?: unknown) => void }>();
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      if (cmd !== 'get_muse_session_telemetry') return Promise.resolve(null);
      const id = args?.nodeId as number;
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
      });
    });

    const { result, rerender } = renderHook(
      ({ nodeId }) => useMuseSessionTelemetry(nodeId, 'muse'),
      { initialProps: { nodeId: 1 } },
    );
    rerender({ nodeId: 2 });
    expect(result.current).toBeNull();

    await act(async () => {
      pending.get(2)?.resolve(snapshot(2, 40));
    });
    await waitFor(() => expect(result.current?.node_id).toBe(2));
    expect(result.current?.cumulative.total_tokens).toBe(40);

    await act(async () => {
      pending.get(1)?.resolve(snapshot(1, 16));
    });
    expect(result.current?.node_id).toBe(2);
    expect(result.current?.cumulative.total_tokens).toBe(40);
  });

  it('does not let an older rejection clear a newer success', async () => {
    const pending = new Map<number, { resolve: (value: ObservedMuseSessionTelemetry) => void; reject: (reason?: unknown) => void }>();
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      if (cmd !== 'get_muse_session_telemetry') return Promise.resolve(null);
      const id = args?.nodeId as number;
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
      });
    });

    const { result, rerender } = renderHook(
      ({ nodeId }) => useMuseSessionTelemetry(nodeId, 'muse'),
      { initialProps: { nodeId: 1 } },
    );
    rerender({ nodeId: 2 });
    await act(async () => {
      pending.get(2)?.resolve(snapshot(2, 40));
    });
    await waitFor(() => expect(result.current?.cumulative.total_tokens).toBe(40));

    await act(async () => {
      pending.get(1)?.reject(new Error('stale'));
    });
    expect(result.current?.node_id).toBe(2);
    expect(result.current?.cumulative.total_tokens).toBe(40);
  });

  it('does not commit after unmount', async () => {
    let resolveFetch: ((value: ObservedMuseSessionTelemetry) => void) | undefined;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd !== 'get_muse_session_telemetry') return Promise.resolve(null);
      return new Promise((resolve) => {
        resolveFetch = resolve;
      });
    });
    const { result, unmount } = renderHook(() => useMuseSessionTelemetry(9, 'muse'));
    unmount();
    await act(async () => {
      resolveFetch?.(snapshot(9, 16));
    });
    expect(result.current).toBeNull();
  });

  it('unsubscribes the Tauri listener on unmount', async () => {
    const unlisten = vi.fn();
    vi.mocked(listen).mockImplementationOnce(() => Promise.resolve(unlisten));
    vi.mocked(invoke).mockResolvedValue(null);
    const { unmount } = renderHook(() => useMuseSessionTelemetry(9, 'muse'));
    await waitFor(() => expect(listen).toHaveBeenCalledWith(
      MUSE_SESSION_TELEMETRY_EVENT,
      expect.any(Function),
    ));
    unmount();
    await waitFor(() => expect(unlisten).toHaveBeenCalledTimes(1));
  });

  it('treats an IPC rejection as no observations for the active node', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_muse_session_telemetry') return Promise.reject(new Error('backend gone'));
      return Promise.resolve(null);
    });
    const { result } = renderHook(() => useMuseSessionTelemetry(9, 'muse'));
    await waitFor(() => expect(result.current).toBeNull());
  });
});
