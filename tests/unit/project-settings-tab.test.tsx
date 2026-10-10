import { seedAgentNodes } from './helpers/seedAgentNodes';
import { openProbeDestination } from './helpers/openProbeDestination';
/**
 * Tests for the Project Settings destination — issue #1460 (was the Mesh
 * Properties tab, issue #375).
 *
 * The tab ports the configuration fields from the legacy
 * `MeshPropertiesPanel`. Issue #1460 made it the single home for project
 * configuration by pulling the worktree-strategy block in from the old
 * Worktree Manager (use-worktree, starting point, worktree mode, pre-spawn
 * pool, worktree directory) and by wrapping the destination in labelled
 * sections with Delete Mesh in a labelled danger zone. The suite below pins
 * all three: the section structure, the moved strategy controls, and the
 * maintenance UI that must NOT come back here.
 *
 * Rendering strategy: mount the full `ProbePanel` with the properties
 * destination opened via `openProbeTab` (the post-#1375 on-demand entry
 * point), so the test also covers the routing wiring in
 * `ProbePanel.tsx` (otherwise the routing test in `probe-panel.test.tsx`
 * would have to know the tab's internal structure).
 */

import { describe, it, expect, vi, beforeAll, beforeEach } from 'vitest';
import { preloadProbeTabs } from './helpers/preloadProbeTabs';
import { render, screen, fireEvent, waitFor, act, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { ProbePanel } from '../../src/components/Probe/ProbePanel';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import type { AgentNode } from '../../src/types/generated/AgentNode';
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

/** Minimal Agent Node fixture. Only `id` / `mesh_id` matter to the Delete
 *  Mesh confirmation's node count, so callers override just those. */
function nodeFixture(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 1,
    mesh_id: 42,
    name: 'node-1',
    path: '/repos/demo/.worktrees/node-1',
    status: 'idle',
    ...overrides,
  } as AgentNode;
}

const MESH_CONFIG = {
  name: 'demo',
  build_command: 'npm run build',
  run_command: 'npm run dev',
  // Per-context commands (issue #802) — unset in the base fixture so the
  // load path exercises the empty-string fallback; a dedicated test sets them.
  root_build_command: null,
  root_run_command: null,
  model: 'opus-4',
  effort: 'high',
  base_ref: 'origin/main',
  use_worktree: true,
  worktree_mode: 'branched',
  default_provider: 'anthropic',
  sandbox: true,
  // Autopilot Policy (issue #481) — disabled by default in the fixture;
  // the dedicated tests below flip it on via a per-test override.
  autopilot_enabled: false,
  autopilot_trigger_label: null,
  autopilot_concurrency_limit: 2,
  autopilot_provider: null,
  autopilot_action_on_success: null,
};

/** Capability descriptor for the test fixtures — every native row in
 *  `list_providers` carries one so the Spawn Menu picker can decide which
 *  harnesses expose a configurations submenu. Mirrors the shape of
 *  `HarnessCapabilities` from `src/types/generated/`. */
function capsFixture(harness_id: string): unknown {
  if (harness_id === 'claude') {
    return {
      harness_id,
      supports_resume: true,
      auto_resume_on_startup: true,
      requires_attention_hook: true,
      produces_readable_transcript: true,
      supports_model_override: true,
      supports_effort_override: true,
      supports_prefill: true,
      is_plain_terminal: false,
      effort_control: { kind: 'closed', allowed: ['low', 'medium', 'high'] },
      available_on: ['windows', 'macos', 'linux'],
    };
  }
  if (harness_id === 'codex') {
    return {
      harness_id,
      supports_resume: true,
      auto_resume_on_startup: true,
      requires_attention_hook: true,
      produces_readable_transcript: true,
      supports_model_override: true,
      supports_effort_override: true,
      supports_prefill: true,
      is_plain_terminal: false,
      effort_control: {
        kind: 'inline_config',
        key: 'model_reasoning_effort',
        allowed: ['none', 'low', 'medium', 'high', 'xhigh'],
      },
      available_on: ['windows', 'macos', 'linux'],
    };
  }
  // Default — no configurable controls (mirrors Terminal / OpenCode).
  return {
    harness_id,
    supports_resume: false,
    auto_resume_on_startup: false,
    requires_attention_hook: false,
    produces_readable_transcript: false,
    supports_model_override: false,
    supports_effort_override: false,
    supports_prefill: false,
    is_plain_terminal: true,
    effort_control: { kind: 'none' },
    available_on: ['windows'],
  };
}

/**
 * Wire the mocked `invoke` to answer each command the tab calls during
 * mount + a single edit cycle. Anything we don't care about resolves
 * with `{}` so other panels can keep loading in the same render.
 *
 * Issue #1460 — the worktree-strategy commands (`update_mesh_use_worktree`,
 * `update_worktree_base_ref`, `update_mesh_pool_size`,
 * `update_mesh_worktree_directory`, `get_mesh_pool_count`,
 * `get_worktree_directory_config`) answer here too. They used to be mocked
 * by the Worktree Manager suite, which no longer owns this surface; without
 * them the moved controls would hit the `{ cmd, args }` fall-through and the
 * pool badge would render an `object` instead of a number.
 *
 * `overrides.meshConfig` replaces the loaded mesh config wholesale (the
 * wire values for use-worktree / base ref / worktree mode / pool size);
 * `poolCount` drives the badge; the two `save*Fails` knobs flip the matching
 * save to a rejecting handler so the "do not revert on failure" rule can be
 * asserted.
 *
 * `sandboxDevMode` answers `sandbox_dev_mode_enabled`. It defaults to
 * `false` — the released-build answer — so the shared mock renders the app
 * as a user actually sees it, and only the tests that specifically exercise
 * the experimental sandbox toggle opt in.
 */
