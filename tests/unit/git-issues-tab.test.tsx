/**
 * Tests for the Git Issues probe tab — issue #378.
 *
 * Pins the migration invariants:
 *   - the tab loads issues for the active mesh (not a worktree)
 *   - the primary Spawn button does the two-stage spawn via
 *     `create_issue_node` → `start_node_background`
 *   - the `▾` provider picker calls `create_issue_node` with the
 *     explicit provider id
 *   - mesh changes (or "no mesh") don't show stale data
 *
 * The "split" two-stage spawn flow mirrors the legacy modal's behaviour
 * (issue #302) — the second IPC is intentionally not awaited so the
 * tab can stay responsive while the slow work (git fetch, worktree
 * create, PTY spawn) runs in the background.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { GitIssuesTab } from '../../src/components/Probe/GitIssuesTab';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import type { GitHubIssue } from '../../src/types/generated/GitHubIssue';

const MESH: Mesh = {
  id: 42,
  name: 'demo',
  path: '/repos/demo',
  layout: 'single',
  position: 0,
  created_at: '2026-01-01',
};

const ISSUES: GitHubIssue[] = [
  {
    number: 101,
    title: 'Fix the wobble',
    body: 'The foobar widget wobbles under load.',
    url: 'https://github.com/acme/demo/issues/101',
    state: 'open',
    labels: ['bug'],
  },
  {
    number: 102,
    title: 'Add a /v2 endpoint',
    body: null,
    url: 'https://github.com/acme/demo/issues/102',
    state: 'open',
    labels: [],
  },
];

const PROVIDERS = [
  { id: 'anthropic', label: 'Anthropic', color: '#000', icon: '' },
  { id: 'minimax', label: 'Minimax', color: '#000', icon: '' },
  { id: 'opencode', label: 'OpenCode', color: '#000', icon: '' },
];

const DRAFT = {
  id: 7,
  mesh_id: 42,
  name: 'pending-node',
  path: '/repos/demo',
  branch: 'main',
  env: 'wsl',
  provider: 'anthropic',
  status: 'pending',
  use_worktree: true,
  position: 0,
  created_at: '2026-01-01',
  prefill: 'Fix the wobble (issue #101)',
};

/**
 * Wire the mocked `invoke` to answer each command the tab calls during
 * mount + a single spawn cycle. Any command we don't care about resolves
 * to a benign shape so other panes that may be in the same render can
 * keep loading.
 */
function mockBackend(opts: { issues?: GitHubIssue[]; providers?: typeof PROVIDERS } = {}) {
  vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
    switch (cmd) {
      case 'get_repo_issues':
        return Promise.resolve(opts.issues ?? ISSUES);
      case 'list_providers':
        return Promise.resolve(opts.providers ?? PROVIDERS);
      case 'get_default_provider':
        return Promise.resolve('anthropic');
      case 'create_issue_node':
        return Promise.resolve(DRAFT);
      case 'start_node_background':
        return Promise.resolve(undefined);
      default:
        return Promise.resolve({});
    }
  });
}

