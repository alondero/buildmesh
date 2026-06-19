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
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
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
  use_sandbox: false,
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
  use_sandbox: false,
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
        return Promise.resolve([
          { id: 'anthropic', label: 'Anthropic', color: '#000', icon: '' },
          { id: 'minimax', label: 'Minimax', color: '#000', icon: '' },
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
      case 'update_mesh_use_sandbox':
        return Promise.resolve();
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
    // The legacy drawer's footer.
    expect(screen.queryByText(/Delete Mesh/i)).toBeNull();
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

  it('renders the Sandbox toggle as an editable checkbox (#498)', async () => {
    await openPropertiesTab();
    const sandbox = (await screen.findByLabelText('Sandbox agent processes')) as HTMLInputElement;
    expect(sandbox.type).toBe('checkbox');
    expect(sandbox.disabled).toBe(false);
  });

  it('preloads the saved use_sandbox state into the checkbox', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_mesh_properties') {
        return Promise.resolve({ ...MESH_CONFIG, use_sandbox: true });
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

  it('saves the Sandbox toggle on change via update_mesh_use_sandbox', async () => {
    const user = userEvent.setup();
    await openPropertiesTab();

    const sandbox = (await screen.findByLabelText('Sandbox agent processes')) as HTMLInputElement;
    expect(sandbox.checked).toBe(false);
    await user.click(sandbox);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_use_sandbox', {
        meshId: 42,
        useSandbox: true,
      });
    });
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
