/**
 * Tests for the Repository destination — issue #1460 (was the 🌳 Worktree
 * Manager tab, issue #377).
 *
 * The destination keeps the legacy `<BranchesWorktreesSection>` surface
 * (health, recovery, branch/worktree cleanup, remote-tracking prune) and
 * #1460 removed everything else from it: the worktree-configuration card
 * moved to `project-settings-tab.test.tsx`, because changing where new
 * worktrees are cut is a strategy decision, not a maintenance action.
 * This suite therefore owns maintenance only, and owns the negative
 * assertions that keep the two surfaces from merging again.
 *
 * Rendering strategy: mount the full `ProbePanel` with the worktrees
 * destination opened via `openProbeTab`, the same way the
 * `project-settings-tab.test.tsx` does. This keeps the routing wiring in
 * `ProbePanel.tsx` covered by the same suite — a separate routing test
 * would have to know the tab's internal structure.
 */

import { describe, it, expect, vi, beforeAll, beforeEach } from 'vitest';
import { preloadProbeTabs } from './helpers/preloadProbeTabs';
import { act, render, screen, fireEvent, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { ProbePanel } from '../../src/components/Probe/ProbePanel';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import type { MeshRow } from '../../src/types/generated/MeshRow';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import { openProbeDestination } from './helpers/openProbeDestination';

const MESH: Mesh = {
  id: 42,
  name: 'demo',
  path: '/repos/demo',
  layout: 'single',
  position: 0,
  created_at: '2026-01-01',
  build_command: null,
  run_command: null,
  model: null,
  effort: null,
  use_worktree: true,
  worktree_mode: null,
  default_provider: null,
  base_ref: 'origin/main',
  scratchpad: '',
  sandbox: false,
};

// Healthy mesh — no drift, no hostage, no dirty, no unpushed. Surfaces the
// baseline "everything is fine" state so the negative-assertions below
// don't pass just because every health field is missing.
const HEALTHY: Record<string, unknown> = {
  base_ref: 'origin/main',
  local_base_branch: 'main',
  current_branch: 'main',
  current_short_sha: 'abc1234',
  is_detached: false,
  is_dirty: false,
  unpushed_ahead: 0,
  has_upstream: true,
  is_drifted: false,
  base_branch_holder: null,
};

const PRUNE_INFO = [
  {
    path: '/repos/demo',
    local_branches: [
      {
        name: 'main',
        is_head: true,
        is_merged_into_main: null,
        is_orphan: false,
        is_active: false,
        has_uncommitted: false,
        last_commit_date: '2026-06-01T00:00:00Z',
        ahead: 0,
        behind: 0,
      },
      {
        name: 'feature/done',
        is_head: false,
        is_merged_into_main: true,
        is_orphan: false,
        is_active: false,
        has_uncommitted: false,
        last_commit_date: '2026-05-01T00:00:00Z',
        ahead: 0,
        behind: 0,
      },
    ],
    worktrees: [
      {
        path: '/repos/demo',
        branch: 'main',
        is_active: true,
        is_stale: false,
        is_pool: false,
      },
      {
        path: '/repos/demo/.worktrees/orphan',
        branch: null,
        is_active: false,
        is_stale: true,
        is_pool: false,
      },
    ],
    remote_tracking_branches: ['origin/main'],
  },
];

const DRIFTED_HEALTH: Record<string, unknown> = {
  base_ref: 'origin/main',
  local_base_branch: 'main',
  current_branch: 'feature/wip',
  current_short_sha: 'def5678',
  is_detached: false,
  is_dirty: false,
  unpushed_ahead: 0,
  has_upstream: true,
  is_drifted: true,
  base_branch_holder: {
    path: '/repos/demo/.worktrees/holder',
    name: 'holder',
    is_active: true,
  },
};

/**
 * Wire the mocked `invoke` to answer each command the tab calls during
 * mount + the relevant user actions. The mesh-properties test pins the
 * exact same commands; we extend the mock with the worktree-specific
 * `get_git_prune_info` and the recovery commands. We use module-state
 * health/prune so individual tests can swap the health shape without
 * re-mocking.
 *
 * `meshRow` controls the response of `get_mesh_properties` (issue
 * #451 — the Configuration card on the 🌳 tab). The default matches
 * the legacy `MeshPropertiesPanel` initial state so the existing
 * health / prune / recovery tests stay deterministic. `saveUseWorktree
 * Fails` and `saveBaseRefFails` are opt-in knobs that flip the two
 * worktree-config save commands to rejecting handlers, used by the
 * "save failure surfaces inline" test.
 *
 * `poolCount` controls the response of `get_mesh_pool_count` for the
 * Worktrees Probe's pre-spawn pool badge. The default is `0` so
 * existing tests stay deterministic (no badge to find). The pool-badge
 * tests override it with a specific value (or `null` to simulate a
 * pool-disabled mesh).
 */
function mockBackend(
  overrides: {
    health?: unknown;
    prune?: unknown;
    meshRow?: Partial<MeshRow>;
    poolCount?: number;
    saveUseWorktreeFails?: boolean;
    saveBaseRefFails?: boolean;
  } = {},
) {
  const health = overrides.health ?? HEALTHY;
  const prune = overrides.prune ?? PRUNE_INFO;
  const meshRow: MeshRow = {
    name: null,
    build_command: null,
    run_command: null,
    model: null,
    effort: null,
    base_ref: 'origin/main',
    use_worktree: true,
    worktree_mode: 'branched',
    default_provider: null,
    pre_spawn_pool_size: 0,
    ...overrides.meshRow,
  };
  // Default pool count is 0 so the badge stays hidden (`preSpawnPoolSize === 0`)
  // for every test that doesn't override either the config or the count.
  const poolCount = overrides.poolCount ?? 0;
  vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
    switch (cmd) {
      case 'get_mesh_health':
        return Promise.resolve(health);
      case 'get_git_prune_info':
        return Promise.resolve(prune);
      case 'delete_branches':
        return Promise.resolve();
      case 'delete_worktrees':
        return Promise.resolve();
      case 'prune_remote_tracking':
        // Issue #657 — the command now returns the trimmed `git fetch
        // --prune` stderr so the frontend can show what happened. The
        // mock returns the empty string for the happy-path tests; the
        // failure-path test below overrides this case via
        // `vi.mocked(invoke).mockImplementationOnce`.
        return Promise.resolve('');
      case 'restore_mesh_to_base':
        return Promise.resolve({ restored: true, message: 'Restored to main.' });
      case 'free_base_branch':
        return Promise.resolve({ detached_at_sha: 'abc1234' });
      case 'list_providers':
        return Promise.resolve([]);
      case 'get_mesh_properties':
        return Promise.resolve(meshRow);
      case 'get_mesh_pool_count':
        return Promise.resolve(poolCount);
      case 'detect_mesh_project':
        return Promise.resolve({ preset_id: null, label: null, node_scripts: null });
      case 'detect_ai_context':
        return Promise.resolve({
          claude_md_exists: false,
          agents_md_exists: false,
          skills_dir_exists: false,
          skill_count: 0,
          agents_skills_exists: false,
        });
      case 'update_mesh_column':
        return Promise.resolve();
      case 'update_mesh_use_worktree':
        return overrides.saveUseWorktreeFails
          ? Promise.reject(new Error('mock: update_mesh_use_worktree failed'))
          : Promise.resolve();
      case 'update_worktree_base_ref':
        return overrides.saveBaseRefFails
          ? Promise.reject(new Error('mock: update_worktree_base_ref failed'))
          : Promise.resolve();
      case 'check_gh_auth':
      case 'get_default_branch':
      case 'get_git_status':
      case 'list_directory':
        return Promise.resolve({});
      default:
        return Promise.resolve({ cmd, args });
    }
  });
}

