/**
 * When buildmesh is force-closed (e.g. user opens and immediately exits)
 * during the auto-resume burst, the app-exit handler races the PTY
 * reader's post-pump "resume-failed" detector. The reader EOF is
 * interpreted as a failed resume and the node's status is overwritten
 * to 'error' — even though the session was never given a chance to
 * actually start. Those error nodes were previously unrecoverable
 * short of deleting and re-spawning them; this test pins the
 * sidebar's new "Restart" affordance that lets a user retry the
 * spawn with the original cli_session_id intact.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, fireEvent, screen } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { NodeItem } from '../../src/components/Sidebar/NodeItem';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';

const mockInvoke = invoke as ReturnType<typeof vi.fn>;

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 42,
    mesh_id: 1,
    name: 'calm-sweet-wolf',
    path: '/repo',
    branch: 'main',
    env: 'windows',
    provider: 'anthropic',
    status: 'error',
    cli_session_id: 'a53dd36f-e703-4f27-9356-8e523472d94e',
    created_at: '',
    use_worktree: true,
    position: 0,
    ...overrides,
  };
}

function renderNode(node: AgentNode, onSelect: () => void = vi.fn()) {
  return render(
    <NodeItem
      node={node}
      meshColor={{ name: 'default', hex: '#000000', textOnDark: '#fff' }}
      isActive={false}
      onSelect={onSelect}
      onDelete={vi.fn()}
    />,
  );
}

describe('NodeItem restart button', () => {
  beforeEach(() => {
    useAgentNodeStore.setState({
      agentNodes: [],
      activeNodeId: null,
      loading: false,
      error: null,
    });
    vi.clearAllMocks();
  });

  it('renders a Restart button when the node is in error state', () => {
    renderNode(makeNode({ status: 'error' }));
    expect(screen.getByTitle('Restart agent')).toBeTruthy();
  });

  it('does NOT render a Restart button when the node is running', () => {
    renderNode(makeNode({ status: 'running' }));
    expect(screen.queryByTitle('Restart agent')).toBeNull();
  });

  it('does NOT render a Restart button when the node is suspended', () => {
    renderNode(makeNode({ status: 'suspended' }));
    expect(screen.queryByTitle('Restart agent')).toBeNull();
  });

  it('does NOT render a Restart button when the node is idle', () => {
    renderNode(makeNode({ status: 'idle' }));
    expect(screen.queryByTitle('Restart agent')).toBeNull();
  });

  it('clicking Restart invokes spawn_agent with the original cli_session_id so the resume re-attempts', async () => {
    mockInvoke.mockResolvedValueOnce(undefined);
    // The store's spawnAgent looks the node up in `agentNodes` to read
    // its cli_session_id for the resume argument. Production populates
    // the store via fetchAgentNodes on startup; the test must too.
    const node = makeNode({ status: 'error' });
    useAgentNodeStore.setState({ agentNodes: [node] });
    renderNode(node);
    fireEvent.click(screen.getByTitle('Restart agent'));
    // spawnAgent's invoke is async; await a microtask so the assertion
    // doesn't race the in-flight promise.
    await Promise.resolve();
    expect(mockInvoke).toHaveBeenCalledWith(
      'spawn_agent',
      expect.objectContaining({
        sessionId: 42,
        provider: 'anthropic',
        resume: 'a53dd36f-e703-4f27-9356-8e523472d94e',
      }),
    );
  });

  it('clicking Restart does NOT also fire the row onSelect (stopPropagation guard)', () => {
    // The Restart button sits inside the row's clickable div. Without
    // e.stopPropagation, a single click would both restart the agent
    // AND switch the active node — confusing UX. The handler stops
    // propagation; this test pins that behaviour so a future refactor
    // can't silently regress it.
    const onSelect = vi.fn();
    renderNode(makeNode({ status: 'error' }), onSelect);
    fireEvent.click(screen.getByTitle('Restart agent'));
    expect(onSelect).not.toHaveBeenCalled();
  });
});
