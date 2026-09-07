import { describe, expect, it } from 'vitest';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { CircuitAgentOwnership } from '../../src/types/generated/CircuitAgentOwnership';
import { getAutopilotNodePresentation, getAutopilotRunDetails } from '../../src/lib/autopilotNodePresentation';

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
});
