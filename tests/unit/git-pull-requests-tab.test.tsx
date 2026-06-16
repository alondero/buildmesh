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
 *     and the panel re-polls the detail endpoint until GitHub settles
 *     (issue #419 — was previously stuck on "Checking…" forever)
 *   - backend errors surface the raw message
 *   - removing the mesh after mount doesn't crash (ProbeTabBody owns the
 *     "no project selected" empty state, like GitIssuesTab)
 *   - open PRs expose a split Spawn button (issue #420) that calls
 *     `create_pr_node` → `start_node_background` mirroring the issue-spawn
 *     flow; dock stays open across the spawn; rejected spawns surface
 *     inline and leave the dock open for retry
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, waitFor, cleanup, act } from '@testing-library/react';
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
// `head_ref` populated on every fixture PR — the spawn flow (issue #420)
// requires it: it's the ref `create_pr_node` stores on the new node's
// `branch` column, and stage-2 fetches `origin/<head_ref>` to cut the
// worktree from it. PRs without a head ref are fork PRs and the spawn flow
// refuses them.
const OPEN_PRS: GitHubPullRequest[] = [
  { number: 201, title: 'Add widget', body: 'Adds the widget', url: 'https://github.com/acme/demo/pull/201', state: 'open', draft: false, head_ref: 'feat/201-add-widget' },
  { number: 202, title: 'Refactor core', body: 'Big refactor', url: 'https://github.com/acme/demo/pull/202', state: 'open', draft: false, head_ref: 'refactor/202-core' },
  { number: 203, title: 'WIP spike', body: '', url: 'https://github.com/acme/demo/pull/203', state: 'open', draft: true, head_ref: 'wip/203-spike' },
  { number: 204, title: 'Fresh PR', body: '', url: 'https://github.com/acme/demo/pull/204', state: 'open', draft: false, head_ref: 'fresh/204-pr' },
];

const CLOSED_PRS: GitHubPullRequest[] = [
  { number: 150, title: 'Old change', body: 'merged ages ago', url: 'https://github.com/acme/demo/pull/150', state: 'closed', draft: false, head_ref: 'old/150-change' },
];

const MERGEABILITY: Record<number, PrMergeability> = {
  201: { mergeable: true, mergeable_state: 'clean' },
  202: { mergeable: false, mergeable_state: 'dirty' },
  204: { mergeable: null, mergeable_state: 'unknown' },
};

const PROVIDERS = [
  { id: 'anthropic', label: 'Anthropic', color: '#000', icon: '' },
  { id: 'minimax', label: 'Minimax', color: '#000', icon: '' },
  { id: 'opencode', label: 'OpenCode', color: '#000', icon: '' },
];

