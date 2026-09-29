import { describe, expect, it } from 'vitest';
import { getNodeStatusConfig, nodeInputContext } from '../../src/lib/status';
import type { LifecycleChangedPayload } from '../../src/types/generated/LifecycleChangedPayload';

const observation = (overrides: Partial<LifecycleChangedPayload> = {}): LifecycleChangedPayload => ({
  session_id: 1, provider: 'anthropic', kind: 'background_running', status: 'running',
  message: 'Waiting for child agents', provider_event: 'Stop', provider_session_id: null,
  completion_reason: null, transcript_path: null, timestamp: '2026-09-29T12:00:00Z',
  signal_health: 'ok', semantic_turn: null, ...overrides,
});

describe('node observation presentation', () => {
  it('retains background work across JSON snapshot round trips', () => {
    const node = JSON.parse(JSON.stringify({ status: 'running', lifecycle: observation() }));
    expect(getNodeStatusConfig(node).label).toBe('Waiting for background work');
    expect(getNodeStatusConfig({ ...node, status: 'ready' }).label).toBe('Ready');
  });

  it('restores the human request and distinguishes it from a completed turn', () => {
    const node = { status: 'awaiting_input' as const, lifecycle: observation({
      kind: 'question_requested', status: 'awaiting_input', message: 'Which branch should I use?',
    }) };
    expect(getNodeStatusConfig(node).label).toBe('Needs an answer');
    expect(nodeInputContext(node)).toBe('Which branch should I use?');
    expect(nodeInputContext({ ...node, status: 'ready' })).toBeUndefined();
  });
});
