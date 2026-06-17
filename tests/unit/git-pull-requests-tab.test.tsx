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

// `@tauri-apps/plugin-opener`'s `openUrl` shells out to the OS to open an
// external URL. Tauri 2's WebView silently drops `target="_blank"` without
// the `core:webview:allow-create-webview-window` capability (which we don't
// grant), so the link's `onClick` must route through `openUrl` instead.
// `vi.hoisted` so the mock factory can capture the spy ref before the
// `vi.mock` call hoists the module replacement.
const { openUrlMock } = vi.hoisted(() => ({
  openUrlMock: vi.fn<[], Promise<void>>().mockResolvedValue(undefined),
}));
vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: openUrlMock,
}));

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
  { number: 201, title: 'Add widget', body: 'Adds the widget', url: 'https://github.com/acme/demo/pull/201', state: 'open', draft: false, head_ref: 'feat/201-add-widget', head_sha: 'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1' },
  { number: 202, title: 'Refactor core', body: 'Big refactor', url: 'https://github.com/acme/demo/pull/202', state: 'open', draft: false, head_ref: 'refactor/202-core', head_sha: 'b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2' },
  { number: 203, title: 'WIP spike', body: '', url: 'https://github.com/acme/demo/pull/203', state: 'open', draft: true, head_ref: 'wip/203-spike', head_sha: 'c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3' },
  { number: 204, title: 'Fresh PR', body: '', url: 'https://github.com/acme/demo/pull/204', state: 'open', draft: false, head_ref: 'fresh/204-pr', head_sha: 'd4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4' },
];

