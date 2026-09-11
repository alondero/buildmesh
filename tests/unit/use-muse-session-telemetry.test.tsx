import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { useMuseSessionTelemetry } from '../../src/hooks/useMuseSessionTelemetry';
import { MUSE_SESSION_TELEMETRY_EVENT } from '../../src/lib/tauri';
import type { ObservedMuseSessionTelemetry } from '../../src/types/generated/ObservedMuseSessionTelemetry';

const SNAPSHOT: ObservedMuseSessionTelemetry = {
  kind: 'observed_session_telemetry',
  node_id: 9,
  session_id: 'sess-aaaa-1111',
  model_id: null,
  last_turn: null,
  cumulative: { prompt_tokens: 11, output_tokens: 5, total_tokens: 16 },
  context: null,
};

describe('useMuseSessionTelemetry (issue #1680)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it('does not fetch telemetry for a non-Muse node', async () => {
    const { result } = renderHook(() => useMuseSessionTelemetry(9, 'anthropic'));
    await Promise.resolve();
    expect(result.current).toBeNull();
    expect(vi.mocked(invoke).mock.calls.some(([cmd]) => cmd === 'get_muse_session_telemetry')).toBe(false);
  });

  it('fetches and then follows live updates for a Muse node', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_muse_session_telemetry') return Promise.resolve(SNAPSHOT);
      return Promise.resolve(null);
    });
    const { result } = renderHook(() => useMuseSessionTelemetry(9, 'muse'));
    await waitFor(() => expect(result.current?.cumulative.total_tokens).toBe(16));
    expect(vi.mocked(invoke)).toHaveBeenCalledWith('get_muse_session_telemetry', { nodeId: 9 });

    const next = { ...SNAPSHOT, cumulative: { prompt_tokens: 20, output_tokens: 10, total_tokens: 30 } };
    await emit(MUSE_SESSION_TELEMETRY_EVENT, next);
    await waitFor(() => expect(result.current?.cumulative.total_tokens).toBe(30));
  });
});
