/**
 * Tests for the clean Mesh Properties tab — issue #375.
 *
 * The new tab ports the configuration fields from the legacy
 * `MeshPropertiesPanel` and *excludes* the Git-maintenance UI (worktree
 * config, branches, uncommitted changes). The suite below pins both:
 * the field surface that must stay, and the surface that must NOT come
 * back when we delete the legacy drawer.
 *
 * Rendering strategy: mount the full `ProbePanel` and click into the
 * properties tab, so the test also covers the routing wiring in
 * `ProbePanel.tsx` (otherwise the routing test in `probe-panel.test.tsx`
 * would have to know the tab's internal structure).
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { ProbePanel } from '../../src/components/Probe/ProbePanel';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';

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

const MESH_CONFIG = {
  name: 'demo',
  build_command: 'npm run build',
  run_command: 'npm run dev',
  model: 'opus-4',
  effort: 'high',
  base_ref: 'origin/main',
  use_worktree: true,
  worktree_mode: 'branched',
  default_provider: 'anthropic',
  sandbox: true,
};

/**
 * Wire the mocked `invoke` to answer each command the tab calls during
 * mount + a single edit cycle. Anything we don't care about resolves
 * with `{}` so other panels can keep loading in the same render.
 */
function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
    switch (cmd) {
      case 'list_providers':
        // Issue #575 / ADR-0016 — Spawn Options carry the full wire shape
        // (harness_id, provider_id, is_proxied, group_key). The mock
        // returns three native harnesses, each its own group, so the
        // selector renders three plain `<option>` rows (no optgroups
        // for a single-row group).
        return Promise.resolve([
          { id: 'claude', label: 'Claude Code', color: '#000', icon: '', resumable: true, harness_id: 'claude', provider_id: null, is_proxied: false, group_key: 'claude' },
          { id: 'anthropic', label: 'Anthropic', color: '#000', icon: '', resumable: true, harness_id: 'claude', provider_id: 'anthropic', is_proxied: true, group_key: 'claude' },
          { id: 'codex', label: 'Codex', color: '#000', icon: '', resumable: false, harness_id: 'codex', provider_id: null, is_proxied: false, group_key: 'codex' },
        ]);
      case 'get_mesh_properties':
        return Promise.resolve(MESH_CONFIG);
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
      case 'delete_mesh':
        return Promise.resolve();
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

async function openPropertiesTab() {
  const user = userEvent.setup();
  render(<ProbePanel />);
  await user.click(screen.getByRole('button', { name: 'Mesh Properties' }));
}

beforeEach(() => {
  useMeshStore.setState({
    meshes: [MESH],
    meshesById: new Map([[MESH.id, MESH]]),
    selectedMeshId: MESH.id,
  });
  useAgentNodeStore.setState({ agentNodes: [], activeNodeId: null });
  useUIStore.setState({ probeOpen: false, probeTab: 'files', activeDiffFile: null });
  mockBackend();
});

describe('MeshPropertiesTab (issue #375)', () => {
  it('renders the config form when the ⚙️ tab is open and a mesh is selected', async () => {
    await openPropertiesTab();

    // Config fields that the new tab must keep. Model / Effort carry a
    // "(cwrap only)" hint next to the visible label, so the regex lets
    // the matcher accept the trailing hint without us hard-coding it.
    expect(await screen.findByLabelText('Name')).toBeTruthy();
    expect(screen.getByLabelText('Directory')).toBeTruthy();
    expect(screen.getByLabelText(/^Model\b/)).toBeTruthy();
    expect(screen.getByLabelText(/^Effort\b/)).toBeTruthy();
    expect(screen.getByLabelText('Default provider')).toBeTruthy();
    expect(screen.getByLabelText('Project preset')).toBeTruthy();
    expect(screen.getByLabelText(/^Build command/)).toBeTruthy();
    expect(screen.getByLabelText(/^Run command/)).toBeTruthy();
  });

  it('shows the active tab label in the probe header', async () => {
    useUIStore.setState({ probeOpen: true, probeTab: 'properties' });
    render(<ProbePanel />);

    const header = screen.getByRole('region', { name: 'Probe panel' });
    expect(header.textContent).toContain('Mesh Properties');
  });

  it('excludes the worktree/branch maintenance fields', async () => {
    await openPropertiesTab();
    // Wait for the form to mount so the negative assertions are stable.
    expect(await screen.findByLabelText('Name')).toBeTruthy();

    // The legacy drawer's Git-maintenance fields must NOT appear.
    expect(screen.queryByLabelText(/^Use worktree$/i)).toBeNull();
    expect(screen.queryByLabelText('Starting point')).toBeNull();
    expect(screen.queryByLabelText('Worktree mode')).toBeNull();
    // The standalone sections from the legacy drawer.
    expect(screen.queryByText(/Branches & Worktrees/i)).toBeNull();
    expect(screen.queryByText('Uncommitted Changes')).toBeNull();
    // The Delete Mesh button IS supposed to come back (it lived on the
    // legacy drawer's footer) — the new tab inherits the destructive
    // operation. See the `Delete Mesh button` describe block below.
  });

  it('preloads the mesh config (Name, Model, Build/Run) from the backend', async () => {
    await openPropertiesTab();

    const name = (await screen.findByLabelText('Name')) as HTMLInputElement;
    expect(name.value).toBe('demo');

    const model = screen.getByLabelText(/^Model\b/) as HTMLInputElement;
    expect(model.value).toBe('opus-4');

    const effort = screen.getByLabelText(/^Effort\b/) as HTMLSelectElement;
    expect(effort.value).toBe('high');

    const build = screen.getByLabelText(/^Build command/) as HTMLInputElement;
    expect(build.value).toBe('npm run build');

    const run = screen.getByLabelText(/^Run command/) as HTMLInputElement;
    expect(run.value).toBe('npm run dev');

    const provider = screen.getByLabelText('Default provider') as HTMLSelectElement;
    expect(provider.value).toBe('anthropic');
  });

  it('groups the default-provider options by harness (issue #575) with no "Legacy" header', async () => {
    await openPropertiesTab();

    const provider = (await screen.findByLabelText('Default provider')) as HTMLSelectElement;
    // Issue #575 / ADR-0016 — the Spawn Menu is harness-grouped. A group
    // with more than one row becomes a native `<optgroup>`; a single-row
    // group stays a plain `<option>` (the common case for one-harness
    // configs). The optgroup label is the native row's friendly
    // `label` (the harness profile's user-facing name, e.g. "Claude
    // Code"), NOT the raw `harness_id` — code-review finding B3.
    const optgroups = provider.querySelectorAll('optgroup');
    expect(optgroups).toHaveLength(1);
    expect(optgroups[0].getAttribute('label')).toBe('Claude Code');
    // The Codex harness is a single-row group, so it stays a plain option.
    expect(provider.querySelector('option[value="claude"]')).toBeTruthy();
    expect(provider.querySelector('option[value="anthropic"]')).toBeTruthy();
    expect(provider.querySelector('option[value="codex"]')).toBeTruthy();
  });

  it('saves text fields on blur via update_mesh_column', async () => {
    const user = userEvent.setup();
    await openPropertiesTab();

    const model = (await screen.findByLabelText(/^Model\b/)) as HTMLInputElement;
    await user.clear(model);
    await user.type(model, 'sonnet-4');
    fireEvent.blur(model);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_column', {
        meshId: 42,
        column: 'model',
        value: 'sonnet-4',
      });
    });
  });

  it('saves Build/Run on blur', async () => {
    const user = userEvent.setup();
    await openPropertiesTab();

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

  // Regression for "Build/Run only editable via preset". The user's manual
  // typing must update the input's *displayed* value on every keystroke,
  // and the field must be a plain editable `<input type="text">` — no
  // `disabled`, no `readOnly`, no parent overlay swallowing keystrokes.
  it('keeps the Build/Run inputs editable: typing updates the value without blurring', async () => {
    const user = userEvent.setup();
    await openPropertiesTab();

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

  it('saves Effort on change and skips writes when cleared to ""', async () => {
    const user = userEvent.setup();
    await openPropertiesTab();

    const effort = (await screen.findByLabelText(/^Effort\b/)) as HTMLSelectElement;
    await user.selectOptions(effort, 'xhigh');

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_column', {
        meshId: 42,
        column: 'effort',
        value: 'xhigh',
      });
    });

    // Reset to "Not set" — the legacy panel deliberately omitted the
    // backend write here (no way to clear a not-null column). The new
    // tab preserves that behaviour.
    vi.mocked(invoke).mockClear();
    await user.selectOptions(effort, '');
    await new Promise((r) => setTimeout(r, 20));
    const effortWrites = vi.mocked(invoke).mock.calls.filter(
      ([cmd, args]) => cmd === 'update_mesh_column' && (args as { column?: string })?.column === 'effort',
    );
    expect(effortWrites.length).toBe(0);
  });

  it('applies a project preset to both Build and Run on a single change', async () => {
    const user = userEvent.setup();
    await openPropertiesTab();

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

// OS-level sandbox toggle (macOS Seatbelt #497 / Windows AppContainer #498).
  // The DB column is `sandbox` and is OS-agnostic at this layer; the OS-
  // specific spawn policy is decided at `spawn_environment::wrap`.
  it('renders the Sandbox toggle as an editable checkbox (#498)', async () => {
    await openPropertiesTab();
    const sandbox = (await screen.findByLabelText('Sandbox agent processes')) as HTMLInputElement;
    expect(sandbox.type).toBe('checkbox');
    expect(sandbox.disabled).toBe(false);
  });

  it('preloads the saved sandbox state into the checkbox', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
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
    await openPropertiesTab();
    const sandbox = (await screen.findByLabelText('Sandbox agent processes')) as HTMLInputElement;
    expect(sandbox.checked).toBe(true);
  });

  it('saves the Sandbox toggle on change via update_mesh_sandbox', async () => {
    const user = userEvent.setup();
    await openPropertiesTab();

    const sandbox = (await screen.findByLabelText('Sandbox agent processes')) as HTMLInputElement;
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
  // alongside the unified #498 OS-agnostic one, both bound to `form.sandbox`.
  // The OS-agnostic surface (canonical per ADR 0012) is the only one allowed.
  it('renders exactly one Sandbox toggle (no duplicate #497 macOS-only block)', async () => {
    await openPropertiesTab();
    // Regex matches both "Sandbox agent processes" and the old
    // "Sandbox agent processes (macOS only)" accessible names.
    const sandboxes = await screen.findAllByLabelText(/Sandbox agent processes/);
    expect(sandboxes).toHaveLength(1);
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
describe('MeshPropertiesTab — Directory field shows the mesh root (not the focused worktree)', () => {
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
    useAgentNodeStore.setState({
      agentNodes: [FOCUSED_NODE],
      activeNodeId: FOCUSED_NODE.id,
    });
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
// The footer button lived outside the form scroll-area, so the new tab renders
// it INSIDE the form for layout simplicity (the probe is a vertical column,
// not a fixed-height drawer). The destructive operation + confirmation dialog
// + probe-close behaviour are ported verbatim from the legacy handleDelete.
describe('MeshPropertiesTab — Delete Mesh button (restored from #380)', () => {
  it('renders a Delete Mesh button at the bottom of the form', async () => {
    await openPropertiesTab();

    // Only the trigger button is in the DOM at this point (no dialog yet).
    const button = await screen.findByRole('button', { name: /delete mesh/i });
    expect(button).toBeTruthy();
  });

  it('opens a confirmation dialog when the Delete Mesh button is clicked', async () => {
    const user = userEvent.setup();
    await openPropertiesTab();

    const trigger = await screen.findByRole('button', { name: /delete mesh/i });
    await user.click(trigger);

    // The dialog's unique confirmation copy — the trigger button never has
    // this text, so this matcher is unambiguous.
    expect(
      await screen.findByText(/all its agent nodes/i)
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

  it('cancels without deleting when Cancel is pressed', async () => {
    const user = userEvent.setup();
    await openPropertiesTab();

    const trigger = await screen.findByRole('button', { name: /delete mesh/i });
    await user.click(trigger);
    await screen.findByText(/all its agent nodes/i);

    await user.click(screen.getByRole('button', { name: 'Cancel' }));

    await waitFor(() => {
      expect(screen.queryByText(/all its agent nodes/i)).toBeNull();
    });
    // The trigger is still in the DOM (no destructive call happened).
    expect(
      screen.getByRole('button', { name: /delete mesh/i })
    ).toBeTruthy();
    expect(invoke).not.toHaveBeenCalledWith('delete_mesh', expect.anything());
  });

  it('confirms: calls delete_mesh via the store, refetches, and closes the probe', async () => {
    const user = userEvent.setup();
    // `openPropertiesTab()` opens the probe by clicking the activity bar
    // (handleTabClick toggles probeOpen false→true on a non-active tab).
    await openPropertiesTab();
    expect(useUIStore.getState().probeOpen).toBe(true);

    const trigger = await screen.findByRole('button', { name: /delete mesh/i });
    await user.click(trigger);
    await screen.findByText(/all its agent nodes/i);

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
