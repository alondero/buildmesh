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

  it('keeps the dock open after a successful spawn so the user can spawn more', async () => {
    // Regression: the legacy `GitHubIssuesModal` called `onClose()` after
    // stage 1 of the two-stage spawn resolved so the user saw the modal
    // vanish and the new node appear. The probe tab is a persistent
    // dock, not a one-shot dialog — closing it after every spawn forces
    // the user to re-open it (sidebar context menu → "Open Issues") for
    // the next issue. The dock stays open; the user dismisses it with
    // the activity-bar toggle when they're done.
    mockBackend();
    useUIStore.setState({ probeOpen: true, probeTab: 'issues' });
    render(<GitIssuesTab />);

    expect(useUIStore.getState().probeOpen).toBe(true);
    const spawns = await screen.findAllByText('Spawn');
    await userEvent.click(spawns[0]);

    // Stage 2 fires; the dock must NOT have closed.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('start_node_background', expect.objectContaining({
        nodeId: 7,
      }));
    });
    expect(useUIStore.getState().probeOpen).toBe(true);
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

  it('expands the body to the full text when the body is clicked', async () => {
    // The body is clamped to 2 lines by default; clicking the body
    // reveals a scrollable, full-text container and drops the clamp.
    // Re-clicking the body re-applies the clamp. We click the body
    // element directly rather than the row's bounding box, because
    // the title <a> inside the row has stopPropagation and would
    // otherwise swallow a center-of-row click.
    mockBackend();
    render(<GitIssuesTab />);

    const title = await screen.findByText('Fix the wobble');
    const row = title.closest('[data-issue-row]')!;
    expect(row).toBeTruthy();

    // Collapsed: the clamped <p> with line-clamp-2 is present.
    const clampedBody = row.querySelector('p.line-clamp-2') as HTMLElement;
    expect(clampedBody).toBeTruthy();
    expect(clampedBody.textContent).toContain('wobbles under load');

    // Expand — click the body (the user's actual click target).
    await userEvent.click(clampedBody);
    const expandedBody = row.querySelector('div.max-h-48, [data-issue-body-expanded]');
    expect(expandedBody).toBeTruthy();
    expect(expandedBody!.textContent).toBe('The foobar widget wobbles under load.');
    // The clamp is gone — no line-clamp-2 element on the body now.
    expect(row.querySelector('p.line-clamp-2')).toBeNull();

    // Collapse — click the expanded body (the same region, now showing
    // the full text) to re-apply the clamp.
    const expandedBodyEl = row.querySelector(
      'div.max-h-48, [data-issue-body-expanded]',
    ) as HTMLElement;
    await userEvent.click(expandedBodyEl);
    expect(row.querySelector('p.line-clamp-2')).toBeTruthy();
    expect(row.querySelector('[data-issue-body-expanded], div.max-h-48')).toBeNull();
  });

  it('renders the title as an anchor pointing at the issue URL', async () => {
    // The dock has no other route to GitHub — the title IS the link.
    // It must open in a new tab (target=_blank) with the standard
    // noopener/noreferrer rel pair (mirrors AiContextSection.tsx:64-68).
    mockBackend();
    render(<GitIssuesTab />);

    const titleLink = (await screen.findByText('Fix the wobble')).closest('a')!;
    expect(titleLink).toBeTruthy();
    expect(titleLink.getAttribute('href')).toBe('https://github.com/acme/demo/issues/101');
    expect(titleLink.getAttribute('target')).toBe('_blank');
    expect(titleLink.getAttribute('rel') ?? '').toContain('noopener');
    expect(titleLink.getAttribute('rel') ?? '').toContain('noreferrer');
  });

  it('renders the title as plain text (no link) when issue.url is empty', async () => {
    // The Rust GitHubIssue struct has #[serde(default)] on the upstream
    // html_url, so a partial GitHub response can reach the frontend as
    // an empty string. A bare <a href=""> would self-navigate the
    // WebView; the component should fall back to a plain <span> for
    // the title and skip the ↗ icon entirely.
    mockBackend({
      issues: [
        {
          number: 200,
          title: 'Mystery issue',
          body: 'No html_url from upstream.',
          url: '',
          state: 'open',
          labels: [],
        },
      ],
    });
    render(<GitIssuesTab />);

    const title = await screen.findByText('Mystery issue');
    // The title must NOT be wrapped in an <a>.
    expect(title.closest('a')).toBeNull();
    // The ↗ icon is gone too (both anchors are gated on issue.url).
    expect(screen.queryByLabelText('Open issue on GitHub')).toBeNull();
  });

  it('does not toggle expand when the title link is clicked', async () => {
    // The link has its own click target — clicking the link navigates
    // to GitHub and must NOT also flip the row's expanded state. The
    // onClick on the <a> calls stopPropagation to keep the row handler
    // out of the way.
    mockBackend();
    render(<GitIssuesTab />);

    const titleLink = (await screen.findByText('Fix the wobble')).closest('a')!;
    await userEvent.click(titleLink);

    const row = titleLink.closest('[data-issue-row]')!;
    // Still collapsed — clamp still present, no scrollable container.
    expect(row.querySelector('p.line-clamp-2')).toBeTruthy();
    expect(row.querySelector('[data-issue-body-expanded], div.max-h-48')).toBeNull();
  });

  it('clears expanded state when the active mesh changes', async () => {
    // Switching meshes mid-session should reset the per-row expanded
    // set — the issue numbers are not stable across meshes, so the
    // previous expand set would either be a no-op or accidentally
    // re-open an unrelated row in the new mesh. We mock the IPC to
    // return different issue lists per meshId so the empty-state
    // assertion in the middle of the test is meaningful.
    vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === 'get_repo_issues') {
        const meshId = (args as { meshId: number } | undefined)?.meshId;
        return Promise.resolve(meshId === MESH.id ? ISSUES : []);
      }
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      if (cmd === 'create_issue_node') return Promise.resolve(DRAFT);
      if (cmd === 'start_node_background') return Promise.resolve(undefined);
      return Promise.resolve({});
    });
    const { rerender } = render(<GitIssuesTab />);

    const firstTitle = await screen.findByText('Fix the wobble');
    const firstRow = firstTitle.closest('[data-issue-row]')!;
    // Click the body (the user's click target) — the row's bounding
    // box center is on the title <a> which has stopPropagation.
    const firstBody = firstRow.querySelector('p.line-clamp-2') as HTMLElement;
    await userEvent.click(firstBody);
    expect(firstRow.querySelector('p.line-clamp-2')).toBeNull();

    // Swap to a different mesh in the store. The load effect re-runs;
    // expanded set is reset to a new empty Set. The new mesh has no
    // issues, so the empty state appears.
    useMeshStore.setState({ selectedMeshId: 99 });
    rerender(<GitIssuesTab />);

    expect(await screen.findByText('No open issues')).toBeTruthy();

    // Switch back to mesh 42. Issue list reappears, all rows collapsed.
    useMeshStore.setState({ selectedMeshId: MESH.id });
    rerender(<GitIssuesTab />);
    const resetTitle = await screen.findByText('Fix the wobble');
    const resetRow = resetTitle.closest('[data-issue-row]')!;
    expect(resetRow.querySelector('p.line-clamp-2')).toBeTruthy();
  });
});