function mockBackend(
  overrides: {
    meshConfig?: Record<string, unknown>;
    poolCount?: number;
    saveUseWorktreeFails?: boolean;
    saveBaseRefFails?: boolean;
    /** Issue #2024 rank 8 — reject the rename IPC so the tab's success
     *  branch can be caught reporting "Saved" for a write the backend
     *  refused. */
    saveNameFails?: boolean;
    sandboxDevMode?: boolean;
  } = {},
) {
  const poolCount = overrides.poolCount ?? 0;
  vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
    switch (cmd) {
      case 'sandbox_dev_mode_enabled':
        return Promise.resolve(overrides.sandboxDevMode ?? false);
      case 'list_providers':
        // Issue #575 / ADR-0016 — Spawn Options carry the full wire shape
        // (harness_id, provider_id, is_proxied, group_key). The mock
        // returns two native harnesses plus one Proxied Provider child so
        // the picker renders native parents and hides the proxied route at
        // the top level. The `capabilities` field is the issue #1149
        // descriptor — required by the Spawn Menu (configurations
        // submenu) and the App Settings harness defaults section (#1150).
        return Promise.resolve([
          { id: 'claude', label: 'Claude Code', color: '#000', icon: '', resumable: true, harness_id: 'claude', provider_id: null, is_proxied: false, group_key: 'claude', capabilities: capsFixture('claude') },
          { id: 'anthropic', label: 'Anthropic', color: '#000', icon: '', resumable: true, harness_id: 'claude', provider_id: 'anthropic', is_proxied: true, group_key: 'claude', capabilities: capsFixture('claude') },
          { id: 'codex', label: 'Codex', color: '#000', icon: '', resumable: false, harness_id: 'codex', provider_id: null, is_proxied: false, group_key: 'codex', capabilities: capsFixture('codex') },
        ]);
      case 'get_mesh_properties':
        return Promise.resolve(overrides.meshConfig ?? MESH_CONFIG);
      case 'get_mesh_pool_count':
        return Promise.resolve(poolCount);
      case 'get_worktree_directory_config':
        return Promise.resolve({
          effective_directory: '/repos/demo/.claude/worktrees',
          app_directory: null,
          mesh_directory: null,
        });
      case 'get_app_preferences':
        return Promise.resolve({ worktree_directory: null });
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
      case 'get_mesh_health':
        return Promise.resolve({
          is_dirty: false,
          is_drifted: false,
          unpushed_ahead: 0,
          base_branch_holder: null,
          local_base_branch: 'main',
          current_branch: 'main',
          current_short_sha: 'abc1234',
          authenticated: false,
        });
      case 'update_mesh_column':
      case 'update_mesh_sandbox':
      case 'update_mesh_pool_size':
      case 'update_mesh_worktree_directory':
      case 'delete_mesh':
        return Promise.resolve();
      case 'update_mesh_use_worktree':
        return overrides.saveUseWorktreeFails
          ? Promise.reject(new Error('mock: update_mesh_use_worktree failed'))
          : Promise.resolve();
      case 'update_worktree_base_ref':
        return overrides.saveBaseRefFails
          ? Promise.reject(new Error('mock: update_worktree_base_ref failed'))
          : Promise.resolve();
      case 'update_mesh_name':
        // Issue #2024 rank 8. `updateMeshName` must reject so the tab's
        // `wrappedSave` takes its failure branch instead of reporting a
        // successful save for a write the backend refused.
        return overrides.saveNameFails
          ? Promise.reject(new Error('mock: update_mesh_name rejected'))
          : Promise.resolve();
      case 'list_meshes':
        // `useMeshStore.deleteMesh` refetches the mesh list after deletion.
        // The default branch returns a list-shaped value so the .map() in
        // `fetchMeshes` doesn't blow up when the refetch runs.
        return Promise.resolve([]);
      case 'git_sync':
      case 'get_git_branch_status':
        return Promise.resolve({});
      default:
        // Default-fall-through to the args so failed-assertion messages
        // show the command we hit.
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
  mockBackend();
});

// Load the lazy Probe tab chunks up front so rendering a tab does not wait on disk.
beforeAll(preloadProbeTabs, 60_000);

describe('ProjectSettingsTab (issue #1460)', () => {
  it('renders the config form when the âš™ï¸ tab is open and a mesh is selected', async () => {
    openProbeDestination('properties');
    const loadedFieldQuery = { timeout: 10_000 };

    // Config fields that the new tab must keep. The label regex anchors
    // at the start with `\b` because the Field component renders the
    // visible label + hint as one accessible name inside `<label>`
    // (e.g. "Model (Claude Code only)"); the open end lets the suffix
    // pass through so a future hint rewrite doesn't break the matcher.
    // Every lookup is awaited: the #1375 open path no longer goes through
    // a rail click whose async settle used to cover the form's load, so
    // each control waits for its own mount (the Default provider picker
    // arrives with the async provider list).
    expect(await screen.findByLabelText('Name', {}, loadedFieldQuery)).toBeTruthy();
    expect(await screen.findByLabelText('Directory', {}, loadedFieldQuery)).toBeTruthy();
    expect(await screen.findByLabelText('Default provider', {}, loadedFieldQuery)).toBeTruthy();
    expect(await screen.findByLabelText('Project preset', {}, loadedFieldQuery)).toBeTruthy();
    expect(await screen.findByLabelText(/^Build command/, {}, loadedFieldQuery)).toBeTruthy();
    expect(await screen.findByLabelText(/^Run command/, {}, loadedFieldQuery)).toBeTruthy();
  });

  it('shows the active tab label in the probe header', async () => {
    useUIStore.setState({ probeOpen: true, probeTab: 'properties' });
    render(<ProbePanel />);

    const header = screen.getByRole('region', { name: 'Probe panel' });
    expect(header.textContent).toContain('Project Settings');
  });

  it('excludes the branch/worktree MAINTENANCE fields', async () => {
    openProbeDestination('properties');
    // Wait for the form to mount so the negative assertions are stable.
    expect(await screen.findByLabelText('Name')).toBeTruthy();

    // The legacy drawer's Git-maintenance fields must NOT appear. Note this is
    // the *maintenance* half: issue #1460 deliberately moved the worktree
    // STRATEGY controls in (Use worktree / Starting point / Worktree mode now
    // live in the "Worktree strategy" section), so only the destructive,
    // per-repo surface stays away.
    expect(screen.queryByText(/Branches & Worktrees/i)).toBeNull();
    expect(screen.queryByText('Uncommitted Changes')).toBeNull();
    expect(screen.queryByRole('button', { name: /Select recommended/i })).toBeNull();
    expect(screen.queryByRole('button', { name: /Delete Selected/i })).toBeNull();
    expect(screen.queryByTestId('repository-health-section')).toBeNull();
    expect(screen.queryByTestId('repository-cleanup-section')).toBeNull();
    // ...and the worktree strategy it DOES own is present, so the negative
    // assertions above are about maintenance, not about a stripped form.
    expect(screen.queryByLabelText('Use worktree')).toBeTruthy();
    // The Delete Mesh button IS supposed to come back (it lived on the
    // legacy drawer's footer) — now in the labelled danger zone. See the
    // `Delete Mesh danger zone` describe block below.
  });

  it('preloads the mesh config (Name, Default provider, Build/Run) from the backend', async () => {
    openProbeDestination('properties');

    const name = (await screen.findByLabelText('Name')) as HTMLInputElement;
    expect(name.value).toBe('demo');

    // Awaited lookups — see the note in the config-form test above.
    const build = (await screen.findByLabelText(/^Build command/)) as HTMLInputElement;
    expect(build.value).toBe('npm run build');

    const run = (await screen.findByLabelText(/^Run command/)) as HTMLInputElement;
    expect(run.value).toBe('npm run dev');

    // The Default provider picker shows the stored selection's label.
    const provider = await screen.findByLabelText('Default provider');
    expect(provider.textContent).toContain('Anthropic');
  });

  it('opens the Spawn Menu (harness-grouped) from the Default provider picker (ADR-0016)', async () => {
    const user = userEvent.setup();
    openProbeDestination('properties');

    const trigger = await screen.findByLabelText('Default provider');
    await user.click(trigger);

    // The inherit row plus the native harness parent rows. The bare
    // Proxied Provider route is NOT a top-level row — it is reachable only
    // through a saved Launch Configuration, matching every spawn surface.
    expect(await screen.findByTestId('spawn-option-picker-menu')).toBeTruthy();
    expect(screen.getByRole('menuitem', { name: /^<Default>/ })).toBeTruthy();
    expect(screen.getByRole('menuitem', { name: 'Claude Code' })).toBeTruthy();
    expect(screen.getByRole('menuitem', { name: 'Codex' })).toBeTruthy();
    expect(screen.queryByRole('menuitem', { name: 'Anthropic' })).toBeNull();
  });

  it('saves Build/Run on blur', async () => {
    const user = userEvent.setup();
    openProbeDestination('properties');

    const build = (await screen.findByLabelText(/^Build command/)) as HTMLInputElement;
    await user.clear(build);
    await user.type(build, 'cargo build');
    fireEvent.blur(build);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_column', {
        meshId: 42,
        column: 'build_command',
        value: 'cargo build',
      });
    });
  });

  // Per-context build/run commands (issue #802).
  it('renders the optional Root build/run command fields with a fallback hint', async () => {
    openProbeDestination('properties');

    const rootBuild = (await screen.findByLabelText(/^Root build command/)) as HTMLInputElement;
    const rootRun = screen.getByLabelText(/^Root run command/) as HTMLInputElement;
    expect(rootBuild).toBeTruthy();
    expect(rootRun).toBeTruthy();
    // The accessible name carries the "(optional — falls back to …)" hint so
    // the user knows leaving it blank reuses the Build / Run command.
    expect(rootBuild.labels?.[0].textContent).toMatch(/optional.*falls back/i);
    expect(rootRun.labels?.[0].textContent).toMatch(/optional.*falls back/i);
    // Empty in the base fixture (both columns null) — the load path maps
    // null â†’ ''.
    expect(rootBuild.value).toBe('');
    expect(rootRun.value).toBe('');
  });

  it('preloads configured Root build/run commands', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_mesh_properties') {
        return Promise.resolve({
          ...MESH_CONFIG,
          root_build_command: 'cargo build --workspace',
          root_run_command: 'npm run lint --workspaces',
        });
      }
      if (cmd === 'list_providers') return Promise.resolve([]);
      return Promise.resolve({});
    });
    openProbeDestination('properties');

    const rootBuild = (await screen.findByLabelText(/^Root build command/)) as HTMLInputElement;
    const rootRun = screen.getByLabelText(/^Root run command/) as HTMLInputElement;
    expect(rootBuild.value).toBe('cargo build --workspace');
    expect(rootRun.value).toBe('npm run lint --workspaces');
  });

  it('saves Root build/run on blur via update_mesh_column', async () => {
    const user = userEvent.setup();
    openProbeDestination('properties');

    const rootBuild = (await screen.findByLabelText(/^Root build command/)) as HTMLInputElement;
    await user.clear(rootBuild);
    await user.type(rootBuild, 'cargo build --workspace');
    fireEvent.blur(rootBuild);
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_column', {
        meshId: 42,
        column: 'root_build_command',
        value: 'cargo build --workspace',
      });
    });

    const rootRun = screen.getByLabelText(/^Root run command/) as HTMLInputElement;
    await user.clear(rootRun);
    await user.type(rootRun, 'cargo run -p app');
    fireEvent.blur(rootRun);
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_column', {
        meshId: 42,
        column: 'root_run_command',
        value: 'cargo run -p app',
      });
    });
  });

  // Regression for "Build/Run only editable via preset". The user's manual
  // typing must update the input's *displayed* value on every keystroke,
  // and the field must be a plain editable `<input type="text">` — no
  // `disabled`, no `readOnly`, no parent overlay swallowing keystrokes.
  it('keeps the Build/Run inputs editable: typing updates the value without blurring', async () => {
    const user = userEvent.setup();
    openProbeDestination('properties');

    const build = (await screen.findByLabelText(/^Build command/)) as HTMLInputElement;
    const run = (await screen.findByLabelText(/^Run command/)) as HTMLInputElement;

    // Editability contract: the inputs are plain text fields, not the
    // "Directory" field which is intentionally read-only.
    expect(build.disabled).toBe(false);
    expect(build.readOnly).toBe(false);
    expect(run.disabled).toBe(false);
    expect(run.readOnly).toBe(false);

    // The placeholder must NOT look like an actual command — that mix-up
    // is the original "only preset works" UX bug. Empty fields carry a
    // clear hint string (starts with "e.g.,") and the field is labelled
    // "(custom)" so the user knows it's a freeform override, not a
    // read-only mirror of the preset above.
    expect(build.placeholder).toMatch(/^e\.g\.,/);
    expect(run.placeholder).toMatch(/^e\.g\.,/);
    // Pin the actual override copy too — `^e.g.,` would let a future
    // rewrite that drops "type to override" still pass, which is the
    // exact bug we're trying to prevent.
    expect(build.placeholder).toContain('type to override');
    expect(run.placeholder).toContain('type to override');
    // The label's "(custom)" hint is the visible signal that this is a
    // freeform override, not a read-only mirror of the preset above.
    // The text is split across a text node + a nested <span>, so use a
    // function matcher (RTL's recommended way to match across siblings)
    // instead of an exact string.
    expect(
      screen.getByText(
        (_content, element) =>
          element?.tagName === 'LABEL' &&
          /Build command\s*\(custom\)/i.test(element.textContent ?? '')
      )
    ).toBeTruthy();
    expect(
      screen.getByText(
        (_content, element) =>
          element?.tagName === 'LABEL' &&
          /Run command\s*\(custom\)/i.test(element.textContent ?? '')
      )
    ).toBeTruthy();

    // Type a custom Build command WITHOUT blurring. The displayed value
    // must reflect the keystrokes — if a parent effect resets the form
    // state, or the field is wrapped in something that prevents onChange
    // propagation, this assertion catches it.
    await user.click(build);
    await user.clear(build);
    await user.keyboard('cargo build --release');
    expect(build.value).toBe('cargo build --release');

    // Same contract for Run.
    await user.click(run);
    await user.clear(run);
    await user.keyboard('./target/debug/myapp');
    expect(run.value).toBe('./target/debug/myapp');
  });

  it('applies a project preset to both Build and Run on a single change', async () => {
    const user = userEvent.setup();
    openProbeDestination('properties');

    const preset = (await screen.findByLabelText('Project preset')) as HTMLSelectElement;
    await user.selectOptions(preset, 'rust');

    await waitFor(() => {
      const calls = vi.mocked(invoke).mock.calls.filter(
        ([cmd]) => cmd === 'update_mesh_column',
      );
      const columns = calls.map(([, args]) => (args as { column: string }).column);
      expect(columns).toContain('build_command');
      expect(columns).toContain('run_command');
    });
  });