// Stage-1 return value — the same shape `create_issue_node` returns, so
// `create_pr_node` reuses the generated `IssueNodeDraft` TS type. The
// `branch` field carries the PR's head ref (set server-side from the
// `head_ref` arg) and `source_pr` records the originating PR number
// (issue #420) so stage-2's `spawn_agent_inner` can override the
// worktree's `base_ref` to `origin/<head_ref>`.
const PR_DRAFT = {
  id: 17,
  mesh_id: 42,
  name: 'pr201-add-widget',
  path: '/repos/demo',
  branch: 'feat/201-add-widget',
  env: 'wsl',
  provider: 'anthropic',
  status: 'pending',
  use_worktree: true,
  source_issue: null,
  source_pr: 201,
  position: 0,
  created_at: '2026-01-01',
  prefill: 'Please review pull request #201 — Add widget\nhttps://github.com/acme/demo/pull/201',
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
      case 'list_providers':
        return Promise.resolve(PROVIDERS);
      case 'get_default_provider':
        return Promise.resolve('anthropic');
      case 'create_pr_node':
        return Promise.resolve(PR_DRAFT);
      case 'merge_pr':
        return Promise.resolve('Merged (squash) via abc123 — done');
      case 'start_node_background':
        return Promise.resolve(undefined);
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

  // RTL doesn't auto-unmount between tests in this vitest setup, so the
  // previous render's DOM would still be in the document — `getByText`
  // then throws on multiple matches. Cleanup after each test.
  afterEach(() => {
    cleanup();
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

    // PR 204 resolves to mergeable: null — stays in the checking state
    // until the bounded re-poll below settles it.
    expect(await screen.findByText('Fresh PR')).toBeTruthy();
    expect(await screen.findByText('Checking…')).toBeTruthy();
  });

  /// Regression for issue #419: when GitHub's detail endpoint returns
  /// `mergeable: null` (still computing), the panel must re-poll the
  /// detail endpoint — not leave the row stuck on "Checking…" forever.
  /// We use fake timers so the retry `setTimeout` is deterministic. With
  /// fake timers on, RTL's async finders (which use setTimeout polling)
  /// can't advance, so we wrap render + timer advances in `act(async …)` —
  /// that flushes React's state updates AND lets vitest's microtask
  /// runner drain the Promise resolutions triggered by the fake clock.
  it('re-polls mergeability when the first response is mergeable: null', async () => {
    vi.useFakeTimers();
    try {
      // PR 204's first probe returns null; the second returns a real value.
      // The rest of the PRs use the standard fixture map.
      const calls204: number[] = [];
      vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
        switch (cmd) {
          case 'get_repo_pulls':
            return Promise.resolve(OPEN_PRS);
          case 'get_pr_mergeability': {
            const n = (args as { prNumber?: number })?.prNumber ?? -1;
            if (n === 204) {
              calls204.push(calls204.length + 1);
              return Promise.resolve(
                calls204.length === 1
                  ? { mergeable: null, mergeable_state: 'unknown' }
                  : { mergeable: true, mergeable_state: 'clean' },
              );
            }
            return Promise.resolve(MERGEABILITY[n] ?? { mergeable: null, mergeable_state: 'unknown' });
          }
          case 'merge_pr':
            return Promise.resolve('Merged (squash) via abc123 — done');
          default:
            return Promise.resolve({});
        }
      });

      // Initial render + initial probe promise resolution.
      await act(async () => {
        render(<GitPullRequestsTab />);
        await vi.advanceTimersByTimeAsync(0);
      });

      // PR 204 is "Checking…" because mergeable is null. (Use getAllByText
      // so the test stays robust if more than one row is in the checking
      // state from the fixture map.)
      expect(screen.getAllByText('Checking…').length).toBeGreaterThanOrEqual(1);
      expect(calls204).toHaveLength(1);

      // Advance past the first retry delay (1.5s base × 1 attempt = 1.5s).
      // The timer's callback re-issues the probe; the second response is
      // mergeable: true, so the row flips to a Merge button.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(1500);
      });

      // PR 201 (clean) and PR 204 (now mergeable on retry) both render
      // Merge buttons. Assert the row is no longer in the checking state.
      const mergeButtons = screen.getAllByRole('button', { name: 'Merge' });
      expect(mergeButtons.length).toBeGreaterThanOrEqual(2);
      expect(screen.queryByText('Checking…')).toBeNull();
      expect(calls204.length).toBeGreaterThanOrEqual(2);
    } finally {
      vi.useRealTimers();
    }
  });

  /// Counterpart to the re-poll test: when the effect tears down (unmount,
  /// filter toggle, or mesh change — all share the same cleanup), pending
  /// retry timers must be cleared. Unmount is the most direct test: cleanup
  /// unmounts the component, no new renders happen, so any further probe call
  /// would be a leaked timer. The mock tracks call count for PR 204 so a
  /// leaked retry shows up as a second call.
  it('cancels pending mergeability retries when the component unmounts', async () => {
    vi.useFakeTimers();
    try {
      const calls204: number[] = [];
      vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
        switch (cmd) {
          case 'get_repo_pulls':
            return Promise.resolve(OPEN_PRS);
          case 'get_pr_mergeability': {
            const n = (args as { prNumber?: number })?.prNumber ?? -1;
            if (n === 204) {
              calls204.push(n);
              // First call: null (schedules retry). Second call: would
              // return a real value — if cleanup doesn't clear the timer,
              // the retry would fire and we'd see length 2.
              return Promise.resolve(
                calls204.length === 1
                  ? { mergeable: null, mergeable_state: 'unknown' }
                  : { mergeable: true, mergeable_state: 'clean' },
              );
            }
            return Promise.resolve({ mergeable: null, mergeable_state: 'unknown' });
          }
          default:
            return Promise.resolve({});
        }
      });

      // Initial render + initial probe promise resolution.
      await act(async () => {
        render(<GitPullRequestsTab />);
        await vi.advanceTimersByTimeAsync(0);
      });
      // Initial probe landed; retry is now scheduled for PR 204.
      expect(calls204).toHaveLength(1);

      // Unmount BEFORE the retry timer fires. The effect's cleanup must
      // clear the pending timer — no second call.
      await act(async () => {
        cleanup();
        await vi.advanceTimersByTimeAsync(2000);
      });

      // The retry was cancelled by the cleanup: call count is still 1.
      expect(calls204).toHaveLength(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('does not crash when the mesh is removed after mount', () => {
    useMeshStore.setState({ meshesById: new Map(), selectedMeshId: null });
    mockBackend();
    expect(() => render(<GitPullRequestsTab />)).not.toThrow();
  });

  // ----- "View changes" button (issue #421) ------------------------------
  // Each row now exposes a read-only button that opens the PR's diff in the
  // Center Workspace Diff Overlay. Unlike the merge control it's available
  // for both open AND closed PRs, and regardless of mergeability — the diff
  // is useful in every state (reviewing a merged change, looking at why a
  // PR was closed, etc.).

  it('renders a "View changes" button on every PR row', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    // One per open PR. Counts are stable regardless of mergeability / draft
    // state — only the merge control depends on those.
    const buttons = await screen.findAllByRole('button', { name: /view changes in pr #/i });
    expect(buttons).toHaveLength(OPEN_PRS.length);
  });

  it('opens the center diff overlay with a PR-source context when View changes is clicked', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    // Wait for the list to render, then click the View changes button on
    // PR 201 ("Add widget"). We identify it by its PR-number-specific
    // accessible name.
    await screen.findByText('Add widget');
    await userEvent.click(
      screen.getByRole('button', { name: 'View changes in PR #201' }),
    );

    // The overlay's DiffContext should pin this PR via source: 'pr' and
    // `filePath: ''` (list mode — click a file in the overlay to drill in).
    // `nodeId: null` because the PR's source branch may not exist locally.
    expect(useUIStore.getState().activeDiffFile).toEqual({
      filePath: '',
      rootPath: MESH.path,
      nodeId: null,
      meshId: MESH.id,
      source: 'pr',
      prNumber: 201,
    });
  });

  it('shows the View changes button for closed PRs too', async () => {
    // Same affordance on the Closed tab — reading a merged PR's diff is a
    // common postmortem / review flow, and we want the panel to feel
    // symmetric across the two filters.
    mockBackend();
    render(<GitPullRequestsTab />);

    await screen.findByText('Add widget');
    await userEvent.click(screen.getByRole('button', { name: 'closed' }));

    expect(
      await screen.findByRole('button', { name: 'View changes in PR #150' }),
    ).toBeTruthy();
  });

  // ----- Spawn button (issue #420) ---------------------------------------
  // Open PRs expose a split spawn button (default provider + ▾ picker) that
  // mirrors `GitIssuesTab`'s two-stage issue-spawn flow:
  //   1. `create_pr_node` — fast DB-only IPC (~20ms) returns a `pending`
  //      node with `branch = head_ref` and `source_pr = Some(n)`, plus the
  //      prefill string the caller must hand to stage-2.
  //   2. `start_node_background` — fire-and-forget; `spawn_agent_inner`
  //      does the slow work (git fetch origin <head_ref>, worktree create
  //      off the head ref, PTY spawn).
  // The dock stays open after a successful spawn so the user can fire off
  // another PR without re-opening the context menu — same UX contract as
  // the issue tab (memory buildmesh-spawn-from-probe-keeps-dock-open).

  it('does the two-stage spawn on the primary Spawn button (issue #420)', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    // findAllByText because every open PR row renders its own "Spawn"
    // button — the first row (PR 201) is what the test exercises.
    const spawns = await screen.findAllByText('Spawn');
    await userEvent.click(spawns[0]);

    // Stage 1 — `create_pr_node` carries the head ref from the fixture,
    // plus the resolved default provider from `get_default_provider`.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('create_pr_node', {
        meshId: 42,
        prNumber: 201,
        prTitle: 'Add widget',
        headRef: 'feat/201-add-widget',
        provider: 'anthropic',
      });
    });
    // Stage 2 — fire-and-forget IPC with the draft id + prefill from stage 1.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('start_node_background', {
        nodeId: 17,
        prefill: 'Please review pull request #201 — Add widget\nhttps://github.com/acme/demo/pull/201',
      });
    });
  });

  it('keeps the dock open after a successful spawn (mirrors issue-tab contract)', async () => {
    // The PR tab is a persistent dock like the issue tab. Closing on every
    // spawn would force the user to re-open the dock for the next PR.
    mockBackend();
    useUIStore.setState({ probeOpen: true, probeTab: 'pulls' });
    render(<GitPullRequestsTab />);

    const spawns = await screen.findAllByText('Spawn');
    await userEvent.click(spawns[0]);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('start_node_background', expect.objectContaining({ nodeId: 17 }));
    });
    expect(useUIStore.getState().probeOpen).toBe(true);
  });

  it('keeps the dock open when create_pr_node rejects (lets the user retry)', async () => {
    // Symmetric to the issue tab — a failed spawn should NOT close the
    // dock, the user needs to be able to retry (e.g. transient `gh` hiccup,
    // a fork PR that the backend refuses, etc.). The error surfaces inline
    // on the row, the spawning label clears.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_pulls') return Promise.resolve(OPEN_PRS);
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      if (cmd === 'create_pr_node') return Promise.reject(new Error("This PR's head branch is on a fork — worktree adoption for fork PRs isn't supported yet"));
      return Promise.resolve({});
    });
    useUIStore.setState({ probeOpen: true, probeTab: 'pulls' });
    render(<GitPullRequestsTab />);

    const spawns = await screen.findAllByText('Spawn');
    await userEvent.click(spawns[0]);

    // The spawning label clears once the rejected promise resolves.
    await waitFor(() => {
      expect(screen.queryByText('Spawning...')).toBeNull();
    });
    // The error message surfaces inline on the row.
    expect(
      await screen.findByText(/This PR's head branch is on a fork/),
    ).toBeTruthy();
    // The dock stays open so the user can try a different PR.
    expect(useUIStore.getState().probeOpen).toBe(true);
  });

  it('disables the split button while a spawn is in flight to block double-clicks', async () => {
    let resolveCreate!: (v: typeof PR_DRAFT) => void;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_pulls') return Promise.resolve(OPEN_PRS);
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      if (cmd === 'create_pr_node') return new Promise((res) => { resolveCreate = res; });
      return Promise.resolve({});
    });
    render(<GitPullRequestsTab />);

    const spawns = await screen.findAllByText('Spawn');
    await userEvent.click(spawns[0]);

    // The in-flight `create_pr_node` leaves the spawning flag set, so both
    // halves of every split button disable. A second click would be a
    // no-op even if it landed — assert the guard rather than the backend
    // idempotency.
    const allSpawning = await screen.findAllByText('Spawning...');
    expect(allSpawning.length).toBeGreaterThan(0);
    expect((allSpawning[0] as HTMLButtonElement).disabled).toBe(true);

    // Resolve the in-flight IPC and confirm the label flips back.
    resolveCreate(PR_DRAFT);
    await waitFor(() => {
      expect(screen.queryByText('Spawning...')).toBeNull();
    });
  });
});
