import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { buildDefaultFixtures } from '../../scripts/ui-mock/default-fixtures';
import { reportCircuitInput, watchAgentInputNotifications } from '../../src/lib/agentInputNotifications';
import { AgentInputLink } from '../../src/components/Notifications/AgentInputLink';
import { isAgentInput, needsAttention, restoreNotifications } from '../../src/lib/notifications';
import { addToast, useToastStore } from '../../src/stores/toastStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useNodeActivityStore } from '../../src/stores/nodeActivityStore';
import { useUIStore } from '../../src/stores/uiStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';

const fixtures = buildDefaultFixtures();
const node = fixtures.list_agent_nodes[1];
const action = { kind: 'agent-input' as const, nodeId: node.id, agentName: node.name, meshName: 'buildmesh' };
let stop: (() => void) | undefined;

beforeEach(() => {
  seedAgentNodes(fixtures.list_agent_nodes);
  useAgentNodeStore.setState({ circuitOwnerships: {} });
  useMeshStore.setState({ meshes: fixtures.list_meshes, meshesById: new Map(fixtures.list_meshes.map(m => [m.id, m])), selectedMeshId: 2 });
  useToastStore.setState({ toasts: [], notifications: [] });
  useNodeActivityStore.setState({ selections: {}, utilities: {}, groups: [] });
  useUIStore.setState({ viewMode: 'mesh', lastNonSingleMode: 'mesh' });
});
afterEach(() => { stop?.(); stop = undefined; });

