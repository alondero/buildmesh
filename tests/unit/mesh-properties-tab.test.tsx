/**
 * Tests for the clean Mesh Properties tab — issue #375.
 *
 * The new tab ports the configuration fields from the legacy
 * `MeshPropertiesPanel` and *excludes* the Git-maintenance UI (worktree * config, branches, uncommitted changes). The suite below pins both:
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
  // Per-Mesh harness overrides (issue #1151) — pre-seed with a Claude
  // override so the section renders an editable row by default. The save
  // lifecycle tests blur into this row's model/effort inputs to exercise
  // the global SaveStatus indicator; the IPC is `upsert_mesh_harness_override`,
  // not `update_mesh_column`. Per-test overrides can flip this off to
  // exercise the empty state.
  harness_overrides: {
    claude: { model: 'opus-4', effort: 'high' },
  },
};

/** Capability descriptor for the test fixtures — every native row in
 *  `list_providers` carries one so the per-Mesh override section
 *  (issue #1151) can render capability-gated controls. Mirrors the
 *  shape of `HarnessCapabilities` from `src/types/generated/`. */
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
 */
function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
    switch (cmd) {
      case 'list_providers':
        // Issue #575 / ADR-0016 — Spawn Options carry the full wire shape
        // (harness_id, provider_id, is_proxied, group_key). The mock
        // returns three native harnesses, each its own group, so the
        // selector renders three plain `<option>` rows (no optgroups
        // for a single-row group). The `capabilities` field is the
        // issue #1149 descriptor — required by the per-Mesh override
        // section (issue #1151) and the App Settings harness defaults
        // section (issue #1150).
        return Promise.resolve([
          { id: 'claude', label: 'Claude Code', color: '#000', icon: '', resumable: true, harness_id: 'claude', provider_id: null, is_proxied: false, group_key: 'claude', capabilities: capsFixture('claude') },
          { id: 'anthropic', label: 'Anthropic', color: '#000', icon: '', resumable: true, harness_id: 'claude', provider_id: 'anthropic', is_proxied: true, group_key: 'claude', capabilities: capsFixture('claude') },
          { id: 'codex', label: 'Codex', color: '#000', icon: '', resumable: false, harness_id: 'codex', provider_id: null, is_proxied: false, group_key: 'codex', capabilities: capsFixture('codex') },
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

    // Config fields that the new tab must keep. The label regex anchors
    // at the start with `\b` because the Field component renders the
    // visible label + hint as one accessible name inside `<label>`
    // (e.g. "Model (Claude Code only)"); the open end lets the suffix
    // pass through so a future hint rewrite doesn't break the matcher.
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

  it('saves text fields on blur via upsert_mesh_harness_override', async () => {
    const user = userEvent.setup();
    await openPropertiesTab();

    const model = (await screen.findByTestId('mesh-override-model-input-claude')) as HTMLInputElement;
    await user.clear(model);
    await user.type(model, 'sonnet-4');
    fireEvent.blur(model);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('upsert_mesh_harness_override', {
        meshId: 42,
        harnessId: 'claude',
        value: { model: 'sonnet-4', effort: 'high' },
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

  // Per-context build/run commands (issue #802).
  it('renders the optional Root build/run command fields with a fallback hint', async () => {
    await openPropertiesTab();

    const rootBuild = (await screen.findByLabelText(/^Root build command/)) as HTMLInputElement;
    const rootRun = screen.getByLabelText(/^Root run command/) as HTMLInputElement;
    expect(rootBuild).toBeTruthy();
    expect(rootRun).toBeTruthy();
    // The accessible name carries the "(optional — falls back to …)" hint so
    // the user knows leaving it blank reuses the Build / Run command.
    expect(rootBuild.labels?.[0].textContent).toMatch(/optional.*falls back/i);
    expect(rootRun.labels?.[0].textContent).toMatch(/optional.*falls back/i);
    // Empty in the base fixture (both columns null) — the load path maps
    // null → ''.
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
    await openPropertiesTab();

    const rootBuild = (await screen.findByLabelText(/^Root build command/)) as HTMLInputElement;
    const rootRun = screen.getByLabelText(/^Root run command/) as HTMLInputElement;
    expect(rootBuild.value).toBe('cargo build --workspace');
    expect(rootRun.value).toBe('npm run lint --workspaces');
  });

  it('saves Root build/run on blur via update_mesh_column', async () => {
    const user = userEvent.setup();
    await openPropertiesTab();

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

    const effort = (await screen.findByTestId('mesh-override-effort-select-claude')) as HTMLSelectElement;
    await user.selectOptions(effort, 'medium');

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('upsert_mesh_harness_override', {
        meshId: 42,
        harnessId: 'claude',
        value: { model: 'opus-4', effort: 'medium' },
      });
    });

    // Reset to "Not set" — clearing the override value removes the
    // sparse map entry rather than writing a blank-value entry.
    vi.mocked(invoke).mockClear();
    await user.selectOptions(effort, '');
    await new Promise((r) => setTimeout(r, 20));
    const overrideWrites = vi.mocked(invoke).mock.calls.filter(
      ([cmd]) => cmd === 'upsert_mesh_harness_override',
    );
    expect(overrideWrites.length).toBe(1);
    const latestArgs = overrideWrites[0][1] as { value?: { model: string | null; effort: string | null } };
    expect(latestArgs.value?.effort).toBe(null);
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

  // ── Regression: Autopilot Policy moved to AutopilotProbeTab (#1013) ─────
  // The `update_mesh_autopilot` IPC and its four-policy-fields shape were
  // intentionally moved out of Mesh Properties (ticket #1013, follow-up to
  // #994). The Mesh Properties tab is no longer the configure surface for
  // Autopilot Policy — that role lives on `AutopilotProbeTab`. The pre-#1013
  // behavioural assertions for the old policy section moved with it (see
  // `autopilot-probe-tab.test.tsx`). The test below pins the regression so a
  // future change can't silently re-introduce the dual-edit surface.

  it('does NOT render Autopilot Policy fields (issue #1013)', async () => {
    await openPropertiesTab();

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

// Save-feedback (issue #729) — `MeshPropertiesTab` was the only probe tab
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
describe('MeshPropertiesTab — save feedback (issue #729)', () => {
  // The store has only one mesh configured by `beforeEach` in the outer
  // suite (MESH = id 42, name "demo"). For the mesh-switch test we
  // need a second mesh to switch INTO.
  const MESH_B: Mesh = {
    ...MESH,
    id: 99,
    name: 'other',
    path: '/repos/other',
  };

  function rejectNextOverrideWrite(message: string) {
    let armed = true;
    vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
      if (armed && cmd === 'upsert_mesh_harness_override') {
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
    await openPropertiesTab();

    const model = (await screen.findByTestId('mesh-override-model-input-claude')) as HTMLInputElement;
    await user.clear(model);
    await user.type(model, 'sonnet-4');
    fireEvent.blur(model);

    // Wait for the save's transition: Saving… → Saved. The save itself
    // is fire-and-await'd inside onBlur, so the indicator resolves once
    // the IPC's `.then` runs.
    expect(await screen.findByText('Saved')).toBeTruthy();
    // And the IPC fired with the right payload.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('upsert_mesh_harness_override', {
        meshId: 42, harnessId: 'claude', value: { model: 'sonnet-4', effort: 'high' },
      });
    });
  });

  it('shows "Save failed: <message>" and rolls the field back to the last confirmed value', async () => {
    const user = userEvent.setup();
    rejectNextOverrideWrite('boom-model-save');
    await openPropertiesTab();

    const model = (await screen.findByTestId('mesh-override-model-input-claude')) as HTMLInputElement;
    expect(model.value).toBe('opus-4');
    await user.clear(model);
    await user.type(model, 'sonnet-4');
    fireEvent.blur(model);

    // The error message surfaces in the global banner. The "Save failed:"
    // prefix is part of the indicator copy so the user distinguishes it
    // from any other surface; the rest is the rejection's `.message`.
    expect(await screen.findByText(/Save failed.*boom-model-save/)).toBeTruthy();

    // The field's text rolls back to the last confirmed value — the
    // user typed "sonnet-4" but the rejected save means we keep the
    // committed "opus-4" visible. Matches the issue #1148 acceptance
    // criterion "preserve the last confirmed override list" on a failed
    // save (the visible draft is rolled back so the user can't mistake
    // an unsaved edit for active configuration).
    await waitFor(() => {
      expect(model.value).toBe('opus-4');
    });
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

    rejectNextOverrideWrite('boom-leak-test');
    await openPropertiesTab();

    const model = (await screen.findByTestId('mesh-override-model-input-claude')) as HTMLInputElement;
    await user.clear(model);
    await user.type(model, 'opus-4-fail');
    fireEvent.blur(model);

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
    // Make the FIRST `upsert_mesh_harness_override` reject on the slow side
    // (returns after a small delay) so we have time to switch meshes
    // before the rejection lands.
    let armed = true;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (armed && cmd === 'upsert_mesh_harness_override') {
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

    await openPropertiesTab();
    const model = (await screen.findByTestId('mesh-override-model-input-claude')) as HTMLInputElement;
    await user.clear(model);
    await user.type(model, 'mesh-a-edit');
    fireEvent.blur(model);

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

  it('clears the previous "Save failed" indicator when a subsequent save succeeds', async () => {
    const user = userEvent.setup();
    rejectNextOverrideWrite('first-failure');
    await openPropertiesTab();

    const model = (await screen.findByTestId('mesh-override-model-input-claude')) as HTMLInputElement;
    await user.clear(model);
    await user.type(model, 'fail-then-succeed');
    fireEvent.blur(model);

    expect(await screen.findByText(/Save failed/)).toBeTruthy();

    // Re-arm — the next `upsert_mesh_harness_override` will succeed (the next
    // blur on the same field). Default mock resolver returns `{}`, which
    // the IPC treats as a clean resolution.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'upsert_mesh_harness_override') return Promise.resolve();
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

    await user.clear(model);
    await user.type(model, 'recovered');
    fireEvent.blur(model);

    await waitFor(() => {
      expect(screen.queryByText(/Save failed/)).toBeNull();
    });
  });
});