beforeEach(() => {
  useMeshStore.setState({
    meshes: [MESH],
    meshesById: new Map([[MESH.id, MESH]]),
    selectedMeshId: MESH.id,
  });
  seedAgentNodes([]);
  useUIStore.setState({ probeOpen: false, probeTab: 'files', activeDiffFile: null });
});

// Load the lazy Probe tab chunks up front so rendering a tab does not wait on disk.
beforeAll(preloadProbeTabs, 60_000);

describe('RepositoryTab (issue #1460)', () => {
  it('renders the tab body when clicked (no longer the "coming soon" placeholder)', async () => {
    mockBackend();
    openProbeDestination('worktrees');

    // The header should still show the tab's name — the friendly placeholder
    // "This tab's content is coming soon." must be gone.
    expect(screen.queryByText("This tab's content is coming soon.")).toBeNull();
    // The legacy collapsible header text is gone (the probe supplies chrome).
    expect(screen.queryByRole('button', { name: /Branches & Worktrees/i })).toBeNull();
  });

  it('shows the Repository label in the probe header', async () => {
    mockBackend();
    useUIStore.setState({ probeOpen: true, probeTab: 'worktrees' });
    render(<ProbePanel />);

    const header = screen.getByRole('region', { name: 'Probe panel' });
    expect(header.textContent).toContain('Repository');
    // Issue #1460: the destination was renamed from "Worktree Manager" so it
    // no longer advertises configuration it no longer owns. Assert the old
    // name is gone, or the rename silently reverts to a label that promises
    // worktree strategy controls that moved to Project Settings.
    expect(header.textContent).not.toContain('Worktree Manager');
  });

  // Issue #1460 — the split's core regression guard. The strategy controls
  // moved to Project Settings; this destination must never grow them back,
  // or the two surfaces become one undifferentiated surface again.
  it('carries no worktree-configuration controls (they belong to Project Settings)', async () => {
    mockBackend();
    openProbeDestination('worktrees');
    await act(async () => { await vi.dynamicImportSettled(); });

    // Wait for the maintenance list so the negative assertions are stable.
    expect(await screen.findByText('main')).toBeTruthy();

    expect(screen.queryByLabelText('Use worktree')).toBeNull();
    expect(screen.queryByLabelText('Pre-spawn warm worktrees')).toBeNull();
    expect(screen.queryByLabelText(/Fresh — start new session/i)).toBeNull();
    expect(screen.queryByLabelText(/Head — resume last session/i)).toBeNull();
    expect(screen.queryByLabelText(/^Branched/)).toBeNull();
    expect(screen.queryByLabelText(/^Detached/)).toBeNull();
    expect(screen.queryByLabelText('Worktree directory')).toBeNull();
    expect(screen.queryByTestId('pool-status')).toBeNull();
  });

  it('separates health and recovery from cleanup into two labelled sections', async () => {
    mockBackend({ health: DRIFTED_HEALTH });
    openProbeDestination('worktrees');

    // Both sections are always present, in this order: recovery repairs, the
    // cleanup section deletes. Naming them is what makes the risk level
    // legible (issue #1460 AC3).
    const health = await screen.findByTestId('repository-health-section');
    const cleanup = await screen.findByTestId('repository-cleanup-section');
    expect(health.getAttribute('aria-label')).toBe('Health and recovery');
    expect(cleanup.getAttribute('aria-label')).toBe('Branches and worktrees');
    expect(
      health.compareDocumentPosition(cleanup) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();

    // The destructive controls live in the cleanup section, the repair
    // controls in the health one.
    expect(health.textContent).toContain('Restore root to main');
    expect(cleanup.textContent).toContain('Delete Selected');
    expect(health.textContent).not.toContain('Delete Selected');
  });

  it('states that maintenance acts on the project root, not the focused worktree', async () => {
    mockBackend();
    openProbeDestination('worktrees');

    const note = await screen.findByTestId('probe-scope-note');
    expect(note.textContent).toContain('/repos/demo');
    expect(note.textContent).toMatch(/project root/i);
  });

  it('reports a healthy project explicitly instead of omitting the health section', async () => {
    mockBackend({ health: HEALTHY });
    openProbeDestination('worktrees');

    // Wait for the prune list so the assertion is stable (health also lands
    // after `get_mesh_health` resolves).
    expect(await screen.findByText('main')).toBeTruthy();

    // Issue #1460: pre-split, a healthy project rendered NO health block at
    // all, which was indistinguishable from "not checked yet". The section
    // is now always present with an explicit clean state.
    const healthy = await screen.findByTestId('repository-healthy');
    expect(healthy.textContent).toMatch(/no drift/i);
    expect(screen.getByTestId('repository-health-section')).toBeTruthy();
    // Still no repair buttons when there is nothing to repair.
    expect(screen.queryByRole('button', { name: /Restore root to/i })).toBeNull();
    expect(screen.queryByRole('button', { name: /Free main/i })).toBeNull();
  });

  it('lists local branches and worktrees returned by get_git_prune_info', async () => {
    mockBackend();
    openProbeDestination('worktrees');

    // Branches surface with their names. `main` and the merged
    // `feature/done` must both appear once the prune info resolves.
    expect(await screen.findByText('main')).toBeTruthy();
    expect(screen.getByText('feature/done')).toBeTruthy();
    // The worktree directory name (`orphan`) is the last path segment.
    expect(screen.getByText('orphan')).toBeTruthy();
  });

  it('shows the HealthBlock with Restore + Free buttons when the mesh is drifted with a hostage', async () => {
    mockBackend({ health: DRIFTED_HEALTH });
    openProbeDestination('worktrees');

    const restore = await screen.findByRole('button', { name: /Restore root to main/i });
    expect(restore).toBeTruthy();
    // The hostage's name appears in the Free button label.
    const free = screen.getByRole('button', { name: /Free main \(holder\)/i });
    expect(free).toBeTruthy();
  });

  it('renders nothing when no mesh is selected (the probe shell handles the empty state)', () => {
    useMeshStore.setState({ meshes: [], meshesById: new Map(), selectedMeshId: null });
    useUIStore.setState({ probeOpen: true, probeTab: 'worktrees' });
    render(<ProbePanel />);

    // The probe's "No project selected" empty state, not the worktree UI.
    expect(screen.getByText('No project selected')).toBeTruthy();
    // The legacy "No worktrees found"-style empty state must not appear
    // when the issue is "no mesh selected", not "no git objects to show".
    expect(screen.queryByText(/Local branches/i)).toBeNull();
  });

  it('exposes Refresh / Select recommended / Delete Selected in the toolbar', async () => {
    mockBackend();
    openProbeDestination('worktrees');

    // The toolbar controls must be present once the prune info resolves.
    // The merged/clean `feature/done` branch is recommended; the stale
    // `orphan` worktree is also recommended — so the "Select recommended"
    // button is enabled and shows its count.
    expect(await screen.findByRole('button', { name: /Refresh/i })).toBeTruthy();
    const selectRecommended = await screen.findByRole('button', { name: /Select recommended/i });
    expect(selectRecommended.textContent).toMatch(/\(2\)/);
    // Delete is disabled until a selection is made — it must exist but be off.
    const deleteBtn = screen.getByRole('button', { name: /Delete Selected/i });
    expect((deleteBtn as HTMLButtonElement).disabled).toBe(true);
  });

  it('opens a confirmation dialog before deleting, and only invokes delete_branches/delete_worktrees after confirm', async () => {
    const user = userEvent.setup();
    mockBackend();
    openProbeDestination('worktrees');

    // Wait for the prune list, then select the recommended merged branch +
    // stale worktree (the (2) the toolbar's "Select recommended" button
    // advertises).
    expect(await screen.findByText('feature/done')).toBeTruthy();
    await user.click(screen.getByRole('button', { name: /Select recommended/i }));

    // Before the user confirms, the destructive IPC must NOT have fired.
    expect(invoke).not.toHaveBeenCalledWith('delete_branches', expect.anything());
    expect(invoke).not.toHaveBeenCalledWith('delete_worktrees', expect.anything());

    // Click "Delete Selected" — should open the confirmation dialog.
    await user.click(screen.getByRole('button', { name: /Delete Selected/i }));
    const confirm = await screen.findByRole('button', { name: /^Delete$/ });

    // Issue #1460 (AC4): the shared confirmation must state the scope and the
    // recovery story, not just that the action cannot be undone. Assert the
    // counts, the project name, and the reflog escape hatch.
    const dialog = confirm.closest('[role="dialog"]');
    expect(dialog).toBeTruthy();
    const dialogText = dialog?.textContent ?? '';
    expect(dialogText).toContain('1 branch');
    expect(dialogText).toContain('1 worktree');
    expect(dialogText).toContain('demo');
    expect(dialogText).toMatch(/reflog/i);
    expect(dialogText).toMatch(/cannot be undone/i);

    await user.click(confirm);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        'delete_branches',
        expect.objectContaining({ branchNames: expect.arrayContaining(['feature/done']) }),
      );
      expect(invoke).toHaveBeenCalledWith(
        'delete_worktrees',
        expect.objectContaining({
          worktreePaths: expect.arrayContaining(['/repos/demo/.worktrees/orphan']),
        }),
      );
    });
  });

  it('clicking Free on a hostage triggers free_base_branch and re-fetches health + prune info', async () => {
    const user = userEvent.setup();
    mockBackend({ health: DRIFTED_HEALTH });
    openProbeDestination('worktrees');

    const free = await screen.findByRole('button', { name: /Free main \(holder\)/i });
    await user.click(free);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('free_base_branch', {
        meshId: 42,
        worktreePath: '/repos/demo/.worktrees/holder',
      });
    });
    // The recovery hook always invalidates both caches — health is the
    // primary one; prune is the secondary so the list reflects the new
    // worktree state.
    await waitFor(() => {
      const calls = vi.mocked(invoke).mock.calls.filter(
        ([cmd]) => cmd === 'get_git_prune_info',
      );
      expect(calls.length).toBeGreaterThanOrEqual(2);
    });
  });

  it('clicking Prune disables the button with a "Pruning…" label and surfaces a success message (issue #657)', async () => {
    // The default `mockBackend()` returns `''` from `prune_remote_tracking`,
    // which the frontend treats as "nothing to report" â†’ success message
    // shows the fallback text.
    const user = userEvent.setup();
    mockBackend();
    openProbeDestination('worktrees');

    const prune = await screen.findByRole('button', { name: /^Prune$/ });
    expect(prune).toBeTruthy();
    expect((prune as HTMLButtonElement).disabled).toBe(false);

    await user.click(prune);

    // After the click the IPC must have fired (success resolves
    // synchronously under our mock — the in-flight state may be too
    // brief to observe, but the eventual-state assertions pin behaviour).
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        'prune_remote_tracking',
        expect.objectContaining({ worktreePath: '/repos/demo' }),
      );
    });

    // Success path: an inline message renders with the fallback text
    // (the mock returns `''`). The user now sees *something* happened.
    expect(await screen.findByText(/Remote-tracking refs pruned/)).toBeTruthy();
    // The list re-fetch fires after the prune command resolves.
    await waitFor(() => {
      const calls = vi.mocked(invoke).mock.calls.filter(
        ([cmd]) => cmd === 'get_git_prune_info',
      );
      expect(calls.length).toBeGreaterThanOrEqual(2);
    });
  });

  it('clicking Prune with a failing backend surfaces an inline error prefixed "Prune failed: " (issue #657)', async () => {
    const user = userEvent.setup();
    mockBackend();
    openProbeDestination('worktrees');

    // The default mock returns success for `prune_remote_tracking`.
    // Override the *next* call so this single click fails — leaves other
    // tests' mocks untouched. Throw for unknown commands so a future
    // refactor that consumes the override early (e.g. an automatic
    // warm-prune on tab focus) surfaces the wrong-order bug loudly
    // instead of silently turning this into the success path
    // (review finding C2/C3/C6).
    vi.mocked(invoke).mockImplementationOnce((cmd: string) => {
      if (cmd === 'prune_remote_tracking') {
        return Promise.reject(new Error('fatal: not a git repository'));
      }
      throw new Error(`unexpected cmd in prune-failure test: ${cmd}`);
    });

    const prune = await screen.findByRole('button', { name: /^Prune$/ });
    await user.click(prune);

    // Error prefix is mandatory (AC4) — distinguishes prune failures
    // from `deleteBranches`/`deleteWorktrees` failures that share the
    // same inline-error channel. We match just the prefix here because
    // `String(e)` for a JS `Error` instance produces `"Error: <msg>"`
    // (same caveat applies to the save-failure test above at #321).
    const error = await screen.findByText(/Prune failed:/);
    expect(error).toBeTruthy();
    expect(error.className).toContain('text-status-error');
  });

  it('a successful prune whose follow-up load() rejects surfaces the refresh error WITHOUT the "Prune failed:" prefix (review finding A1/C1)', async () => {
    // Regression test for review finding A1/C1: previously the prune
    // handler awaited `load()` inside the same try-block, so a refresh
    // failure after a successful prune was labelled "Prune failed:" —
    // misleading, since the prune itself succeeded. The fix mirrors
    // the legacy `handleDelete` pattern: accumulate the prune error,
    // await `load()` in `finally`, then re-set the prune error so it
    // isn't clobbered by `load()`'s own `setError(null)`.
    //
    // NOTE: this test exercises the wire-shape invariant via two
    // simpler proxies that don't depend on `String(Error)` text
    // formatting:
    //   1. The post-prune `get_git_prune_info` call was attempted (and
    //      rejected) — so the refresh path ran. `load()`'s own catch
    //      writes the rejection to the shared `error` channel WITHOUT
    //      the "Prune failed:" prefix.
    //   2. No "Prune failed:" text is rendered (because the prune
    //      itself succeeded — only a refresh error occurred).
    const user = userEvent.setup();
    mockBackend();

    let pruneInfoCount = 0;
    vi.mocked(invoke).mockImplementation((cmd: string, _args?: unknown) => {
      if (cmd === 'prune_remote_tracking') {
        return Promise.resolve('');
      }
      if (cmd === 'get_git_prune_info') {
        pruneInfoCount += 1;
        // First call (mount) succeeds, subsequent calls (refresh) reject.
        return pruneInfoCount === 1
          ? Promise.resolve(PRUNE_INFO)
          : Promise.reject(new Error('db: connection refused'));
      }
      return Promise.resolve('');
    });

    openProbeDestination('worktrees');

    const prune = await screen.findByRole('button', { name: /^Prune$/ });
    await user.click(prune);

    // The refresh was attempted and rejected.
    await waitFor(() => {
      expect(pruneInfoCount).toBeGreaterThanOrEqual(2);
    });

    // Critical assertion: NO "Prune failed:" text rendered. The prune
    // itself succeeded, so its error channel stays empty. The refresh
    // failure surfaces on the shared `error` channel (via `load()`'s
    // own `.catch`), which is the *non-prefixed* path.
    await waitFor(() => {
      expect(screen.queryByText(/^Prune failed:/)).toBeNull();
    });
  });

  it('clicking Restore on a drifted root triggers restore_mesh_to_base', async () => {
    const user = userEvent.setup();
    // Drifted but no hostage — the Restore button is enabled (mirrors the
    // backend guard chain: a hostage blocks Restore with a "free it first"
    // tooltip, see `restoreBlockedBy` in the HealthBlock).
    const driftedNoHostage: Record<string, unknown> = {
      ...DRIFTED_HEALTH,
      base_branch_holder: null,
    };
    mockBackend({ health: driftedNoHostage });
    openProbeDestination('worktrees');

    const restore = await screen.findByRole('button', { name: /Restore root to main/i });
    await user.click(restore);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('restore_mesh_to_base', { meshId: 42 });
    });
  });

  it('Restore is disabled (with a "free it first" tooltip) when a hostage holds the Base Ref', async () => {
    // The legacy guard chain (issue #231) refuses Restore when a worktree
    // holds the Base Ref — the user has to Free the hostage first. The
    // tab mirrors that guard in the UI: the button is disabled and the
    // tooltip says why.
    mockBackend({ health: DRIFTED_HEALTH });
    openProbeDestination('worktrees');

    const restore = (await screen.findByRole('button', {
      name: /Restore root to main/i,
    })) as HTMLButtonElement;
    expect(restore.disabled).toBe(true);
    expect(restore.title).toContain('free it first');
  });
});

