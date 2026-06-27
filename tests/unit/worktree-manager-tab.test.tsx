/**
 * Tests for the 🌳 Worktree Manager tab — issue #377.
 *
 * The tab ports the legacy `<BranchesWorktreesSection>` (which used to live
 * at the bottom of the old MeshPropertiesPanel drawer) into the unified
 * Probe Panel. The migration is "lifted sections" — the new tab drops the
 * collapsible header (the probe already provides the surface chrome) and
 * the always-visible one-liner (the probe header shows the tab name).
 *
 * Rendering strategy: mount the full `ProbePanel` and click the 🌳 tab
 * button, the same way the existing `mesh-properties-tab.test.tsx` does
 * for ⚙️. This keeps the routing wiring in `ProbePanel.tsx` covered by
 * the same suite — a separate routing test would have to know the tab's
 * internal structure.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { ProbePanel } from '../../src/components/Probe/ProbePanel';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import type { MeshRow } from '../../src/types/generated/MeshRow';
import { POOL_COUNT_CHANGED_EVENT } from '../../src/hooks/usePoolChanged';

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
      },
      {
        path: '/repos/demo/.worktrees/orphan',
        branch: null,
        is_active: false,
        is_stale: true,
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
        return Promise.resolve();
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

async function openWorktreesTab() {
  const user = userEvent.setup();
  render(<ProbePanel />);
  await user.click(screen.getByRole('button', { name: 'Worktree Manager' }));
}

beforeEach(() => {
  useMeshStore.setState({
    meshes: [MESH],
    meshesById: new Map([[MESH.id, MESH]]),
    selectedMeshId: MESH.id,
  });
  useAgentNodeStore.setState({ agentNodes: [], activeNodeId: null });
  useUIStore.setState({ probeOpen: false, probeTab: 'files', activeDiffFile: null });
});

describe('WorktreeManagerTab (issue #377)', () => {
  it('renders the 🌳 tab body when clicked (no longer the "coming soon" placeholder)', async () => {
    mockBackend();
    await openWorktreesTab();

    // The header should still show the tab's name — the friendly placeholder
    // "This tab's content is coming soon." must be gone.
    expect(screen.queryByText("This tab's content is coming soon.")).toBeNull();
    // The legacy collapsible header text is gone (the probe supplies chrome).
    expect(screen.queryByRole('button', { name: /Branches & Worktrees/i })).toBeNull();
  });

  it('shows the Worktree Manager label in the probe header', async () => {
    mockBackend();
    useUIStore.setState({ probeOpen: true, probeTab: 'worktrees' });
    render(<ProbePanel />);

    const header = screen.getByRole('region', { name: 'Probe panel' });
    expect(header.textContent).toContain('Worktree Manager');
  });

  it('lists local branches and worktrees returned by get_git_prune_info', async () => {
    mockBackend();
    await openWorktreesTab();

    // Branches surface with their names. `main` and the merged
    // `feature/done` must both appear once the prune info resolves.
    expect(await screen.findByText('main')).toBeTruthy();
    expect(screen.getByText('feature/done')).toBeTruthy();
    // The worktree directory name (`orphan`) is the last path segment.
    expect(screen.getByText('orphan')).toBeTruthy();
  });

  it('shows no HealthBlock when the mesh is clean (no drift / no hostage)', async () => {
    mockBackend({ health: HEALTHY });
    await openWorktreesTab();

    // Wait for the prune list to render so the negative assertion is stable
    // (the health block also mounts after `get_mesh_health` resolves).
    expect(await screen.findByText('main')).toBeTruthy();

    // No drift badge, no Restore / Free buttons when the mesh is healthy.
    expect(screen.queryByRole('button', { name: /Restore root to/i })).toBeNull();
    expect(screen.queryByRole('button', { name: /Free main/i })).toBeNull();
  });

  it('shows the HealthBlock with Restore + Free buttons when the mesh is drifted with a hostage', async () => {
    mockBackend({ health: DRIFTED_HEALTH });
    await openWorktreesTab();

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
    await openWorktreesTab();

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
    await openWorktreesTab();

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
    await openWorktreesTab();

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
    await openWorktreesTab();

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
    await openWorktreesTab();

    const restore = (await screen.findByRole('button', {
      name: /Restore root to main/i,
    })) as HTMLButtonElement;
    expect(restore.disabled).toBe(true);
    expect(restore.title).toContain('free it first');
  });
});

describe('ProbePanel routing for the 🌳 tab (issue #377)', () => {
  beforeEach(() => {
    mockBackend();
  });

  it('the 🌳 tab no longer renders the "coming soon" placeholder when a mesh is selected', async () => {
    useUIStore.setState({ probeOpen: true, probeTab: 'worktrees' });
    render(<ProbePanel />);

    // The placeholder text must be gone — the new tab is wired in.
    expect(screen.queryByText("This tab's content is coming soon.")).toBeNull();
  });

  it('the 🌳 tab in the activity bar opens the panel on the worktrees tab', () => {
    render(<ProbePanel />);
    fireEvent.click(screen.getByRole('button', { name: 'Worktree Manager' }));
    expect(useUIStore.getState().probeOpen).toBe(true);
    expect(useUIStore.getState().probeTab).toBe('worktrees');
  });
});

describe('WorktreeManagerTab Configuration card (issue #451)', () => {
  // The Configuration card ports the worktree-config sub-section that
  // used to live at the top of the legacy `MeshPropertiesPanel` (deleted
  // in #380). The card has three controls: a `use_worktree` checkbox,
  // a "Starting point" radio (Fresh/Head ↔ origin/main/HEAD on the
  // wire), and a "Worktree mode" radio (Branched/Detached).

  it('initial load populates the form from get_mesh_properties', async () => {
    mockBackend();
    await openWorktreesTab();

    // Wait for the form to populate from the load effect.
    const checkbox = (await screen.findByLabelText('Use worktree')) as HTMLInputElement;
    expect(checkbox.checked).toBe(true);
    // Default base_ref is origin/main → Fresh; default mode is branched.
    const fresh = (await screen.findByLabelText(/Fresh — start new session/i)) as HTMLInputElement;
    expect(fresh.checked).toBe(true);
    const branched = (await screen.findByLabelText(/Branched/i)) as HTMLInputElement;
    expect(branched.checked).toBe(true);

    // The load IPC was issued with the active mesh id.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('get_mesh_properties', { meshId: 42 });
    });
  });

  it('toggling the use_worktree checkbox calls update_mesh_use_worktree and collapses the radios', async () => {
    const user = userEvent.setup();
    mockBackend();
    await openWorktreesTab();

    // Wait for the form to populate.
    const checkbox = (await screen.findByLabelText('Use worktree')) as HTMLInputElement;
    expect(checkbox.checked).toBe(true);

    await user.click(checkbox);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_use_worktree', {
        meshId: 42,
        useWorktree: false,
      });
    });
    // The radios are gated on the checkbox; collapsing it removes the
    // section from the DOM (matches the legacy panel's
    // `{form.useWorktree && <div className="pl-4 border-l …">…}` block).
    expect(checkbox.checked).toBe(false);
    expect(screen.queryByText('Starting point')).toBeNull();
    expect(screen.queryByText('Worktree mode')).toBeNull();
  });

  it('selecting the Head radio calls update_worktree_base_ref with HEAD on the wire', async () => {
    const user = userEvent.setup();
    mockBackend();
    await openWorktreesTab();

    // The default is Fresh (base_ref: origin/main). Click Head to flip.
    const headRadio = (await screen.findByLabelText(/Head — resume last session/i)) as HTMLInputElement;
    await user.click(headRadio);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_worktree_base_ref', {
        meshId: 42,
        baseRef: 'HEAD',
      });
    });
    expect(headRadio.checked).toBe(true);
    // The Fresh radio is no longer checked.
    const fresh = screen.getByLabelText(/Fresh — start new session/i) as HTMLInputElement;
    expect(fresh.checked).toBe(false);
  });

  it('selecting the Detached radio calls update_mesh_column with column=worktree_mode', async () => {
    const user = userEvent.setup();
    mockBackend();
    await openWorktreesTab();

    // The default is branched. Click Detached to flip.
    const detachedRadio = (await screen.findByLabelText(/Detached/i)) as HTMLInputElement;
    await user.click(detachedRadio);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_column', {
        meshId: 42,
        column: 'worktree_mode',
        value: 'detached',
      });
    });
    expect(detachedRadio.checked).toBe(true);
    // The Branched radio is no longer checked.
    const branched = screen.getByLabelText(/Branched/i) as HTMLInputElement;
    expect(branched.checked).toBe(false);
  });

  it('collapsing use_worktree hides the two radio groups', async () => {
    const user = userEvent.setup();
    mockBackend();
    await openWorktreesTab();

    // Both group labels are present while use_worktree is true.
    expect(await screen.findByText('Starting point')).toBeTruthy();
    expect(screen.getByText('Worktree mode')).toBeTruthy();

    // Toggle use_worktree off.
    const checkbox = screen.getByLabelText('Use worktree') as HTMLInputElement;
    await user.click(checkbox);

    // Both group labels are gone.
    expect(screen.queryByText('Starting point')).toBeNull();
    expect(screen.queryByText('Worktree mode')).toBeNull();
  });

  it('save failure surfaces inline and does NOT revert the form state', async () => {
    const user = userEvent.setup();
    mockBackend({ saveUseWorktreeFails: true });
    await openWorktreesTab();

    const checkbox = (await screen.findByLabelText('Use worktree')) as HTMLInputElement;
    expect(checkbox.checked).toBe(true);

    await user.click(checkbox);

    // The save error appears below the card, in the same inline-error
    // style as the prune error at `WorktreeManagerTab.tsx:304`.
    const error = await screen.findByText(/Failed to update use_worktree/);
    expect(error).toBeTruthy();
    expect(error.className).toContain('text-status-error');

    // The form was *not* reverted: the checkbox stays unchecked even
    // though the backend rejected the save. This matches the legacy
    // "form mirrors user intent" rule — the user retries by toggling
    // again rather than the UI silently undoing their click.
    expect(checkbox.checked).toBe(false);
  });

  it('null worktree_mode defaults to branched on load', async () => {
    // A fresh mesh row returns `worktree_mode: null` from the wire.
    // The card must fall back to the default rather than rendering
    // an empty radio group.
    mockBackend({ meshRow: { worktree_mode: null } });
    await openWorktreesTab();

    const branched = (await screen.findByLabelText(/Branched/i)) as HTMLInputElement;
    expect(branched.checked).toBe(true);
    const detached = screen.getByLabelText(/Detached/i) as HTMLInputElement;
    expect(detached.checked).toBe(false);
  });

  // ── Open-in-file-explorer (regression for the lost affordance) ─────
  // The 🌳 tab used to host an open-in-OS-file-manager action per
  // worktree row in the legacy MeshPropertiesPanel; the lift to Probe
  // (#377) kept the path text but dropped the icon button. The new
  // tests pin both the repo-path button and the per-worktree button
  // so the affordance can't be silently lost again.

  it('renders a repo-path open-in-explorer button that calls open_in_file_manager with the repo path', async () => {
    mockBackend();
    await openWorktreesTab();

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
    await openWorktreesTab();

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
    await openWorktreesTab();
    await screen.findByText('orphan');

    const orphanButton = screen.getByTestId('worktree-open-w:/repos/demo/.worktrees/orphan');
    fireEvent.click(orphanButton);

    // Delete Selected is disabled iff no rows are selected — the
    // clearest cross-test pin for "the click did NOT toggle the
    // checkbox on the orphan row".
    const deleteBtn = screen.getByRole('button', { name: /Delete Selected/i });
    expect((deleteBtn as HTMLButtonElement).disabled).toBe(true);
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

describe('WorktreeManagerTab Pre-spawn Pool badge', () => {
  // Capture every Tauri event handler the probe attaches so individual
  // tests can fire the pool-count-changed event and assert the badge
  // re-fetches. Other tests in this file don't care about events, so
  // we keep the capture scoped to this describe block.
  let poolEventHandler: ((e: { payload: unknown }) => void) | null = null;
  const listenMock = vi.fn();

  beforeEach(() => {
    poolEventHandler = null;
    listenMock.mockClear();
    // Default mock: no-op unlisten + capture only the pool-count-changed
    // handler. Other events (none today, but future-proof) are ignored.
    // The `listenMock(event, handler)` call mirrors the
    // `useProviderListInvalidation` test fixture so a future
    // `expect(listenMock).toHaveBeenCalledWith(POOL_COUNT_CHANGED_EVENT, …)`
    // has something to assert against.
    vi.mocked(listen).mockImplementation((event: string, handler: (e: unknown) => void) => {
      listenMock(event, handler);
      if (event === POOL_COUNT_CHANGED_EVENT) {
        poolEventHandler = handler as (e: { payload: unknown }) => void;
      }
      return Promise.resolve(() => {});
    });
  });

  it('hides the badge when preSpawnPoolSize === 0 (pool disabled)', async () => {
    // Default meshRow has pre_spawn_pool_size: 0; mockBackend default
    // poolCount: 0. The badge must NOT render.
    mockBackend();
    await openWorktreesTab();

    // Wait for the config card to render so the negative assertion is
    // stable (the badge mounts after the form populates from the load).
    await screen.findByLabelText('Pre-spawn warm worktrees');
    expect(screen.queryByTestId('pool-status')).toBeNull();
  });

  it('shows "X / Y ready" when the pool is enabled', async () => {
    mockBackend({
      meshRow: { pre_spawn_pool_size: 3 },
      poolCount: 2,
    });
    await openWorktreesTab();

    // The badge uses the live count (2) and the persisted target (3).
    const status = await screen.findByTestId('pool-status');
    expect(status).toBeTruthy();
    const text = screen.getByTestId('pool-status-text');
    expect(text.textContent).toBe('2 / 3 ready');
    // role="status" + aria-live="polite" so screen readers announce
    // pool-count-changed events without interrupting other speech.
    expect(status.getAttribute('role')).toBe('status');
    expect(status.getAttribute('aria-live')).toBe('polite');
    expect(status.getAttribute('aria-label')).toBe(
      '2 of 3 pre-spawn worktrees ready',
    );
  });

  it('shows "0 / Y ready" with a present-but-empty bar when the pool is enabled but empty', async () => {
    // After a shrink (target 3 → 0 → 1) the badge must show 0 / 1 ready,
    // not "…/1 ready" (the count IS known, just zero). The Plan agent's
    // synchronous-drain UX fix is what makes this settle immediately on
    // a config change — but the badge format itself is tested here.
    mockBackend({
      meshRow: { pre_spawn_pool_size: 1 },
      poolCount: 0,
    });
    await openWorktreesTab();

    const text = await screen.findByTestId('pool-status-text');
    expect(text.textContent).toBe('0 / 1 ready');
  });

  it('re-fetches the pool count when pool-count-changed fires', async () => {
    // Seed the initial fetch with 1 ready, then fire the event and
    // assert the badge updated to 4 (simulating the Rust side emitting
    // pool-count-changed after a successful prewarm_one).
    mockBackend({
      meshRow: { pre_spawn_pool_size: 5 },
      poolCount: 1,
    });
    await openWorktreesTab();

    // Sanity check: initial fetch resolved.
    await waitFor(() => {
      expect(screen.getByTestId('pool-status-text').textContent).toBe(
        '1 / 5 ready',
      );
    });

    // Override get_mesh_pool_count so the NEXT call (the one triggered
    // by the event) returns 4. The flag flips inside the override's
    // closure, so the initial fetch (already done) isn't affected by
    // this swap.
    let eventFired = false;
    const prevImpl = vi.mocked(invoke).getMockImplementation();
    vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === 'get_mesh_pool_count') {
        return Promise.resolve(eventFired ? 4 : 1);
      }
      return prevImpl?.(cmd, args) ?? Promise.resolve({});
    });

    // Fire the pool-count-changed event from the backend.
    eventFired = true;
    poolEventHandler?.({ payload: 42 });

    await waitFor(() => {
      expect(screen.getByTestId('pool-status-text').textContent).toBe(
        '4 / 5 ready',
      );
    });
    // And the listener was actually attached (not just lying around).
    expect(listenMock).toHaveBeenCalledWith(
      POOL_COUNT_CHANGED_EVENT,
      expect.any(Function),
    );
  });

  it('re-fetches when activeMeshId switches', async () => {
    // Seed mesh 42 with 2 ready; switch to mesh 7 with 4 ready. The
    // badge must re-fetch (via the useAsyncEffect on activeMeshId) and
    // land on 4 / 3 ready.
    mockBackend({
      meshRow: { pre_spawn_pool_size: 3 },
      poolCount: 2,
    });
    await openWorktreesTab();
    await waitFor(() => {
      expect(screen.getByTestId('pool-status-text').textContent).toBe(
        '2 / 3 ready',
      );
    });

    // Switch to a second mesh and override the response.
    const mesh2: Mesh = {
      ...MESH,
      id: 7,
      name: 'demo2',
      path: '/repos/demo2',
    };
    useMeshStore.setState({
      meshes: [MESH, mesh2],
      meshesById: new Map([
        [MESH.id, MESH],
        [mesh2.id, mesh2],
      ]),
      selectedMeshId: mesh2.id,
    });
    // Override get_mesh_pool_count for the new mesh's id.
    const impl = vi.mocked(invoke).getMockImplementation();
    vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === 'get_mesh_pool_count') {
        // Differentiate by meshId from the args.
        const a = args as { meshId?: number } | undefined;
        if (a?.meshId === mesh2.id) return Promise.resolve(4);
        return Promise.resolve(2);
      }
      return impl?.(cmd, args) ?? Promise.resolve({});
    });

    await waitFor(() => {
      expect(screen.getByTestId('pool-status-text').textContent).toBe(
        '4 / 3 ready',
      );
    });
  });
});
