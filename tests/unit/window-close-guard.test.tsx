/**
 * WindowCloseGuard (issue #1501) — Tauri onCloseRequested interception.
 *
 * The veto decision is a synchronous store read (no IPC on the close
 * path): `confirmBeforeQuit` comes from `useExitPromptStore`, nodes from
 * `useAgentNodeStore.getState()`. The provider-list fetch rides the shared
 * `listProviders` cache and only runs after the synchronous veto.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { ProviderInfo } from '../../src/types/generated/ProviderInfo';

const windowApi = vi.hoisted(() => ({
  onCloseRequested: vi.fn<(cb: (e: { preventDefault: () => void }) => void | Promise<void>) => Promise<() => void>>(),
  destroy: vi.fn(),
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => windowApi,
}));

const tauriMocks = vi.hoisted(() => ({
  listProviders: vi.fn(),
  cancelWindowClose: vi.fn(),
}));

vi.mock('../../src/lib/tauri', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../src/lib/tauri')>()),
  listProviders: tauriMocks.listProviders,
  cancelWindowClose: tauriMocks.cancelWindowClose,
}));

import { WindowCloseGuard } from '../../src/components/WindowCloseGuard/WindowCloseGuard';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useExitPromptStore } from '../../src/stores/exitPromptStore';

function makeNode(overrides: Partial<AgentNode>): AgentNode {
  return {
    id: 1,
    mesh_id: 1,
    name: 'agent-1',
    path: '/repo',
    branch: 'main',
    env: 'Windows',
    provider: 'claude',
    status: 'running',
    cli_session_id: 'sess-1',
    worktree_name: null,
    use_worktree: false,
    is_pinned: false,
    source_issue: null,
    source_pr: null,
    head_repo_owner: null,
    head_repo_clone_url: null,
    source_pr_pinned_sha: null,
    signal_health: null,
    position: 0,
    created_at: '2026-01-01T00:00:00Z',
    worktree_path: null,
    ...overrides,
  } as AgentNode;
}

function providerList(): ProviderInfo[] {
  const caps = (harness_id: string, supports_resume: boolean) => ({
    harness_id,
    supports_resume,
    auto_resume_on_startup: supports_resume,
    requires_attention_hook: false,
    attention_capability: null,
    supports_passive_turn_watcher: false,
    produces_readable_transcript: true,
    supports_model_override: false,
    supports_effort_override: false,
    supports_extra_args: false,
    supports_prefill: false,
    is_plain_terminal: harness_id === 'terminal',
    effort_control: { kind: 'none', allowed: [] },
  });
  return [
    {
      id: 'claude', label: 'Claude Code', color: '#000', icon: 'C', resumable: true,
      harness_id: 'claude', provider_id: null, is_proxied: false, group_key: 'claude',
      capabilities: caps('claude', true),
    },
    {
      id: 'terminal', label: 'Terminal', color: '#000', icon: 'T', resumable: false,
      harness_id: 'terminal', provider_id: null, is_proxied: false, group_key: 'terminal',
      capabilities: caps('terminal', false),
    },
  ] as unknown as ProviderInfo[];
}

let closeHandler: ((e: { preventDefault: () => void }) => Promise<void>) | null = null;

beforeEach(() => {
  closeHandler = null;
  windowApi.onCloseRequested.mockReset().mockImplementation((cb) => {
    closeHandler = cb;
    return Promise.resolve(() => {});
  });
  windowApi.destroy.mockReset().mockResolvedValue(undefined);
  tauriMocks.listProviders.mockReset().mockResolvedValue(providerList());
  tauriMocks.cancelWindowClose.mockReset().mockResolvedValue(undefined);
  useAgentNodeStore.setState({
    nodesById: {},
    nodeIds: [],
    activeNodeId: null,
    loading: false,
    error: null,
  });
  useExitPromptStore.setState({ pending: null, exiting: false, confirmBeforeQuit: true });
});

function seedNodes(nodes: AgentNode[]) {
  const nodesById: Record<number, AgentNode> = {};
  for (const n of nodes) nodesById[n.id] = n;
  useAgentNodeStore.setState({
    nodesById,
    nodeIds: nodes.map((n) => n.id),
  });
}

async function fireClose(): Promise<{ prevented: boolean }> {
  let prevented = false;
  await act(async () => {
    await closeHandler!({ preventDefault: () => { prevented = true; } });
  });
  return { prevented };
}

describe('WindowCloseGuard (issue #1501)', () => {
  it('allows close immediately when no nodes are active', async () => {
    seedNodes([makeNode({ id: 1, status: 'idle', cli_session_id: null })]);
    render(<WindowCloseGuard />);
    await act(async () => {});
    const { prevented } = await fireClose();
    expect(prevented).toBe(false);
    expect(screen.queryByRole('heading', { name: 'Exit Buildmesh?' })).toBeNull();
    expect(tauriMocks.listProviders).not.toHaveBeenCalled();
  });

  it('prompts with active count and warns about the non-resumable agent', async () => {
    seedNodes([
      makeNode({ id: 1, name: 'resumable-one', provider: 'claude', status: 'running', cli_session_id: 's1' }),
      makeNode({ id: 2, name: 'fresh-agent', provider: 'claude', status: 'running', cli_session_id: null }),
    ]);
    render(<WindowCloseGuard />);
    await act(async () => {});
    const { prevented } = await fireClose();
    expect(prevented).toBe(true);
    await screen.findByRole('heading', { name: 'Exit Buildmesh?' });
    expect(screen.getByText('You have 2 active agent session(s) running.')).toBeTruthy();
    expect(screen.getByRole('alert').textContent).toContain('fresh-agent (Claude Code)');
  });

  it('vetoes synchronously before the async provider read resolves', async () => {
    // Slow IPC must not let the window close before the veto arrives:
    // preventDefault lands in the same tick as the close event, not
    // after the provider promise settles.
    let resolveProviders!: (v: ProviderInfo[]) => void;
    tauriMocks.listProviders.mockReturnValue(
      new Promise((resolve) => { resolveProviders = resolve; }),
    );
    seedNodes([makeNode({ status: 'running' })]);
    render(<WindowCloseGuard />);
    await act(async () => {});
    let prevented = false;
    const pendingClose = closeHandler!({ preventDefault: () => { prevented = true; } });
    expect(prevented).toBe(true);
    await act(async () => {
      resolveProviders(providerList());
      await pendingClose;
    });
    await screen.findByRole('heading', { name: 'Exit Buildmesh?' });
  });

  it('a second close while the modal is open stays vetoed without refetching', async () => {
    seedNodes([makeNode({ status: 'running' })]);
    render(<WindowCloseGuard />);
    await act(async () => {});
    await fireClose();
    await screen.findByRole('heading', { name: 'Exit Buildmesh?' });
    expect(tauriMocks.listProviders).toHaveBeenCalledTimes(1);
    const { prevented } = await fireClose();
    expect(prevented).toBe(true);
    expect(tauriMocks.listProviders).toHaveBeenCalledTimes(1);
    expect(windowApi.destroy).not.toHaveBeenCalled();
  });

  it('Keep Working dismisses and retracts the backend expected-exit marking', async () => {
    seedNodes([makeNode({ status: 'running' })]);
    render(<WindowCloseGuard />);
    await act(async () => {});
    await fireClose();
    await screen.findByRole('heading', { name: 'Exit Buildmesh?' });
    fireEvent.click(screen.getByRole('button', { name: 'Keep Working' }));
    await waitFor(() =>
      expect(screen.queryByRole('heading', { name: 'Exit Buildmesh?' })).toBeNull(),
    );
    expect(tauriMocks.cancelWindowClose).toHaveBeenCalledTimes(1);
    expect(windowApi.destroy).not.toHaveBeenCalled();
  });

  it('Exit Buildmesh destroys the window for graceful shutdown', async () => {
    seedNodes([makeNode({ status: 'running' })]);
    render(<WindowCloseGuard />);
    await act(async () => {});
    await fireClose();
    await screen.findByRole('heading', { name: 'Exit Buildmesh?' });
    fireEvent.click(screen.getByRole('button', { name: 'Exit Buildmesh' }));
    await waitFor(() => expect(windowApi.destroy).toHaveBeenCalledTimes(1));
  });

  it('respects the opt-out preference synchronously with no IPC and no destroy dance', async () => {
    useExitPromptStore.setState({ confirmBeforeQuit: false });
    seedNodes([makeNode({ status: 'running' })]);
    render(<WindowCloseGuard />);
    await act(async () => {});
    const { prevented } = await fireClose();
    expect(prevented).toBe(false);
    expect(screen.queryByRole('heading', { name: 'Exit Buildmesh?' })).toBeNull();
    expect(tauriMocks.listProviders).not.toHaveBeenCalled();
    expect(windowApi.destroy).not.toHaveBeenCalled();
  });
});
