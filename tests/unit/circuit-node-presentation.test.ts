import { describe, expect, it } from 'vitest';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { CircuitAgentOwnership } from '../../src/types/generated/CircuitAgentOwnership';
import type { SemanticTurnPayload } from '../../src/types/generated/SemanticTurnPayload';
import { getCircuitNodePresentation, hasActiveCircuitOwnership, resolveCircuitOutcome } from '../../src/lib/circuitNodePresentation';

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

describe('getCircuitNodePresentation', () => {
  it('keeps unpiloted nodes visually empty', () => {
    expect(getCircuitNodePresentation(node())).toBeNull();
  });

  it.each([
    ['running', 'active'],
    ['spawning', 'active'],
  ] as const)('maps a Circuit-managed %s node to the active pilot light', (status, phase) => {
    expect(getCircuitNodePresentation(node(status), ownership('running'))?.phase).toBe(phase);
  });

  it.each(['ready', 'idle', 'awaiting_input', 'pending', 'suspended'] as const)(
    'maps a Circuit-managed node that is not driving (%s) to waiting',
    (status) => {
      expect(getCircuitNodePresentation(node(status), ownership('running'))?.phase).toBe('waiting');
    },
  );

  it('keeps Circuit waiting details tied to the lifecycle state', () => {
    expect(getCircuitNodePresentation(node('ready'), ownership('running'))?.detail)
      .toBe('Circuit is between turns after this Agent Node yielded cleanly.');
    expect(getCircuitNodePresentation(node('idle'), ownership('running'))?.detail)
      .toBe('Circuit is waiting to start the next turn.');
    expect(getCircuitNodePresentation(node('awaiting_input'), ownership('running'))?.detail)
      .toBe('Circuit is waiting for input from you.');
    expect(getCircuitNodePresentation(node('pending'), ownership('running'))?.detail)
      .toBe('Circuit is waiting for this Agent Node to start.');
    expect(getCircuitNodePresentation({ ...node('suspended'), cli_session_id: 'session-1' }, ownership('running'))?.detail)
      .toBe('Circuit is waiting for this Agent Node to recover its existing session.');
    expect(getCircuitNodePresentation(node('suspended'), ownership('running'))?.detail)
      .toBe('Circuit is waiting for this Agent Node to recover before starting a new session.');
  });

  it.each(['completed'] as const)('maps Circuit %s to done', (state) => {
    expect(getCircuitNodePresentation(node('completed'), ownership(state))?.phase).toBe('done');
  });

  it('maps a failed legacy run to waiting with an attention tone', () => {
    const result = getCircuitNodePresentation(node(), ownership('failed'));
    expect(result).toMatchObject({ phase: 'waiting', tone: 'error', label: 'Circuit needs attention' });
  });

  it('keeps an error lifecycle in waiting with an attention tone', () => {
    expect(getCircuitNodePresentation(node('error'), ownership('running'))).toMatchObject({
      phase: 'waiting',
      tone: 'error',
    });
    expect(getCircuitNodePresentation(node('error'), ownership('running'))).toMatchObject({
      phase: 'waiting',
      tone: 'error',
    });
  });

  it('gives Circuit ownership precedence over a Circuit-managed state during migration', () => {
    expect(getCircuitNodePresentation(node(), ownership('paused'))?.phase).toBe('waiting');
  });

  it('maps a failed Circuit run to waiting with an attention tone', () => {
    expect(getCircuitNodePresentation(node(), ownership('failed'))).toMatchObject({
      phase: 'waiting',
      tone: 'error',
    });
  });

  it.each(['pending', 'paused'] as const)('maps a non-driving Circuit run (%s) to waiting', (state) => {
    expect(getCircuitNodePresentation(node('running'), ownership(state))?.phase).toBe('waiting');
  });

  it('distinguishes an explicit Circuit pause and queued Circuit work', () => {
    expect(getCircuitNodePresentation(node('running'), ownership('paused'))?.detail)
      .toBe('Circuit is paused.');
    expect(getCircuitNodePresentation(node('running'), ownership('pending'))?.detail)
      .toBe('Circuit is queued for the next step.');
  });

  it('maps a running Circuit run driving a running node to active', () => {
    expect(getCircuitNodePresentation(node('running'), ownership('running'))?.phase).toBe('active');
  });

  it('reconciles a live ownership row on a completed node as waiting', () => {
    expect(getCircuitNodePresentation(node('completed'), ownership('running'))).toMatchObject({
      phase: 'waiting',
      tone: 'warning',
      detail: 'Circuit ownership is reconciling; the run is still live while this Agent Node reports completed.',
    });
  });

  it('retains a terminal Circuit source as done', () => {
    expect(getCircuitNodePresentation(node('completed'), ownership('completed'))?.phase).toBe('done');
  });

  it('does not show a compact indicator for a cancelled or archived Circuit node', () => {
    expect(getCircuitNodePresentation(node(), ownership('cancelled'))).toBeNull();
    expect(getCircuitNodePresentation(node('archived'), ownership('completed'))).toBeNull();
  });

  it('treats unknown Circuit states as an attention state instead of active', () => {
    expect(getCircuitNodePresentation(node(), ownership('mystery'))).toMatchObject({
      phase: 'waiting',
      tone: 'error',
    });
  });

  it('keeps malformed dual ownership conservative', () => {
    expect(getCircuitNodePresentation(node(), ownership('mystery'))).toMatchObject({
      phase: 'waiting',
      tone: 'error',
      label: 'Circuit needs attention',
    });
  });

  it('only treats live ownership as active for suspended-node recovery suppression', () => {
    expect(hasActiveCircuitOwnership(ownership('running'))).toBe(true);
    expect(hasActiveCircuitOwnership(ownership('completed'))).toBe(false);
    expect(hasActiveCircuitOwnership(ownership('running'))).toBe(true);
    expect(hasActiveCircuitOwnership(ownership('completed'))).toBe(false);
    expect(hasActiveCircuitOwnership(ownership('completed'))).toBe(false);
  });
});

