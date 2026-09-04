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
import { seedAgentNodes } from './helpers/seedAgentNodes';

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
    useAgentNodeStore.setState({ nodesById: {}, nodeIds: [],activeNodeId: null,
      autopilotStates: {},
      loading: false,
      error: null,
    });
    vi.clearAllMocks();
  });

  it('distinguishes a missing conversation from a resumable or approval-gated node', () => {
    const node = makeNode({ status: 'suspended', cli_session_id: null });
    const { rerender } = renderNode(node);
    expect(screen.getByText('Lost conversation')).toBeTruthy();
    useAgentNodeStore.setState({ autopilotStates: { [node.id]: 'implementing' } });
    rerender(<NodeItem node={node} meshColor={{ name: 'default', hex: '#000', textOnDark: '#fff' }}
      isActive={false} onSelect={vi.fn()} onDelete={vi.fn()} />);
    expect(screen.queryByText('Lost conversation')).toBeNull();
  });

  it('renders a Retry resume button when the node is in error state with a cli_session_id', () => {
    renderNode(makeNode({ status: 'error', cli_session_id: 'a53dd36f-e703-4f27-9356-8e523472d94e' }));
    expect(screen.getByTitle('Retry resume with existing session')).toBeTruthy();
  });

  it('renders a Restart button when the node is in error state without a cli_session_id', () => {
    renderNode(makeNode({ status: 'error', cli_session_id: null }));
    expect(screen.getByTitle('Restart agent')).toBeTruthy();
  });

  it('does NOT render a Restart button when the node is running', () => {
    renderNode(makeNode({ status: 'running' }));
    expect(screen.queryByTestId('restart-button')).toBeNull();
  });

  it('renders a Resume button when the node is suspended AND has a cli_session_id', () => {
    // The Resume affordance is the user-driven recovery for Suspended
    // nodes — a Suspended node with a captured `cli_session_id` (the
    // crash-recovery / app-exit case) can be brought back by re-
    // attempting the same `--resume` the failed auto-resume tried.
    renderNode(makeNode({
      status: 'suspended',
      cli_session_id: 'a53dd36f-e703-4f27-9356-8e523472d94e',
    }));
    expect(screen.getByTitle('Resume agent')).toBeTruthy();
  });

  it('does NOT render a Resume button when the node is suspended but has no cli_session_id (autopilot gate)', () => {
    // Autopilot-gate Suspended rows are parked at creation with no
    // session id — the autopilot's own "Approve Sandbox Run" action
    // is the recovery surface there. Surfacing a Resume button that
    // would just surface a "no CLI session ID is stored" toast is a
    // worse UX than no affordance.
    renderNode(makeNode({ status: 'suspended', cli_session_id: null }));
    expect(screen.queryByTitle('Resume agent')).toBeNull();
  });

  it('does NOT render a Restart button when the node is idle', () => {
    renderNode(makeNode({ status: 'idle' }));
    expect(screen.queryByTestId('restart-button')).toBeNull();
  });

  it('clicking Restart invokes spawn_agent with the original cli_session_id so the resume re-attempts', async () => {
    mockInvoke.mockResolvedValueOnce(undefined);
    // The store's spawnAgent looks the node up in `agentNodes` to read
    // its cli_session_id for the resume argument. Production populates
    // the store via fetchAgentNodes on startup; the test must too.
    const node = makeNode({ status: 'error' });
    seedAgentNodes([node]);
    renderNode(node);
    fireEvent.click(screen.getByTestId('restart-button'));
    // spawnAgent's invoke is async; await a microtask so the assertion
    // doesn't race the in-flight promise.
    await Promise.resolve();
    expect(mockInvoke).toHaveBeenCalledWith(
      'spawn_agent',
      expect.objectContaining({
        request: expect.objectContaining({
          sessionId: 42,
          provider: 'anthropic',
          intent: { type: 'resume' },
        }),
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
    fireEvent.click(screen.getByTestId('restart-button'));
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('clicking Resume invokes spawn_agent with the stored cli_session_id so the resume re-attempts', async () => {
    // The Resume button re-attempts the same `--resume` the failed
    // auto-resume tried. Mirrors the Restart click test (line 81-101)
    // — both call `spawn_agent` with `intent: { type: 'resume' }`. For
    // adapters that don't honour resume (OpenCode, Terminal) the
    // backend now falls through to Fresh; the IPC contract here is
    // unchanged (the fall-through is internal to `spawn_with_intent`).
    mockInvoke.mockResolvedValueOnce(undefined);
    const node = makeNode({
      status: 'suspended',
      cli_session_id: 'a53dd36f-e703-4f27-9356-8e523472d94e',
    });
    seedAgentNodes([node]);
    renderNode(node);
    fireEvent.click(screen.getByTitle('Resume agent'));
    await Promise.resolve();
    expect(mockInvoke).toHaveBeenCalledWith(
      'spawn_agent',
      expect.objectContaining({
        request: expect.objectContaining({
          sessionId: 42,
          provider: 'anthropic',
          intent: { type: 'resume' },
        }),
      }),
    );
  });

  it('clicking Resume does NOT also fire the row onSelect (stopPropagation guard)', () => {
    // Same propagation contract as the Restart button — pinning it for
    // the Resume affordance so a future refactor can't silently
    // regress it.
    const onSelect = vi.fn();
    renderNode(makeNode({
      status: 'suspended',
      cli_session_id: 'a53dd36f-e703-4f27-9356-8e523472d94e',
    }), onSelect);
    fireEvent.click(screen.getByTitle('Resume agent'));
    expect(onSelect).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// Issue #1306 — restartFreshAgent and spawnAgent fresh option:
// passes `intent: { type: 'fresh' }` to spawn_agent IPC, clearing stale session ID and booting fresh.
// ---------------------------------------------------------------------------
describe('AgentNodeStore restartFreshAgent (issue #1306)', () => {
  beforeEach(() => {
    useAgentNodeStore.setState({ nodesById: {}, nodeIds: [],activeNodeId: null,
      loading: false,
      error: null,
    });
    vi.clearAllMocks();
  });

  it('restartFreshAgent calls spawn_agent with a Fresh intent when node has a cli_session_id', async () => {
    mockInvoke.mockResolvedValueOnce(undefined); // spawn_agent
    mockInvoke.mockResolvedValueOnce([]);        // list_agent_nodes
    mockInvoke.mockResolvedValueOnce([]);        // list_autopilot_runs

    const node = makeNode({ id: 42, status: 'error', cli_session_id: 'stale-uuid-1234' });
    seedAgentNodes([node]);

    await useAgentNodeStore.getState().restartFreshAgent(42);

    expect(mockInvoke).toHaveBeenCalledWith(
      'spawn_agent',
      expect.objectContaining({
        request: expect.objectContaining({
          sessionId: 42,
          provider: 'anthropic',
          intent: { type: 'fresh' },
        }),
      }),
    );
  });

  it('restartFreshAgent passes custom rows/cols when provided', async () => {
    mockInvoke.mockResolvedValueOnce(undefined); // spawn_agent
    mockInvoke.mockResolvedValueOnce([]);        // list_agent_nodes
    mockInvoke.mockResolvedValueOnce([]);        // list_autopilot_runs

    const node = makeNode({ id: 42, status: 'error', cli_session_id: 'stale-uuid-1234' });
    seedAgentNodes([node]);

    await useAgentNodeStore.getState().restartFreshAgent(42, { rows: 30, cols: 100 });

    expect(mockInvoke).toHaveBeenCalledWith(
      'spawn_agent',
      expect.objectContaining({
        request: expect.objectContaining({
          sessionId: 42,
          provider: 'anthropic',
          intent: { type: 'fresh' },
          rows: 30,
          cols: 100,
        }),
      }),
    );
  });

  it('throws an error if node is not found', async () => {
    await expect(
      useAgentNodeStore.getState().restartFreshAgent(999),
    ).rejects.toThrow('Node 999 not found');
  });
});

describe('NodeItem closing state', () => {
  beforeEach(() => {
    useAgentNodeStore.setState({ nodesById: {}, nodeIds: [],activeNodeId: null,
      loading: false,
      error: null,
      closingNodeIds: new Set(),
    });
    vi.clearAllMocks();
  });

  it('shows a closing spinner and hides the delete button while the node is closing', () => {
    // Closing a node first runs a (potentially slow) worktree safety check
    // before the row can be removed. During that window the row must look
    // busy rather than frozen, so the Ã— is swapped for a spinner.
    const node = makeNode({ id: 77, status: 'idle' });
    useAgentNodeStore.setState({ closingNodeIds: new Set([77]) });
    renderNode(node);

    expect(screen.getByTitle('Closing…')).toBeTruthy();
    expect(screen.queryByTitle('Delete node')).toBeNull();
  });

  it('shows the delete button (not a spinner) when the node is not closing', () => {
    renderNode(makeNode({ id: 77, status: 'idle' }));

    expect(screen.getByTitle('Delete node')).toBeTruthy();
    expect(screen.queryByTitle('Closing…')).toBeNull();
  });
});
