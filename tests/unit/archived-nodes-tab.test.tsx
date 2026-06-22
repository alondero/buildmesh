/**
 * Tests for the Archive probe tab (issue #378; component renamed from
 * ArchivedNodesTab → ArchivedNodesTab after PR #523 set the visible
 * label to "Archive").
 *
 * Pins the migration invariants:
 *   - the tab discovers sessions for the active *mesh root* path (not
 *     a focused worktree's working directory)
 *   - the search input filters by message / branch / worktree
 *   - the primary Resume button does the import → set-active → spawn
 *     sequence and toggles the probe off so the user lands on the
 *     terminal
 *   - the `▾` provider picker only exposes providers whose backend-
 *     supplied `resumable` flag is true. The flag replaces the
 *     hardcoded `['anthropic','minimax','kimi']` allow-list so a
 *     custom Claude-compatible profile (e.g. "DeepSeek via Claude")
 *     shows up automatically (#550 follow-up).
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { ArchivedNodesTab } from '../../src/components/Probe/ArchivedNodesTab';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { __resetProviderCachesForTests } from '../../src/lib/tauri';
import type { ArchivedAgentNode } from '../../src/lib/tauri';

const MESH: Mesh = {
  id: 42,
  name: 'demo',
  path: '/repos/demo',
  layout: 'single',
  position: 0,
  created_at: '2026-01-01',
  scratchpad: '',
  sandbox: false,
};

const SESSIONS: ArchivedAgentNode[] = [
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

// Issue #575 / ADR-0016 — Spawn Options carry the full wire shape
// (harness_id, provider_id, is_proxied, group_key). The fixture here
// stands in for the post-#575 backend: every Proxied Provider row
// uses the composite id `claude:<account>`, and `resumable: true` is
// the derivation from `supports_resume() && produces_readable_transcript()`.
type ProviderFixture = {
  id: string;
  label: string;
  color: string;
  icon: string;
  resumable: boolean;
  harness_id: string;
  provider_id: string | null;
  is_proxied: boolean;
  group_key: string;
};

const PROVIDERS: ProviderFixture[] = [
  { id: 'claude', label: 'Claude Code', color: '#000', icon: '', resumable: true, harness_id: 'claude', provider_id: null, is_proxied: false, group_key: 'claude' },
  { id: 'claude:minimax', label: 'Minimax', color: '#000', icon: '', resumable: true, harness_id: 'claude', provider_id: 'minimax', is_proxied: true, group_key: 'claude' },
  { id: 'claude:kimi', label: 'Kimi', color: '#000', icon: '', resumable: true, harness_id: 'claude', provider_id: 'kimi', is_proxied: true, group_key: 'claude' },
  // OpenCode / Antigravity write transcripts the coordinator read API
  // doesn't parse, so resume would corrupt the session — backend sets
  // `resumable: false` and the picker hides them.
  { id: 'opencode', label: 'OpenCode', color: '#000', icon: '', resumable: false, harness_id: 'opencode', provider_id: null, is_proxied: false, group_key: 'opencode' },
  { id: 'antigravity', label: 'Antigravity', color: '#000', icon: '', resumable: false, harness_id: 'agy', provider_id: null, is_proxied: false, group_key: 'agy' },
];

// Custom Claude-compatible profile — the exact case the old id allow-list
// silently hid. A user-defined id + name; the `resumable` flag is what
// matters for picker inclusion.
const CUSTOM_PROVIDERS: ProviderFixture[] = [
  ...PROVIDERS,
  { id: 'claude:deepseek', label: 'DeepSeek (via Claude)', color: '#000', icon: '', resumable: true, harness_id: 'claude', provider_id: 'deepseek', is_proxied: true, group_key: 'claude' },
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

function mockBackend(opts: {
  sessions?: ArchivedAgentNode[];
  defaultProvider?: string;
  providers?: typeof PROVIDERS;
} = {}) {
  vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
    switch (cmd) {
      case 'discover_agent_nodes':
        return Promise.resolve(opts.sessions ?? SESSIONS);
      case 'list_providers':
        return Promise.resolve(opts.providers ?? PROVIDERS);
      case 'get_default_provider':
        return Promise.resolve(opts.defaultProvider ?? 'anthropic');
      case 'import_discovered_agent_node':
        return Promise.resolve(RESUMED_NODE);
      case 'spawn_agent':
        return Promise.resolve(undefined);
      case 'list_agent_nodes':
        return Promise.resolve([]);
      default:
        return Promise.resolve({});
    }
  });
}

describe('ArchivedNodesTab (#378)', () => {
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
    render(<ArchivedNodesTab />);

    expect(await screen.findByText('Add a /v2 endpoint')).toBeTruthy();
    expect(screen.getByText('Fix the wobble')).toBeTruthy();
    // Branch and worktree labels render alongside the message.
    expect(screen.getByText('feat/v2')).toBeTruthy();
    expect(screen.getByText('agent-v2')).toBeTruthy();
  });

  it('filters by first_message text', async () => {
    mockBackend();
    render(<ArchivedNodesTab />);

    const search = await screen.findByPlaceholderText('Filter by message, branch, or worktree…');
    await userEvent.type(search, 'wobble');

    await waitFor(() => {
      expect(screen.queryByText('Add a /v2 endpoint')).toBeNull();
    });
    expect(screen.getByText('Fix the wobble')).toBeTruthy();
  });

  it('filters by branch name', async () => {
    mockBackend();
    render(<ArchivedNodesTab />);

    const search = await screen.findByPlaceholderText('Filter by message, branch, or worktree…');
    await userEvent.type(search, 'feat/v2');

    await waitFor(() => {
      expect(screen.queryByText('Fix the wobble')).toBeNull();
    });
    expect(screen.getByText('Add a /v2 endpoint')).toBeTruthy();
  });

  it('filters by worktree name', async () => {
    mockBackend();
    render(<ArchivedNodesTab />);

    const search = await screen.findByPlaceholderText('Filter by message, branch, or worktree…');
    await userEvent.type(search, 'agent-v2');

    await waitFor(() => {
      expect(screen.queryByText('Fix the wobble')).toBeNull();
    });
    expect(screen.getByText('Add a /v2 endpoint')).toBeTruthy();
  });

  it('shows a "No previous sessions found" empty state when discovery is empty', async () => {
    mockBackend({ sessions: [] });
    render(<ArchivedNodesTab />);

    expect(await screen.findByText('No previous sessions found')).toBeTruthy();
  });

  it('shows "No matches" when a search filters everything out', async () => {
    mockBackend();
    render(<ArchivedNodesTab />);

    const search = await screen.findByPlaceholderText('Filter by message, branch, or worktree…');
    await userEvent.type(search, 'definitely-not-a-match');

    expect(await screen.findByText('No matches')).toBeTruthy();
  });

  it('does the import → spawn sequence on the primary Resume button and hides the probe', async () => {
    mockBackend();
    useUIStore.setState({ probeOpen: true, probeTab: 'sessions' });
    render(<ArchivedNodesTab />);

    // `findAllByText` — each session row renders its own "Resume" button.
    // This test wants the first row's primary action.
    const resumes = await screen.findAllByText('Resume');
    await userEvent.click(resumes[0]);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('import_discovered_agent_node', {
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
    // `nodeId` (matching the original tauri command's snake_case).
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
    render(<ArchivedNodesTab />);

    // `s-abc-1` is the first item; click the second Resume to hit the
    // fallback path.
    const resumes = await screen.findAllByText('Resume');
    await userEvent.click(resumes[1]);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('import_discovered_agent_node', expect.objectContaining({
        cliSessionId: 's-abc-2',
        branch: 'main',
        worktreeName: null,
      }));
    });
  });

  it('uses the explicit provider from the `▾` picker for the resume', async () => {
    mockBackend();
    render(<ArchivedNodesTab />);

    // Open the picker for the first session via the "Choose provider"
    // title (stable selector on the caret half of the split button).
    const carets = await screen.findAllByTitle('Choose provider');
    fireEvent.click(carets[0]);

    const minimaxOption = await screen.findByText('Minimax');
    await userEvent.click(minimaxOption);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('import_discovered_agent_node', expect.objectContaining({
        provider: 'claude:minimax',
      }));
    });
  });

  it('only exposes resumable providers in the `▾` picker', async () => {
    // The picker filters by the backend-supplied `resumable` flag (the
    // resolved adapter's `supports_resume() && produces_readable_transcript()`).
    // Antigravity and OpenCode write transcripts the coordinator read API
    // can't parse, so the backend reports `resumable: false` and the
    // picker hides them.
    mockBackend();
    render(<ArchivedNodesTab />);

    const carets = await screen.findAllByTitle('Choose provider');
    fireEvent.click(carets[0]);

    expect(await screen.findByText('Claude Code')).toBeTruthy();
    expect(screen.getByText('Minimax')).toBeTruthy();
    expect(screen.getByText('Kimi')).toBeTruthy();
    expect(screen.queryByText('OpenCode')).toBeNull();
    expect(screen.queryByText('Antigravity')).toBeNull();
  });

  it('exposes custom Claude-compatible profiles in the `▾` picker (issue #550 follow-up)', async () => {
    // Regression: the picker used to filter by a hardcoded
    // `['anthropic','minimax','kimi']` id allow-list, which silently hid
    // any custom Claude-compatible profile (e.g. "DeepSeek via Claude").
    // The fix is a backend-supplied `resumable` flag on ProviderInfo, so
    // the frontend just renders whatever the backend reports.
    //
    // `listProviders` is memoised module-wide (issue #405), so a previous
    // test's cached promise would otherwise shadow this mock and the
    // dropdown would only see `PROVIDERS`. Reset the cache *before*
    // re-installing the mock so the next `listProviders()` call hits the
    // freshly-installed mock.
    __resetProviderCachesForTests();
    mockBackend({ providers: CUSTOM_PROVIDERS });
    render(<ArchivedNodesTab />);

    const carets = await screen.findAllByTitle('Choose provider');
    fireEvent.click(carets[0]);
    await screen.findByText('Claude Code');

    // The user-chosen label must appear — the picker no longer cares
    // about the underlying id, only the `resumable` flag.
    expect(screen.getByText('DeepSeek (via Claude)')).toBeTruthy();
    // Picking it should round-trip to the backend with that exact
    // composite id (issue #575 / ADR-0016).
    const deepseekOption = screen.getByText('DeepSeek (via Claude)');
    await userEvent.click(deepseekOption);
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('import_discovered_agent_node', expect.objectContaining({
        provider: 'claude:deepseek',
      }));
    });
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
    render(<ArchivedNodesTab />);

    const carets = await screen.findAllByTitle('Choose provider');
    fireEvent.click(carets[0]);

    // The picker's container is still in the document at the moment the
    // click event fires on the option.
    const minimaxOption = await screen.findByText('Minimax');
    expect(minimaxOption.isConnected).toBe(true);
    await userEvent.click(minimaxOption);

    // The resume call landed, so the click event reached its handler.
    // Issue #575 — composite id `claude:minimax`, not the legacy bare id.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('import_discovered_agent_node', expect.objectContaining({
        provider: 'claude:minimax',
      }));
    });
  });

  it('surfaces backend errors from discover_agent_nodes with the raw message', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'discover_agent_nodes') return Promise.reject(new Error('claude dir not found'));
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      return Promise.resolve({});
    });
    render(<ArchivedNodesTab />);

    expect(await screen.findByText('Failed to discover sessions')).toBeTruthy();
    expect(screen.getByText('claude dir not found')).toBeTruthy();
  });
});