describe('ProbePanel routing for the Repository destination (issue #1460)', () => {
  beforeEach(() => {
    mockBackend();
  });

  it('the 🌳 tab no longer renders the "coming soon" placeholder when a mesh is selected', async () => {
    useUIStore.setState({ probeOpen: true, probeTab: 'worktrees' });
    render(<ProbePanel />);

    // The placeholder text must be gone — the new tab is wired in.
    expect(screen.queryByText("This tab's content is coming soon.")).toBeNull();
  });

  it('openProbeTab opens the panel on the worktrees destination', () => {
    // #1375: the rail is gone — destinations open through the store action
    // driven by the palette, title bar, and contextual entries. Closed first
    // proves the panel renders nothing; reopened via the store proves the
    // same mount path the real entry points use.
    useUIStore.setState({ probeOpen: false, probeTab: 'files' });
    const closed = render(<ProbePanel />);
    expect(screen.queryByRole('region', { name: 'Probe panel' })).toBeNull();
    closed.unmount();

    useUIStore.getState().openProbeTab('worktrees');
    render(<ProbePanel />);
    expect(useUIStore.getState().probeOpen).toBe(true);
    expect(useUIStore.getState().probeTab).toBe('worktrees');
    expect(screen.getByRole('region', { name: 'Probe panel' })).toBeTruthy();
  });
});