// Experimental sandbox toggle (macOS Seatbelt #497 / Windows restricted
  // token #528). The DB column is `sandbox` and is OS-agnostic at this layer;
  // the OS-specific spawn policy is decided at `spawn_environment::wrap`.
  //
  // Issue #2034 — the whole feature is developer-gated behind
  // BUILDMESH_SANDBOX=1, so these tests opt in through
  // `sandbox_dev_mode_enabled` rather than relying on the toggle always
  // being present. The release default (gate closed) is asserted separately
  // below.
  it('renders the Sandbox toggle as an editable checkbox when the dev gate is open', async () => {
    mockBackend({ sandboxDevMode: true });
    openProbeDestination('properties');
    const sandbox = (await screen.findByLabelText(/Sandbox agent processes/)) as HTMLInputElement;
    expect(sandbox.type).toBe('checkbox');
    expect(sandbox.disabled).toBe(false);
  });

  // The release guarantee: with BUILDMESH_SANDBOX unset the backend discards
  // `meshes.sandbox`, so offering the control would advertise a setting that
  // silently does nothing.
  it('hides the Sandbox toggle when the developer gate is closed (release default)', async () => {
    mockBackend({ sandboxDevMode: false });
    openProbeDestination('properties');
    await screen.findByLabelText('Default provider');
    expect(screen.queryByLabelText(/Sandbox agent processes/)).toBeNull();
  });

  it('preloads the saved sandbox state into the checkbox', async () => {
    mockBackend({ sandboxDevMode: true });
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'sandbox_dev_mode_enabled') return Promise.resolve(true);
      if (cmd === 'get_mesh_properties') {
        return Promise.resolve({ ...MESH_CONFIG, sandbox: true });
      }
      if (cmd === 'list_providers') return Promise.resolve([]);
      if (cmd === 'detect_mesh_project')
        return Promise.resolve({ preset_id: null, label: null, node_scripts: null });
      if (cmd === 'detect_ai_context')
        return Promise.resolve({
          claude_md_exists: false,
          agents_md_exists: false,
          skills_dir_exists: false,
          skill_count: 0,
          agents_skills_exists: false,
        });
      return Promise.resolve({});
    });
    openProbeDestination('properties');
    const sandbox = (await screen.findByLabelText(/Sandbox agent processes/)) as HTMLInputElement;
    expect(sandbox.checked).toBe(true);
  });

  it('saves the Sandbox toggle on change via update_mesh_sandbox', async () => {
    const user = userEvent.setup();
    mockBackend({ sandboxDevMode: true });
    openProbeDestination('properties');

    const sandbox = (await screen.findByLabelText(/Sandbox agent processes/)) as HTMLInputElement;
    expect(sandbox.checked).toBe(true);
    await user.click(sandbox);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_sandbox', {
        meshId: 42,
        sandbox: false,
      });
    });
  });

  // Regression: the legacy #497 macOS-Seatbelt-specific toggle used to render
  // alongside the unified OS-agnostic one, both bound to `form.sandbox`.
  // The OS-agnostic surface (canonical per ADR 0012) is the only one allowed.
  it('renders exactly one Sandbox toggle (no duplicate #497 macOS-only block)', async () => {
    mockBackend({ sandboxDevMode: true });
    openProbeDestination('properties');
    // Regex matches both "Sandbox agent processes" and the old
    // "Sandbox agent processes (macOS only)" accessible names.
    const sandboxes = await screen.findAllByLabelText(/Sandbox agent processes/);
    expect(sandboxes).toHaveLength(1);
  });

  // Legacy Autopilot policy controls were removed from the application.
  // Keep Project Settings free of the retired fields and IPC calls.

  it('does not render legacy Autopilot policy fields', async () => {
    openProbeDestination('properties');

    // Master toggle + 4 policy fields, none of which should be in the DOM.
    // The 'Autopilot Mode' label previously anchored the section's master
    // toggle; once it's gone, the four policy-field labels follow.
    expect(screen.queryByLabelText('Autopilot Mode')).toBeNull();
    expect(screen.queryByLabelText('Trigger label')).toBeNull();
    expect(screen.queryByLabelText('Max concurrent autopilot nodes')).toBeNull();
    expect(screen.queryByLabelText('Autopilot provider')).toBeNull();
    expect(screen.queryByLabelText('On success')).toBeNull();
    // And the atomic IPC never fires from this tab — even on a no-op render.
    expect(
      vi.mocked(invoke).mock.calls.some(
        ([cmd]) => (cmd as string) === 'update_mesh_autopilot'
      )
    ).toBe(false);
  });

  it('renders nothing when no mesh is selected (the probe shell handles the empty state)', () => {
    useMeshStore.setState({ meshes: [], meshesById: new Map(), selectedMeshId: null });
    useUIStore.setState({ probeOpen: true, probeTab: 'properties' });
    render(<ProbePanel />);

    // The Probe's "No project selected" empty state, not the form.
    expect(screen.getByText('No project selected')).toBeTruthy();
    expect(screen.queryByLabelText('Name')).toBeNull();
  });
});

