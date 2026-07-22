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
import { addToast } from '../../src/stores/toastStore';
import type { GitHubIssue } from '../../src/types/generated/GitHubIssue';

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

// Issue #1001 — the trigger-label toggle now surfaces IPC failures
// through the shared `addToast` wrapper from `stores/toastStore` instead
// of inline `<span data-trigger-label-error>` state. Mock the wrapper
// the same way Terminal.tsx is mocked in `agent-node-store.test.ts`:
// the production code imports the named export, tests capture the spy
// via `vi.mocked(addToast)`, and `addToastMock.mockReset()` in
// `beforeEach` clears call history between tests.
const { addToastMock } = vi.hoisted(() => ({
  addToastMock: vi.fn(),
}));
vi.mock('../../src/stores/toastStore', () => ({
  addToast: addToastMock,
  // `dismissToast` is exported by the module but isn't exercised by this
  // test file; pass through to keep the module shape intact.
  dismissToast: vi.fn(),
}));

const MESH: Mesh = {
  id: 42,
  name: 'demo',
  path: '/repos/demo',
  layout: 'single',
  position: 0,
  created_at: '2026-01-01',
  scratchpad: '',
  sandbox: false,
  // Issue #979 — autopilot trigger label drives the new toggle badge.
  // Real meshes default to `buildmesh:run`; the test uses the same value
  // so the badge renders and the toggle IPC is verifiable end-to-end.
  autopilot_trigger_label: 'buildmesh:run',
};

const ISSUES: GitHubIssue[] = [
  {
    number: 101,
    title: 'Fix the wobble',
    body: 'The foobar widget wobbles under load.',
    url: 'https://github.com/acme/demo/issues/101',
    state: 'open',
    labels: ['bug'],
    blocked_by: [],
  },
  {
    number: 102,
    title: 'Add a /v2 endpoint',
    body: null,
    url: 'https://github.com/acme/demo/issues/102',
    state: 'open',
    labels: [],
    blocked_by: [],
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
      // Issue #979 — trigger-label toggle. Default to success; tests
      // that want a failing path override via mockImplementation /
      // mockRejectedValueOnce. The `args` are logged to the IPC record
      // so tests can assert the action ('add'/'remove') + label passed.
      case 'set_issue_label':
        return Promise.resolve(undefined);
      default:
        return Promise.resolve({});
    }
  });
}

