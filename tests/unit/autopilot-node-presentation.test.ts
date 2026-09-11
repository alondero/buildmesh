import { describe, expect, it } from 'vitest';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { AutopilotRunState } from '../../src/types/generated/AutopilotRunStateKind';
import type { CircuitAgentOwnership } from '../../src/types/generated/CircuitAgentOwnership';
import type { SemanticTurnPayload } from '../../src/types/generated/SemanticTurnPayload';
import { getAutopilotNodePresentation, getAutopilotRunDetails, hasActiveAutopilotOwnership, resolveAutopilotOutcome } from '../../src/lib/autopilotNodePresentation';

const node = (status: AgentNode['status'] = 'running'): AgentNode => ({
  id: 1,
  mesh_id: 1,
  name: 'node',
  path: '/repo',
  branch: 'main',
  env: 'windows',
  provider: 'anthropic',
  status,
  use_worktree: false,
  position: 0,
  created_at: '',
});

const ownership = (state: string): CircuitAgentOwnership => ({
  node_id: 1,
  run_id: 2,
  circuit_id: 3,
  circuit_name: 'Review',
  state,
  parent_node_id: null,
});

describe('getAutopilotNodePresentation', () => {
  it('keeps rich legacy pipeline copy beside the shared presentation mapping', () => {
    expect(getAutopilotRunDetails('suffix_pending')).toEqual({
      label: 'autopilot · suffix',
      title: expect.stringContaining('suffix prompt'),
    });
  });

  it('keeps unpiloted nodes visually empty', () => {
    expect(getAutopilotNodePresentation(node())).toBeNull();
  });

  it.each([
    ['running', 'active'],
    ['spawning', 'active'],
  ] as const)('maps a legacy %s node to the active pilot light', (status, phase) => {
    expect(getAutopilotNodePresentation(node(status), 'implementing')?.phase).toBe(phase);
  });

  it.each(['ready', 'idle', 'awaiting_input', 'pending', 'suspended'] as const)(
    'maps a legacy node that is not driving (%s) to waiting',
    (status) => {
      expect(getAutopilotNodePresentation(node(status), 'finishing')?.phase).toBe('waiting');
    },
  );

  it('keeps legacy waiting details tied to the lifecycle state', () => {
    expect(getAutopilotNodePresentation(node('ready'), 'implementing')?.detail)
      .toBe('Autopilot is between turns after this Agent Node yielded cleanly.');
    expect(getAutopilotNodePresentation(node('idle'), 'implementing')?.detail)
      .toBe('Autopilot is waiting to start the next turn.');
    expect(getAutopilotNodePresentation(node('awaiting_input'), 'implementing')?.detail)
      .toBe('Autopilot is waiting for input from you.');
    expect(getAutopilotNodePresentation(node('pending'), 'implementing')?.detail)
      .toBe('Autopilot is waiting for this Agent Node to start.');
    expect(getAutopilotNodePresentation({ ...node('suspended'), cli_session_id: 'session-1' }, 'implementing')?.detail)
      .toBe('Autopilot is waiting for this Agent Node to recover its existing session.');
    expect(getAutopilotNodePresentation(node('suspended'), 'implementing')?.detail)
      .toBe('Autopilot is waiting for this Agent Node to recover before starting a new session.');
  });

  it.each(['completed', 'merged'] as const)('maps legacy %s to done', (state) => {
    expect(getAutopilotNodePresentation(node('completed'), state)?.phase).toBe('done');
  });

  it('maps a failed legacy run to waiting with an attention tone', () => {
    const result = getAutopilotNodePresentation(node(), 'failed');
    expect(result).toMatchObject({ phase: 'waiting', tone: 'error', label: 'Autopilot needs attention' });
  });

  it('keeps an error lifecycle in waiting with an attention tone', () => {
    expect(getAutopilotNodePresentation(node('error'), 'implementing')).toMatchObject({
      phase: 'waiting',
      tone: 'error',
    });
    expect(getAutopilotNodePresentation(node('error'), undefined, ownership('running'))).toMatchObject({
      phase: 'waiting',
      tone: 'error',
    });
  });

  it('gives Circuit ownership precedence over a legacy state during migration', () => {
    expect(getAutopilotNodePresentation(node(), 'implementing', ownership('paused'))?.phase).toBe('waiting');
  });

  it('maps a failed Circuit run to waiting with an attention tone', () => {
    expect(getAutopilotNodePresentation(node(), undefined, ownership('failed'))).toMatchObject({
      phase: 'waiting',
      tone: 'error',
    });
  });

  it.each(['pending', 'paused'] as const)('maps a non-driving Circuit run (%s) to waiting', (state) => {
    expect(getAutopilotNodePresentation(node('running'), undefined, ownership(state))?.phase).toBe('waiting');
  });

  it('distinguishes an explicit Circuit pause and queued Circuit work', () => {
    expect(getAutopilotNodePresentation(node('running'), undefined, ownership('paused'))?.detail)
      .toBe('Autopilot is paused by the Circuit.');
    expect(getAutopilotNodePresentation(node('running'), undefined, ownership('pending'))?.detail)
      .toBe('Autopilot is queued for the next Circuit step.');
  });

  it('maps a running Circuit run driving a running node to active', () => {
    expect(getAutopilotNodePresentation(node('running'), undefined, ownership('running'))?.phase).toBe('active');
  });

  it('reconciles a live ownership row on a completed node as waiting', () => {
    expect(getAutopilotNodePresentation(node('completed'), undefined, ownership('running'))).toMatchObject({
      phase: 'waiting',
      tone: 'warning',
      detail: 'Autopilot ownership is reconciling; the run is still live while this Agent Node reports completed.',
    });
  });

  it('retains a terminal Circuit source as done', () => {
    expect(getAutopilotNodePresentation(node('completed'), undefined, ownership('completed'))?.phase).toBe('done');
  });

  it('does not show a compact indicator for a cancelled or archived Circuit node', () => {
    expect(getAutopilotNodePresentation(node(), undefined, ownership('cancelled'))).toBeNull();
    expect(getAutopilotNodePresentation(node('archived'), undefined, ownership('completed'))).toBeNull();
  });

  it('treats unknown Circuit states as an attention state instead of active', () => {
    expect(getAutopilotNodePresentation(node(), undefined, ownership('mystery'))).toMatchObject({
      phase: 'waiting',
      tone: 'error',
    });
  });

  it('keeps malformed dual ownership conservative', () => {
    expect(getAutopilotNodePresentation(node(), 'implementing', ownership('mystery'))).toMatchObject({
      phase: 'waiting',
      tone: 'error',
      label: 'Autopilot needs attention',
    });
  });

  it('only treats live ownership as active for suspended-node recovery suppression', () => {
    expect(hasActiveAutopilotOwnership('implementing')).toBe(true);
    expect(hasActiveAutopilotOwnership('completed')).toBe(false);
    expect(hasActiveAutopilotOwnership(undefined, ownership('running'))).toBe(true);
    expect(hasActiveAutopilotOwnership(undefined, ownership('completed'))).toBe(false);
    expect(hasActiveAutopilotOwnership('implementing', ownership('completed'))).toBe(false);
  });
});