// Regression: "Directory" displays the *mesh* root, not the focused node's
// worktree path. The tab is editing MESH-level config (Name, Build, Run,
// etc.) — surfacing a worktree subdir under the "Directory" label silently
// misleads the user, AND keeps the previous mesh's path displayed after a
// sidebar switch when the focused node still belongs to the old mesh
// (`activeNodeId` is not cleared by `selectMesh`). `useProbeContext` already
// exposes `activeMeshPath` for this case (the hook itself documents the
// distinction); the Directory input binds to the wrong field.
describe('ProjectSettingsTab — Directory field shows the mesh root (not the focused worktree)', () => {
  // Two meshes with distinct paths so a "wrong path" assertion is sharp.
  const MESH_A: Mesh = { ...MESH, id: 1, name: 'alpha', path: '/repos/alpha' };
  const MESH_B: Mesh = { ...MESH, id: 2, name: 'beta',  path: '/repos/beta' };

  // A focused agent node in Mesh A — its `path` is the worktree subdir,
  // distinct from the mesh root. This is the trigger condition for the
  // bug: when this node is focused, `useProbeContext().activePath`
  // resolves to the node's path (the worktree), not the mesh root.
  const FOCUSED_NODE = {
    id: 100,
    mesh_id: 1,                    // belongs to Mesh A
    name: 'agent-x',
    path: '/repos/alpha/.claude/worktrees/agent-x',
    branch: 'main',
    env: 'windows',
    provider: 'anthropic',
    status: 'idle',
    use_worktree: true,
    position: 0,
    created_at: '2026-01-01',
  };

  function setupTwoMeshesWithFocusedNode() {
    useMeshStore.setState({
      meshes: [MESH_A, MESH_B],
      meshesById: new Map<number, Mesh>([
        [MESH_A.id, MESH_A],
        [MESH_B.id, MESH_B],
      ]),
      selectedMeshId: MESH_A.id,
    });
    seedAgentNodes([FOCUSED_NODE], FOCUSED_NODE.id);
    useUIStore.setState({ probeOpen: true, probeTab: 'properties' });
  }

  it('shows the mesh root when an agent node is focused', async () => {
    setupTwoMeshesWithFocusedNode();
    render(<ProbePanel />);

    const dir = (await screen.findByLabelText('Directory')) as HTMLInputElement;
    // Must show the mesh root — NOT the focused worktree subdir. The
    // worktree path slips into `activePath` only because `useProbeContext`
    // routes "where am I working" through the focused node's path; the
    // Directory field on Mesh Properties is a mesh-level property and
    // should always read from the mesh row.
    expect(dir.value).toBe('/repos/alpha');
    expect(dir.value).not.toContain('.claude/worktrees');
  });

  it('updates the Directory field when switching to a different mesh, even with a focused node still pointing at the old mesh', async () => {
    // This is the exact user-reported symptom: "the directory that is
    // shown on the mesh properties probe sometimes doesn't update when
    // switching meshes." The "sometimes" is the focused-node case —
    // without a focused node, `activePath` already falls back to the
    // mesh root, so the bug is invisible. With a focused node still
    // pointing at Mesh A's worktree, switching the sidebar to Mesh B
    // leaves `activeNodeId` set (selectMesh only updates selectedMeshId),
    // so `activePath` keeps resolving to Mesh A's worktree.
    setupTwoMeshesWithFocusedNode();
    render(<ProbePanel />);

    // Sanity: directory starts on Mesh A's path.
    const dir = (await screen.findByLabelText('Directory')) as HTMLInputElement;
    expect(dir.value).toBe('/repos/alpha');

    // Switch the sidebar selection to Mesh B. This is exactly what
    // `Sidebar.handleSelectMesh` does on click — `selectMesh(meshId)`.
    act(() => {
      useMeshStore.getState().selectMesh(MESH_B.id);
    });

    await waitFor(() => {
      const updated = screen.getByLabelText('Directory') as HTMLInputElement;
      expect(updated.value).toBe('/repos/beta');
    });
  });
});