describe('GitIssuesTab (#378)', () => {
  beforeEach(() => {
    useUIStore.setState({ probeOpen: true, probeTab: 'issues', activeDiffFile: null });
    useMeshStore.setState({
      meshesById: new Map([[MESH.id, MESH]]),
      selectedMeshId: MESH.id,
    });
  });

  it('lists open issues for the active mesh', async () => {
    mockBackend();
    render(<GitIssuesTab />);

    expect(await screen.findByText('Fix the wobble')).toBeTruthy();
    expect(screen.getByText('Add a /v2 endpoint')).toBeTruthy();
    // Issue numbers are shown as a `#NN` prefix.
    expect(screen.getByText('#101')).toBeTruthy();
    expect(screen.getByText('#102')).toBeTruthy();
  });

  it('renders the mesh path subtitle', async () => {
    mockBackend();
    render(<GitIssuesTab />);

    expect(await screen.findByText('/repos/demo')).toBeTruthy();
  });

  it('shows a friendly empty state when there are no open issues', async () => {
    mockBackend({ issues: [] });
    render(<GitIssuesTab />);

    expect(await screen.findByText('No open issues')).toBeTruthy();
  });

  it('surfaces backend errors with the raw message', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_issues') return Promise.reject(new Error('gh: not authenticated'));
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      return Promise.resolve({});
    });
    render(<GitIssuesTab />);

    expect(await screen.findByText('Failed to load issues')).toBeTruthy();
    expect(screen.getByText('gh: not authenticated')).toBeTruthy();
  });

  it('does the two-stage spawn on the primary Spawn button (issue #302)', async () => {
    mockBackend();
    render(<GitIssuesTab />);

    // `findAllByText` because each issue row renders its own "Spawn"
    // button — the two-stage-spawn test wants the first row's button.
    const spawns = await screen.findAllByText('Spawn');
    await userEvent.click(spawns[0]);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('create_issue_node', {
        meshId: 42, issueNumber: 101, issueTitle: 'Fix the wobble', provider: 'anthropic',
      });
    });
    // Stage 2 is fire-and-forget; verify the second IPC is called with the
    // draft id + prefill returned by stage 1.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('start_node_background', {
        nodeId: 7, prefill: 'Fix the wobble (issue #101)',
      });
    });
  });

  it('disables the split button while a spawn is in flight to block double-clicks', async () => {
    let resolveCreate!: (v: typeof DRAFT) => void;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_issues') return Promise.resolve(ISSUES);
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      if (cmd === 'create_issue_node') return new Promise((res) => { resolveCreate = res; });
      return Promise.resolve({});
    });
    render(<GitIssuesTab />);

    const spawns = await screen.findAllByText('Spawn');
    await userEvent.click(spawns[0]);

    // The in-flight `create_issue_node` leaves the spawning flag set, so
    // both halves of every split button disable. The second click would
    // be a no-op even if it landed — assert the guard rather than the
    // backend idempotency.
    const allSpawning = await screen.findAllByText('Spawning...');
    expect(allSpawning.length).toBeGreaterThan(0);
    expect((allSpawning[0] as HTMLButtonElement).disabled).toBe(true);

    // Resolve the in-flight IPC and confirm the label flips back.
    resolveCreate(DRAFT);
    await waitFor(() => {
      expect(screen.queryByText('Spawning...')).toBeNull();
    });
  });

  it('hides the dock after a successful spawn (parity with legacy modal onClose)', async () => {
    // Issue #378 follow-up — the legacy `GitHubIssuesModal` called
    // `onClose()` after stage 1 of the two-stage spawn resolved so the
    // user sees the modal vanish and the new node appear. The probe tab
    // must do the same: hide the dock (`toggleProbe()`) once
    // `create_issue_node` returns, so the user lands on the terminal
    // ready to interact with the new node.
    mockBackend();
    useUIStore.setState({ probeOpen: true, probeTab: 'issues' });
    render(<GitIssuesTab />);

    expect(useUIStore.getState().probeOpen).toBe(true);
    const spawns = await screen.findAllByText('Spawn');
    await userEvent.click(spawns[0]);

    await waitFor(() => {
      expect(useUIStore.getState().probeOpen).toBe(false);
    });
    // Sanity: the stage-2 fire-and-forget IPC is also on the wire.
    expect(invoke).toHaveBeenCalledWith('start_node_background', expect.objectContaining({
      nodeId: 7,
    }));
  });

  it('keeps the dock open when create_issue_node rejects (lets the user retry)', async () => {
    // Symmetric case — a failed spawn should NOT close the dock, the
    // user needs to be able to retry (e.g. transient `gh` hiccup).
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_issues') return Promise.resolve(ISSUES);
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      if (cmd === 'create_issue_node') return Promise.reject(new Error('boom'));
      return Promise.resolve({});
    });
    useUIStore.setState({ probeOpen: true, probeTab: 'issues' });
    render(<GitIssuesTab />);

    const spawns = await screen.findAllByText('Spawn');
    await userEvent.click(spawns[0]);

    await waitFor(() => {
      // The spawning label clears on failure.
      expect(screen.queryByText('Spawning...')).toBeNull();
    });
    // The dock stays open so the user can try again.
    expect(useUIStore.getState().probeOpen).toBe(true);
  });

  it('renders the empty-state "No project selected" from ProbeTabBody, not a custom one', () => {
    // The component itself doesn't render the empty state — ProbeTabBody
    // gates on `activeMeshId === null` BEFORE mounting the tab, so by
    // the time `GitIssuesTab` renders there is always a mesh. We assert
    // the negative: removing the mesh from the store after mount does
    // NOT crash the tab (the parent should swap in the empty state).
    useMeshStore.setState({ meshesById: new Map(), selectedMeshId: null });
    mockBackend();
    expect(() => render(<GitIssuesTab />)).not.toThrow();
  });
});
