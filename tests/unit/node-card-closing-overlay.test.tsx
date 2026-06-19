/**
 * Closing an agent node first runs a worktree safety check that can take
 * seconds on a large repo. During that window the node's card stays mounted
 * with its (now inert) terminal still on screen. To make the closure
 * obviously in progress, NodeCard overlays its viewport with a spinner and a
 * "Closing…" label, driven by the store's transient `closingNodeIds` set.
 *
 * NodeCard's real children (AgentTerminal/xterm, GridNodeHeader, dnd-kit) are
 * heavy and irrelevant to the overlay, so they're stubbed — this test pins
 * just the overlay wiring against the store flag.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen } from '@testing-library/react';

vi.mock('@dnd-kit/core', () => ({
  useDraggable: () => ({ setNodeRef: vi.fn(), listeners: {}, attributes: {}, isDragging: false }),
  useDroppable: () => ({ setNodeRef: vi.fn() }),
}));
vi.mock('../../src/components/Terminal/Terminal', () => ({
  AgentTerminal: () => <div data-testid="agent-terminal" />,
}));
vi.mock('../../src/components/Terminal/BuildRunTerminal', () => ({
  BuildRunTerminal: () => <div data-testid="build-run-terminal" />,
}));
vi.mock('../../src/components/AgentNodeView/GridNodeHeader', () => ({
  GridNodeHeader: () => <div data-testid="grid-node-header" />,
}));
vi.mock('../../src/components/AgentNodeView/nodeDrag', () => ({
  NodeDropCue: () => null,
}));

import { NodeCard } from '../../src/components/AgentNodeView/NodeCard';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 5, mesh_id: 1, name: 'bold-keen-brook', path: '/a', branch: 'main',
    env: 'windows', provider: 'anthropic', status: 'idle', created_at: '',
    use_worktree: true, position: 0,
    ...overrides,
  };
}

function renderCard(node: AgentNode) {
  return render(
    <NodeCard
      node={node}
      isActive={false}
      onActivate={vi.fn()}
      onBuildRun={vi.fn()}
      buildRunOpen={null}
      setBuildRunOpen={vi.fn()}
    />,
  );
}

describe('NodeCard closing overlay', () => {
  beforeEach(() => {
    useAgentNodeStore.setState({
      agentNodes: [],
      activeNodeId: null,
      loading: false,
      error: null,
      closingNodeIds: new Set(),
    });
    vi.clearAllMocks();
  });

  it('overlays the viewport with a Closing… label when the node is closing', () => {
    useAgentNodeStore.setState({ closingNodeIds: new Set([5]) });
    renderCard(makeNode({ id: 5 }));

    expect(screen.getByText('Closing…')).toBeTruthy();
  });

  it('shows no overlay when the node is not closing', () => {
    renderCard(makeNode({ id: 5 }));

    expect(screen.queryByText('Closing…')).toBeNull();
  });

  it('overlays only the card whose node is closing, not a sibling card', () => {
    useAgentNodeStore.setState({ closingNodeIds: new Set([5]) });
    renderCard(makeNode({ id: 6 }));

    expect(screen.queryByText('Closing…')).toBeNull();
  });
});
