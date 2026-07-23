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
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import type { GitHubPullRequest } from '../../src/types/generated/GitHubPullRequest';
import type { PrMergeability } from '../../src/types/generated/PrMergeability';
import type { PrMergeabilityEntry } from '../../src/types/generated/PrMergeabilityEntry';

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

// Issue #780 — spy on the Open PR cache invalidation the merge flow
// fires for any agent node whose branch matches the merged PR's head
// ref. We keep the rest of `useOpenPr` real (the `useOpenPr` hook is
// imported transitively by nothing in this test file, so the spy is
// the only thing the merge flow sees).
const { refreshOpenPrByPathSpy } = vi.hoisted(() => ({
  refreshOpenPrByPathSpy: vi.fn(),
}));
vi.mock('../../src/hooks/useOpenPr', async () => {
  const actual = await vi.importActual<typeof import('../../src/hooks/useOpenPr')>(
    '../../src/hooks/useOpenPr',
  );
  return { ...actual, refreshOpenPrByPath: refreshOpenPrByPathSpy };
});

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

/**
 * Convert the per-PR `MERGEABILITY` map into the batched-entry wire shape
 * (`get_prs_mergeability` returns `Vec<PrMergeabilityEntry>`, one per
 * requested PR number). Issue #418 — the panel now makes one batched
 * call instead of N per-PR calls.
 */
