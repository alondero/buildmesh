import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { AttentionList } from '../../src/components/Sidebar/AttentionList';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useToastStore } from '../../src/stores/toastStore';
import type { LifecycleChangedPayload } from '../../src/types/generated/LifecycleChangedPayload';
import { seedAgentNodes } from './helpers/seedAgentNodes';

function node(id: number, status: AgentNode['status'], extra: Partial<AgentNode> = {}): AgentNode {
  return {
    id, status, name: `Session ${id}`, mesh_id: 1, path: '/repo', branch: 'main',
    env: 'Windows', provider: 'anthropic', cli_session_id: null, worktree_name: null,
    use_worktree: false, is_pinned: false, source_issue: null, source_pr: null,
    head_repo_owner: null, head_repo_clone_url: null, source_pr_pinned_sha: null,
    created_at: '2026-10-16T12:00:00Z', ...extra,
  };
}

function observation(status: AgentNode['status'], kind: LifecycleChangedPayload['kind'], message: string): LifecycleChangedPayload {
  return {
    session_id: 1, provider: 'anthropic', status, kind, message,
    timestamp: '2026-10-16T12:00:00Z', signal_health: 'ok', semantic_turn: null,
    provider_event: null, provider_session_id: null, completion_reason: null, transcript_path: null,
  };
}

beforeEach(() => {
  seedAgentNodes([]);
  useMeshStore.setState({ meshes: [], meshesById: new Map() });
  useToastStore.setState({ toasts: [] });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe('Attention inbox', () => {
  it('groups failures before input and paused sessions, with counts and stable order', () => {
    seedAgentNodes([node(5, 'suspended'), node(3, 'lost'), node(2, 'awaiting_input'), node(1, 'error'), node(4, 'running')]);
    render(<AttentionList />);
    expect(screen.getAllByRole('heading', { level: 3 }).map(heading => heading.textContent)).toEqual([
      'Failed sessions 2', 'Needs input 1', 'Paused sessions 1',
    ]);
    expect(within(screen.getByRole('region', { name: 'Failed sessions 2' })).getAllByRole('heading', { level: 4 }).map(heading => heading.textContent)).toEqual(['Session 1', 'Session 3']);
    expect(screen.queryByText('Session 4')).toBeNull();
    expect(screen.getAllByRole('button', { name: /Open terminal for/ })).toHaveLength(4);
    expect(screen.queryByRole('button', { name: 'Session 1' })).toBeNull();
  });

  it('updates groups and removes resolved cards as live node state changes', () => {
    seedAgentNodes([node(1, 'awaiting_input'), node(2, 'error')]);
    render(<AttentionList />);
    act(() => useAgentNodeStore.getState().patchAgentNode(1, { status: 'suspended' }));
    expect(screen.queryByRole('heading', { name: /Needs input/ })).toBeNull();
    expect(screen.getByRole('heading', { name: 'Paused sessions 1' })).toBeTruthy();
    act(() => {
      useAgentNodeStore.getState().patchAgentNode(1, { status: 'running' });
      useAgentNodeStore.getState().patchAgentNode(2, { status: 'idle' });
    });
    expect(screen.getByRole('status').textContent).toBe('No agents need attention.');
    expect(screen.queryByRole('heading')).toBeNull();
    expect(screen.queryByRole('button')).toBeNull();
  });

  it('shows current questions and permission requests without presenting historical failures as current', () => {
    seedAgentNodes([
      node(1, 'awaiting_input', { lifecycle: observation('awaiting_input', 'question_requested', 'Which database should I use?') }),
      node(2, 'awaiting_input', { lifecycle: observation('awaiting_input', 'permission_requested', 'Allow npm install?') }),
      node(3, 'error', { lifecycle: observation('error', 'process_error', 'Signing certificate missing') }),
    ]);
    render(<AttentionList />);
    for (const text of ['Needs an answer', 'Which database should I use?', 'Needs permission', 'Allow npm install?']) {
      expect(screen.getByText(text)).toBeTruthy();
    }
    expect(screen.queryByText('Signing certificate missing')).toBeNull();
    expect(screen.getAllByText('Repository 1')).toHaveLength(3);
  });

  it('uses semantic request context and never shows context from another status', () => {
    const lifecycle = observation('awaiting_input', 'question_requested', 'Generic callback');
    lifecycle.semantic_turn = { node_id: 1, kind: 'command_confirmation', description: 'Approve the release command?' };
    seedAgentNodes([node(1, 'awaiting_input', { lifecycle }), node(2, 'error', { lifecycle })]);
    render(<AttentionList />);
    expect(screen.getAllByText('Approve the release command?')).toHaveLength(1);
    expect(screen.queryByText('Generic callback')).toBeNull();
    expect(within(screen.getByRole('region', { name: 'Failed sessions 1' })).queryByText('Approve the release command?')).toBeNull();
  });

  it('offers Resume only when the paused session has a saved identity', () => {
    seedAgentNodes([node(1, 'suspended'), node(2, 'suspended', { cli_session_id: '' }), node(3, 'suspended', { cli_session_id: 'saved' })]);
    render(<AttentionList />);
    expect(screen.getAllByRole('button', { name: /^Resume/ })).toHaveLength(1);
    expect(screen.getByRole('button', { name: 'Resume Session 3' })).toBeTruthy();
    expect(screen.getAllByRole('button', { name: /Open terminal for/ })).toHaveLength(3);
  });

  it('keeps obsolete failures out of cards after a rejected real store retry and a sidebar remount', async () => {
    seedAgentNodes([node(1, 'error', { lifecycle: observation('error', 'process_error', 'Signing certificate missing') })]);
    vi.mocked(invoke).mockRejectedValueOnce(new Error('Please sign in'));
    const { unmount } = render(<AttentionList />);
    fireEvent.click(screen.getByRole('button', { name: 'Retry Session 1' }));
    await waitFor(() => expect(useToastStore.getState().toasts.some(toast => toast.message === 'Please sign in')).toBe(true));
    expect(screen.queryByText('Signing certificate missing')).toBeNull();
    expect(useAgentNodeStore.getState().nodesById[1].status).toBe('error');
    unmount();
    render(<AttentionList />);
    expect(screen.queryByText('Signing certificate missing')).toBeNull();
    expect(screen.getByRole('button', { name: 'Retry Session 1' })).toBeTruthy();
  });

  it('prevents duplicate recovery, reports rejection, and enables another attempt', async () => {
    let reject!: (reason: Error) => void;
    const pending = new Promise<void>((_, no) => { reject = no; });
    const spawn = vi.spyOn(useAgentNodeStore.getState(), 'spawnAgent').mockReturnValue(pending);
    seedAgentNodes([node(1, 'error')]);
    render(<AttentionList />);
    const retry = screen.getByRole('button', { name: 'Retry Session 1' }) as HTMLButtonElement;
    fireEvent.click(retry);
    fireEvent.click(retry);
    expect(retry.disabled).toBe(true);
    expect(retry.textContent).toBe('Starting…');
    expect(spawn).toHaveBeenCalledOnce();
    await act(async () => reject(new Error('Please sign in')));
    await waitFor(() => expect(retry.disabled).toBe(false));
    expect(useToastStore.getState().toasts.some(toast => toast.message === 'Please sign in')).toBe(true);
    expect(screen.getByRole('heading', { name: 'Session 1' })).toBeTruthy();
  });
});
