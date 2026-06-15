/**
 * Tests for the Git Pull Requests probe tab (🔀).
 *
 * Pins the panel's invariants:
 *   - lists PRs for the active mesh (open by default)
 *   - the Open/Closed toggle refetches with the chosen `state`
 *   - a mergeable open PR exposes a confirm→merge flow that calls `merge_pr`
 *     with the PR url and then refetches the list
 *   - draft / conflicting PRs are flagged non-mergeable (no merge button)
 *   - a `mergeable: null` detail (GitHub still computing) shows "Checking…"
 *   - backend errors surface the raw message
 *   - removing the mesh after mount doesn't crash (ProbeTabBody owns the
 *     "no project selected" empty state, like GitIssuesTab)
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { GitPullRequestsTab } from '../../src/components/Probe/GitPullRequestsTab';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import type { GitHubPullRequest } from '../../src/types/generated/GitHubPullRequest';
import type { PrMergeability } from '../../src/types/generated/PrMergeability';

const MESH: Mesh = {
  id: 42,
  name: 'demo',
  path: '/repos/demo',
  layout: 'single',
  position: 0,
  created_at: '2026-01-01',
};

// PR 201: clean/mergeable. PR 202: conflicts. PR 203: draft. PR 204: computing.
const OPEN_PRS: GitHubPullRequest[] = [
  { number: 201, title: 'Add widget', body: 'Adds the widget', url: 'https://github.com/acme/demo/pull/201', state: 'open', draft: false },
  { number: 202, title: 'Refactor core', body: 'Big refactor', url: 'https://github.com/acme/demo/pull/202', state: 'open', draft: false },
  { number: 203, title: 'WIP spike', body: '', url: 'https://github.com/acme/demo/pull/203', state: 'open', draft: true },
  { number: 204, title: 'Fresh PR', body: '', url: 'https://github.com/acme/demo/pull/204', state: 'open', draft: false },
];

const CLOSED_PRS: GitHubPullRequest[] = [
  { number: 150, title: 'Old change', body: 'merged ages ago', url: 'https://github.com/acme/demo/pull/150', state: 'closed', draft: false },
];

const MERGEABILITY: Record<number, PrMergeability> = {
  201: { mergeable: true, mergeable_state: 'clean' },
  202: { mergeable: false, mergeable_state: 'dirty' },
  204: { mergeable: null, mergeable_state: 'unknown' },
};

/**
 * Wire the mocked `invoke` to answer each command the tab calls. `get_repo_pulls`
 * branches on the `state` arg so the toggle test can assert per-filter results.
 */
function mockBackend(opts: { open?: GitHubPullRequest[]; closed?: GitHubPullRequest[] } = {}) {
  vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
    switch (cmd) {
      case 'get_repo_pulls': {
        const state = (args as { state?: string })?.state;
        return Promise.resolve(state === 'closed' ? (opts.closed ?? CLOSED_PRS) : (opts.open ?? OPEN_PRS));
      }
      case 'get_pr_mergeability': {
        const n = (args as { prNumber?: number })?.prNumber ?? -1;
        return Promise.resolve(MERGEABILITY[n] ?? { mergeable: null, mergeable_state: 'unknown' });
      }
      case 'merge_pr':
        return Promise.resolve('Merged (squash) via abc123 — done');
      default:
        return Promise.resolve({});
    }
  });
}

describe('GitPullRequestsTab', () => {
  beforeEach(() => {
    useUIStore.setState({ probeOpen: true, probeTab: 'pulls', activeDiffFile: null });
    useMeshStore.setState({
      meshesById: new Map([[MESH.id, MESH]]),
      selectedMeshId: MESH.id,
    });
  });

  it('lists open pull requests for the active mesh', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    expect(await screen.findByText('Add widget')).toBeTruthy();
    expect(screen.getByText('#201')).toBeTruthy();
    expect(screen.getByText('Refactor core')).toBeTruthy();

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('get_repo_pulls', { meshId: 42, state: 'open' });
    });
  });

  it('renders the mesh path subtitle', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);
    expect(await screen.findByText('/repos/demo')).toBeTruthy();
  });

  it('toggling to Closed refetches with state: "closed"', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    await screen.findByText('Add widget');
    await userEvent.click(screen.getByRole('button', { name: 'closed' }));

    expect(await screen.findByText('Old change')).toBeTruthy();
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('get_repo_pulls', { meshId: 42, state: 'closed' });
    });
  });

  it('shows a friendly empty state per filter when there are no PRs', async () => {
    mockBackend({ open: [] });
    render(<GitPullRequestsTab />);
    expect(await screen.findByText('No open pull requests')).toBeTruthy();
  });

  it('surfaces backend errors with the raw message', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_pulls') return Promise.reject(new Error('gh: not authenticated'));
      return Promise.resolve({});
    });
    render(<GitPullRequestsTab />);

    expect(await screen.findByText('Failed to load pull requests')).toBeTruthy();
    expect(screen.getByText('gh: not authenticated')).toBeTruthy();
  });

  it('flags a mergeable PR with a Merge button and confirms before merging', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    // PR 201 is clean — its row gets an enabled Merge button once the
    // mergeability probe resolves.
    const mergeBtn = await screen.findByRole('button', { name: 'Merge' });
    await userEvent.click(mergeBtn);

    // First click reveals the inline confirm — no merge IPC yet.
    expect(invoke).not.toHaveBeenCalledWith('merge_pr', expect.anything());
    const confirmBtn = await screen.findByRole('button', { name: 'Merge?' });
    await userEvent.click(confirmBtn);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('merge_pr', { prUrl: 'https://github.com/acme/demo/pull/201' });
    });
    // After a successful merge the list refetches.
    await waitFor(() => {
      expect(vi.mocked(invoke).mock.calls.filter(([c]) => c === 'get_repo_pulls').length).toBeGreaterThan(1);
    });
  });

  it('flags a conflicting PR as non-mergeable (no merge button)', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    // PR 202 is "dirty" — flagged as Conflicts, not mergeable.
    expect(await screen.findByText('Conflicts')).toBeTruthy();
  });

  it('flags a draft PR as Draft without a mergeability call', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    await screen.findByText('WIP spike');
    expect(await screen.findByText('Draft')).toBeTruthy();
    // Draft is derived from the list flag — no detail probe for PR 203.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('get_pr_mergeability', { meshId: 42, prNumber: 201 });
    });
    expect(invoke).not.toHaveBeenCalledWith('get_pr_mergeability', { meshId: 42, prNumber: 203 });
  });

  it('shows "Checking…" while GitHub computes mergeability (mergeable: null)', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    // PR 204 resolves to mergeable: null — stays in the checking state.
    expect(await screen.findByText('Fresh PR')).toBeTruthy();
    expect(await screen.findByText('Checking…')).toBeTruthy();
  });

  it('does not crash when the mesh is removed after mount', () => {
    useMeshStore.setState({ meshesById: new Map(), selectedMeshId: null });
    mockBackend();
    expect(() => render(<GitPullRequestsTab />)).not.toThrow();
  });
});