// Delete Mesh — restored from the legacy `MeshPropertiesPanel` (deleted in #380).
// Issue #1460 moved the trigger from a bare red button at the foot of an
// undifferentiated form into a LABELLED danger zone that states the impact
// and the recovery story before the confirmation. The destructive operation
// + shared confirmation dialog + probe-close behaviour are unchanged.
describe('ProjectSettingsTab — Delete Mesh danger zone (issue #1460)', () => {
  it('renders Delete Mesh inside a labelled danger zone that explains impact and recovery', async () => {
    openProbeDestination('properties');

    // Only the trigger button is in the DOM at this point (no dialog yet).
    const button = await screen.findByRole('button', { name: /delete mesh/i });
    expect(button).toBeTruthy();

    // Issue #1460 (AC1/AC4): the destructive control must be inside an
    // explicitly labelled danger zone, and the zone must state both the
    // impact and the recovery story BEFORE the user is asked to confirm.
    // Asserting the trigger alone is not enough - a red button at the bottom
    // of a form is exactly what the issue rejected.
    const zone = await screen.findByTestId('project-settings-danger-zone');
    expect(zone.getAttribute('aria-label')).toBe('Danger zone');
    expect(zone.getAttribute('data-tone')).toBe('danger');
    expect(zone.textContent).toMatch(/stops its agents/i);
    expect(zone.textContent).toMatch(/left alone|can be added again/i);
    // The trigger is inside the zone, not a sibling of it.
    expect(zone.contains(button)).toBe(true);
  });

  it('keeps every non-destructive section outside the danger zone', async () => {
    openProbeDestination('properties');

    const zone = await screen.findByTestId('project-settings-danger-zone');
    for (const label of ['Name', 'Build command', 'Run command', 'Default provider']) {
      expect(zone.textContent).not.toContain(label);
    }
  });

  it('opens a confirmation dialog when the Delete Mesh button is clicked', async () => {
    const user = userEvent.setup();
    openProbeDestination('properties');

    const trigger = await screen.findByRole('button', { name: /delete mesh/i });
    await user.click(trigger);

    // The dialog's unique confirmation copy — the trigger button never has
    // this text, so this matcher is unambiguous.
    expect(
      await screen.findByText(/agent node/)
    ).toBeTruthy();
    // The dialog heading is an <h2>; the trigger is a <button>, so this
    // assertion also distinguishes the two "Delete Mesh" text nodes.
    expect(
      screen.getByRole('heading', { name: 'Delete Mesh' })
    ).toBeTruthy();
    // Two actions: Cancel + the destructive confirm (label "Delete", not
    // "Delete Mesh" — that's only the trigger).
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeTruthy();
    expect(
      screen.getByRole('button', { name: 'Delete', exact: true })
    ).toBeTruthy();
  });

  it('names the exact agent node count in the confirmation (issue #1460 AC4)', async () => {
    const user = userEvent.setup();
    // Two nodes belong to mesh 42; one belongs to a different project and must
    // not be counted, or the confirmation overstates the blast radius.
    seedAgentNodes([
      nodeFixture({ id: 1, mesh_id: 42 }),
      nodeFixture({ id: 2, mesh_id: 42 }),
      nodeFixture({ id: 3, mesh_id: 99 }),
    ]);
    openProbeDestination('properties');

    await user.click(await screen.findByRole('button', { name: /delete mesh/i }));

    // Issue #1460 review finding: the release note claimed the confirmation
    // "names the project and the node count" while the message said only "all
    // its agent nodes". The count is the actionable part.
    const dialog = (await screen.findByRole('button', { name: 'Delete', exact: true }))
      .closest('[role="dialog"]');
    const text = dialog?.textContent ?? '';
    expect(text).toContain('demo');
    expect(text).toContain('its 2 agent nodes');
    expect(text).not.toContain('all its agent nodes');
  });

  it('uses the singular for a one-node project', async () => {
    const user = userEvent.setup();
    seedAgentNodes([nodeFixture({ id: 1, mesh_id: 42 })]);
    openProbeDestination('properties');

    await user.click(await screen.findByRole('button', { name: /delete mesh/i }));
    await screen.findByText(/its 1 agent node\?/);
  });

  it('cancels without deleting when Cancel is pressed', async () => {
    const user = userEvent.setup();
    openProbeDestination('properties');

    const trigger = await screen.findByRole('button', { name: /delete mesh/i });
    await user.click(trigger);
    await screen.findByText(/agent node/);

    await user.click(screen.getByRole('button', { name: 'Cancel' }));

    await waitFor(() => {
      expect(screen.queryByText(/agent node/)).toBeNull();
    });
    // The trigger is still in the DOM (no destructive call happened).
    expect(
      screen.getByRole('button', { name: /delete mesh/i })
    ).toBeTruthy();
    expect(invoke).not.toHaveBeenCalledWith('delete_mesh', expect.anything());
  });

  it('confirms: calls delete_mesh via the store, refetches, and closes the probe', async () => {
    const user = userEvent.setup();
    // Issue #1375: the rail is gone, so the probe is opened by routing
    // through the same store action the palette / title bar / contextual
    // entries use. `openProbeDestination` is synchronous; the assertion
    // below pins probeOpen=true before the click flow runs.
    openProbeDestination('properties');
    expect(useUIStore.getState().probeOpen).toBe(true);

    const trigger = await screen.findByRole('button', { name: /delete mesh/i });
    await user.click(trigger);
    await screen.findByText(/agent node/);

    await user.click(
      screen.getByRole('button', { name: 'Delete', exact: true })
    );

    // The store's `deleteMesh` calls `delete_mesh` and then `list_meshes`
    // (to refresh the sidebar). We assert the destructive IPC fired with
    // the right mesh id — that's the user-visible contract.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('delete_mesh', { meshId: 42 });
    });
    // Legacy parity: closing the drawer after delete. The probe is the
    // drawer now, so `toggleProbe()` flips probeOpen to false.
    expect(useUIStore.getState().probeOpen).toBe(false);
  });
});