const CLOSED_PRS: GitHubPullRequest[] = [
  { number: 150, title: 'Old change', body: 'merged ages ago', url: 'https://github.com/acme/demo/pull/150', state: 'closed', draft: false, head_ref: 'old/150-change', head_sha: 'e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5' },
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
  // Mirrors the Rust `source_pr_pinned_sha` (issue #444) — the backend
  // stores the PR's head SHA here for the exact-pinning drift check.
  // OPEN_PRS[0] (PR 201) has head_sha 'a1a1...a1', so the persisted
  // value is the same. Without this field, future tests reading
  // `draft.node.source_pr_pinned_sha` would see `undefined` at runtime
  // (the fixture is inferred, no TS type to catch the drift).
  source_pr_pinned_sha: 'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1',
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
    // `mockReset` (not `mockClear`) wipes both call history AND any
    // per-test `mockImplementationOnce` / `mockRejectedValueOnce`
    // overrides — guards against a future test that adds a one-off
    // override and accidentally leaks the override into siblings.
    // Re-establish the default resolved value after the reset.
    openUrlMock.mockReset();
    openUrlMock.mockResolvedValue(undefined);
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
    // mergeability probe resolves. The button is now icon-only (git-merge
    // SVG) so the accessible name comes from the aria-label.
    const mergeBtn = await screen.findByRole('button', { name: 'Merge pull request #201' });
    await userEvent.click(mergeBtn);

    // First click reveals the inline confirm — no merge IPC yet.
    expect(invoke).not.toHaveBeenCalledWith('merge_pr', expect.anything());
    const confirmBtn = await screen.findByRole('button', { name: /confirm squash merge/i });
    await userEvent.click(confirmBtn);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('merge_pr', { prUrl: 'https://github.com/acme/demo/pull/201' });
    });
    // After a successful merge the list refetches.
    await waitFor(() => {
      expect(vi.mocked(invoke).mock.calls.filter(([c]) => c === 'get_repo_pulls').length).toBeGreaterThan(1);
    });
  });

  /// The confirm step now exposes a Cancel button (icon-only) so the user
  /// can back out of an accidental click. It must dismiss the confirm state
  /// without firing `merge_pr`. Pin both the label and the cancel behavior
  /// so a future regression that hides the cancel button (or wires the
  /// wrong handler) is caught here.
  it('exposes a Cancel button in the confirm state that dismisses without merging', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    const mergeBtn = await screen.findByRole('button', { name: 'Merge pull request #201' });
    await userEvent.click(mergeBtn);

    const cancelBtn = await screen.findByRole('button', { name: /cancel merge/i });
    await userEvent.click(cancelBtn);

    // No merge IPC after cancellation, and the original Merge button
    // re-appears (confirm state cleared).
    expect(invoke).not.toHaveBeenCalledWith('merge_pr', expect.anything());
    expect(await screen.findByRole('button', { name: 'Merge pull request #201' })).toBeTruthy();
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
      // The accessible name now embeds the PR number (icon-only button).
      const mergeButtons = screen.getAllByRole('button', { name: /merge pull request #/i });
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
    // plus the resolved default provider from `get_default_provider`. The
    // `headSha` (issue #444) is the exact-pinning handle: the backend
    // persists it as `source_pr_pinned_sha` and verifies the local
    // `origin/<head_ref>` SHA matches it after `git fetch`, emitting a
    // non-fatal `pr_sha_drift` `mesh-sync-warning` on mismatch.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('create_pr_node', {
        meshId: 42,
        prNumber: 201,
        prTitle: 'Add widget',
        headRef: 'feat/201-add-widget',
        headSha: 'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1',
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

  // Issue #444 — exact-pinning. `create_pr_node` MUST receive the PR's head
  // commit SHA on every spawn path (primary + provider-picker) so the backend
  // can persist it as `source_pr_pinned_sha` and verify the local
  // `origin/<head_ref>` SHA matches after `git fetch`. An empty `head_sha`
  // (partial GitHub response) is passed through unchanged — the backend
  // treats it as "skip the drift check" (same fail-open semantics as the
  // existing `pr_head_unfetchable` fallback).
  it('plumbs head_sha through to create_pr_node (issue #444 exact-pinning)', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    const spawns = await screen.findAllByText('Spawn');
    await userEvent.click(spawns[0]);

    await waitFor(() => {
      // PR 201's fixture has head_sha 'a1a1...a1' (40 hex chars). The
      // matching `headSha` arg MUST be present on the create_pr_node call
      // — without it the backend can't pin the worktree to the exact commit
      // and the drift check is skipped (silent UX regression).
      expect(invoke).toHaveBeenCalledWith(
        'create_pr_node',
        expect.objectContaining({
          prNumber: 201,
          headRef: 'feat/201-add-widget',
          headSha: 'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1',
        }),
      );
    });
  });

  it('passes an empty head_sha when the GitHub response omits it (fail-open)', async () => {
    // Real-world case: the `/pulls` list endpoint occasionally returns a
    // partial response (older caches, rate-limited retries) where `head.sha`
    // is missing. `GitHubPullRequest.head_sha` defaults to "" via
    // `#[serde(default)]` on the Rust struct (and the same default in the
    // generated TS type), so the frontend passes "" through to the backend.
    // The backend treats "" as "skip the drift check" — same fail-open
    // semantics as `pr_head_unfetchable` — and the worktree proceeds on
    // whatever `origin/<head_ref>` is currently at.
    mockBackend();
    // Override the PR list to omit head_sha for PR 201 (the first row).
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_pulls') {
        return Promise.resolve(OPEN_PRS.map((pr) =>
          pr.number === 201 ? { ...pr, head_sha: '' } : pr,
        ));
      }
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      if (cmd === 'create_pr_node') return Promise.resolve(PR_DRAFT);
      if (cmd === 'start_node_background') return Promise.resolve(undefined);
      return Promise.resolve({});
    });
    render(<GitPullRequestsTab />);

    const spawns = await screen.findAllByText('Spawn');
    await userEvent.click(spawns[0]);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        'create_pr_node',
        expect.objectContaining({
          prNumber: 201,
          headRef: 'feat/201-add-widget',
          headSha: '', // empty — backend skips drift check
        }),
      );
    });
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

  // ----- Expand body + title link (issue #461) --------------------------

  // Per memory feedback-probe-tab-test-and-jsdoc-gotchas §4: row's
  // bounding-box center is on the title <a> (stopPropagation), so
  // click the body / link directly, not the row.
  it('expands the body to the full text when the body is clicked', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    const title = await screen.findByText('Add widget');
    const row = title.closest('[data-pr-row]')!;
    expect(row).toBeTruthy();

    const clampedBody = row.querySelector('p.line-clamp-2') as HTMLElement;
    expect(clampedBody).toBeTruthy();
    expect(clampedBody.textContent).toContain('Adds the widget');

    await userEvent.click(clampedBody);
    const expandedBody = row.querySelector('div.max-h-48, [data-pr-body-expanded]');
    expect(expandedBody).toBeTruthy();
    expect(expandedBody!.textContent).toBe('Adds the widget');
    expect(row.querySelector('p.line-clamp-2')).toBeNull();

    const expandedBodyEl = row.querySelector(
      'div.max-h-48, [data-pr-body-expanded]',
    ) as HTMLElement;
    await userEvent.click(expandedBodyEl);
    expect(row.querySelector('p.line-clamp-2')).toBeTruthy();
    expect(row.querySelector('[data-pr-body-expanded], div.max-h-48')).toBeNull();
  });

  it('renders the title as an anchor pointing at the PR URL', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    const titleLink = (await screen.findByText('Add widget')).closest('a')!;
    expect(titleLink).toBeTruthy();
    expect(titleLink.getAttribute('href')).toBe('https://github.com/acme/demo/pull/201');
    expect(titleLink.getAttribute('target')).toBe('_blank');
    expect(titleLink.getAttribute('rel') ?? '').toContain('noopener');
    expect(titleLink.getAttribute('rel') ?? '').toContain('noreferrer');
  });

  it('renders the title as plain text (no link) when pr.url is empty', async () => {
    // Defensive: PullRequest.html_url has no #[serde(default)] today,
    // so the empty case is theoretical. The pin prevents a future
    // widening from regressing to a bare <a href="">.
    mockBackend({
      open: [
        {
          number: 999,
          title: 'Mystery PR',
          body: 'No html_url from upstream.',
          url: '',
          state: 'open',
          draft: false,
          head_ref: 'mystery/999',
          head_sha: 'f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9',
        },
      ],
    });
    render(<GitPullRequestsTab />);

    const title = await screen.findByText('Mystery PR');
    expect(title.closest('a')).toBeNull();
    expect(screen.queryByLabelText(/open pull request on github/i)).toBeNull();
  });

  it('does not toggle expand when the title link is clicked', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    const titleLink = (await screen.findByText('Add widget')).closest('a')!;
    await userEvent.click(titleLink);

    const row = titleLink.closest('[data-pr-row]')!;
    expect(row.querySelector('p.line-clamp-2')).toBeTruthy();
    expect(row.querySelector('[data-pr-body-expanded], div.max-h-48')).toBeNull();
  });

  // ----- Tauri 2 WebView external-link routing ---------------------------
  // Tauri 2's WebView is NOT a browser. `target="_blank"` is silently
  // dropped without the `core:webview:allow-create-webview-window`
  // capability (which we don't grant in `capabilities/default.json`),
  // so the link's onClick must call `openUrl` from
  // `@tauri-apps/plugin-opener` to delegate to the OS. These tests pin
  // that routing — a future port that drops the handler would regress
  // to "the URL is in the DOM but clicking does nothing".

  it('opens the PR URL via openUrl when the title link is clicked', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    const titleLink = (await screen.findByText('Add widget')).closest('a')!;
    await userEvent.click(titleLink);

    // `openUrl` is called once with the PR's `url` field. The PR tab
    // has more action buttons (Merge / Spawn / View changes) than the
    // issues tab, so it's especially easy for a future refactor to
    // drop the link's onClick handler — this test catches that.
    expect(openUrlMock).toHaveBeenCalledTimes(1);
    expect(openUrlMock).toHaveBeenCalledWith('https://github.com/acme/demo/pull/201');
  });

  it('opens the PR URL via openUrl when the ↗ icon is clicked', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    // Wait for the list to render before querying the icons — the
    // initial render is the "Loading pull requests…" spinner, so a
    // sync getByLabelText would throw on a not-yet-mounted element.
    // The ↗ icon is the discoverability hint — same target, same
    // routing. aria-label distinguishes it from the issue-tab variant.
    // The fixture has 4 open PRs, so there are 4 icon links with the
    // same aria-label. Click the first and assert it routes to that
    // PR's URL.
    const iconLinks = await screen.findAllByLabelText(/open pull request on github/i);
    expect(iconLinks.length).toBeGreaterThan(0);
    await userEvent.click(iconLinks[0]);

    expect(openUrlMock).toHaveBeenCalledTimes(1);
    expect(openUrlMock).toHaveBeenCalledWith('https://github.com/acme/demo/pull/201');
  });

  it('does not call openUrl when the row body is clicked', async () => {
    // The row's expand handler fires for clicks on the body / chevron /
    // padding — NOT for clicks on the link (those route through
    // openUrl). Clicking the body should toggle expand and leave
    // openUrl untouched. Guards against a future refactor that wires
    // the row's onClick to openUrl "as a convenience" — that would
    // fire openUrl on every expand click.
    mockBackend();
    render(<GitPullRequestsTab />);

    const title = await screen.findByText('Add widget');
    const row = title.closest('[data-pr-row]')!;
    const clampedBody = row.querySelector('p.line-clamp-2') as HTMLElement;
    await userEvent.click(clampedBody);

    expect(openUrlMock).not.toHaveBeenCalled();
  });

  it('does not call openUrl when View changes is clicked (separate action)', async () => {
    // View changes opens the center diff overlay (a UI action), NOT
    // the external GitHub URL. Make sure the new link-routing didn't
    // accidentally get wired into the View changes button — the
    // overlay is the user-facing affordance for reading a PR's diff.
    mockBackend();
    render(<GitPullRequestsTab />);

    // findBy* waits for the row to mount; the View changes button is
    // icon-only with an aria-label, identified by the PR number.
    const viewBtn = await screen.findByRole('button', { name: 'View changes in PR #201' });
    await userEvent.click(viewBtn);

    expect(openUrlMock).not.toHaveBeenCalled();
    // Confirm the View changes action did its real job.
    expect(useUIStore.getState().activeDiffFile?.prNumber).toBe(201);
  });

  it('does not call openUrl when the Merge button is clicked (separate action)', async () => {
    // Symmetric to View changes — Merge is a backend IPC (`merge_pr`),
    // not an external navigation. Pin the routing.
    mockBackend();
    render(<GitPullRequestsTab />);

    const mergeBtn = await screen.findByRole('button', { name: 'Merge pull request #201' });
    await userEvent.click(mergeBtn);
    // First click reveals the confirm state; second click (Confirm) fires merge_pr.
    const confirmBtn = await screen.findByRole('button', { name: /confirm squash merge/i });
    await userEvent.click(confirmBtn);

    expect(openUrlMock).not.toHaveBeenCalled();
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('merge_pr', {
        prUrl: 'https://github.com/acme/demo/pull/201',
      });
    });
  });

  it('does not navigate or openUrl when a link with empty url is clicked', async () => {
    // The defensive empty-URL guard renders a <span> instead of an <a>
    // — so clicking the title (now plain text) must not invoke openUrl.
    // Pins against a future widening that re-introduces bare <a href="">.
    mockBackend({
      open: [
        {
          number: 999,
          title: 'Mystery PR',
          body: 'No html_url from upstream.',
          url: '',
          state: 'open',
          draft: false,
          head_ref: 'mystery/999',
          head_sha: 'f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9',
        },
      ],
    });
    render(<GitPullRequestsTab />);

    const title = await screen.findByText('Mystery PR');
    await userEvent.click(title);

    expect(openUrlMock).not.toHaveBeenCalled();
  });

  /// Regression: the PR tab has three action groups (Merge, split Spawn,
  /// View changes) where the Issues tab only has one. Long PR titles
  /// were wrapping to multiple lines and visually colliding with the
  /// action buttons to the right. Root cause was a flexbox `truncate`
  /// trap: the nested flex (caret / # / title / ↗) had no `min-w-0`, so
  /// the title's `truncate` class could not shrink it below its
  /// intrinsic content width and the text wrapped instead. Pin the
  /// classes on the title element + its flex parent so a future
  /// refactor can't silently drop the fix.
  it('truncates a long PR title so it does not wrap into the action buttons', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    const titleLink = (await screen.findByText('Add widget')).closest('a')!;
    expect(titleLink).toBeTruthy();
    // Title <a> needs all three: `truncate` (white-space:nowrap +
    // overflow:hidden + ellipsis), `min-w-0` (allow shrinking below
    // content width inside a flex parent), and `flex-1` (grow to fill
    // remaining space so the action buttons sit clearly to the right).
    expect(titleLink.className).toContain('truncate');
    expect(titleLink.className).toContain('min-w-0');
    expect(titleLink.className).toContain('flex-1');
    // The parent flex (caret / # / title / ↗) also needs `min-w-0` —
    // its children's `min-w-0` won't help if the flex container itself
    // can't shrink below the sum of its children's intrinsic widths.
    const parentFlex = titleLink.parentElement!;
    expect(parentFlex.className).toContain('flex');
    expect(parentFlex.className).toContain('min-w-0');
  });

  it('clears expanded state when the open/closed filter changes', async () => {
    // PR numbers don't carry across the open/closed filter (PR 201 vs
    // PR 150), so the prior Set would either no-op or re-open a
    // different row. The same `load` reset covers mesh swaps (mesh
    // change re-triggers `load` via the same effect dep list).
    mockBackend();
    render(<GitPullRequestsTab />);

    const firstTitle = await screen.findByText('Add widget');
    const firstRow = firstTitle.closest('[data-pr-row]')!;
    const firstBody = firstRow.querySelector('p.line-clamp-2') as HTMLElement;
    await userEvent.click(firstBody);
    expect(firstRow.querySelector('p.line-clamp-2')).toBeNull();

    await userEvent.click(screen.getByRole('button', { name: 'closed' }));
    expect(await screen.findByText('Old change')).toBeTruthy();

    await userEvent.click(screen.getByRole('button', { name: 'open' }));
    const resetTitle = await screen.findByText('Add widget');
    const resetRow = resetTitle.closest('[data-pr-row]')!;
    expect(resetRow.querySelector('p.line-clamp-2')).toBeTruthy();
  });

  /// Regression: the user wanted the textual "Merge" and "View changes"
  /// buttons replaced with intuitive icons so they take less of the 360px
  /// dock width. Pin the icon-only contract by asserting each action
  /// button renders exactly one inline `<svg>` and exposes the semantic
  /// name via `aria-label`/`title` (no text children). A future refactor
  /// that regresses to a text button would either lack the SVG (text-only)
  /// or expose the icon's accessible name only — both fail this test.
  it('renders the Merge and View changes buttons as icon-only with semantic labels', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    // Wait for PR 201 to be both rendered AND mergeable so the Merge
    // button exists in its initial (non-confirm) state.
    const mergeBtn = await screen.findByRole('button', { name: 'Merge pull request #201' });
    const viewBtn = await screen.findByRole('button', { name: 'View changes in PR #201' });

    // Icon-only: no visible text. Inner text of the button is empty.
    expect(mergeBtn.textContent?.trim() ?? '').toBe('');
    expect(viewBtn.textContent?.trim() ?? '').toBe('');

    // Each button has exactly one SVG (the icon). A regressed text
    // button would have 0; a regressed "icon + text" hybrid would also
    // fail the empty-textContent check above, but this one pin catches
    // the "two icons stacked" case if a future change ever adds a badge.
    expect(mergeBtn.querySelectorAll('svg')).toHaveLength(1);
    expect(viewBtn.querySelectorAll('svg')).toHaveLength(1);

    // `title` provides a hover tooltip — this is the discoverability
    // mechanism that replaces the visible text. Without it the icon
    // would be a mystery to anyone who doesn't know the convention.
    expect(mergeBtn.getAttribute('title')).toMatch(/merge/i);
    expect(viewBtn.getAttribute('title')).toMatch(/view changes|diff/i);
  });
});
