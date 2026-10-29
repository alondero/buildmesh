/**
 * Agent History's recovery actions.
 *
 * The sidebar Attention view used to own the cross-repository triage surface:
 * a failed agent could be retried from a list of agents drawn from every
 * repository, a duplicate retry was refused, and a refused recovery was
 * reported rather than swallowed. That view is retired (ADR-0048), and Agent
 * History's Lifecycle list is the surface that took over the job — so the
 * behaviour is pinned here, on the surface that now carries it.
 *
 * Without these cases the contract would have been covered only by the
 * retired view's own test file, and deleting the view would have retired the
 * coverage with it.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act, render, screen, fireEvent, within, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { AgentHistoryTab } from '../../src/components/Probe/AgentHistoryTab';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';

const MESH = { id: 1, name: 'api', path: '/repos/api', base_ref: 'main', use_worktree: true };
const OTHER_MESH = { id: 2, name: 'web', path: '/repos/web', base_ref: 'main', use_worktree: true };

const realSpawnAgent = useAgentNodeStore.getState().spawnAgent;

function node(id: number, name: string, extra: Partial<AgentNode> = {}): AgentNode {
  return {
    id, mesh_id: MESH.id, name, path: MESH.path, branch: 'main', env: 'windows',
    provider: 'claude', use_worktree: false, status: 'archived', cli_session_id: 'saved-session',
    created_at: '2026-10-17T00:00:00Z',
    ...extra,
  } as AgentNode;
}

/** The card that lists one agent. */
function card(name: string): HTMLElement {
  return screen.getByText(name).parentElement as HTMLElement;
}

function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    switch (cmd) {
      case 'list_agent_history': return Promise.resolve([]);
      case 'discover_agent_nodes': return Promise.resolve([]);
      case 'list_providers': return Promise.resolve([]);
      default: return Promise.resolve({});
    }
  });
}

beforeEach(() => {
  useUIStore.setState({ probeOpen: true, probeTab: 'sessions', activeDiffFile: null, viewMode: 'mesh' });
  useMeshStore.setState({
    selectedMeshId: MESH.id,
    meshes: [MESH, OTHER_MESH] as never,
    meshesById: new Map([[MESH.id, MESH], [OTHER_MESH.id, OTHER_MESH]]) as never,
  });
  seedAgentNodes([]);
  mockBackend();
});

afterEach(() => {
  useAgentNodeStore.setState({ spawnAgent: realSpawnAgent });
  vi.restoreAllMocks();
});

describe('Agent History recovery actions', () => {
  it('offers the recovery action each lifecycle state allows', async () => {
    seedAgentNodes([
      node(1, 'running-agent', { status: 'running' }),
      node(2, 'failed-agent', { status: 'error' }),
      node(3, 'paused-agent', { status: 'suspended' }),
      node(4, 'closed-agent', { status: 'archived' }),
    ]);
    render(<AgentHistoryTab />);
    await screen.findByText('failed-agent');

    // A healthy agent has a terminal to open but no recovery control.
    expect(within(card('running-agent')).getByRole('button', { name: 'Open terminal' })).toBeTruthy();
    expect(within(card('running-agent')).queryByRole('button', { name: /^(Retry|Resume|Reopen)$/ })).toBeNull();

    // A failed one can be retried, a paused one resumed.
    expect(within(card('failed-agent')).getByRole('button', { name: 'Open terminal' })).toBeTruthy();
    expect(within(card('failed-agent')).getByRole('button', { name: 'Retry' })).toBeTruthy();
    expect(within(card('paused-agent')).getByRole('button', { name: 'Open terminal' })).toBeTruthy();
    expect(within(card('paused-agent')).getByRole('button', { name: 'Resume' })).toBeTruthy();

    // A closed one is restored rather than resumed, and has no terminal to open.
    expect(within(card('closed-agent')).queryByRole('button', { name: 'Open terminal' })).toBeNull();
    expect(within(card('closed-agent')).getByRole('button', { name: 'Reopen' })).toBeTruthy();
  });

  it('lists agents from every repository and recovers the one it names', async () => {
    const spawn = vi.fn().mockResolvedValue(undefined);
    useAgentNodeStore.setState({ spawnAgent: spawn as never });
    seedAgentNodes([
      node(2, 'api-failure', { status: 'error' }),
      node(3, 'web-failure', { mesh_id: OTHER_MESH.id, path: OTHER_MESH.path, status: 'error' }),
    ]);
    render(<AgentHistoryTab />);
    await screen.findByText('api-failure');

    // The list spans repositories, and each card names the one it belongs to.
    expect(screen.getByText('api-failure')).toBeTruthy();
    expect(screen.getByText('web-failure')).toBeTruthy();
    expect(screen.getByText('api · main')).toBeTruthy();
    expect(screen.getByText('web · main')).toBeTruthy();

    // Retrying the second repository's agent recovers that agent.
    fireEvent.click(within(card('web-failure')).getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(spawn).toHaveBeenCalledWith(3, 'claude'));
    expect(spawn).toHaveBeenCalledOnce();
  });

  it('opens the terminal of the agent it lists', async () => {
    seedAgentNodes([node(2, 'failed-agent', { status: 'error' })]);
    render(<AgentHistoryTab />);
    await screen.findByText('failed-agent');

    fireEvent.click(within(card('failed-agent')).getByRole('button', { name: 'Open terminal' }));

    expect(useAgentNodeStore.getState().activeNodeId).toBe(2);
    expect(useMeshStore.getState().selectedMeshId).toBe(MESH.id);
    expect(useUIStore.getState().viewMode).toBe('single');
  });

  it('refuses a second recovery while one is in flight, and reports a rejected one', async () => {
    let reject!: (reason: Error) => void;
    const pending = new Promise<void>((_, no) => { reject = no; });
    const spawn = vi.fn().mockReturnValue(pending);
    useAgentNodeStore.setState({ spawnAgent: spawn as never });
    seedAgentNodes([node(2, 'failed-agent', { status: 'error' })]);
    render(<AgentHistoryTab />);
    await screen.findByText('failed-agent');

    const retry = within(card('failed-agent')).getByRole('button', { name: 'Retry' }) as HTMLButtonElement;
    fireEvent.click(retry);
    fireEvent.click(retry);

    // One attempt, not two: the second click is refused while the first runs.
    expect(spawn).toHaveBeenCalledOnce();
    expect(spawn).toHaveBeenCalledWith(2, 'claude');
    expect(retry.disabled).toBe(true);
    expect(retry.textContent).toBe('Opening…');

    await act(async () => reject(new Error('Please sign in')));

    // A rejected recovery is reported, and the control is usable again.
    await waitFor(() => expect(retry.disabled).toBe(false));
    expect(screen.getByRole('alert').textContent).toContain('Please sign in');
  });
});