// Save-feedback (issue #729) — `ProjectSettingsTab` was the only probe tab
// without a Saving…/Saved/Save failed indicator, and its blur handlers
// produced unhandled rejections on IPC failure (the field still showed
// the user's unsaved text with no message). These tests pin the fix:
//   - successful writes flip the global SaveIndicator to "Saved"
//   - a rejected write flips it to "Save failed" with the error message
//     (the unhandled-rejection path is gone — the catch lives inside
//      the save hook, the IPC promise resolves cleanly)
//   - the field's text is preserved after a failed save (acceptance
//     criterion: don't revert on failure)
//   - a slow save from the outgoing mesh does NOT surface its result
//     on the new mesh (review finding #1; the `wrappedSave` adapter
//     discards late results on mesh-switch)
//   - a re-saved-then-resolved write clears the prior "Save failed"
//
// The third test ("does NOT leak an unhandled rejection…") attaches its
// own per-test `unhandledrejection` listener because there is no
// project-wide listener for jsdom-emitted promise rejections. The
// listener captures the rejection reason and the test fails if any
// rejection lands.
describe('ProjectSettingsTab — save feedback (issue #729)', () => {
  // The store has only one mesh configured by `beforeEach` in the outer
  // suite (MESH = id 42, name "demo"). For the mesh-switch test we
  // need a second mesh to switch INTO.
  const MESH_B: Mesh = {
    ...MESH,
    id: 99,
    name: 'other',
    path: '/repos/other',
  };

  function rejectNextBuildWrite(message: string) {
    let armed = true;
    vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
      if (armed && cmd === 'update_mesh_column') {
        armed = false;
        return Promise.reject(new Error(message));
      }
      if (cmd === 'list_providers') {
        return Promise.resolve([
          { id: 'claude', label: 'Claude Code', color: '#000', icon: '', resumable: true, harness_id: 'claude', provider_id: null, is_proxied: false, group_key: 'claude', capabilities: capsFixture('claude') },
        ]);
      }
      if (cmd === 'get_mesh_properties') return Promise.resolve(MESH_CONFIG);
      if (cmd === 'detect_mesh_project')
        return Promise.resolve({ preset_id: null, label: null, node_scripts: null });
      if (cmd === 'detect_ai_context')
        return Promise.resolve({
          claude_md_exists: false, agents_md_exists: false, skills_dir_exists: false,
          skill_count: 0, agents_skills_exists: false,
        });
      if (cmd === 'get_mesh_health')
        return Promise.resolve({
          is_dirty: false, is_drifted: false, unpushed_ahead: 0,
          base_branch_holder: null, local_base_branch: 'main',
          current_branch: 'main', current_short_sha: 'abc1234', authenticated: false,
        });
      if (cmd === 'list_meshes') return Promise.resolve([]);
      return Promise.resolve({});
    });
  }

  it('shows a global "Saved" indicator after a successful blur save', async () => {
    const user = userEvent.setup();
    openProbeDestination('properties');

    const build = (await screen.findByLabelText(/^Build command/)) as HTMLInputElement;
    await user.clear(build);
    await user.type(build, 'cargo build --release');
    fireEvent.blur(build);

    // Wait for the save's transition: Saving… → Saved. The save itself
    // is fire-and-await'd inside onBlur, so the indicator resolves once
    // the IPC's `.then` runs.
    expect(await screen.findByText('Saved')).toBeTruthy();
    // And the IPC fired with the right payload.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_column', {
        meshId: 42, column: 'build_command', value: 'cargo build --release',
      });
    });
  });

  it('shows "Save failed: <message>" and keeps the typed value for retry', async () => {
    const user = userEvent.setup();
    rejectNextBuildWrite('boom-build-save');
    openProbeDestination('properties');

    const build = (await screen.findByLabelText(/^Build command/)) as HTMLInputElement;
    expect(build.value).toBe('npm run build');
    await user.clear(build);
    await user.type(build, 'cargo build --release');
    fireEvent.blur(build);

    // The error message surfaces in the global banner. The "Save failed:"
    // prefix is part of the indicator copy so the user distinguishes it
    // from any other surface; the rest is the rejection's `.message`.
    expect(await screen.findByText(/Save failed.*boom-build-save/)).toBeTruthy();

    // The tab's documented "do not revert on failure" rule: the field keeps
    // the user's input so they can retry rather than losing their edit.
    expect(build.value).toBe('cargo build --release');
  });

  it('does NOT leak an unhandled rejection on a failing blur save', async () => {
    const user = userEvent.setup();
    let captured: unknown = null;
    const listener = (ev: PromiseRejectionEvent) => {
      captured = ev.reason;
      ev.preventDefault();
    };
    // Listen on the actual `unhandledrejection` channel so a leak lands
    // in `captured` even if the tab's component tree ate the warning.
    window.addEventListener('unhandledrejection', listener);

    rejectNextBuildWrite('boom-leak-test');
    openProbeDestination('properties');

    const build = (await screen.findByLabelText(/^Build command/)) as HTMLInputElement;
    await user.clear(build);
    await user.type(build, 'cargo build --fail');
    fireEvent.blur(build);

    // Wait past the awaited IPC + the next microtask boundary.
    await waitFor(() => {
      expect(screen.queryByText(/Save failed/)).toBeTruthy();
    });

    // Give the event loop a tick to dispatch any leaked rejection.
    await new Promise((r) => setTimeout(r, 0));
    if (captured !== null) {
      throw new Error(
        `Unhandled rejection detected: ${captured instanceof Error ? captured.message : String(captured)}`,
      );
    }
    window.removeEventListener('unhandledrejection', listener);
  });

  it('discards a stale save result when the user switches meshes mid-flight (review finding #1)', async () => {
    const user = userEvent.setup();
    // Register Mesh B so the sidebar can switch to it. The store's
    // beforeEach wires Mesh (id 42) as the selected mesh; we add B
    // alongside without unselecting A.
    useMeshStore.setState({
      meshes: [MESH, MESH_B],
      meshesById: new Map<number, Mesh>([
        [MESH.id, MESH],
        [MESH_B.id, MESH_B],
      ]),
      selectedMeshId: MESH.id,
    });
    // Make the FIRST `update_mesh_column` reject on the slow side
    // (returns after a small delay) so we have time to switch meshes
    // before the rejection lands.
    let armed = true;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (armed && cmd === 'update_mesh_column') {
        armed = false;
        return new Promise((_, reject) =>
          setTimeout(() => reject(new Error('boom-after-switch')), 50),
        );
      }
      if (cmd === 'list_providers') {
        return Promise.resolve([
          { id: 'claude', label: 'Claude Code', color: '#000', icon: '', resumable: true, harness_id: 'claude', provider_id: null, is_proxied: false, group_key: 'claude', capabilities: capsFixture('claude') },
        ]);
      }
      if (cmd === 'get_mesh_properties') return Promise.resolve(MESH_CONFIG);
      if (cmd === 'detect_mesh_project') return Promise.resolve({ preset_id: null, label: null, node_scripts: null });
      if (cmd === 'detect_ai_context') return Promise.resolve({ claude_md_exists: false, agents_md_exists: false, skills_dir_exists: false, skill_count: 0, agents_skills_exists: false });
      if (cmd === 'get_mesh_health') return Promise.resolve({ is_dirty: false, is_drifted: false, unpushed_ahead: 0, base_branch_holder: null, local_base_branch: 'main', current_branch: 'main', current_short_sha: 'abc1234', authenticated: false });
      if (cmd === 'list_meshes') return Promise.resolve([]);
      return Promise.resolve({});
    });

    openProbeDestination('properties');
    const build = (await screen.findByLabelText(/^Build command/)) as HTMLInputElement;
    await user.clear(build);
    await user.type(build, 'mesh-a-edit');
    fireEvent.blur(build);

    // Switch to mesh B before the 50ms-delayed rejection lands. The
    // SaveIndicator's `useEffect([activeMeshId])` reset already wipes
    // the indicator to idle; the `wrappedSave` mesh-switch guard must
    // additionally stop the late reject() from re-applying "Save failed".
    await act(async () => {
      useMeshStore.getState().selectMesh(MESH_B.id);
    });

    // Wait long enough for the 50ms-delayed rejection to fire AND for
    // the post-resolve setState to flush.
    await new Promise((r) => setTimeout(r, 100));

    // The indicator must NOT show "Save failed: boom-after-switch" —
    // that error belongs to mesh A's edit, not to mesh B's view.
    expect(screen.queryByText(/Save failed.*boom-after-switch/)).toBeNull();
    // And the `console.error` from the adapter's stale-rejection path
    // is the only visible trace of the failure (the indicator stays clean).
  });

  it('discards a stale worktree-strategy save failure when the user switches meshes (issue #1460 review)', async () => {
    const user = userEvent.setup();
    useMeshStore.setState({
      meshes: [MESH, MESH_B],
      meshesById: new Map<number, Mesh>([
        [MESH.id, MESH],
        [MESH_B.id, MESH_B],
      ]),
      selectedMeshId: MESH.id,
    });
    // Arm the FIRST `update_mesh_use_worktree` to reject on a delay so the
    // switch happens while it is still in flight.
    let armed = true;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (armed && cmd === 'update_mesh_use_worktree') {
        armed = false;
        return new Promise((_, reject) =>
          setTimeout(() => reject(new Error('boom-strategy-after-switch')), 50),
        );
      }
      if (cmd === 'list_providers') return Promise.resolve([]);
      if (cmd === 'get_mesh_properties') return Promise.resolve(MESH_CONFIG);
      if (cmd === 'detect_mesh_project') return Promise.resolve({ preset_id: null, label: null, node_scripts: null });
      if (cmd === 'detect_ai_context') return Promise.resolve({ claude_md_exists: false, agents_md_exists: false, skills_dir_exists: false, skill_count: 0, agents_skills_exists: false });
      if (cmd === 'get_mesh_health') return Promise.resolve({ is_dirty: false, is_drifted: false, unpushed_ahead: 0, base_branch_holder: null, local_base_branch: 'main', current_branch: 'main', current_short_sha: 'abc1234', authenticated: false });
      if (cmd === 'list_meshes') return Promise.resolve([]);
      return Promise.resolve({});
    });

    openProbeDestination('properties');
    await user.click(await screen.findByLabelText('Use worktree'));

    // Switch projects before the delayed rejection lands.
    await act(async () => {
      useMeshStore.getState().selectMesh(MESH_B.id);
    });
    await new Promise((r) => setTimeout(r, 120));

    // Mesh A's failure must not surface in mesh B's form. The section-local
    // error channel needs the same mesh-switch guard `wrappedSave` has, or the
    // `useEffect([activeMeshId])` reset is immediately undone by the late
    // reject.
    expect(screen.queryByText(/Failed to update use_worktree/)).toBeNull();
  });

  it('clears the previous "Save failed" indicator when a subsequent save succeeds', async () => {
    const user = userEvent.setup();
    rejectNextBuildWrite('first-failure');
    openProbeDestination('properties');

    const build = (await screen.findByLabelText(/^Build command/)) as HTMLInputElement;
    await user.clear(build);
    await user.type(build, 'fail-then-succeed');
    fireEvent.blur(build);

    expect(await screen.findByText(/Save failed/)).toBeTruthy();

    // Re-arm — the next `update_mesh_column` will succeed (the next blur
    // on the same field). Default mock resolver returns `{}`, which the
    // IPC treats as a clean resolution.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'update_mesh_column') return Promise.resolve();
      if (cmd === 'list_providers') return Promise.resolve([]);
      if (cmd === 'get_mesh_properties') return Promise.resolve(MESH_CONFIG);
      if (cmd === 'detect_mesh_project') return Promise.resolve({ preset_id: null, label: null, node_scripts: null });
      if (cmd === 'detect_ai_context') return Promise.resolve({ claude_md_exists: false, agents_md_exists: false, skills_dir_exists: false, skill_count: 0, agents_skills_exists: false });
      if (cmd === 'get_mesh_health') return Promise.resolve({ is_dirty: false, is_drifted: false, unpushed_ahead: 0, base_branch_holder: null, local_base_branch: 'main', current_branch: 'main', current_short_sha: 'abc1234', authenticated: false });
      if (cmd === 'list_meshes') return Promise.resolve([]);
      return Promise.resolve({});
    });
    // Avoid the auto-clear of "saved" racing this assertion — we just
    // want to confirm the error copy disappears, not test the timer.
    // (The timer is independently covered in `useSaveStatus` tests.)

    await user.clear(build);
    await user.type(build, 'recovered');
    fireEvent.blur(build);

    await waitFor(() => {
      expect(screen.queryByText(/Save failed/)).toBeNull();
    });
  });
});