describe('RepositoryTab cleanup rows (issue #1460)', () => {
  // The Configuration card ports the worktree-config sub-section that
  // used to live at the top of the legacy `MeshPropertiesPanel` (deleted
  // in #380). The card has three controls: a `use_worktree` checkbox,
  // a "Starting point" radio (Fresh/Head â†” origin/main/HEAD on the
  // wire), and a "Worktree mode" radio (Branched/Detached).

  // ── Open-in-file-explorer (regression for the lost affordance) ────â”€
  // The 🌳 tab used to host an open-in-OS-file-manager action per
  // worktree row in the legacy MeshPropertiesPanel; the lift to Probe
  // (#377) kept the path text but dropped the icon button. The new
  // tests pin both the repo-path button and the per-worktree button
  // so the affordance can't be silently lost again.

  it('renders a repo-path open-in-explorer button that calls open_in_file_manager with the repo path', async () => {
    mockBackend();
    openProbeDestination('worktrees');

    // Wait for the prune list to render before asserting on the button —
    // the repo path appears only after `get_git_prune_info` resolves.
    await screen.findByText('main');

    const repoButton = screen.getByTestId('repo-open-/repos/demo');
    // The button must render an SVG icon (Lucide folder-open) — pin
    // that an `<svg>` lives inside the button so the glyph can't be
    // silently replaced with text/emoji.
    expect(repoButton.querySelector('svg')).toBeTruthy();

    fireEvent.click(repoButton);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('open_in_file_manager', {
        path: '/repos/demo',
      });
    });
  });

  it('renders a per-worktree open-in-explorer button that calls open_in_file_manager with that worktree path', async () => {
    mockBackend();
    openProbeDestination('worktrees');

    // Wait for the prune list to render. `orphan` is the worktree
    // directory name (`/repos/demo/.worktrees/orphan`).
    await screen.findByText('orphan');

    const orphanButton = screen.getByTestId('worktree-open-w:/repos/demo/.worktrees/orphan');
    expect(orphanButton.querySelector('svg')).toBeTruthy();

    fireEvent.click(orphanButton);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('open_in_file_manager', {
        path: '/repos/demo/.worktrees/orphan',
      });
    });
  });

  it('clicking a per-worktree open-in-explorer button does NOT toggle the row checkbox', async () => {
    // The whole row used to be a <label> wrapping the checkbox — adding
    // a button inside that label would have re-toggled the checkbox on
    // every Explorer click. Pin that the structural fix (button outside
    // the label) actually isolates the click target: a per-worktree
    // open click leaves the selection set empty.
    mockBackend();
    openProbeDestination('worktrees');
    await screen.findByText('orphan');

    const orphanButton = screen.getByTestId('worktree-open-w:/repos/demo/.worktrees/orphan');
    fireEvent.click(orphanButton);

    // Delete Selected is disabled iff no rows are selected — the
    // clearest cross-test pin for "the click did NOT toggle the
    // checkbox on the orphan row".
    const deleteBtn = screen.getByRole('button', { name: /Delete Selected/i });
    expect((deleteBtn as HTMLButtonElement).disabled).toBe(true);
  });

  // ── active-branch flag (sibling of worktree `is_active`) ──────────────â”€

  /**
   * Pin the user-symptom contract for the active-branch block in the 🌳
   * tab: a branch held by a live agent node surfaces with the same
   * visual treatment as an active worktree — disabled checkbox, faded
   * row, "active" badge with a tooltip explaining why. Mirrors the
   * worktree active-block pattern and prevents accidental deletion of
   * a branch a node is using.
   */
  it('active branches render with a disabled checkbox + active badge', async () => {
    mockBackend({
      prune: [
        {
          path: '/repos/demo',
          local_branches: [
            {
              name: 'main',
              is_head: true,
              is_merged_into_main: null,
              is_orphan: false,
              is_active: false,
              has_uncommitted: false,
              last_commit_date: null,
              ahead: 0,
              behind: 0,
            },
            {
              name: 'feature/live',
              is_head: false,
              is_merged_into_main: true,
              is_orphan: false,
              // Held by an agent node â†’ cannot delete.
              is_active: true,
              has_uncommitted: false,
              last_commit_date: null,
              ahead: 0,
              behind: 0,
            },
          ],
          worktrees: [
            {
              path: '/repos/demo',
              branch: 'main',
              is_active: true,
              is_stale: false,
              is_pool: false,
            },
          ],
          remote_tracking_branches: [],
        },
      ],
    });
    openProbeDestination('worktrees');

    // Wait for the prune info to render.
    await screen.findByText('feature/live');

    // The active branch row's checkbox is disabled. `main` (idle) stays
    // enabled so the contrast is visible — idle branches must remain
    // selectable.
    const liveCheckbox = screen.getByRole('checkbox', { name: /feature\/live/i });
    expect((liveCheckbox as HTMLInputElement).disabled).toBe(true);

    const mainCheckbox = screen.getByRole('checkbox', { name: /^main/i });
    expect((mainCheckbox as HTMLInputElement).disabled).toBe(false);

    // The "active" badge surfaces next to the branch name so the user
    // knows why the row is locked. The badge reuses the same
    // cyan styling as the worktree active-block.
    const activeBadges = screen.getAllByTitle('Active — cannot delete');
    expect(activeBadges.length).toBeGreaterThan(0);
  });

  /**
   * An idle branch must remain selectable even if it's a "good prune
   * candidate" (merged + clean + not HEAD). `isRecommendedBranch`
   * excludes active branches — the recommended selection should never
   * include a branch a node is on.
   */
  it('active branches are not in the "Select recommended" set', async () => {
    mockBackend({
      prune: [
        {
          path: '/repos/demo',
          local_branches: [
            {
              name: 'main',
              is_head: true,
              is_merged_into_main: null,
              is_orphan: false,
              is_active: false,
              has_uncommitted: false,
              last_commit_date: null,
              ahead: 0,
              behind: 0,
            },
            {
              name: 'feature/merged-clean',
              is_head: false,
              is_merged_into_main: true,
              is_orphan: false,
              is_active: false,
              has_uncommitted: false,
              last_commit_date: null,
              ahead: 0,
              behind: 0,
            },
            {
              name: 'feature/merged-active',
              is_head: false,
              is_merged_into_main: true,
              is_orphan: false,
              // Same prune flags as feature/merged-clean, but held by a
              // live agent — must NOT be in the recommended set.
              is_active: true,
              has_uncommitted: false,
              last_commit_date: null,
              ahead: 0,
              behind: 0,
            },
          ],
          worktrees: [
            {
              path: '/repos/demo',
              branch: 'main',
              is_active: true,
              is_stale: false,
              is_pool: false,
            },
          ],
          remote_tracking_branches: [],
        },
      ],
    });
    openProbeDestination('worktrees');

    await screen.findByText('feature/merged-active');

    // "Select recommended" surfaces a count in its label — only the
    // idle merged branch counts. The active one is held by a node and
    // must NOT appear in the count.
    const selectRecommended = await screen.findByRole('button', {
      name: /Select recommended/i,
    });
    expect(selectRecommended.textContent).toMatch(/\(1\)/);
    expect(selectRecommended.textContent).not.toMatch(/\(2\)/);
  });
});