describe('resolveCircuitOutcome', () => {
  const member = (id: number, status: AgentNode['status']): AgentNode => ({ ...node(status), id });
  const sources = (
    states: Record<number, string> = {},
    circuitOwnerships: Record<number, CircuitAgentOwnership> = {},
    semanticTurns: Record<number, SemanticTurnPayload> = {},
  ) => ({ circuitOwnerships: { ...Object.fromEntries(Object.entries(states).map(([id, state]) =>
    [id, { ...ownership(state === 'failed' ? 'failed' : state === 'completed' ? 'completed' : 'running'), node_id: Number(id) }])), ...circuitOwnerships }, semanticTurns });

  it('returns null for a healthy piloted node with no terminal outcome', () => {
    expect(resolveCircuitOutcome([member(1, 'running')], sources({ 1: 'implementing' }))).toBeNull();
  });

  it('returns null for an unpiloted, healthy node', () => {
    expect(resolveCircuitOutcome([member(1, 'running')], sources())).toBeNull();
  });

  it('surfaces an awaited run as needs_input and carries the requested action', () => {
    const result = resolveCircuitOutcome(
      [member(1, 'awaiting_input')],
      sources({ 1: 'finishing' }, {}, { 1: { node_id: 1, kind: 'permission_request', description: 'approve the deploy' } }),
    );
    expect(result).toMatchObject({ kind: 'needs_input', phase: 'waiting', tone: 'warning', label: 'Needs input', nodeId: 1, nodeIds: [1] });
    expect(result?.detail).toContain('approve the deploy');
  });

  it('still flags an unowned awaiting node, so the chip is not Circuit-only', () => {
    const result = resolveCircuitOutcome([member(1, 'awaiting_input')], sources());
    expect(result?.detail).toContain('This agent is waiting');
  });

  it('aggregates every attention session, failures first', () => {
    const result = resolveCircuitOutcome(
      [member(1, 'awaiting_input'), member(2, 'error')],
      sources({ 2: 'failed' }),
    );
    // The failure leads the copy, but the waiting session stays cyclable.
    expect(result).toMatchObject({ kind: 'failed', tone: 'error', nodeId: 2 });
    expect(result?.nodeIds).toEqual([2, 1]);
  });

  it('surfaces a background member failure on a multi-member card', () => {
    // The title bar draws one Pilot light — the focused member's — so a sibling
    // that failed must still reach the chip or it stays invisible on the canvas.
    const result = resolveCircuitOutcome(
      [member(1, 'running'), member(2, 'error')],
      sources({ 2: 'failed' }),
      1,
    );
    expect(result).toMatchObject({ kind: 'failed', tone: 'error', label: 'Circuit failed', nodeId: 2 });
    expect(result?.nodeIds).toEqual([2]);
  });

  it('leaves completed automation to the existing green signals (no chip)', () => {
    // A finished run needs no action and is already shown by the lifecycle
    // dot, the Pilot-light check, and the PR pill.
    expect(resolveCircuitOutcome([member(1, 'completed')], sources({ 1: 'completed' }))).toBeNull();
    expect(resolveCircuitOutcome([member(1, 'completed')], sources({}, { 1: ownership('completed') }))).toBeNull();
  });

  it('still flags a waiting member beside a finished one', () => {
    const mixed = resolveCircuitOutcome(
      [member(1, 'awaiting_input'), member(2, 'completed')],
      sources({ 1: 'implementing', 2: 'completed' }),
    );
    expect(mixed?.kind).toBe('needs_input');
  });

  it('keeps every attention session in cycling order, failures first', () => {
    const result = resolveCircuitOutcome(
      [member(1, 'awaiting_input'), member(2, 'error'), member(3, 'awaiting_input')],
      sources({ 1: 'finishing', 2: 'failed', 3: 'finishing' }),
    );
    expect(result?.nodeIds).toEqual([2, 1, 3]);
    expect(result?.nodeId).toBe(2);
  });

  it('describes the focused session when it is one of the attention sessions', () => {
    const result = resolveCircuitOutcome(
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
    const result = resolveCircuitOutcome(
      [member(1, 'error'), member(2, 'awaiting_input')],
      sources({ 1: 'failed', 2: 'finishing' }),
      99,
    );
    expect(result).toMatchObject({ kind: 'failed', nodeId: 1 });
  });

  it('attributes responsibility from the focused member, not evaluation order', () => {
    const result = resolveCircuitOutcome(
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
    expect(resolveCircuitOutcome(members, srcs, 1)?.detail).toContain('approve the first command');
    expect(resolveCircuitOutcome(members, srcs, 2)?.detail).toContain('approve the second command');
  });

  it('suppresses the chip for a solo card whose focused member failed under Circuit', () => {
    // The lone Pilot light is already visibly red for this member and a chip
    // click could only cycle back to it, so the chip is pure duplication.
    expect(resolveCircuitOutcome([member(1, 'error')], sources({ 1: 'failed' }), 1)).toBeNull();
    expect(resolveCircuitOutcome([member(1, 'completed')], sources({}, { 1: ownership('failed') }), 1)).toBeNull();
  });

  it.each(['cancelled', 'completed'] as const)(
    'keeps the failure chip for a solo card with a %s Circuit, where no Pilot light is drawn',
    (state) => {
      // An unpiloted node and a cancelled or terminal Circuit render no Pilot
      // light at all, so the text chip is the card's only failure disclosure.
      const result = resolveCircuitOutcome([member(1, 'error')], sources({}, { 1: ownership(state) }), 1);
      expect(result).toMatchObject({ kind: 'failed', tone: 'error', label: 'Agent failed', nodeId: 1 });
    },
  );

  it('keeps the failure chip for a solo card whose failed member is not focused', () => {
    // Focus is what puts the light on screen; if the failed member is not the
    // one being shown, the chip is the only route back to it.
    const result = resolveCircuitOutcome([member(1, 'error')], sources({ 1: 'failed' }), 99);
    expect(result).toMatchObject({ kind: 'failed', label: 'Circuit failed', nodeId: 1 });
  });

  it('keeps the failure chip on a multi-member card even when the focused member failed', () => {
    const result = resolveCircuitOutcome(
      [member(1, 'error'), member(2, 'running')],
      sources({ 1: 'failed' }),
      1,
    );
    expect(result).toMatchObject({ kind: 'failed', label: 'Circuit failed', nodeId: 1 });
  });

  it('attributes a failed Circuit run to Circuit', () => {
    const result = resolveCircuitOutcome([member(1, 'completed')], sources({}, { 1: ownership('failed') }));
    expect(result).toMatchObject({ kind: 'failed', tone: 'error', label: 'Circuit failed', nodeId: 1 });
  });

  it('attributes a Circuit-managed failed run to Circuit', () => {
    const result = resolveCircuitOutcome([member(1, 'error')], sources({ 1: 'failed' }));
    expect(result).toMatchObject({ kind: 'failed', label: 'Circuit failed' });
  });

  it.each(['cancelled', 'completed'] as const)(
    'does not attribute a %s Circuit to Circuit (retained history is not responsibility)',
    (state) => {
      const result = resolveCircuitOutcome([member(1, 'error')], sources({}, { 1: ownership(state) }));
      expect(result).toMatchObject({ kind: 'failed', tone: 'error', label: 'Agent failed' });
      expect(result?.detail).toContain('This agent is in an error state');
    },
  );

  it('attributes a paused Circuit gate waiting on a human to Circuit', () => {
    const result = resolveCircuitOutcome([member(1, 'awaiting_input')], sources({}, { 1: ownership('paused') }));
    expect(result).toMatchObject({ kind: 'needs_input', label: 'Needs input' });
    expect(result?.detail).toContain('Circuit is waiting for your input');
  });

  it('does not attribute awaiting input on a cancelled Circuit to Circuit', () => {
    const result = resolveCircuitOutcome([member(1, 'awaiting_input')], sources({}, { 1: ownership('cancelled') }));
    expect(result?.detail).toContain('This agent is waiting');
  });
});