// ── Issue #1460: sections + the worktree strategy that moved here ────────────

/**
 * The section structure is the deliverable, not decoration. Issue #1460's
 * premise was that a form with fields stacked one under another gave the user
 * no way to tell a preference from a strategy decision from a destructive
 * action. These tests pin the headings, their order, and their contents, so a
 * future field lands in a section rather than back in an undifferentiated
 * column.
 */
describe('ProjectSettingsTab — section structure (issue #1460)', () => {
  it('renders the labelled sections in configuration order, danger zone last', async () => {
    openProbeDestination('properties');

    const general = await screen.findByTestId('project-settings-general');
    const runtime = await screen.findByTestId('project-settings-runtime');
    const buildRun = await screen.findByTestId('project-settings-build-run');
    const strategy = await screen.findByTestId('project-settings-worktree-strategy');
    const danger = await screen.findByTestId('project-settings-danger-zone');

    expect([
      [general.getAttribute('aria-label'), runtime.getAttribute('aria-label'), buildRun.getAttribute('aria-label'), strategy.getAttribute('aria-label'), danger.getAttribute('aria-label')],
    ]).toEqual([['General', 'Agent runtime', 'Build and run', 'Worktree strategy', 'Danger zone']]);

    // Document order must match reading order — assert the DOM order, not
    // just that the nodes exist, or a re-ordered form would still pass.
    const inOrder = [general, runtime, buildRun, strategy, danger].every(
      (node, i, all) => i === 0 || (all[i - 1].compareDocumentPosition(node) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0,
    );
    expect(inOrder).toBe(true);
  });

  it('places each existing field in the section that describes it', async () => {
    // The sandbox toggle only renders behind the developer gate (#2034), so
    // this structural assertion has to open it.
    mockBackend({ sandboxDevMode: true });
    openProbeDestination('properties');
    await screen.findByTestId('project-settings-general');

    const general = screen.getByTestId('project-settings-general');
    expect(within(general).getByLabelText('Name')).toBeTruthy();
    expect(within(general).getByLabelText('Directory')).toBeTruthy();

    const runtime = screen.getByTestId('project-settings-runtime');
    expect(within(runtime).getByLabelText('Default provider')).toBeTruthy();
    expect(within(runtime).getByLabelText(/Sandbox agent processes/i)).toBeTruthy();

    const buildRun = screen.getByTestId('project-settings-build-run');
    expect(within(buildRun).getByLabelText(/^Build command/i)).toBeTruthy();
    expect(within(buildRun).getByLabelText(/^Run command/i)).toBeTruthy();
    expect(within(buildRun).getByLabelText(/^Root build command/i)).toBeTruthy();
    expect(within(buildRun).getByLabelText(/^Root run command/i)).toBeTruthy();
    // The worktree strategy is its own section, not a tail on Build and run.
    expect(within(buildRun).queryByLabelText('Use worktree')).toBeNull();
    expect(within(screen.getByTestId('project-settings-worktree-strategy')).getByLabelText('Use worktree')).toBeTruthy();
  });

  it('states that the settings apply to the project root, not the focused worktree', async () => {
    openProbeDestination('properties');
    const note = await screen.findByTestId('probe-scope-note');
    expect(note.textContent).toMatch(/project root/i);
    expect(note.textContent).toMatch(/focused agent/i);
  });
});

/**
 * The worktree-strategy block moved here from the Worktree Manager tab
 * (issue #1460). These tests moved with it: same behaviour, same wire values,
 * new owner. The wire contract (`origin/main`/`HEAD`, `branched`/`detached`,
 * the pool size clamp) is unchanged — only the destination is.
 */
describe('ProjectSettingsTab — worktree strategy (moved from the Worktree Manager)', () => {
  it('loads the strategy controls from get_mesh_properties', async () => {
    mockBackend({
      meshConfig: {
        ...MESH_CONFIG,
        use_worktree: true,
        base_ref: 'HEAD',
        worktree_mode: 'detached',
        pre_spawn_pool_size: 3,
      },
    });
    openProbeDestination('properties');

    const useWorktree = (await screen.findByLabelText('Use worktree')) as HTMLInputElement;
    expect(useWorktree.checked).toBe(true);
    // HEAD round-trips to the 'Head' form option, not the raw wire string.
    const head = (await screen.findByLabelText(/Head/)) as HTMLInputElement;
    expect(head.value).toBe('head');
    expect(head.checked).toBe(true);
    const fresh = screen.getByLabelText(/Fresh/) as HTMLInputElement;
    expect(fresh.value).toBe('fresh');
    expect(fresh.checked).toBe(false);
    const detached = (await screen.findByLabelText(/^Detached/)) as HTMLInputElement;
    expect(detached.checked).toBe(true);
    const branched = screen.getByLabelText(/^Branched/) as HTMLInputElement;
    expect(branched.checked).toBe(false);
  });

  it('toggling use-worktree calls update_mesh_use_worktree and collapses the radios', async () => {
    const user = userEvent.setup();
    openProbeDestination('properties');

    const useWorktree = await screen.findByLabelText('Use worktree');
    await user.click(useWorktree);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_use_worktree', {
        meshId: 42,
        useWorktree: false,
      });
    });
    // The strategy sub-controls collapse with the gate.
    expect(screen.queryByLabelText(/Fresh/)).toBeNull();
  });

  it('selecting Head calls update_worktree_base_ref with HEAD on the wire', async () => {
    const user = userEvent.setup();
    openProbeDestination('properties');

    await user.click(await screen.findByLabelText(/Head/));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_worktree_base_ref', {
        meshId: 42,
        baseRef: 'HEAD',
      });
    });
  });

  it('selecting Detached calls update_mesh_column with column=worktree_mode', async () => {
    const user = userEvent.setup();
    openProbeDestination('properties');

    await user.click(await screen.findByLabelText(/^Detached/));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_column', {
        meshId: 42,
        column: 'worktree_mode',
        value: 'detached',
      });
    });
  });

  it('defaults a null worktree_mode to branched on load', async () => {
    mockBackend({ meshConfig: { ...MESH_CONFIG, worktree_mode: null } });
    openProbeDestination('properties');

    const branched = (await screen.findByLabelText(/^Branched/)) as HTMLInputElement;
    expect(branched.checked).toBe(true);
  });

  it('surfaces a save failure inline, names the control, and does NOT revert the form', async () => {
    const user = userEvent.setup();
    mockBackend({ saveUseWorktreeFails: true });
    openProbeDestination('properties');

    const useWorktree = (await screen.findByLabelText('Use worktree')) as HTMLInputElement;
    expect(useWorktree.checked).toBe(true);
    await user.click(useWorktree);

    const error = await screen.findByTestId('worktree-strategy-error');
    expect(error.textContent).toContain('Failed to update use_worktree');
    // The "do not revert on failure" rule: the checkbox keeps the user's
    // click so the message is actionable.
    expect(useWorktree.checked).toBe(false);
  });

  it('hides the pool badge when the pool is disabled (size 0)', async () => {
    mockBackend({ meshConfig: { ...MESH_CONFIG, pre_spawn_pool_size: 0 }, poolCount: 0 });
    openProbeDestination('properties');

    await screen.findByLabelText('Use worktree');
    expect(screen.queryByTestId('pool-status')).toBeNull();
  });

  it('shows "X / Y ready" when the pool is enabled', async () => {
    mockBackend({ meshConfig: { ...MESH_CONFIG, pre_spawn_pool_size: 3 }, poolCount: 2 });
    openProbeDestination('properties');

    const badge = await screen.findByTestId('pool-status');
    expect(badge.textContent).toContain('2 / 3 ready');
    // A11y: the full sentence is the accessible name, not the placeholder.
    expect(badge.getAttribute('aria-label')).toBe('2 of 3 pre-spawn worktrees ready');
  });

  it('re-fetches the pool count when pool-count-changed fires', async () => {
    mockBackend({ meshConfig: { ...MESH_CONFIG, pre_spawn_pool_size: 3 }, poolCount: 1 });
    openProbeDestination('properties');
    expect((await screen.findByTestId('pool-status-text')).textContent).toContain('1 / 3');

    // A claim or a refill in the pool worker fires this event; the badge must
    // follow it, because the count is what the user uses to decide whether to
    // wait. Fire it inside act() so the refetch's setState is flushed.
    mockBackend({ meshConfig: { ...MESH_CONFIG, pre_spawn_pool_size: 3 }, poolCount: 3 });
    await act(async () => {
      await emit(POOL_COUNT_CHANGED_EVENT, 42);
    });

    expect((await screen.findByTestId('pool-status-text')).textContent).toContain('3 / 3');
  });

  it('hydrates the worktree directory override and effective path on first open', async () => {
    // Regression guard: the field must show the project's stored override
    // before any save, otherwise the section reads as "unset" for a project
    // that has one and the user cannot tell an inherited path from a missing
    // one. The move out of the Worktree Manager (issue #1460) briefly dropped
    // this mount hydration, and a test that only asserted the post-save state
    // passed regardless.
    mockBackend({
      meshConfig: { ...MESH_CONFIG, worktree_directory: 'wt/here' },
    });
    openProbeDestination('properties');

    const input = (await screen.findByLabelText('Worktree directory')) as HTMLInputElement;
    expect(input.value).toBe('wt/here');
    // The effective path is backend-authoritative, not a TS re-spelling.
    const effective = (await screen.findByText(/Effective:/)).textContent ?? '';
    expect(effective).toContain('/repos/demo/.claude/worktrees');
    // An override is no longer "inherited", so the reset affordance appears.
    expect(screen.getByRole('button', { name: /reset to inherited/i })).toBeTruthy();
  });

  it('shows no reset affordance and marks the path inherited when unset', async () => {
    mockBackend({ meshConfig: { ...MESH_CONFIG, worktree_directory: null } });
    openProbeDestination('properties');

    await screen.findByLabelText('Worktree directory');
    const effective = (await screen.findByText(/Effective:/)).textContent ?? '';
    expect(effective).toContain('(inherited)');
    expect(screen.queryByRole('button', { name: /reset to inherited/i })).toBeNull();
  });

  it('saves the worktree directory override on blur', async () => {
    const user = userEvent.setup();
    openProbeDestination('properties');

    const input = (await screen.findByLabelText('Worktree directory')) as HTMLInputElement;
    expect(input.value).toBe('');

    await user.type(input, 'wt/here');
    await user.tab();

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_worktree_directory', {
        meshId: 42,
        directory: 'wt/here',
      });
    });
    // The resolved effective path is backend-authoritative, not a TS guess.
    // Match the line, not the nested <code> (testing-library's text matcher
    // returns the deepest element that matches).
    const effective = (await screen.findByText(/Effective:/)).textContent ?? '';
    expect(effective).toContain('/repos/demo/.claude/worktrees');
  });
});