describe('Circuit input notifications', () => {
  it('names the agent and Mesh, hides issue zero and keeps the destination after persistence', () => {
    reportCircuitInput({ node_id: node.id, issue: 0 });
    const { toasts, notifications } = useToastStore.getState();
    expect(toasts[0].message).toContain('diff-panel-refactor in buildmesh needs your input.');
    expect(toasts[0].message).not.toMatch(/node 2|issue #0/);
    expect(toasts[0].action).toEqual(action);
    expect(restoreNotifications(JSON.parse(JSON.stringify(notifications)))[0].action).toEqual(action);
    expect(needsAttention(notifications[0])).toBe(true);
  });

  it('includes a real issue and the observed question without inventing an answer', () => {
    useAgentNodeStore.getState().patchAgentNode(node.id, {
      lifecycle: {
        session_id: node.id, provider: node.provider, kind: 'question_requested', status: 'awaiting_input',
        message: 'Which branch should I target?', provider_event: null, provider_session_id: null,
        completion_reason: null, transcript_path: null, timestamp: '2026-10-31T23:00:00Z', signal_health: 'ok', semantic_turn: null,
      },
    });
    reportCircuitInput({ node_id: node.id, issue: 42 });
    expect(useToastStore.getState().toasts[0].message).toContain('(issue #42). Which branch should I target?');
  });

  it('keeps identically named agents as separate destinations and coalesces repeated requests per agent', () => {
    addToast('Needs input', 'Same name needs your input', 'warning', action);
    addToast('Needs input', 'Same name needs your input', 'warning', { ...action, nodeId: 3 });
    addToast('Needs input', 'Same name needs your input', 'warning', action);
    expect(useToastStore.getState().toasts.map(t => isAgentInput(t.action) && t.action.nodeId)).toEqual([node.id, 3]);
    expect(useToastStore.getState().notifications).toHaveLength(2);
    // Cleanup queue reconciliation cannot resolve an input request.
    useToastStore.getState().reconcileWorktrees([], useToastStore.getState().worktreeRevision);
    expect(useToastStore.getState().notifications.every(needsAttention)).toBe(true);
  });

  it('opens the exact grouped reviewer across Meshes, leaving its request unresolved until answered', () => {
    useAgentNodeStore.setState({ circuitOwnerships: {
      [node.id]: { node_id: node.id, run_id: 1001, circuit_id: 101, circuit_name: 'Review', state: 'running', parent_node_id: 1 },
    } });
    useNodeActivityStore.setState({ selections: { 1: { nodeId: 1, utility: false, overview: true } } });
    reportCircuitInput({ node_id: node.id, issue: 0 });
    render(<AgentInputLink action={action} />);
    fireEvent.click(screen.getByRole('button', { name: /Open agent/ }));
    expect(useMeshStore.getState().selectedMeshId).toBe(1);
    expect(useUIStore.getState().viewMode).toBe('single');
    expect(useAgentNodeStore.getState().activeNodeId).toBe(node.id);
    expect(useNodeActivityStore.getState().selections[1]).toEqual({ nodeId: node.id, utility: false });
    expect(useToastStore.getState().notifications[0].read).toBe(true);
    expect(needsAttention(useToastStore.getState().notifications[0])).toBe(true);
  });

  it('shows a useful error for a missing agent and does not change the selected Mesh', () => {
    seedAgentNodes([]);
    render(<AgentInputLink action={action} />);
    fireEvent.click(screen.getByRole('button', { name: /Open agent/ }));
    expect(screen.getByRole('alert').textContent).toContain('no longer available');
    expect(useMeshStore.getState().selectedMeshId).toBe(2);
  });

  it('resolves history and withdraws the toast when input ends, but retains the destination', () => {
    reportCircuitInput({ node_id: node.id, issue: 0 });
    stop = watchAgentInputNotifications();
    useAgentNodeStore.getState().patchAgentNode(node.id, { status: 'running' });
    const { toasts, notifications } = useToastStore.getState();
    expect(toasts).toEqual([]);
    expect(needsAttention(notifications[0])).toBe(false);
    expect(notifications[0].action).toMatchObject({ nodeId: node.id, resolution: 'The agent is no longer waiting for input.' });
    reportCircuitInput({ node_id: node.id, issue: 0 });
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  });

  it('does not resolve an alert on unrelated updates before its status event arrives', () => {
    useAgentNodeStore.getState().patchAgentNode(node.id, { status: 'running' });
    stop = watchAgentInputNotifications();
    reportCircuitInput({ node_id: node.id, issue: 0 });
    useAgentNodeStore.getState().patchAgentNode(1, { name: 'renamed' });
    expect(useToastStore.getState().notifications.every(needsAttention)).toBe(true);
    useAgentNodeStore.getState().patchAgentNode(node.id, { status: 'awaiting_input' });
    useAgentNodeStore.getState().patchAgentNode(node.id, { status: 'running' });
    expect(useToastStore.getState().notifications.some(needsAttention)).toBe(false);
  });

  it('reconciles restored requests against the successful startup snapshot', () => {
    reportCircuitInput({ node_id: node.id, issue: 0 });
    seedAgentNodes([]);
    stop = watchAgentInputNotifications();
    expect(useToastStore.getState().notifications[0].action?.resolution).toBe('This agent is no longer available.');
    expect(useToastStore.getState().toasts).toEqual([]);
  });

  it('retains a Ready-agent Circuit intervention across restart until fresh work is observed', () => {
    useAgentNodeStore.getState().patchAgentNode(node.id, { status: 'ready' });
    reportCircuitInput({ node_id: node.id, issue: 0 });
    // A blocked classification or missing-result checkpoint can name a Ready
    // agent. The successful boot snapshot has not resolved that intervention.
    useToastStore.setState({ notifications: restoreNotifications(JSON.parse(JSON.stringify(useToastStore.getState().notifications))) });
    stop = watchAgentInputNotifications();
    expect(useToastStore.getState().notifications.every(needsAttention)).toBe(true);
    useAgentNodeStore.getState().patchAgentNode(node.id, { name: 'renamed' });
    expect(useToastStore.getState().notifications.every(needsAttention)).toBe(true);
    useAgentNodeStore.getState().patchAgentNode(node.id, { status: 'running' });
    expect(useToastStore.getState().notifications[0].action?.resolution).toBe('The agent has resumed work.');
    expect(useToastStore.getState().toasts).toEqual([]);
  });

  it('withdraws a Ready-agent intervention if its agent is removed', () => {
    useAgentNodeStore.getState().patchAgentNode(node.id, { status: 'ready' });
    stop = watchAgentInputNotifications();
    reportCircuitInput({ node_id: node.id, issue: 0 });
    seedAgentNodes([]);
    expect(useToastStore.getState().notifications[0].action?.resolution).toBe('This agent is no longer available.');
  });

  it.each([0, -1, 1.5, '2', null])('rejects a corrupt persisted destination %s', nodeId => {
    const notice = { id: 1, provider: 'Needs input', message: 'request', createdAt: 1, read: false, action: { ...action, nodeId } };
    expect(restoreNotifications([notice])).toEqual([]);
  });
});
