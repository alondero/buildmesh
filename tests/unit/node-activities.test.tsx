import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useNodeActivityStore } from '../../src/stores/nodeActivityStore';
import { activityRootId, groupActivityNodes, indexAgentNodes } from '../../src/lib/nodeActivities';
import { deriveVisibleNodes } from '../../src/components/AgentNodeView/gridFilterSort';
import { NodeCard } from '../../src/components/AgentNodeView/NodeCard';
import { jumpToNextAwaitingNode } from '../../src/lib/awaitingInputShortcuts';

vi.mock('@dnd-kit/core', () => ({
  useDraggable: () => ({ setNodeRef: vi.fn(), listeners: {}, attributes: {}, isDragging: false }),
  useDroppable: () => ({ setNodeRef: vi.fn() }),
}));
vi.mock('../../src/components/AgentNodeView/nodeDrag', () => ({ NodeDropCue: () => null }));
vi.mock('../../src/components/Terminal/Terminal', () => ({
  AgentTerminal: ({ nodeId }: { nodeId: number }) => <textarea aria-label={`Agent ${nodeId}`} />,
  terminalManager: { getInstance: () => null },
}));
vi.mock('../../src/components/Terminal/BuildRunTerminal', () => ({
  BuildRunTerminal: ({ sessionId, mode }: { sessionId: number; mode: string }) =>
    <div><span>{`${mode} output ${sessionId}`}</span></div>,
}));
const { disposeUtility } = vi.hoisted(() => ({ disposeUtility: vi.fn() }));
vi.mock('../../src/components/Terminal/BuildRunTerminalRegistry', () => ({
  buildRunTerminalManager: { dispose: disposeUtility, getInstance: () => null },
}));
vi.mock('../../src/components/AgentNodeView/GridNodeHeader', () => ({
  GridNodeHeader: ({ nodeId, titleNodeId, activity, onAttention, onBuildRun }: { nodeId: number; titleNodeId: number; activity?: { label: string }; onAttention: () => void; onBuildRun: (id: number, mode: string) => void }) =>
    <div><span>Title {titleNodeId}</span><span role="status">{activity?.label}</span><span>Controls {nodeId}</span><button onClick={onAttention}>Show attention</button><button onClick={() => onBuildRun(nodeId, 'terminal')}>Open terminal</button></div>,
}));

const node = (id: number, overrides: Partial<AgentNode> = {}): AgentNode => ({
  id, mesh_id: 1, name: `Agent ${id}`, status: 'ready', provider: 'terminal',
  path: '/repo', branch: 'main', env: 'windows', use_worktree: false, position: id,
  created_at: '', cli_session_id: null, worktree_name: null, is_pinned: false,
  source_issue: null, source_pr: null, head_repo_owner: null, head_repo_clone_url: null,
  source_pr_pinned_sha: null, signal_health: null, worktree_path: null, ...overrides,
});
const ownership = (id: number, parent: number | null = null) => ({
  node_id: id, run_id: 10, circuit_id: 3, circuit_name: 'Review', state: 'running', parent_node_id: parent,
});
const nodes = [node(1), node(2, { status: 'running', name: 'Reviewer' }), node(3)];
const ownerships = { 1: ownership(1), 2: ownership(2, 1) };
const controls = { gridSearchQuery: '', gridProviderFilter: null, gridStatusFilter: null,
  gridSortBy: 'custom' as const, gridSortDirection: 'asc' as const };

function card() {
  return <NodeCard nodeId={1} memberIds={[1, 2, 4]} isActive onActivate={id => useAgentNodeStore.getState().setActiveNode(id)} />;
}

beforeEach(() => {
  useAgentNodeStore.setState({ nodeIds: nodes.map(n => n.id), nodesById: Object.fromEntries(nodes.map(n => [n.id, n])),
    circuitOwnerships: ownerships, activeNodeId: 1, closingNodeIds: new Set(), semanticTurns: {} });
  useNodeActivityStore.setState({ selections: {}, utilities: {} });
  disposeUtility.mockClear();
});