describe('GitIssuesTab (#378)', () => {
  beforeEach(() => {
    // `mockReset` (not `mockClear`) wipes both call history AND any
    // per-test `mockImplementationOnce` / `mockRejectedValueOnce`
    // overrides — guards against a future test that adds a one-off
    // override and accidentally leaks the override into siblings.
    // Re-establish the default resolved value after the reset.
    openUrlMock.mockReset();
    openUrlMock.mockResolvedValue(undefined);
    // Issue #1001 — clear toast call history between tests so the
    // "calls addToast on failure" assertion below doesn't see calls
    // from a prior test.
    addToastMock.mockReset();
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

    // Each issue row renders its own `SpawnButtonCluster` whose `+` button
    // carries `data-testid="spawn-default"` — the two-stage-spawn test
    // wants the first row's button.
    const spawns = await screen.findAllByTestId('spawn-default');
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

    const spawns = await screen.findAllByTestId('spawn-default');
    await userEvent.click(spawns[0]);

    // The in-flight `create_issue_node` leaves the spawning flag set, so
    // the row's `SpawnButtonCluster` rewrites the `+` label to "Spawning..."
    // and disables both halves. The second click would be a no-op even if
    // it landed — assert the guard rather than the backend idempotency.
    const spawnButtons = await screen.findAllByTestId('spawn-default');
    const spawningRow = spawnButtons.find((b) => b.textContent === 'Spawning...');
    expect(spawningRow).toBeTruthy();
    expect((spawningRow as HTMLButtonElement).disabled).toBe(true);

    // Resolve the in-flight IPC and confirm the label flips back.
    resolveCreate(DRAFT);
    await waitFor(() => {
      const stillSpawning = (screen.queryAllByTestId('spawn-default') as HTMLButtonElement[])
        .find((b) => b.textContent === 'Spawning...');
      expect(stillSpawning).toBeUndefined();
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
    const spawns = await screen.findAllByTestId('spawn-default');
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

    const spawns = await screen.findAllByTestId('spawn-default');
    await userEvent.click(spawns[0]);

    await waitFor(() => {
      // The spawning label clears on failure — no row should still show
      // "Spawning..." after the rejected IPC resolves.
      const stillSpawning = (screen.queryAllByTestId('spawn-default') as HTMLButtonElement[])
        .find((b) => b.textContent === 'Spawning...');
      expect(stillSpawning).toBeUndefined();
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
          blocked_by: [],
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

  // ----- Tauri 2 WebView external-link routing ---------------------------
  // Tauri 2's WebView is NOT a browser. `target="_blank"` is silently
  // dropped without the `core:webview:allow-create-webview-window`
  // capability (which we don't grant in `capabilities/default.json`),
  // so the link's onClick must call `openUrl` from
  // `@tauri-apps/plugin-opener` to delegate to the OS. These tests pin
  // that routing — a future port that drops the handler would regress
  // to "the URL is in the DOM but clicking does nothing".

  it('opens the issue URL via openUrl when the title link is clicked', async () => {
    mockBackend();
    render(<GitIssuesTab />);

    const titleLink = (await screen.findByText('Fix the wobble')).closest('a')!;
    await userEvent.click(titleLink);

    // `openUrl` is called once with the issue's `url` field — the same
    // string the href carries, so right-click "open in browser" and
    // left-click both go to the same place.
    expect(openUrlMock).toHaveBeenCalledTimes(1);
    expect(openUrlMock).toHaveBeenCalledWith('https://github.com/acme/demo/issues/101');
    // The link's onClick must preventDefault so the WebView's dead
    // default action doesn't compete with `openUrl`. jsdom doesn't
    // navigate, but we can assert the click was marked handled by
    // checking the openUrl side-effect (the URL is "opened" via the
    // plugin, not via DOM navigation).
    expect(openUrlMock).toHaveBeenCalled();
  });

  it('opens the issue URL via openUrl when the ↗ icon is clicked', async () => {
    mockBackend();
    render(<GitIssuesTab />);

    // Wait for the list to render before querying the icons — the
    // initial render is the "Loading issues…" spinner, so a sync
    // getByLabelText would throw on a not-yet-mounted element.
    // The ↗ icon is the discoverability hint next to the title — same
    // target, same routing. The fixture has 2 issues, so there are
    // 2 icon links with the same aria-label. Click the first and
    // assert it routes to that issue's URL.
    const iconLinks = await screen.findAllByLabelText('Open issue on GitHub');
    expect(iconLinks.length).toBeGreaterThan(0);
    await userEvent.click(iconLinks[0]);

    expect(openUrlMock).toHaveBeenCalledTimes(1);
    expect(openUrlMock).toHaveBeenCalledWith('https://github.com/acme/demo/issues/101');
  });

  it('does not call openUrl when the row body is clicked', async () => {
    // The row's expand handler fires for clicks on the body / arrow
    // chevron / padding — NOT for clicks on the link (those route
    // through openUrl). Clicking the body should toggle expand and
    // leave openUrl untouched. Guards against a future refactor that
    // wires the row's onClick to openUrl "as a convenience" — that
    // would fire openUrl on every expand click, which is wrong.
    mockBackend();
    render(<GitIssuesTab />);

    const title = await screen.findByText('Fix the wobble');
    const row = title.closest('[data-issue-row]')!;
    const clampedBody = row.querySelector('p.line-clamp-2') as HTMLElement;
    await userEvent.click(clampedBody);

    expect(openUrlMock).not.toHaveBeenCalled();
  });

  it('does not navigate or openUrl when a link with empty url is clicked', async () => {
    // Symmetric to the empty-URL title-span guard. When `issue.url` is
    // empty, no <a> is rendered at all, so there's nothing to click —
    // but we assert `openUrl` is never called with an empty string
    // even after a click on the (now-text) title, pinning the guard
    // against a future regression that mounts a bare <a href="">.
    mockBackend({
      issues: [
        {
          number: 200,
          title: 'Mystery issue',
          body: 'No html_url from upstream.',
          url: '',
          state: 'open',
          labels: [],
          blocked_by: [],
        },
      ],
    });
    render(<GitIssuesTab />);

    const title = await screen.findByText('Mystery issue');
    await userEvent.click(title);

    expect(openUrlMock).not.toHaveBeenCalled();
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

  // ----- Blocked-by indicator (issue #481 follow-up) -----------------
  // The Issues Probe renders a red flag directly below the Spawn button
  // when an issue's `blocked_by` list contains at least one issue that's
  // still in the loaded open-issues set. The cross-reference is
  // frontend-side (no extra API call) so closed blockers / cross-repo
  // blockers / paginated blockers don't trigger the flag — see the plan
  // for the full list of limitations.
  //
  // Test selectors all pivot off the `[data-blocked-by]` attribute on
  // the flag's container so we don't depend on icon library / SVG path
  // details — keeps the test resilient to icon-style tweaks.
  // -------------------------------------------------------------------

  it('shows the blocked-by flag when a blocker is in the loaded open issues', async () => {
    // #102 is blocked by #101, which is itself in the loaded open set.
    // The flag must render on the #102 row with a tooltip mentioning #101.
    mockBackend({
      issues: [
        {
          number: 101,
          title: 'Upstream blocker',
          body: 'Body',
          url: 'https://github.com/acme/demo/issues/101',
          state: 'open',
          labels: [],
          blocked_by: [],
        },
        {
          number: 102,
          title: 'Add a /v2 endpoint',
          body: null,
          url: 'https://github.com/acme/demo/issues/102',
          state: 'open',
          labels: [],
          blocked_by: [101],
        },
      ],
    });
    render(<GitIssuesTab />);

    // The #102 row carries the flag (because #101 is in the loaded set).
    // The #101 row does NOT (because #101 is itself unblocked).
    const title102 = await screen.findByText('Add a /v2 endpoint');
    const row102 = title102.closest('[data-issue-row]')!;
    const flag102 = row102.querySelector('[data-blocked-by]') as HTMLElement;
    expect(flag102).toBeTruthy();
    // Tooltip must mention the blocking issue number so the user knows
    // what to resolve first. Title resolution (the "Upstream blocker"
    // text) is a nice-to-have; the number is the contract.
    expect(flag102.getAttribute('title')).toContain('#101');
    expect(flag102.getAttribute('title')).toMatch(/Blocked by/);

    // #101 row has no flag.
    const title101 = screen.getByText('Upstream blocker');
    const row101 = title101.closest('[data-issue-row]')!;
    expect(row101.querySelector('[data-blocked-by]')).toBeNull();
  });

  it('hides the blocked-by flag when the blocker is not in the loaded open issues', async () => {
    // #102 lists #999 as a blocker, but #999 is not in the loaded set.
    // That covers three cases the cross-reference collapses together:
    //   - blocker closed
    //   - blocker in a different repo
    //   - blocker beyond the >100-open pagination cap
    // In all three cases, the flag must NOT render — the user can't act
    // on a dependency they can't see in this view.
    mockBackend({
      issues: [
        {
          number: 102,
          title: 'Add a /v2 endpoint',
          body: null,
          url: 'https://github.com/acme/demo/issues/102',
          state: 'open',
          labels: [],
          blocked_by: [999],
        },
      ],
    });
    render(<GitIssuesTab />);

    const title = await screen.findByText('Add a /v2 endpoint');
    const row = title.closest('[data-issue-row]')!;
    expect(row.querySelector('[data-blocked-by]')).toBeNull();
  });

  it('shows "+N more" suffix in the flag tooltip when blocked_by has more than one entry', async () => {
    // Three blockers, all in the loaded set. Tooltip must surface the
    // count so the user knows there's more than one (the SVG is tiny).
    mockBackend({
      issues: [
        { number: 481, title: 'A', body: '', url: 'https://github.com/acme/demo/issues/481', state: 'open', labels: [], blocked_by: [] },
        { number: 482, title: 'B', body: '', url: 'https://github.com/acme/demo/issues/482', state: 'open', labels: [], blocked_by: [] },
        { number: 483, title: 'C', body: '', url: 'https://github.com/acme/demo/issues/483', state: 'open', labels: [], blocked_by: [] },
        {
          number: 500,
          title: 'Blocked by three',
          body: null,
          url: 'https://github.com/acme/demo/issues/500',
          state: 'open',
          labels: [],
          blocked_by: [481, 482, 483],
        },
      ],
    });
    render(<GitIssuesTab />);

    const title = await screen.findByText('Blocked by three');
    const row = title.closest('[data-issue-row]')!;
    const flag = row.querySelector('[data-blocked-by]') as HTMLElement;
    expect(flag).toBeTruthy();
    // First blocker is named in full; the rest are folded into "+N more".
    expect(flag.getAttribute('title')).toMatch(/#481/);
    expect(flag.getAttribute('title')).toMatch(/\+ ?2 more/);
  });

  it('hides the blocked-by flag on first paint, then shows it after issues resolve', async () => {
    // Regression guard: `loadedOpen` is built from the issues list, so
    // before the first IPC resolves the set is empty and EVERY row
    // would have `stillBlockedBy = []`. The cross-reference is correct
    // here (we genuinely don't know yet), but it's worth pinning so a
    // future "always show flag when blocked_by is non-empty" refactor
    // surfaces as a test failure rather than a confusing first paint.
    let resolveIssues!: (issues: GitHubIssue[]) => void;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_issues') return new Promise((res) => { resolveIssues = res as never; });
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      if (cmd === 'create_issue_node') return Promise.resolve(DRAFT);
      if (cmd === 'start_node_background') return Promise.resolve(undefined);
      return Promise.resolve({});
    });
    render(<GitIssuesTab />);

    // Loading state — no rows yet, so no flags yet (trivially).
    expect(screen.getByText(/Loading issues/i)).toBeTruthy();

    // Resolve with an issue that IS blocked by another that's also in the set.
    resolveIssues([
      {
        number: 101,
        title: 'Upstream',
        body: null,
        url: 'https://github.com/acme/demo/issues/101',
        state: 'open',
        labels: [],
        blocked_by: [],
      },
      {
        number: 102,
        title: 'Downstream',
        body: null,
        url: 'https://github.com/acme/demo/issues/102',
        state: 'open',
        labels: [],
        blocked_by: [101],
      },
    ]);

    const downstreamTitle = await screen.findByText('Downstream');
    const downstreamRow = downstreamTitle.closest('[data-issue-row]')!;
    const flag = downstreamRow.querySelector('[data-blocked-by]');
    expect(flag).toBeTruthy();
  });

  // ----- "View on GitHub" header link (mesh context menu + Issues probe) -----
  //
  // The link is a `<SafeLink>` to `{githubUrl}/issues` — a git@github.com
  // origin resolves to https://github.com/{owner}/{repo}, and the issues
  // list is the natural destination from the Issues probe. SafeLink
  // routes the click through `openUrl` (Tauri 2's `target="_blank"` is
  // silently dropped without an explicit capability we don't grant).
  // The non-GitHub case collapses to a `<span>` (SafeLink's empty-URL
  // fallback) so the layout stays stable.

  it('renders a "View on GitHub" header link to the repo issues list', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_issues') return Promise.resolve(ISSUES);
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      if (cmd === 'get_github_url_for_mesh') {
        return Promise.resolve('https://github.com/acme/demo');
      }
      return Promise.resolve({});
    });
    render(<GitIssuesTab />);

    const link = await screen.findByLabelText("Open this repo's issues list on GitHub");
    // SafeLink renders an <a> with the URL on the wire — assert on the
    // href so a future refactor that drops SafeLink for a raw <a> is
    // caught (the href is the part the browser actually uses).
    expect(link.tagName).toBe('A');
    expect(link.getAttribute('href')).toBe('https://github.com/acme/demo/issues');
    expect(link.getAttribute('target')).toBe('_blank');
  });

  it('clicking the "View on GitHub" header link opens the issues list URL', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_issues') return Promise.resolve(ISSUES);
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      if (cmd === 'get_github_url_for_mesh') {
        return Promise.resolve('https://github.com/acme/demo');
      }
      return Promise.resolve({});
    });
    render(<GitIssuesTab />);

    const link = await screen.findByLabelText("Open this repo's issues list on GitHub");
    await userEvent.click(link);

    // SafeLink routes through `openUrl` (Tauri 2 WebView drops
    // `target="_blank"` clicks without an explicit capability). The
    // URL must be the full `{base}/issues` form, NOT the bare repo URL
    // — the probe's whole purpose is the issues list, not the repo home.
    expect(openUrlMock).toHaveBeenCalledWith('https://github.com/acme/demo/issues');
  });

  it('renders an inert label (no link, no link-style cursor) when the mesh has no GitHub origin', async () => {
    // `get_github_url_for_mesh` returns `null` for non-GitHub meshes
    // (GitLab origin, no origin, etc.). SafeLink falls back to a
    // <span> for the "View on GitHub ↗" affordance — but a previous
    // implementation applied the caller's link-styling classes
    // verbatim, so the empty case LOOKED like a clickable link (cyan
    // + hover) right next to working controls, and clicking did
    // nothing. That was the user-reported "links aren't links"
    // regression on the Issues probe header. The fix strips the
    // link-styling tokens at the SafeLink level — pin the
    // empty-URL shape here so a future regression that re-applies
    // the cyan/hover classes to the fallback is caught at the seam
    // that is most directly named in the user's bug report.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_issues') return Promise.resolve(ISSUES);
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      if (cmd === 'get_github_url_for_mesh') return Promise.resolve(null);
      return Promise.resolve({});
    });
    render(<GitIssuesTab />);

    await screen.findByText('Fix the wobble');
    const fallback = await screen.findByText(/View on GitHub/);
    expect(fallback.tagName).toBe('SPAN');
    expect(fallback.getAttribute('href')).toBeNull();
    expect(fallback.getAttribute('aria-label')).toBeNull();
    // The user-reported "links aren't links" pin — visually inert,
    // no link-style className escapes through.
    expect(fallback.className).not.toMatch(/text-accent-cyan/);
    expect(fallback.className).not.toMatch(/cursor-pointer/);
    expect(fallback.className).toMatch(/cursor-default/);
  });

  // ----- Trigger-label toggle (issue #979) ---------------------------
  // The Issues Probe surfaces the mesh's configured autopilot trigger
  // label as a click-to-toggle badge under the Spawn button:
  //   - green check (✓ buildmesh:run) when the issue carries the label,
  //   - neutral plus (+ buildmesh:run) when it doesn't.
  // Clicking the badge adds/removes the label on GitHub with optimistic
  // UI + rollback on failure (see the locked design decisions #1-#5 in
  // map #979). Tests below pin the visible states, the IPC contract,
  // the confirm-on-remove gate, and the error-revert path.
  // -------------------------------------------------------------------

  it('renders the trigger-label badge in the present state when the issue already carries the label', async () => {
    // Issue #101 has `buildmesh:run` in its labels. The badge must
    // render with data-trigger-label="remove" (because clicking it will
    // trigger the remove action) and the green check glyph.
    mockBackend({
      issues: [
        {
          number: 101,
          title: 'Tagged for autopilot',
          body: 'body',
          url: 'https://github.com/acme/demo/issues/101',
          state: 'open',
          labels: ['buildmesh:run'],
          blocked_by: [],
        },
      ],
    });
    render(<GitIssuesTab />);

    const badge = await screen.findByLabelText(
      'Remove buildmesh:run label from issue #101',
    );
    expect(badge).toBeTruthy();
    // The data-trigger-label attribute encodes the next action — `remove`
    // means the label IS present (the click will remove it). Pin the
    // attribute so a future refactor that flips the action vs state
    // surfaces here rather than at the IPC seam.
    expect(badge.getAttribute('data-trigger-label')).toBe('remove');
    // The label name must be visible text so the user knows which label
    // they're toggling — the default `buildmesh:run` plus the ✓ check.
    expect(badge.textContent).toContain('buildmesh:run');
  });

  it('renders the trigger-label badge in the absent state when the issue does not carry the label', async () => {
    // Issue #101 has NO `buildmesh:run` label. The badge renders with
    // data-trigger-label="add" (next action is add) and the neutral + glyph.
    mockBackend();
    render(<GitIssuesTab />);

    const badge = await screen.findByLabelText(
      'Add buildmesh:run label to issue #101',
    );
    expect(badge).toBeTruthy();
    expect(badge.getAttribute('data-trigger-label')).toBe('add');
    expect(badge.textContent).toContain('+');
    expect(badge.textContent).toContain('buildmesh:run');
  });

  it('does not render the badge when the mesh has no trigger label configured', async () => {
    // Decision #5: the badge renders only when `autopilot_trigger_label`
    // is non-empty. A mesh with `null` (or any other falsy value) shows
    // no badge — there's no label to toggle toward.
    useMeshStore.setState({
      meshesById: new Map([[MESH.id, { ...MESH, autopilot_trigger_label: null }]]),
      selectedMeshId: MESH.id,
    });
    mockBackend();
    render(<GitIssuesTab />);

    await screen.findByText('Fix the wobble');
    expect(
      screen.queryByLabelText(/Add buildmesh:run label/),
    ).toBeNull();
    expect(
      screen.queryByLabelText(/Remove buildmesh:run label/),
    ).toBeNull();
  });

  it('clicks the absent badge → calls set_issue_label with action=add (no confirm)', async () => {
    // Add is idempotent on GitHub, so no confirm prompt — the click
    // flows straight into the IPC.
    mockBackend();
    render(<GitIssuesTab />);

    const badge = await screen.findByLabelText(
      'Add buildmesh:run label to issue #101',
    );
    await userEvent.click(badge);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('set_issue_label', {
        meshId: 42,
        issueNumber: 101,
        label: 'buildmesh:run',
        action: 'add',
      });
    });
  });

  it('flips the badge to the present state after a successful add', async () => {
    // Regression guard for the optimistic UI contract (decision #3):
    // after the IPC resolves, the badge moves from `data-trigger-label="add"`
    // to `data-trigger-label="remove"` (because clicking it next will
    // trigger the remove action). The previous test asserts only the IPC
    // shape — this one pins the visible flip so a future refactor that
    // forgets to update the optimistic override on success surfaces here.
    mockBackend();
    render(<GitIssuesTab />);

    // Initially absent — clicking it will fire action=add.
    const badge = await screen.findByLabelText(
      'Add buildmesh:run label to issue #101',
    );
    await userEvent.click(badge);

    // After the IPC resolves, the optimistic override sticks until the
    // next list refresh. The same row now reads as "Remove buildmesh:run
    // label" because the next click would remove the (optimistically
    // applied) label.
    await waitFor(() => {
      expect(
        screen.getByLabelText('Remove buildmesh:run label from issue #101'),
      ).toBeTruthy();
    });
  });

  it('clicks the present badge + confirms → calls set_issue_label with action=remove', async () => {
    // The window.confirm prompt is the destructive-action gate. We
    // stub it to accept here so the click flows into the IPC.
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true);
    mockBackend({
      issues: [
        {
          number: 101,
          title: 'Tagged',
          body: 'body',
          url: 'https://github.com/acme/demo/issues/101',
          state: 'open',
          labels: ['buildmesh:run'],
          blocked_by: [],
        },
      ],
    });
    render(<GitIssuesTab />);

    const badge = await screen.findByLabelText(
      'Remove buildmesh:run label from issue #101',
    );
    await userEvent.click(badge);

    expect(confirmSpy).toHaveBeenCalledTimes(1);
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('set_issue_label', {
        meshId: 42,
        issueNumber: 101,
        label: 'buildmesh:run',
        action: 'remove',
      });
    });
    confirmSpy.mockRestore();
  });

  it('cancels the confirm on remove → no IPC, no state change', async () => {
    // The destructive-action gate's whole point: a click that the user
    // didn't mean must not reach the network. Verify the IPC is silent
    // and the badge stays in the present state.
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(false);
    mockBackend({
      issues: [
        {
          number: 101,
          title: 'Tagged',
          body: 'body',
          url: 'https://github.com/acme/demo/issues/101',
          state: 'open',
          labels: ['buildmesh:run'],
          blocked_by: [],
        },
      ],
    });
    render(<GitIssuesTab />);

    const badge = await screen.findByLabelText(
      'Remove buildmesh:run label from issue #101',
    );
    await userEvent.click(badge);

    expect(confirmSpy).toHaveBeenCalledTimes(1);
    expect(invoke).not.toHaveBeenCalledWith('set_issue_label', expect.anything());
    // The badge is still in the present state — no flip happened.
    expect(
      screen.queryByLabelText('Remove buildmesh:run label from issue #101'),
    ).toBeTruthy();
    confirmSpy.mockRestore();
  });

  it('reverts + calls addToast when set_issue_label rejects', async () => {
    // The optimistic flip on click is followed by an IPC failure (the
    // backend surfaces the 422 → LabelNotFound message verbatim). The
    // badge must revert to its pre-click state (decision #3 rollback)
    // and the failure must reach the shared toast pipeline (issue #1001).
    //
    // Pre-#1001 this asserted `data-trigger-label-error` rendered inline
    // below the badge; the toast pipeline is now the single source of
    // truth for IPC failure surfacing across the app, so we assert the
    // wrapper was called with the exact provider + backend message +
    // severity. The toast itself is rendered by App.tsx — covered by
    // `tests/unit/toast-store.test.ts` and the visual layer (not
    // asserted here).
    const errorMessage =
      'Label `buildmesh:run` doesn\'t exist on the repo — create it on GitHub first';
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'set_issue_label') return Promise.reject(new Error(errorMessage));
      if (cmd === 'get_repo_issues') return Promise.resolve(ISSUES);
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      return Promise.resolve({});
    });
    render(<GitIssuesTab />);

    // Initially: issue 102 carries no buildmesh:run label, so the badge
    // is in the absent state with action=add.
    const addBadge = await screen.findByLabelText(
      'Add buildmesh:run label to issue #102',
    );
    await userEvent.click(addBadge);

    // IPC rejects → the optimistic flip reverts → badge returns to
    // absent (Add buildmesh:run label ...).
    await waitFor(() => {
      expect(
        screen.getByLabelText('Add buildmesh:run label to issue #102'),
      ).toBeTruthy();
    });

    // Issue #1001: the failure reaches the shared toast pipeline with
    // the backend's verbatim message. The wrapper is mocked at module
    // load — the production code's `import { addToast }` resolves to
    // `addToastMock`, captured via `vi.mocked(addToast)` so the
    // assertion carries the same type as the production call site.
    expect(vi.mocked(addToast)).toHaveBeenCalledWith('GitHub', errorMessage, 'error');
  });

  it('disables the badge while a toggle is in flight to block double-clicks', async () => {
    // While the IPC is pending, the badge must be disabled so a second
    // click can't race the in-flight write. The guard mirrors the
    // spawn-button's `disabled={spawning !== null}` pattern.
    let resolveSet!: (v: void) => void;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_issues') return Promise.resolve(ISSUES);
      if (cmd === 'list_providers') return Promise.resolve(PROVIDERS);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      if (cmd === 'set_issue_label') return new Promise<void>((res) => { resolveSet = res; });
      return Promise.resolve({});
    });
    render(<GitIssuesTab />);

    const badge = await screen.findByLabelText(
      'Add buildmesh:run label to issue #101',
    ) as HTMLButtonElement;
    expect(badge.disabled).toBe(false, 'badge starts enabled');
    await userEvent.click(badge);

    // Re-query: the same label is reused after the optimistic flip
    // (which moves the badge to data-trigger-label="remove"), but we
    // only care about the disabled state of the toggle button on this
    // row. The data-pending attribute is the test seam — pin it AND
    // the disabled attribute so a future refactor that drops one is
    // caught.
    const pendingBadge = await waitFor(() => {
      const el = document.querySelector(
        '[data-trigger-label][data-pending="true"]',
      ) as HTMLButtonElement | null;
      if (!el) throw new Error('badge not yet marked pending');
      return el;
    });
    expect(pendingBadge.disabled).toBe(true);

    // Resolve the in-flight IPC; the badge clears its pending state.
    resolveSet();
    await waitFor(() => {
      const el = document.querySelector(
        '[data-trigger-label][data-pending="true"]',
      );
      expect(el).toBeNull();
    });
  });
});