// Issue #2024 rank 8 — a rejected rename must not report success.
//
// `saveName` awaits `meshStore.updateMeshName`, which used to catch the
// `update_mesh_name` IPC failure and resolve. `wrappedSave` then ran its
// success branch, so the tab showed "Saved" for a write the backend had
// refused, while the sidebar (which only updates on success) kept the old
// name. The contract pinned here: the rejection propagates, the draft the
// user typed survives, the sidebar is untouched, and a successful retry
// makes both views agree.
describe('ProjectSettingsTab — rejected rename reports failure (issue #2024 rank 8)', () => {
  it('keeps the draft, shows no Saved, and leaves the sidebar on the old name — then converges on retry', async () => {
    const user = userEvent.setup();
    mockBackend({ saveNameFails: true });
    openProbeDestination('properties');

    const input = (await screen.findByLabelText('Name')) as HTMLInputElement;
    expect(input.value).toBe('demo');

    await user.clear(input);
    await user.type(input, 'renamed');
    await user.tab();

    // The rejection is surfaced, not swallowed into a success.
    await waitFor(() => {
      expect(screen.getByTestId('save-indicator').textContent).toContain('Save failed');
    });
    expect(screen.getByTestId('save-indicator').textContent).toContain(
      'mock: update_mesh_name rejected',
    );
    // No "Saved" anywhere in the indicator — the decisive assertion: this
    // is the exact string the old swallowed rejection produced.
    expect(screen.getByTestId('save-indicator').textContent).not.toContain('Saved');

    // The draft survives the failure so the user can fix and retry.
    expect((screen.getByLabelText('Name') as HTMLInputElement).value).toBe('renamed');

    // The sidebar source of truth did not move.
    expect(useMeshStore.getState().meshesById.get(MESH.id)?.name).toBe('demo');

    // Retry against a backend that accepts: both views converge.
    mockBackend({});
    await user.clear(input);
    await user.type(input, 'renamed');
    await user.tab();

    await waitFor(() => {
      expect(useMeshStore.getState().meshesById.get(MESH.id)?.name).toBe('renamed');
    });
    expect(screen.getByTestId('save-indicator').textContent).toContain('Saved');
  });
});