describe('node activities', () => {
  it('groups reviewers in all/mesh grids and keeps child-only filter and pin matches accessible', () => {
    expect(deriveVisibleNodes('all', nodes, 1, 2, controls, ownerships).map(n => n.id)).toEqual([1, 3]);
    expect(deriveVisibleNodes('mesh', nodes, 1, 2, controls, ownerships).map(n => n.id)).toEqual([1, 3]);
    expect(deriveVisibleNodes('filtered', nodes, 1, 2, { ...controls, gridSearchQuery: 'Reviewer' }, ownerships).map(n => n.id)).toEqual([1]);
    expect(deriveVisibleNodes('pinned', nodes.map(n => ({ ...n, is_pinned: n.id === 2 })), 1, 2, controls, ownerships).map(n => n.id)).toEqual([1]);
  });

  it('leaves orphaned, cross-mesh and cyclic relationships accessible', () => {
    expect(groupActivityNodes([nodes[1]], indexAgentNodes([nodes[1]]), ownerships).map(n => n.id)).toEqual([2]);
    expect(activityRootId(2, indexAgentNodes([node(1, { mesh_id: 9 }), nodes[1]]), ownerships)).toBe(2);
    expect(activityRootId(2, indexAgentNodes([node(1, { status: 'archived' }), nodes[1]]), ownerships)).toBe(2);
    expect(groupActivityNodes(nodes, indexAgentNodes(nodes), { 1: ownership(1, 2), 2: ownership(2, 1) }).map(n => n.id)).toEqual([1, 2, 3]);
  });

  it('shows actual activity independently of the selected tab and directs controls/focus to the reviewer', () => {
    render(card());
    expect(screen.getByRole('status').textContent).toBe('Reviewing');
    expect(screen.getByRole('tab', { name: /^Implementation/ }).getAttribute('aria-selected')).toBe('true');
    fireEvent.click(screen.getByRole('tab', { name: /^Review.*Reviewer/ }));
    expect(screen.getByLabelText('Agent 2')).toBeTruthy();
    expect(screen.getByText('Controls 2')).toBeTruthy();
    expect(screen.getByText('Title 1')).toBeTruthy();
    expect(useAgentNodeStore.getState().activeNodeId).toBe(2);
    act(() => useAgentNodeStore.setState({ nodesById: { ...useAgentNodeStore.getState().nodesById, 1: node(1, { status: 'running' }) } }));
    expect(screen.getByRole('status').textContent).toBe('Implementing + reviewing');
  });

  it('uses keyboard tabs, remembers selection on remount and follows external node selection', () => {
    const first = render(card());
    fireEvent.keyDown(screen.getByRole('tab', { name: /^Implementation/ }), { key: 'ArrowRight' });
    expect(screen.getByLabelText('Agent 2')).toBeTruthy();
    first.unmount();
    render(card());
    expect(screen.getByLabelText('Agent 2')).toBeTruthy();
    act(() => useAgentNodeStore.setState({ activeNodeId: 1 }));
    expect(screen.getByLabelText('Agent 2')).toBeTruthy();
  });

  it('opens a full utility tab, retains it across tab switches/remounts and closes it explicitly', async () => {
    const first = render(card());
    fireEvent.click(screen.getByText('Open terminal'));
    expect(await screen.findByText('terminal output 1')).toBeTruthy();
    expect(screen.queryByLabelText('Agent 1')).toBeNull();
    fireEvent.click(screen.getByRole('tab', { name: /^Implementation/ }));
    expect(screen.getByLabelText('Agent 1')).toBeTruthy();
    expect(screen.queryByText('terminal output 1')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: /Close Terminal/ }));
    expect(screen.queryByRole('tab', { name: /Terminal/ })).toBeNull();
    fireEvent.click(screen.getByText('Open terminal'));
    expect(await screen.findByText('terminal output 1')).toBeTruthy();
    fireEvent.click(screen.getByRole('tab', { name: /Terminal.*Agent 1/ }));
    first.unmount();
    render(card());
    expect(await screen.findByText('terminal output 1')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: /Close Terminal/ }));
    expect(screen.queryByRole('tab', { name: /Terminal/ })).toBeNull();
    expect(screen.getByLabelText('Agent 1')).toBeTruthy();
    expect(disposeUtility).toHaveBeenCalledWith(1, 'terminal', false);
  });

  it('keeps the selected reviewer when closing a background utility, and preserves utilities on switching', async () => {
    render(card());
    fireEvent.click(screen.getByText('Open terminal'));
    await screen.findByText('terminal output 1');
    fireEvent.click(screen.getByRole('tab', { name: /^Review.*Reviewer/ }));
    expect(disposeUtility).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: /Close Terminal/ }));
    expect(screen.getByLabelText('Agent 2')).toBeTruthy();
    expect(screen.getByText('Controls 2')).toBeTruthy();
    expect(disposeUtility).toHaveBeenCalledWith(1, 'terminal', false);
  });

  it('reveals background attention and exposes full session names in the session list', () => {
    useAgentNodeStore.setState({ nodesById: { ...useAgentNodeStore.getState().nodesById, 2: node(2, { name: 'Security reviewer', status: 'awaiting_input' }) } });
    render(card());
    fireEvent.click(screen.getByText('Show attention'));
    expect(screen.getByLabelText('Agent 2')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'All sessions (2)' }));
    expect(screen.getByRole('menuitem', { name: /Security reviewer.*awaiting input/ })).toBeTruthy();
    fireEvent.click(screen.getByRole('menuitem', { name: /^Implementation/ }));
    expect(screen.getByLabelText('Agent 1')).toBeTruthy();
  });

  it('does not allocate tabs for a standalone agent without a utility', () => {
    render(<NodeCard nodeId={1} isActive onActivate={() => {}} />);
    expect(screen.queryByRole('tablist')).toBeNull();
    expect(screen.getByLabelText('Agent 1')).toBeTruthy();
  });

  it('toggles the session list closed with a second pointer click', async () => {
    render(card());
    const trigger = screen.getByRole('button', { name: 'All sessions (2)' });
    await userEvent.click(trigger);
    expect(screen.getByRole('menu', { name: 'All sessions' })).toBeTruthy();
    await userEvent.click(trigger);
    expect(screen.queryByRole('menu', { name: 'All sessions' })).toBeNull();
  });

  it('returns to the implementation when a reviewer is retired and accepts its replacement', () => {
    render(card());
    fireEvent.click(screen.getByRole('tab', { name: /^Review.*Reviewer/ }));
    act(() => useAgentNodeStore.setState({ nodeIds: [1, 3], nodesById: { 1: nodes[0], 3: nodes[2] }, activeNodeId: null }));
    expect(screen.getByLabelText('Agent 1')).toBeTruthy();
    expect(screen.queryByRole('tab', { name: /Review/ })).toBeNull();
    act(() => useAgentNodeStore.setState({ nodeIds: [1, 3, 4], nodesById: { 1: nodes[0], 3: nodes[2], 4: node(4) },
      circuitOwnerships: { ...ownerships, 4: ownership(4, 1) } }));
    fireEvent.click(screen.getByRole('tab', { name: /Review.*Agent 4/ }));
    expect(screen.getByLabelText('Agent 4')).toBeTruthy();
  });

  it('reveals the same awaiting agent from a utility tab through the attention shortcut', async () => {
    useAgentNodeStore.setState({ nodesById: { ...useAgentNodeStore.getState().nodesById, 1: node(1, { status: 'awaiting_input' }) } });
    render(card());
    fireEvent.click(screen.getByText('Open terminal'));
    expect(await screen.findByText('terminal output 1')).toBeTruthy();
    act(() => { expect(jumpToNextAwaitingNode()).toBe(1); });
    expect(screen.getByLabelText('Agent 1')).toBeTruthy();
    expect(screen.queryByText('terminal output 1')).toBeNull();
  });
});
