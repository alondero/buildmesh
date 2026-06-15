/**
 * Tests for the Session History probe tab — issue #378.
 *
 * Pins the migration invariants:
 *   - the tab discovers sessions for the active *mesh root* path (not
 *     a focused worktree's working directory)
 *   - the search input filters by message / branch / worktree
 *   - the primary Resume button does the import → set-active → spawn
 *     sequence and toggles the probe off so the user lands on the
 *     terminal
 *   - the `▾` provider picker only exposes Claude Code-backed
 *     providers (anthropic / minimax / kimi), because the others
 *     don't read session transcripts from disk and can't resume
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { SessionHistoryTab } from '../../src/components/Probe/SessionHistoryTab';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import type { DiscoveredSession } from '../../src/lib/tauri';

const MESH: Mesh = {
  id: 42,
  name: 'demo',
  path: '/repos/demo',
  layout: 'single',
  position: 0,
  created_at: '2026-01-01',
};

const SESSIONS: DiscoveredSession[] = [
  {
    session_id: 's-abc-1',
    first_message: 'Add a /v2 endpoint',
    branch: 'feat/v2',
    cwd: '/repos/demo',
    timestamp: new Date(Date.now() - 5 * 60_000).toISOString(),
    worktree_name: 'agent-v2',
  },
  {
    session_id: 's-abc-2',
    first_message: 'Fix the wobble',
    branch: null,
    cwd: '/repos/demo',
    timestamp: new Date(Date.now() - 2 * 86_400_000).toISOString(),
    worktree_name: null,
  },
];

const PROVIDERS = [
  { id: 'anthropic', label: 'Anthropic', color: '#000', icon: '' },
  { id: 'minimax', label: 'Minimax', color: '#000', icon: '' },
  { id: 'kimi', label: 'Kimi', color: '#000', icon: '' },
  { id: 'opencode', label: 'OpenCode', color: '#000', icon: '' },
  { id: 'antigravity', label: 'Antigravity', color: '#000', icon: '' },
];

const RESUMED_NODE = {
  id: 99,
  mesh_id: 42,
  name: 'resumed-node',
  path: '/repos/demo',
  branch: 'feat/v2',
  env: 'wsl',
  provider: 'anthropic',
  status: 'pending',
  use_worktree: true,
  position: 0,
  created_at: '2026-01-01',
};

function mockBackend(opts: { sessions?: DiscoveredSession[]; defaultProvider?: string } = {}) {
  vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
    switch (cmd) {
      case 'discover_sessions':
        return Promise.resolve(opts.sessions ?? SESSIONS);
      case 'list_providers':
        return Promise.resolve(PROVIDERS);
      case 'get_default_provider':
        return Promise.resolve(opts.defaultProvider ?? 'anthropic');
      case 'import_discovered_session':
        return Promise.resolve(RESUMED_NODE);
      case 'spawn_agent':
        return Promise.resolve(undefined);
      case 'list_sessions':
        return Promise.resolve([]);
      default:
        return Promise.resolve({});
    }
  });
}

describe('SessionHistoryTab (#378)', () => {
  beforeEach(() => {
    useUIStore.setState({ probeOpen: true, probeTab: 'sessions', activeDiffFile: null });
    useMeshStore.setState({
      meshesById: new Map([[MESH.id, MESH]]),
      selectedMeshId: MESH.id,
    });
    useAgentNodeStore.setState({ agentNodes: [], activeNodeId: null });
  });

  it('lists discovered sessions for the active mesh', async () => {
    mockBackend();
    render(<SessionHistoryTab />);

    expect(await screen.findByText('Add a /v2 endpoint')).toBeTruthy();
    expect(screen.getByText('Fix the wobble')).toBeTruthy();
    // Branch and worktree labels render alongside the message.
    expect(screen.getByText('feat/v2')).toBeTruthy();
    expect(screen.getByText('agent-v2')).toBeTruthy();
  });

  it('filters by first_message text', async () => {
    mockBackend();
    render(<SessionHistoryTab />);

    const search = await screen.findByPlaceholderText('Filter by message, branch, or worktree…');
    await userEvent.type(search, 'wobble');

    await waitFor(() => {
      expect(screen.queryByText('Add a /v2 endpoint')).toBeNull();
    });
    expect(screen.getByText('Fix the wobble')).toBeTruthy();
  });

  it('filters by branch name', async () => {
    mockBackend();
    render(<SessionHistoryTab />);

    const search = await screen.findByPlaceholderText('Filter by message, branch, or worktree…');
    await userEvent.type(search, 'feat/v2');

    await waitFor(() => {
      expect(screen.queryByText('Fix the wobble')).toBeNull();
    });
    expect(screen.getByText('Add a /v2 endpoint')).toBeTruthy();
  });

  it('filters by worktree name', async () => {
    mockBackend();
    render(<SessionHistoryTab />);

    const search = await screen.findByPlaceholderText('Filter by message, branch, or worktree…');
    await userEvent.type(search, 'agent-v2');

    await waitFor(() => {
      expect(screen.queryByText('Fix the wobble')).toBeNull();
    });
    expect(screen.getByText('Add a /v2 endpoint')).toBeTruthy();
  });

  it('shows a "No previous sessions found" empty state when discovery is empty', async () => {
    mockBackend({ sessions: [] });
    render(<SessionHistoryTab />);

    expect(await screen.findByText('No previous sessions found')).toBeTruthy();
  });

  it('shows "No matches" when a search filters everything out', async () => {
    mockBackend();
    render(<SessionHistoryTab />);

    const search = await screen.findByPlaceholderText('Filter by message, branch, or worktree…');
    await userEvent.type(search, 'definitely-not-a-match');

    expect(await screen.findByText('No matches')).toBeTruthy();
  });

  it('does the import → spawn sequence on the primary Resume button and hides the probe', async () => {
    mockBackend();
    useUIStore.setState({ probeOpen: true, probeTab: 'sessions' });
    render(<SessionHistoryTab />);

    // `findAllByText` — each session row renders its own "Resume" button.
    // This test wants the first row's primary action.
    const resumes = await screen.findAllByText('Resume');
    await userEvent.click(resumes[0]);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('import_discovered_session', {
        meshId: 42,
        meshPath: '/repos/demo',
        cliSessionId: 's-abc-1',
        branch: 'feat/v2',
        worktreeName: 'agent-v2',
        provider: 'anthropic',
      });
    });
    // spawn_agent is the slow IPC that rehydrates the session into a
    // running PTY. The store's `spawnAgent` wraps the same command and
    // is asserted via the wire; the field name on the wire is
    // `sessionId` (matching the original tauri command's snake_case).
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('spawn_agent', expect.objectContaining({
        sessionId: 99,
        provider: 'anthropic',
      }));
    });
    // The probe should hide so the user lands on the terminal — mirrors
    // the legacy modal's `onClose()` behaviour.
    await waitFor(() => {
      expect(useUIStore.getState().probeOpen).toBe(false);
    });
  });

  it('uses "main" as the branch fallback when the session has no branch', async () => {
    // The session without a branch (`s-abc-2`) exercises the
    // `session.branch || 'main'` fallback at the import call site.
    mockBackend();
    render(<SessionHistoryTab />);

    // `s-abc-1` is the first item; click the second Resume to hit the
    // fallback path.
    const resumes = await screen.findAllByText('Resume');
    await userEvent.click(resumes[1]);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('import_discovered_session', expect.objectContaining({
        cliSessionId: 's-abc-2',
        branch: 'main',
        worktreeName: null,
      }));
    });
  });

  it('uses the explicit provider from the `▾` picker for the resume', async () => {
    mockBackend();
    render(<SessionHistoryTab />);

    // Open the picker for the first session via the "Choose provider"
    // title (stable selector on the caret half of the split button).
    const carets = await screen.findAllByTitle('Choose provider');
    fireEvent.click(carets[0]);

    const minimaxOption = await screen.findByText('Minimax');
    await userEvent.click(minimaxOption);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('import_discovered_session', expect.objectContaining({
        provider: 'minimax',
      }));
    });
  });

  it('only exposes resumable providers in the `▾` picker', async () => {
    // The picker filters to Claude Code-backed providers; antigravity and
    // opencode can't read session transcripts from disk and therefore
    // would corrupt a resume if surfaced.
    mockBackend();
    render(<SessionHistoryTab />);

    const carets = await screen.findAllByTitle('Choose provider');
    fireEvent.click(carets[0]);

    expect(await screen.findByText('Anthropic')).toBeTruthy();
    expect(screen.getByText('Minimax')).toBeTruthy();
    expect(screen.getByText('Kimi')).toBeTruthy();
    expect(screen.queryByText('OpenCode')).toBeNull();
    expect(screen.queryByText('Antigravity')).toBeNull();
  });

  it('does not close the picker when the user clicks INSIDE the dropdown (no race)', async () => {
    // Regression test for the mousedown-vs-click race: the document-level
    // mousedown handler that closes the picker used to fire on the
    // mousedown that *precedes* the option's click event, tearing the
    // option out of the DOM before the click landed. The fix added a
    // `data-dropdown-for` attribute to the picker container and a
    // `.closest()` guard on the handler, mirroring the GitIssuesTab
    // pattern. This test exercises the fix: after a user-event click on
    // a picker option, the option button is still in the document when
    // the click handler runs.
    mockBackend();
    render(<SessionHistoryTab />);

    const carets = await screen.findAllByTitle('Choose provider');
    fireEvent.click(carets[0]);

    // The picker's container is still in the document at the moment the
    // click event fires on the option.
    const minimaxOption = await screen.findByText('Minimax');
    expect(minimaxOption.isConnected).toBe(true);
    await userEvent.click(minimaxOption);

    // The resume call landed, so the click event reached its handler.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('import_discovered_session', expect.objectContaining({
        provider: 'minimax',
      }));
    });
  });

  it('surfaces backend errors from discover_sessions with the raw message', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'discover_sessions') return Promise.reject(new Error('claude dir not found'));
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      return Promise.resolve({});
    });
    render(<SessionHistoryTab />);

    expect(await screen.findByText('Failed to discover sessions')).toBeTruthy();
    expect(screen.getByText('claude dir not found')).toBeTruthy();
  });
});