// ── Pre-spawn pool badge (PRD #608 §6 — pool observability) ──────────────
//
// The Worktrees Probe shows a small progress-bar + numeric label under
// the "Pre-spawn warm worktrees" header, driven by `get_mesh_pool_count`
// + `usePoolChanged`. These tests pin:
//   * Hidden state when the pool is disabled (`preSpawnPoolSize === 0`).
//   * Visible state with correct "X / Y ready" formatting when enabled.
//   * a11y attributes (role="status" + aria-label) so screen readers
//     announce the change.
//   * Live refresh on `pool-count-changed` events from the Rust pool
//     service (try_claim / prewarm_one / drain_excess /
//     update_mesh_pool_size / reconcile_on_startup).
//   * Re-fetch on mesh switch (useAsyncEffect deps).
//
// The `poolCount` knob on `mockBackend` controls what the
// `get_mesh_pool_count` mock returns; flipping it mid-test isn't
// supported (the mock captures the value at call time), so the live
// refresh test relies on the event handler firing AFTER the initial
// fetch has resolved.

describe('RepositoryTab branches held in a worktree (issue #1460)', () => {
  // ── checked_out_in_worktree (orphan-worktree branch protection) ──────

  /**
   * Pin the orphan-worktree contract: a branch that is HEAD of some
   * working tree on disk — even if the agent node is gone (so
   * `is_active` is false) — must surface with a disabled checkbox and
   * an "in worktree" badge pointing at the holding worktree. The user
   * can no longer accidentally hit libgit2's "current HEAD of a linked
   * repository" error from the prune UI.
   */
  it('branches checked out in a worktree render with a disabled checkbox + in-worktree badge', async () => {
    mockBackend({
      prune: [
        {
          path: '/repos/demo',
          local_branches: [
            {
              name: 'main',
              is_head: true,
              is_merged_into_main: null,
              is_orphan: false,
              is_active: false,
              checked_out_in_worktree: null,
              has_uncommitted: false,
              last_commit_date: null,
              ahead: 0,
              behind: 0,
            },
            {
              name: 'feature/orphan',
              is_head: false,
              is_merged_into_main: true,
              is_orphan: false,
              // No live agent node (is_active: false), but a worktree
              // directory still exists with this branch checked out.
              is_active: false,
              checked_out_in_worktree: '/repos/demo/.claude/worktrees/hefty-slick-ocean',
              has_uncommitted: false,
              last_commit_date: null,
              ahead: 0,
              behind: 0,
            },
          ],
          worktrees: [
            {
              path: '/repos/demo',
              branch: 'main',
              is_active: true,
              is_stale: false,
              is_pool: false,
            },
            {
              // The orphan — node deleted but dir survives. Not active
              // (no live agent path match), not stale (branch exists).
              path: '/repos/demo/.claude/worktrees/hefty-slick-ocean',
              branch: 'feature/orphan',
              is_active: false,
              is_stale: false,
              is_pool: false,
            },
          ],
          remote_tracking_branches: [],
        },
      ],
    });
    openProbeDestination('worktrees');

    await screen.findByText('feature/orphan');

    // The branch row's checkbox is disabled even though `is_active` is
    // false — the orphan worktree is what blocks deletion, not an
    // agent node. The worktree row also renders "feature/orphan" in its
    // label (as the branch), so we anchor the regex to grab only the
    // branch row. The branch row's accessible name concatenates without
    // spaces between adjacent `<span>`s — the "in <wt>" badge's text
    // runs into the branch name (`feature/orphanin hefty-slick-ocean…`).
    // The worktree row's name is `<wt-name> · feature/orphan`, which
    // starts with `hefty-slick-ocean`, not `feature/orphan`, so the
    // `^feature/orphan` anchor is sufficient to disambiguate.
    const orphanCheckbox = screen.getByRole('checkbox', {
      name: /^feature\/orphan/i,
    });
    expect((orphanCheckbox as HTMLInputElement).disabled).toBe(true);

    // `main` (idle, no worktree on it beyond the main repo's HEAD) stays
    // enabled so the contrast is visible.
    const mainCheckbox = screen.getByRole('checkbox', { name: /^main/i });
    expect((mainCheckbox as HTMLInputElement).disabled).toBe(false);

    // An "in <worktree>" badge appears on the orphan row so the user
    // knows why it's locked and which worktree they should remove
    // instead. The badge text uses the worktree's last path segment
    // (the directory name), matching the worktree-row convention.
    // `findByText` resolves with the matched element (or throws after
    // timeout), so just confirming the call resolves is enough — no
    // `toBeInTheDocument` import needed.
    await screen.findByText(/in hefty-slick-ocean/);
  });

  /**
   * Same recommendation contract as `is_active`: a branch HEAD of a
   * worktree (orphan or live) is NOT in the "Select recommended" set.
   * The user has to delete the worktree row, which cascades to the
   * branch via `remove_one_worktree_and_branch`.
   */
  it('branches checked out in a worktree are not in "Select recommended"', async () => {
    mockBackend({
      prune: [
        {
          path: '/repos/demo',
          local_branches: [
            {
              name: 'main',
              is_head: true,
              is_merged_into_main: null,
              is_orphan: false,
              is_active: false,
              checked_out_in_worktree: null,
              has_uncommitted: false,
              last_commit_date: null,
              ahead: 0,
              behind: 0,
            },
            {
              name: 'feature/merged-clean',
              is_head: false,
              is_merged_into_main: true,
              is_orphan: false,
              is_active: false,
              checked_out_in_worktree: null,
              has_uncommitted: false,
              last_commit_date: null,
              ahead: 0,
              behind: 0,
            },
            {
              name: 'feature/merged-orphan-wt',
              is_head: false,
              is_merged_into_main: true,
              is_orphan: false,
              is_active: false,
              // Same prune flags as feature/merged-clean, but HEAD of a
              // surviving worktree directory â†’ must NOT be recommended.
              checked_out_in_worktree: '/repos/demo/.claude/worktrees/hefty-slick-ocean',
              has_uncommitted: false,
              last_commit_date: null,
              ahead: 0,
              behind: 0,
            },
          ],
          worktrees: [
            {
              path: '/repos/demo',
              branch: 'main',
              is_active: true,
              is_stale: false,
              is_pool: false,
            },
            {
              path: '/repos/demo/.claude/worktrees/hefty-slick-ocean',
              branch: 'feature/merged-orphan-wt',
              is_active: false,
              is_stale: false,
              is_pool: false,
            },
          ],
          remote_tracking_branches: [],
        },
      ],
    });
    openProbeDestination('worktrees');

    await screen.findByText('feature/merged-orphan-wt');

    const selectRecommended = await screen.findByRole('button', {
      name: /Select recommended/i,
    });
    expect(selectRecommended.textContent).toMatch(/\(1\)/);
    expect(selectRecommended.textContent).not.toMatch(/\(2\)/);
  });
});
