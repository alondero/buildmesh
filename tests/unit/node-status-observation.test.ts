import { describe, expect, it } from 'vitest';
import { getNodeStatusConfig, isSignalHealthProblem, nodeInputContext, signalHealthNote } from '../../src/lib/status';
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

describe('signal health presentation', () => {
  // The badge is the scarce title-bar slot (DESIGN.md principle 6). "Nothing
  // observed yet" is not a fault the user can act on, so it must never claim it.
  it.each([
    ['ok', false],
    ['unverified', false],
    ['degraded', true],
    ['unavailable', true],
  ] as const)('treats %s as a problem: %s', (health, expected) => {
    expect(isSignalHealthProblem(health)).toBe(expected);
  });

  it('treats an unknown health as no problem rather than a fault', () => {
    expect(isSignalHealthProblem(null)).toBe(false);
    expect(isSignalHealthProblem(undefined)).toBe(false);
    expect(signalHealthNote(null)).toBeUndefined();
    expect(signalHealthNote('ok')).toBeUndefined();
  });

  it('keeps unverified discoverable through the status tooltip', () => {
    expect(signalHealthNote('unverified')).toContain('not confirmed yet');
    const node = { status: 'running' as const, lifecycle: observation(), signal_health: 'unverified' as const };
    expect(getNodeStatusConfig(node).title).toContain('not confirmed yet');
  });

  it('keeps a real fault in the tooltip without a live observation', () => {
    const node = { status: 'idle' as const, lifecycle: null, signal_health: 'unavailable' as const };
    const config = getNodeStatusConfig(node);
    expect(config.title).toContain('No status signal is reaching Buildmesh');
    expect(config.label).toBe('Idle');
  });

  // The two faults are different failures and must not share generic copy:
  // `degraded` means a signal arrived unreadable, `unavailable` means none does.
  it('distinguishes the two faults in their copy', () => {
    expect(signalHealthNote('degraded')).toContain('arrived but could not be interpreted');
    expect(signalHealthNote('unavailable')).toContain('No status signal is reaching Buildmesh');
    expect(signalHealthNote('degraded')).not.toBe(signalHealthNote('unavailable'));
  });
});
