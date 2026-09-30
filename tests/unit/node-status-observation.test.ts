import { describe, expect, it } from 'vitest';
import { getNodeStatusConfig, isSignalHealthProblem, nodeInputContext, nodeInputRequest, signalHealthNote } from '../../src/lib/status';
import type { LifecycleChangedPayload } from '../../src/types/generated/LifecycleChangedPayload';

const observation = (overrides: Partial<LifecycleChangedPayload> = {}): LifecycleChangedPayload => ({
  session_id: 1, provider: 'anthropic', kind: 'background_running', status: 'running',
  message: 'Waiting for child agents', provider_event: 'Stop', provider_session_id: null,
  completion_reason: null, transcript_path: null, timestamp: '2026-09-29T12:00:00Z',
  signal_health: 'ok', semantic_turn: null, ...overrides,
});

const awaiting = (overrides: Partial<LifecycleChangedPayload> = {}) => ({
  status: 'awaiting_input' as const,
  lifecycle: observation({ status: 'awaiting_input', kind: 'input_required', ...overrides }),
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

describe('input request semantics (issue #1966)', () => {
  it('offers permission actions only for a permission observation', () => {
    expect(nodeInputRequest(awaiting({ kind: 'permission_requested' }))?.mode).toBe('permission');
    // A question, an unclassified yield, and an unobserved request must all
    // refuse the yes/no chips: `y`/`n` are a guess about a harness prompt
    // Buildmesh never read.
    expect(nodeInputRequest(awaiting({ kind: 'question_requested' }))?.mode).toBe('question');
    expect(nodeInputRequest(awaiting({ kind: 'input_required' }))?.mode).toBe('unknown');
    expect(nodeInputRequest({ status: 'awaiting_input', lifecycle: null })?.mode).toBe('unknown');
  });

  it('ignores an observation that no longer describes the current status', () => {
    // The DB drops a stale snapshot, but a client may hold one: a running
    // node's old permission observation must not shape its reply controls.
    const node = { status: 'awaiting_input' as const, lifecycle: observation({ kind: 'permission_requested' }) };
    expect(nodeInputRequest(node)?.mode).toBe('unknown');
  });

  it('reports no request for a node that is not waiting on one', () => {
    expect(nodeInputRequest({ status: 'running', lifecycle: observation() })).toBeUndefined();
  });

  it('surfaces enumerated choices only when the harness supplied them', () => {
    const offered = awaiting({ kind: 'question_requested', request: { choices: ['Staging', 'Production'] } });
    expect(nodeInputRequest(offered)?.choices).toEqual(['Staging', 'Production']);
    // An open question, and a request snapshot from before the field existed.
    expect(nodeInputRequest(awaiting({ kind: 'question_requested' }))?.choices).toEqual([]);
    expect(nodeInputRequest({ status: 'awaiting_input', lifecycle: null })?.choices).toEqual([]);
  });

  it('never reads a choice list off a permission decision', () => {
    const node = awaiting({ kind: 'permission_requested', request: { choices: ['Yes', 'No'] } });
    expect(nodeInputRequest(node)?.choices).toEqual([]);
  });

  it('gives each request its own identity, including a replacement on the same node', () => {
    const first = awaiting({ kind: 'question_requested', timestamp: '2026-09-29T12:00:00Z' });
    const second = awaiting({ kind: 'question_requested', timestamp: '2026-09-29T12:01:00Z' });
    expect(nodeInputRequest(first)?.key).not.toBe(nodeInputRequest(second)?.key);
    // A different kind at the same instant is a different request too.
    expect(nodeInputRequest(first)?.key).not.toBe(
      nodeInputRequest(awaiting({ kind: 'permission_requested', timestamp: '2026-09-29T12:00:00Z' }))?.key,
    );
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