describe('resolveAutopilotOutcome', () => {
  const member = (id: number, status: AgentNode['status']): AgentNode => ({ ...node(status), id });
  const sources = (
    autopilotStates: Record<number, AutopilotRunState> = {},
    circuitOwnerships: Record<number, CircuitAgentOwnership> = {},
    semanticTurns: Record<number, SemanticTurnPayload> = {},
  ) => ({ autopilotStates, circuitOwnerships, semanticTurns });

  it('returns null for a healthy piloted node with no terminal outcome', () => {
    expect(resolveAutopilotOutcome([member(1, 'running')], sources({ 1: 'implementing' }))).toBeNull();
  });

  it('returns null for an unpiloted, healthy node', () => {
    expect(resolveAutopilotOutcome([member(1, 'running')], sources())).toBeNull();
  });

  it('surfaces an awaited run as needs_input and carries the requested action', () => {
    const result = resolveAutopilotOutcome(
      [member(1, 'awaiting_input')],
      sources({ 1: 'finishing' }, {}, { 1: { node_id: 1, kind: 'permission_request', description: 'approve the deploy' } }),
    );
    expect(result).toMatchObject({ kind: 'needs_input', phase: 'waiting', tone: 'warning', label: 'Needs input', nodeId: 1, nodeIds: [1] });
    expect(result?.detail).toContain('approve the deploy');
  });

  it('still flags an unowned awaiting node, so the chip is not Autopilot-only', () => {
    const result = resolveAutopilotOutcome([member(1, 'awaiting_input')], sources());
    expect(result?.detail).toContain('This agent is waiting');
  });

  it('aggregates every attention session, failures first', () => {
    const result = resolveAutopilotOutcome(
      [member(1, 'awaiting_input'), member(2, 'error')],
      sources({ 2: 'failed' }),
    );
    // The failure leads the copy, but the waiting session stays cyclable.
    expect(result).toMatchObject({ kind: 'failed', tone: 'error', nodeId: 2 });
    expect(result?.nodeIds).toEqual([2, 1]);
  });

  it('leaves completed automation to the existing green signals (no chip)', () => {
    // A finished run needs no action and is already told by the lifecycle dot,
    // the Pilot-light check, the PR pill, and the autopilot-pr-created toast.
    expect(resolveAutopilotOutcome([member(1, 'completed')], sources({ 1: 'completed' }))).toBeNull();
    expect(resolveAutopilotOutcome([member(1, 'completed')], sources({}, { 1: ownership('completed') }))).toBeNull();
  });

  it('still flags a waiting member beside a finished one', () => {
    const mixed = resolveAutopilotOutcome(
      [member(1, 'awaiting_input'), member(2, 'completed')],
      sources({ 1: 'implementing', 2: 'completed' }),
    );
    expect(mixed?.kind).toBe('needs_input');
  });

  it('keeps every attention session in cycling order, failures first', () => {
    const result = resolveAutopilotOutcome(
      [member(1, 'awaiting_input'), member(2, 'error'), member(3, 'awaiting_input')],
      sources({ 1: 'finishing', 2: 'failed', 3: 'finishing' }),
    );
    expect(result?.nodeIds).toEqual([2, 1, 3]);
    expect(result?.nodeId).toBe(2);
  });

  it('describes the focused session when it is one of the attention sessions', () => {
    const result = resolveAutopilotOutcome(
      [member(1, 'error'), member(2, 'awaiting_input')],
      sources(
        { 1: 'failed', 2: 'finishing' },
        {},
        { 2: { node_id: 2, kind: 'permission_request', description: 'approve the deploy' } },
      ),
      2,
    );
    expect(result).toMatchObject({ kind: 'needs_input', nodeId: 2 });
    expect(result?.detail).toContain('approve the deploy');
    expect(result?.nodeIds).toEqual([1, 2]);
  });

  it('falls back to the highest-priority session when focus is elsewhere', () => {
    const result = resolveAutopilotOutcome(
      [member(1, 'error'), member(2, 'awaiting_input')],
      sources({ 1: 'failed', 2: 'finishing' }),
      99,
    );
    expect(result).toMatchObject({ kind: 'failed', nodeId: 1 });
  });

  it('attributes responsibility from the focused member, not evaluation order', () => {
    const result = resolveAutopilotOutcome(
      [member(1, 'awaiting_input'), member(2, 'error')],
      sources({ 2: 'failed' }),
      1,
    );
    expect(result).toMatchObject({ kind: 'needs_input', label: 'Needs input', nodeId: 1 });
    expect(result?.detail).toContain('This agent is waiting');
  });

  it('reflects each focused session’s own prompt', () => {
    const turns = {
      1: { node_id: 1, kind: 'permission_request' as const, description: 'approve the first command' },
      2: { node_id: 2, kind: 'permission_request' as const, description: 'approve the second command' },
    };
    const members = [member(1, 'awaiting_input'), member(2, 'awaiting_input')];
    const srcs = sources({ 1: 'finishing', 2: 'finishing' }, {}, turns);
    expect(resolveAutopilotOutcome(members, srcs, 1)?.detail).toContain('approve the first command');
    expect(resolveAutopilotOutcome(members, srcs, 2)?.detail).toContain('approve the second command');
  });

  it('attributes a failed Circuit run to Autopilot', () => {
    const result = resolveAutopilotOutcome([member(1, 'completed')], sources({}, { 1: ownership('failed') }));
    expect(result).toMatchObject({ kind: 'failed', tone: 'error', label: 'Autopilot failed', nodeId: 1 });
  });

  it('attributes a legacy failed run to Autopilot', () => {
    const result = resolveAutopilotOutcome([member(1, 'error')], sources({ 1: 'failed' }));
    expect(result).toMatchObject({ kind: 'failed', label: 'Autopilot failed' });
  });

  it.each(['cancelled', 'completed'] as const)(
    'does not attribute a %s Circuit to Autopilot (retained history is not responsibility)',
    (state) => {
      const result = resolveAutopilotOutcome([member(1, 'error')], sources({}, { 1: ownership(state) }));
      expect(result).toMatchObject({ kind: 'failed', tone: 'error', label: 'Agent failed' });
      expect(result?.detail).toContain('This agent is in an error state');
    },
  );

  it('attributes a paused Circuit gate waiting on a human to Autopilot', () => {
    const result = resolveAutopilotOutcome([member(1, 'awaiting_input')], sources({}, { 1: ownership('paused') }));
    expect(result).toMatchObject({ kind: 'needs_input', label: 'Needs input' });
    expect(result?.detail).toContain('Autopilot is waiting for your input');
  });

  it('does not attribute awaiting input on a cancelled Circuit to Autopilot', () => {
    const result = resolveAutopilotOutcome([member(1, 'awaiting_input')], sources({}, { 1: ownership('cancelled') }));
    expect(result?.detail).toContain('This agent is waiting');
  });
});