function mergeabilityEntriesFor(numbers: number[]): PrMergeabilityEntry[] {
  return numbers.map((n) => {
    const m = MERGEABILITY[n] ?? { mergeable: null, mergeable_state: 'unknown' };
    return { number: n, mergeable: m.mergeable, mergeable_state: m.mergeable_state };
  });
}

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
 * `get_prs_mergeability` (issue #418) takes a `prNumbers: number[]` and returns
 * one entry per requested number; the legacy `get_pr_mergeability` is no longer
 * called from the desktop panel — keeping it in the mock just in case is not
 * necessary because the panel doesn't issue it anymore.
 */
function mockBackend(opts: { open?: GitHubPullRequest[]; closed?: GitHubPullRequest[] } = {}) {
  vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
    switch (cmd) {
      case 'get_repo_pulls': {
        const state = (args as { state?: string })?.state;
        return Promise.resolve(state === 'closed' ? (opts.closed ?? CLOSED_PRS) : (opts.open ?? OPEN_PRS));
      }
      case 'get_prs_mergeability': {
        const nums = (args as { prNumbers?: number[] })?.prNumbers ?? [];
        return Promise.resolve(mergeabilityEntriesFor(nums));
      }
      case 'list_providers':
        return Promise.resolve(PROVIDERS);
      case 'get_default_provider':
        return Promise.resolve('anthropic');
      case 'create_pr_node':
        return Promise.resolve(PR_DRAFT);
      case 'merge_pr':
        return Promise.resolve('Merged (squash) via abc123 — done');
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
    refreshOpenPrByPathSpy.mockReset();
    useUIStore.setState({ probeOpen: true, probeTab: 'pulls', activeDiffFile: null });
    useMeshStore.setState({
      meshesById: new Map([[MESH.id, MESH]]),
      selectedMeshId: MESH.id,
    });
    // Clear any agent nodes left over from sibling tests — this tab
    // doesn't render them, but the merge-wiring test below reads them
    // via `useAgentNodeStore`.
    useAgentNodeStore.setState({ agentNodes: [], activeNodeId: null });
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

  // Issue #780 — the merge flow must force-invalidate the Open PR
  // cache for any agent node whose branch matches the merged PR's head
  // ref, so the chip in GridNodeHeader updates immediately instead of
  // waiting up to 60s for the freshness window to expire. The cache
  // primitive is unit-tested in `dual-key-cache.test.ts`; this test
  // pins the wiring between the merge flow and the cache primitive.
  it('after a successful merge, refreshOpenPrByPath fires for matching agent nodes', async () => {
    // Agent node whose branch matches PR 201's head ref (`feat/201-add-widget`).
    // Worktree name is empty → `getNodeGitPath` returns `node.path`
    // (the mesh root), which is the simplest assertion target.
    const matchingNode = {
      id: 99,
      mesh_id: 42,
      name: 'feat-201',
      path: '/repos/demo',
      branch: 'feat/201-add-widget',
      worktree_name: null,
      use_worktree: false,
      env: 'wsl',
      provider: 'anthropic',
      status: 'idle',
      session_id: null,
      created_at: '2026-01-01',
      position: 0,
      source_issue: null,
      source_pr: null,
      is_pinned: false,
    };
    // Sibling node whose branch does NOT match — must NOT trigger refresh.
    const unrelatedNode = {
      ...matchingNode,
      id: 100,
      name: 'other-branch',
      branch: 'feat/999-different',
    };
    useAgentNodeStore.setState({ agentNodes: [matchingNode, unrelatedNode] });

    mockBackend();
    render(<GitPullRequestsTab />);

    const mergeBtn = await screen.findByRole('button', { name: 'Merge pull request #201' });
    await userEvent.click(mergeBtn);
    const confirmBtn = await screen.findByRole('button', { name: /confirm squash merge/i });
    await userEvent.click(confirmBtn);

    // The merge flow must invalidate the cache for the matching node's
    // resolved git path (mesh root here, because `use_worktree: false`).
    await waitFor(() => {
      expect(refreshOpenPrByPathSpy).toHaveBeenCalledWith('/repos/demo');
    });
    // …and NOT for the sibling node (different branch) — refresh is
    // scoped to chips that actually change.
    expect(refreshOpenPrByPathSpy).not.toHaveBeenCalledWith(expect.stringContaining('feat/999'));
    // Called exactly once for the matching node (no double-fire from
    // sibling hook instances on the same path).
    expect(refreshOpenPrByPathSpy).toHaveBeenCalledTimes(1);
  });

  it('after a successful merge, refreshOpenPrByPath fires with the worktree subdir for Worktree Nodes', async () => {
    // Pin the path derivation: a Worktree Node (use_worktree: true +
    // worktree_name) resolves to `<path>/.claude/worktrees/<name>` per
    // `getNodeGitPath`, not the mesh root. The merge-flow invalidation
    // must use the same path the chip's `useOpenPr` subscribed to,
    // otherwise the subscriber wouldn't fire (path mismatch).
    const worktreeNode = {
      id: 99,
      mesh_id: 42,
      name: 'feat-201',
      path: '/repos/demo',
      branch: 'feat/201-add-widget',
      worktree_name: 'swift-otter',
      use_worktree: true,
      env: 'wsl',
      provider: 'anthropic',
      status: 'idle',
      session_id: null,
      created_at: '2026-01-01',
      position: 0,
      source_issue: null,
      source_pr: null,
      is_pinned: false,
    };
    useAgentNodeStore.setState({ agentNodes: [worktreeNode] });

    mockBackend();
    render(<GitPullRequestsTab />);

    const mergeBtn = await screen.findByRole('button', { name: 'Merge pull request #201' });
    await userEvent.click(mergeBtn);
    const confirmBtn = await screen.findByRole('button', { name: /confirm squash merge/i });
    await userEvent.click(confirmBtn);

    await waitFor(() => {
      expect(refreshOpenPrByPathSpy).toHaveBeenCalledWith('/repos/demo/.claude/worktrees/swift-otter');
    });
  });

  it('after a successful merge, refreshOpenPrByPath does NOT fire when no agent node matches the head ref', async () => {
    // The invalidation is scoped to matching branches — if no agent
    // node has the merged PR's head ref as its branch, there is no
    // chip to update and the refresh is a no-op (the spy confirms no
    // spurious invalidations are emitted for unrelated nodes).
    useAgentNodeStore.setState({ agentNodes: [] });

    mockBackend();
    render(<GitPullRequestsTab />);

    const mergeBtn = await screen.findByRole('button', { name: 'Merge pull request #201' });
    await userEvent.click(mergeBtn);
    const confirmBtn = await screen.findByRole('button', { name: /confirm squash merge/i });
    await userEvent.click(confirmBtn);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('merge_pr', expect.anything());
    });
    expect(refreshOpenPrByPathSpy).not.toHaveBeenCalled();
  });

  it('cancelled merge does NOT fire refreshOpenPrByPath', async () => {
    // Pin the negative case: the confirm-then-cancel path dismisses
    // the confirm UI without firing `merge_pr`, and must therefore NOT
    // touch the Open PR cache. A regression that wires refresh into
    // the wrong branch (e.g. the cancel button's onClick) would be
    // caught here.
    useAgentNodeStore.setState({
      agentNodes: [{
        id: 99,
        mesh_id: 42,
        name: 'feat-201',
        path: '/repos/demo',
        branch: 'feat/201-add-widget',
        worktree_name: null,
        use_worktree: false,
        env: 'wsl',
        provider: 'anthropic',
        status: 'idle',
        session_id: null,
        created_at: '2026-01-01',
        position: 0,
        source_issue: null,
        source_pr: null,
        is_pinned: false,
      }],
    });

    mockBackend();
    render(<GitPullRequestsTab />);

    const mergeBtn = await screen.findByRole('button', { name: 'Merge pull request #201' });
    await userEvent.click(mergeBtn);
    const cancelBtn = await screen.findByRole('button', { name: /cancel merge/i });
    await userEvent.click(cancelBtn);

    // No merge IPC after cancellation.
    expect(invoke).not.toHaveBeenCalledWith('merge_pr', expect.anything());
    expect(refreshOpenPrByPathSpy).not.toHaveBeenCalled();
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

  it('flags a draft PR as Draft without including it in the batched mergeability call', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    await screen.findByText('WIP spike');
    expect(await screen.findByText('Draft')).toBeTruthy();
    // Draft is derived from the list flag — it's filtered out of the
    // batched mergeability call (issue #418). The batched call carries
    // 201/202/204 (the non-draft open PRs); 203 must not appear.
    await waitFor(() => {
      const batchCalls = vi
        .mocked(invoke)
        .mock.calls.filter(([c]) => c === 'get_prs_mergeability');
      expect(batchCalls.length).toBeGreaterThan(0);
      // The last batched call (post-list) carries every non-draft PR.
      const lastNumbers = (batchCalls[batchCalls.length - 1][1] as { prNumbers: number[] }).prNumbers;
      expect(lastNumbers).toEqual(expect.arrayContaining([201, 202, 204]));
      expect(lastNumbers).not.toContain(203);
    });
    // The legacy per-PR command is NOT called from the desktop panel
    // anymore — the panel goes through the batched endpoint only.
    expect(invoke).not.toHaveBeenCalledWith('get_pr_mergeability', expect.anything());
  });

  it('shows "Checking…" while GitHub computes mergeability (mergeable: null)', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    // PR 204 resolves to mergeable: null — stays in the checking state
    // until the bounded re-poll below settles it.
    expect(await screen.findByText('Fresh PR')).toBeTruthy();
    expect(await screen.findByText('Checking…')).toBeTruthy();
  });

  /// Regression for issue #419: when GitHub's batched mergeability returns
  /// `mergeable: null` for a PR (still computing), the panel must re-poll
  /// — not leave the row stuck on "Checking…" forever. Issue #418 changed
  /// the wire from per-PR fan-out to a single batched IPC, but the retry
  /// semantics are unchanged: a single batched call carries only the
  /// still-null PRs, so the retry path is itself a batched call. We use
  /// fake timers so the retry `setTimeout` is deterministic. With fake
  /// timers on, RTL's async finders (which use setTimeout polling) can't
  /// advance, so we wrap render + timer advances in `act(async …)` — that
  /// flushes React's state updates AND lets vitest's microtask runner
  /// drain the Promise resolutions triggered by the fake clock.
  it('re-polls mergeability when the first batch returns mergeable: null for a PR', async () => {
    vi.useFakeTimers();
    try {
      // PR 204's first probe returns null; the second returns a real
      // value. The rest of the PRs use the standard fixture map.
      // `callsByNumber` tracks the batched-call sequence so we can pin
      // BOTH "retry happened" AND "retry was a batched call carrying
      // only PR 204" (issue #418 — auth cost amortises per-stuck-PR).
      const callsByNumber: number[][] = [];
      vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
        switch (cmd) {
          case 'get_repo_pulls':
            return Promise.resolve(OPEN_PRS);
          case 'get_prs_mergeability': {
            const nums = (args as { prNumbers?: number[] })?.prNumbers ?? [];
            callsByNumber.push([...nums]);
            return Promise.resolve(
              nums.map((n) => {
                if (n === 204) {
                  // First call for PR 204 → null; second → real.
                  const prior = callsByNumber.flat().filter((x) => x === 204).length;
                  return {
                    number: 204,
                    mergeable: prior === 1 ? null : true,
                    mergeable_state: prior === 1 ? 'unknown' : 'clean',
                  };
                }
                const m = MERGEABILITY[n] ?? { mergeable: null, mergeable_state: 'unknown' };
                return { number: n, mergeable: m.mergeable, mergeable_state: m.mergeable_state };
              }),
            );
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
      // Initial batch carried all 3 non-draft open PRs (201, 202, 204).
      expect(callsByNumber[0]).toEqual(expect.arrayContaining([201, 202, 204]));

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
      // Issue #418 — the retry is itself a batched call carrying ONLY
      // PR 204, not the full list. Pin both: ≥2 calls total (initial +
      // retry), AND the retry's numbers are exactly [204] (not the
      // full list). If a future refactor reverts to per-PR retries or
      // re-batches the whole list, this assertion catches it.
      expect(callsByNumber.length).toBeGreaterThanOrEqual(2);
      const retryCall = callsByNumber[callsByNumber.length - 1];
      expect(retryCall).toEqual([204]);
    } finally {
      vi.useRealTimers();
    }
  });

  /// Counterpart to the re-poll test: when the effect tears down (unmount,
  /// filter toggle, or mesh change — all share the same cleanup), pending
  /// retry timers must be cleared. Unmount is the most direct test: cleanup
  /// unmounts the component, no new renders happen, so any further probe
  /// call would be a leaked timer. The mock tracks the batched-call
  /// sequence so a leaked retry shows up as a second batched call for
  /// PR 204.
  it('cancels pending mergeability retries when the component unmounts', async () => {
    vi.useFakeTimers();
    try {
      const callsByNumber: number[][] = [];
      vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
        switch (cmd) {
          case 'get_repo_pulls':
            return Promise.resolve(OPEN_PRS);
          case 'get_prs_mergeability': {
            const nums = (args as { prNumbers?: number[] })?.prNumbers ?? [];
            callsByNumber.push([...nums]);
            return Promise.resolve(
              nums.map((n) => ({
                number: n,
                // First call for any PR: null (schedules retry). Any
                // second call: would return a real value — if cleanup
                // doesn't clear the timer, the retry would fire.
                mergeable: callsByNumber.flat().filter((x) => x === n).length === 1
                  ? null
                  : true,
                mergeable_state: 'clean',
              })),
            );
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
      // Initial batch landed; retries are scheduled for each null entry.
      expect(callsByNumber.length).toBe(1);

      // Unmount BEFORE any retry timer fires. The effect's cleanup must
      // clear every pending timer — no second batched call.
      await act(async () => {
        cleanup();
        await vi.advanceTimersByTimeAsync(2000);
      });

      // No retry fired after unmount: still exactly one batched call.
      expect(callsByNumber).toHaveLength(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('does not crash when the mesh is removed after mount', () => {
    useMeshStore.setState({ meshesById: new Map(), selectedMeshId: null });
    mockBackend();
    expect(() => render(<GitPullRequestsTab />)).not.toThrow();
  });

  // ----- Batched mergeability (issue #418) --------------------------------
  //
  // The whole point of the batched endpoint is one IPC round-trip per
  // panel render (was N per-PR calls, each costing a fresh GitHubClient
  // construction + token resolution). These tests pin the perf contract:
  // a list of N open PRs triggers exactly ONE `get_prs_mergeability` call
  // carrying all N PR numbers, never N individual `get_pr_mergeability`
  // calls (the legacy per-PR shape). A future refactor that reverts to
  // the per-PR fan-out breaks both tests below.

  /// With N non-draft open PRs, the panel makes exactly ONE batched call
  /// carrying all N numbers — not N individual calls. The fixture has
  /// 3 non-draft PRs (201, 202, 204); draft PR 203 is filtered out
  /// before the call. Pin the exact prNumbers array so a future refactor
  /// that re-introduces per-PR fan-out (or that drops the draft filter)
  /// is caught here.
  it('makes one batched mergeability call for all non-draft PRs (issue #418)', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    // Wait for the list to load + the enrichment effect to fire.
    await screen.findByText('Add widget');
    await waitFor(() => {
      const calls = vi
        .mocked(invoke)
        .mock.calls.filter(([c]) => c === 'get_prs_mergeability');
      expect(calls.length).toBe(1);
    });

    const batchCall = vi
      .mocked(invoke)
      .mock.calls.find(([c]) => c === 'get_prs_mergeability')!;
    const args = batchCall[1] as { meshId: number; prNumbers: number[] };
    expect(args.meshId).toBe(42);
    // All 3 non-draft open PRs; draft PR 203 filtered out.
    expect(args.prNumbers).toEqual([201, 202, 204]);

    // The legacy per-PR command must NOT be called from the desktop
    // panel at all (issue #418). If a future refactor leaves both
    // commands wired up, this catches the redundant per-PR fan-out.
    expect(invoke).not.toHaveBeenCalledWith('get_pr_mergeability', expect.anything());
  });

  /// A list with zero non-draft PRs (all drafts, or empty list) must
  /// skip the IPC entirely — the backend also short-circuits, but the
  /// panel doesn't even open the wire call. Pin the no-IPC behaviour so
  /// a future refactor that always fires the batched call (with an
  /// empty array) is caught here. Empty-prNumbers is a degenerate
  /// invariant — useful to keep the wire off the slow path.
  it('skips the mergeability batch when there are no non-draft PRs (issue #418)', async () => {
    mockBackend({
      // One draft PR, no non-draft entries → nothing to enrich.
      open: [
        { number: 203, title: 'WIP spike', body: '', url: 'https://github.com/acme/demo/pull/203', state: 'open', draft: true, head_ref: 'wip/203-spike', head_sha: 'c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3' },
      ],
    });
    render(<GitPullRequestsTab />);

    await screen.findByText('WIP spike');
    // Give the enrichment effect a tick to (NOT) fire — `waitFor` polls
    // microtasks and finds that nothing arrived. The panel's
    // `candidates.length === 0` short-circuit must skip the IPC
    // entirely; if a future refactor fires the call with an empty
    // array, this assertion catches it.
    await new Promise((resolve) => setTimeout(resolve, 50));

    expect(invoke).not.toHaveBeenCalledWith('get_prs_mergeability', expect.anything());
    expect(invoke).not.toHaveBeenCalledWith('get_pr_mergeability', expect.anything());
  });

  /// The closed-PR filter must NOT fire the enrichment effect — closed
  /// PRs are read-only on the panel, so the per-PR HTTP cost is pure
  /// waste. The early-return on `stateFilter !== 'open'` (line 291) is
  /// load-bearing for the perf fix's value proposition; if a future
  /// refactor drops it, the Closed view fires one batched call per
  /// render that the panel ignores. Pin the negative so the guard is
  /// not silently removed.
  it('does not fire get_prs_mergeability on the Closed filter (issue #418)', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    // Wait for the Open list to load + enrichment to fire (so we know
    // the IPC seam is open).
    await screen.findByText('Add widget');
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('get_prs_mergeability', expect.anything());
    });

    // Reset the call history so the Closed-toggle's call to
    // `get_prs_mergeability` (which we expect to be absent) is what we
    // assert on, not the Open render's call.
    vi.mocked(invoke).mockClear();

    // Toggle to Closed.
    await userEvent.click(screen.getByRole('button', { name: 'closed' }));
    expect(await screen.findByText('Old change')).toBeTruthy();

    // Give the enrichment effect a tick to (NOT) fire.
    await new Promise((resolve) => setTimeout(resolve, 50));

    // The Closed view only calls `get_repo_pulls`; no enrichment.
    const enrichmentCalls = vi
      .mocked(invoke)
      .mock.calls.filter(([c]) => c === 'get_prs_mergeability');
    expect(enrichmentCalls).toHaveLength(0);
  });

  /// Per-PR error sentinel exhausts the bounded retry budget and leaves
  /// the row in "Checking…" (no early bailout, no false conflict). Pin
  /// the path because the batched endpoint's value proposition is "one
  /// failed probe doesn't fail the batch" — the retry path then has to
  /// also gracefully handle repeated failures without entering a tight
  /// loop. The mock returns the error sentinel for all 4 attempts; we
  /// advance fake timers past the full retry budget and assert no
  /// further probes fire AND no extra row text is rendered.
  it('exhausts the bounded retry budget when every attempt returns the error sentinel', async () => {
    vi.useFakeTimers();
    try {
      const batchedCalls: number[][] = [];
      vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
        switch (cmd) {
          case 'get_repo_pulls':
            return Promise.resolve(OPEN_PRS);
          case 'get_prs_mergeability': {
            const nums = (args as { prNumbers?: number[] })?.prNumbers ?? [];
            batchedCalls.push([...nums]);
            // Every attempt returns the error sentinel for every PR.
            // PRs not in the fixture map (e.g. 201's standard clean
            // result) also fail in this scenario to exercise the retry
            // budget for ALL three non-draft PRs (201, 202, 204).
            return Promise.resolve(
              nums.map((n) => ({
                number: n,
                mergeable: null,
                mergeable_state: 'error: GitHub API error (503): Service Unavailable',
              })),
            );
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

      // Initial batch landed with all 3 non-draft PRs.
      expect(batchedCalls.length).toBe(1);
      expect(batchedCalls[0]).toEqual([201, 202, 204]);

      // Advance past every retry budget window: 1.5s + 3s + 4.5s = 9s.
      // After all 3 retries, the row stays in "Checking…" — no 4th call.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(10_000);
      });

      // Initial + 3 retries = 4 batched calls. No 5th. The 5th would
      // mean the retry path failed to honour MAX_MERGEABILITY_ATTEMPTS.
      expect(batchedCalls.length).toBe(4);

      // Every retry is a single batched call carrying ALL still-null PRs
      // (the issue #418 perf contract holds on the retry path too —
      // confirms the shared-timer rewrite).
      expect(batchedCalls[1]).toEqual([201, 202, 204]);
      expect(batchedCalls[2]).toEqual([201, 202, 204]);
      expect(batchedCalls[3]).toEqual([201, 202, 204]);

      // The rows stayed in "Checking…" — no false "Conflicts" labelling.
      // PRs 202 (dirty in the standard fixture) and 204 (null in the
      // standard fixture) both render "Checking…" because every entry
      // carried the error sentinel, NOT the standard fixture result.
      const checkingLabels = screen.getAllByText('Checking…');
      expect(checkingLabels.length).toBeGreaterThanOrEqual(1);
    } finally {
      vi.useRealTimers();
    }
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

  // ----- Spawn button (issue #420, #247) ---------------------------------
  // One backend-owned call. The PR row's `+` button is on the
  // `SpawnButtonCluster`; the tab now relies on `create_pr_node` to
  // accept the row and start the intent-driven launch in the background.
  // The dock stays open after a successful spawn (mirrors the issue tab
  // UX contract — memory buildmesh-spawn-from-probe-keeps-dock-open).

  it('does the one-stage spawn on the primary Spawn button (issue #420, #247)', async () => {
    mockBackend();
    render(<GitPullRequestsTab />);

    // Every open PR row renders its own `SpawnButtonCluster` whose `+`
    // button carries `data-testid="spawn-default"` — the first row
    // (PR 201) is what the test exercises.
    const spawns = await screen.findAllByTestId('spawn-default');
    await userEvent.click(spawns[0]);

    // One accepted call carries the head ref, the SHA (issue #444) and the
    // resolved default provider. The backend owns the slow launch.
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
    expect(invoke).not.toHaveBeenCalledWith('start_node_background', expect.anything());
  });

  it('keeps the dock open after a successful spawn (mirrors issue-tab contract)', async () => {
    // The PR tab is a persistent dock like the issue tab. Closing on every
    // spawn would force the user to re-open the dock for the next PR.
    mockBackend();
    useUIStore.setState({ probeOpen: true, probeTab: 'pulls' });
    render(<GitPullRequestsTab />);

    const spawns = await screen.findAllByTestId('spawn-default');
    await userEvent.click(spawns[0]);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('create_pr_node', expect.objectContaining({ prNumber: 201 }));
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

    const spawns = await screen.findAllByTestId('spawn-default');
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
    vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === 'get_repo_pulls') {
        return Promise.resolve(OPEN_PRS.map((pr) =>
          pr.number === 201 ? { ...pr, head_sha: '' } : pr,
        ));
      }
      if (cmd === 'get_prs_mergeability') {
        // Issue #418 — the panel now fires a batched mergeability call.
        // Reuse the fixture's standard per-PR map.
        const nums = (args as { prNumbers?: number[] })?.prNumbers ?? [];
        return Promise.resolve(mergeabilityEntriesFor(nums));
      }
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      if (cmd === 'create_pr_node') return Promise.resolve(PR_DRAFT);
      return Promise.resolve({});
    });
    render(<GitPullRequestsTab />);

    const spawns = await screen.findAllByTestId('spawn-default');
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
    vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === 'get_repo_pulls') return Promise.resolve(OPEN_PRS);
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      if (cmd === 'get_prs_mergeability') {
        // Issue #418 — reuse the standard per-PR fixture map.
        const nums = (args as { prNumbers?: number[] })?.prNumbers ?? [];
        return Promise.resolve(mergeabilityEntriesFor(nums));
      }
      if (cmd === 'create_pr_node') return Promise.reject(new Error("PR's fork info is incomplete (head_repo_owner and head_repo_clone_url must both be present, or both absent). Reload the PR list and retry."));
      return Promise.resolve({});
    });
    useUIStore.setState({ probeOpen: true, probeTab: 'pulls' });
    render(<GitPullRequestsTab />);

    const spawns = await screen.findAllByTestId('spawn-default');
    await userEvent.click(spawns[0]);

    // The spawning label clears once the rejected promise resolves.
    await waitFor(() => {
      const stillSpawning = (screen.queryAllByTestId('spawn-default') as HTMLButtonElement[])
        .find((b) => b.textContent === 'Spawning...');
      expect(stillSpawning).toBeUndefined();
    });
    // The error message surfaces inline on the row.
    expect(
      await screen.findByText(/PR's fork info is incomplete/),
    ).toBeTruthy();
    // The dock stays open so the user can try a different PR.
    expect(useUIStore.getState().probeOpen).toBe(true);
  });

  it('disables the split button while a spawn is in flight to block double-clicks', async () => {
    let resolveCreate!: (v: typeof PR_DRAFT) => void;
    vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === 'get_repo_pulls') return Promise.resolve(OPEN_PRS);
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      if (cmd === 'get_prs_mergeability') {
        // Issue #418 — reuse the standard per-PR fixture map.
        const nums = (args as { prNumbers?: number[] })?.prNumbers ?? [];
        return Promise.resolve(mergeabilityEntriesFor(nums));
      }
      if (cmd === 'create_pr_node') return new Promise((res) => { resolveCreate = res; });
      return Promise.resolve({});
    });
    render(<GitPullRequestsTab />);

    const spawns = await screen.findAllByTestId('spawn-default');
    await userEvent.click(spawns[0]);

    // The in-flight `create_pr_node` leaves the spawning flag set, so the
    // row's `SpawnButtonCluster` rewrites the `+` label to "Spawning..." and
    // disables both halves. A second click would be a no-op even if it
    // landed — assert the guard rather than the backend idempotency.
    const spawnButtons = await screen.findAllByTestId('spawn-default');
    const spawningRow = spawnButtons.find((b) => b.textContent === 'Spawning...');
    expect(spawningRow).toBeTruthy();
    expect((spawningRow as HTMLButtonElement).disabled).toBe(true);

    // Resolve the in-flight IPC and confirm the label flips back.
    resolveCreate(PR_DRAFT);
    await waitFor(() => {
      const stillSpawning = (screen.queryAllByTestId('spawn-default') as HTMLButtonElement[])
        .find((b) => b.textContent === 'Spawning...');
      expect(stillSpawning).toBeUndefined();
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

  // ----- "View on GitHub" header link (PRs probe) -----
  //
  // The link is a `<SafeLink>` to `{githubUrl}/pulls` — same shape as
  // the Issues probe's header, just the `/pulls` list URL. Mirrors
  // git-issues-tab.test.tsx's "View on GitHub" cluster; the
  // `mockBackend` helper here doesn't wire `get_github_url_for_mesh`
  // because most tests don't care about the header, so the new tests
  // set up the mock inline.

  it('renders a "View on GitHub" header link to the repo pull requests list', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_pulls') return Promise.resolve(OPEN_PRS);
      if (cmd === 'get_prs_mergeability') {
        return Promise.resolve(mergeabilityEntriesFor([201, 202, 203, 204]));
      }
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      if (cmd === 'get_github_url_for_mesh') {
        return Promise.resolve('https://github.com/acme/demo');
      }
      return Promise.resolve({});
    });
    render(<GitPullRequestsTab />);

    const link = await screen.findByLabelText("Open this repo's pull requests list on GitHub");
    expect(link.tagName).toBe('A');
    expect(link.getAttribute('href')).toBe('https://github.com/acme/demo/pulls');
    expect(link.getAttribute('target')).toBe('_blank');
  });

  it('clicking the "View on GitHub" header link opens the pull requests list URL', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_pulls') return Promise.resolve(OPEN_PRS);
      if (cmd === 'get_prs_mergeability') {
        return Promise.resolve(mergeabilityEntriesFor([201, 202, 203, 204]));
      }
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      if (cmd === 'get_github_url_for_mesh') {
        return Promise.resolve('https://github.com/acme/demo');
      }
      return Promise.resolve({});
    });
    render(<GitPullRequestsTab />);

    const link = await screen.findByLabelText("Open this repo's pull requests list on GitHub");
    await userEvent.click(link);

    // The full {base}/pulls URL must be the one routed to openUrl —
    // pin the wire shape so a future "open the repo home instead"
    // regression is caught.
    expect(openUrlMock).toHaveBeenCalledWith('https://github.com/acme/demo/pulls');
  });

  it('renders an inert label (no link, no link-style cursor) when the mesh has no GitHub origin', async () => {
    // Mirrors the same test in git-issues-tab.test.tsx. A null URL
    // collapses SafeLink to a <span> with the link-styling tokens
    // stripped — see the "Empty-URL span is INERT" note in SafeLink's
    // file header. Pin the empty-URL shape here so a future
    // regression that re-applies the cyan/hover classes to the
    // fallback is caught at the seam named in the user's "links
    // aren't links" bug report.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_pulls') return Promise.resolve(OPEN_PRS);
      if (cmd === 'get_prs_mergeability') {
        return Promise.resolve(mergeabilityEntriesFor([201, 202, 203, 204]));
      }
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      if (cmd === 'get_github_url_for_mesh') return Promise.resolve(null);
      return Promise.resolve({});
    });
    render(<GitPullRequestsTab />);

    await screen.findByText('Add widget');
    const fallback = await screen.findByText(/View on GitHub/);
    expect(fallback.tagName).toBe('SPAN');
    expect(fallback.getAttribute('href')).toBeNull();
    expect(fallback.getAttribute('aria-label')).toBeNull();
    // User-reported "links aren't links" pin — visually inert.
    expect(fallback.className).not.toMatch(/text-accent-cyan/);
    expect(fallback.className).not.toMatch(/cursor-pointer/);
    expect(fallback.className).toMatch(/cursor-default/);
    // The Open/Closed toggle must keep working — pin it so a future
    // refactor that breaks the header row when githubUrl is null
    // doesn't pass vacuously.
    expect(screen.getByRole('button', { name: 'open' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'closed' })).toBeTruthy();
  });
});
