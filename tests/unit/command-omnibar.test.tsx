/**
 * Tests for the <CommandOmnibar> palette (issue #1411).
 *
 * Coverage map to the ticket's acceptance criteria:
 *   - WAI-ARIA 1.2 combobox semantics (role=combobox on the input,
 *     aria-expanded / aria-controls / aria-activedescendant, role=listbox,
 *     role=option + aria-selected).
 *   - Keyboard interaction: ArrowUp/Down wrap-around with auto-scroll into
 *     view, Enter executes the active selection, Escape and backdrop click
 *     dismiss, Tab drills into the active result's domain prefix.
 *   - Focus restoration on close (captured on open, restored on unmount).
 *   - Terminal persistence proxy: opening/closing the palette never
 *     remounts sibling content (the terminal grid is a sibling overlay
 *     target — the same invariant TerminalManager relies on).
 */
import { describe, it, expect, vi, beforeEach, afterEach, beforeAll } from 'vitest';
import { render, cleanup, fireEvent, screen, act, waitFor } from '@testing-library/react';
import { CommandOmnibar } from '../../src/components/CommandOmnibar/CommandOmnibar';
import { executeOmnibarItem, runOmnibarCommand } from '../../src/components/CommandOmnibar/omnibarActions';
import { useUIStore, type OmnibarMode } from '../../src/stores/uiStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { Mesh } from '../../src/types/generated/Mesh';
import type { SpawnOption } from '../../src/lib/groups';
import { TOOL_DISCOVERY_GROUPS } from '../../src/components/CommandOmnibar/toolDiscovery';
import { PROBE_TAB_ORDER } from '../../src/lib/probeContext';
import { seedAgentNodes } from './helpers/seedAgentNodes';

const { loadSpawnOptionsMock } = vi.hoisted(() => ({
  loadSpawnOptionsMock: vi.fn(),
}));

vi.mock('../../src/components/CommandOmnibar/omnibarActions', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/components/CommandOmnibar/omnibarActions')>();
  return {
    ...actual,
    loadSpawnOptions: loadSpawnOptionsMock,
  };
});

// jsdom doesn't implement scrollIntoView; the palette calls it on every
// active-index move ("auto-scroll into view").
beforeAll(() => {
  Element.prototype.scrollIntoView = vi.fn();
});

const mesh: Mesh = {
  id: 1,
  name: 'buildmesh',
  path: 'F:/src/buildmesh',
  layout: 'grid',
  position: 0,
  created_at: '2026-01-01T00:00:00Z',
  build_command: null,
  run_command: null,
  model: null,
  effort: null,
  use_worktree: true,
  worktree_mode: null,
  default_provider: null,
  base_ref: 'main',
  scratchpad: '',
  sandbox: false,
  pre_spawn_pool_size: 1,
  color: null,
  autopilot_enabled: false,
  autopilot_trigger_label: null,
  autopilot_concurrency_limit: 2,
  autopilot_provider: null,
  autopilot_action_on_success: null,
  root_build_command: null,
  root_run_command: null,
  autopilot_mode: 'issue_driven',
  loop_initial_prompt: null,
  loop_suffix_prompt: null,
  loop_max_iterations: null,
  loop_interval_seconds: 0,
  loop_consecutive_failures: 0,
  harness_overrides: {},
};

const node: AgentNode = {
  id: 11,
  mesh_id: 1,
  name: 'alpha-node',
  path: 'F:/src/buildmesh/.worktrees/alpha',
  branch: 'feat/alpha',
  env: 'windows',
  provider: 'anthropic',
  status: 'running',
  cli_session_id: null,
  worktree_name: 'alpha',
  use_worktree: true,
  is_pinned: false,
  source_issue: null,
  source_pr: null,
  head_repo_owner: null,
  head_repo_clone_url: null,
  source_pr_pinned_sha: null,
  position: 0,
  created_at: '2026-01-01T00:00:00Z',
};

const spawnOption: SpawnOption = {
  id: 'claude',
  label: 'Claude Code',
  icon: '',
  harness_id: 'claude',
  provider_id: 'claude',
  is_proxied: false,
  group_key: 'claude',
  color: 'bg-blue-500',
};

function seedStores(): void {
  useUIStore.setState({
    omnibarOpen: false,
    omnibarMode: 'files',
    viewMode: 'all',
    cheatsheetOpen: false,
    appSettingsOpen: false,
    remoteAccessOpen: false,
    probeOpen: false,
    probeTab: 'files',
    activeDiffFile: null,
  });
  seedAgentNodes([node], null);
  useMeshStore.setState({ meshesById: new Map([[mesh.id, mesh]]), selectedMeshId: null });
}

function openOmnibar(mode: OmnibarMode = 'files'): void {
  act(() => {
    useUIStore.getState().openOmnibar(mode);
  });
}

function closeOmnibar(): void {
  act(() => {
    useUIStore.getState().closeOmnibar();
  });
}

function type(query: string): void {
  const input = screen.getByRole('combobox') as HTMLInputElement;
  fireEvent.change(input, { target: { value: query } });
}

function options(): HTMLElement[] {
  return screen.queryAllByTestId('command-omnibar-option');
}

/** Map a rendered view-command row's label to the ViewMode it switches to. */
function viewModeForRow(rowText: string): string {
  if (rowText.includes('Single')) return 'single';
  if (rowText.includes('Mesh Grid')) return 'mesh';
  if (rowText.includes('Pinned')) return 'pinned';
  if (rowText.includes('All Nodes')) return 'all';
  if (rowText.includes('Filtered')) return 'filtered';
  throw new Error(`Unexpected view row text: ${rowText}`);
}

beforeEach(() => {
  seedStores();
  loadSpawnOptionsMock.mockResolvedValue([]);
});

afterEach(cleanup);

describe('CommandOmnibar — mount/unmount discipline', () => {
  it('renders nothing while closed and the combobox while open', () => {
    const { container } = render(<CommandOmnibar />);
    expect(container.firstChild).toBeNull();
    openOmnibar();
    expect(screen.getByRole('combobox')).toBeTruthy();
    closeOmnibar();
    expect(container.firstChild).toBeNull();
  });

  it('seeds the query from omnibarMode: empty for files, ">" for commands', () => {
    render(<CommandOmnibar />);
    openOmnibar('commands');
    const input = screen.getByRole('combobox') as HTMLInputElement;
    expect(input.value).toBe('>');
    // Command mode shows the whole command domain with a bare prefix.
    expect(options().length).toBeGreaterThan(0);
  });

  it('never remounts sibling content across open/close (terminal persistence invariant)', () => {
    // The ref callback re-runs if the sibling ever remounts, so identity
    // surviving the open/close cycles is the assertion.
    const siblingRef = { current: null as HTMLDivElement | null };
    render(
      <>
        <div
          data-testid="omnibar-sibling"
          ref={(el) => {
            siblingRef.current = el;
          }}
        />
        <CommandOmnibar />
      </>,
    );
    const before = siblingRef.current;
    expect(before).not.toBeNull();
    openOmnibar();
    closeOmnibar();
    openOmnibar();
    closeOmnibar();
    // Same DOM node, still connected — nothing underneath the overlay was
    // torn down or remounted by the palette cycling.
    expect(siblingRef.current).toBe(before);
    expect(before!.isConnected).toBe(true);
  });
});

describe('CommandOmnibar — WAI-ARIA combobox semantics', () => {
  it('wires combobox â†’ listbox â†’ option per WAI-ARIA 1.2', () => {
    render(<CommandOmnibar />);
    openOmnibar('commands');
    const input = screen.getByRole('combobox');
    expect(input.getAttribute('aria-expanded')).toBe('true');
    const controls = input.getAttribute('aria-controls');
    expect(controls).toBeTruthy();
    const listbox = document.getElementById(controls!);
    expect(listbox?.getAttribute('role')).toBe('listbox');

    const activeDescendant = input.getAttribute('aria-activedescendant');
    expect(activeDescendant).toBeTruthy();
    const activeOption = document.getElementById(activeDescendant!);
    expect(activeOption?.getAttribute('role')).toBe('option');
    expect(activeOption?.getAttribute('aria-selected')).toBe('true');
    // Exactly one selected option.
    const selected = options().filter((o) => o.getAttribute('aria-selected') === 'true');
    expect(selected).toHaveLength(1);
  });

  it('reports the discovery grid as the combobox popup for an empty files query', () => {
    render(<CommandOmnibar />);
    openOmnibar('files');
    const input = screen.getByRole('combobox');
    expect((input as HTMLInputElement).value).toBe('');
    // The grid below the input is the popup: expanded + controls point at
    // it, and the first tile is the active descendant. Focus itself stays
    // in the input.
    expect(input.getAttribute('aria-expanded')).toBe('true');
    expect(input.getAttribute('aria-controls')).toBe('command-omnibar-tool-groups');
    expect(input.getAttribute('aria-activedescendant')).toBe('command-omnibar-tool-files');
    expect(document.activeElement).toBe(input);
    // First open is a discovery surface (Option A): the grouped tool
    // shortcuts render instead of the old "No matching results" state.
    expect(screen.getByTestId('command-omnibar-tool-groups')).toBeTruthy();
    expect(screen.queryByTestId('command-omnibar-empty')).toBeNull();
  });

  it('narrows results per keystroke and keeps the highlight highlightable', () => {
    render(<CommandOmnibar />);
    openOmnibar('commands');
    type('settings');
    const rows = options();
    expect(rows.length).toBeGreaterThan(0);
    expect(rows[0].textContent).toMatch(/open settings/i);
    type('zzzznope');
    expect(options()).toHaveLength(0);
    expect(screen.getByTestId('command-omnibar-empty')).toBeTruthy();
  });
});

describe('CommandOmnibar — keyboard interaction', () => {
  it('moves the active option with ArrowDown/ArrowUp and wraps around', () => {
    render(<CommandOmnibar />);
    openOmnibar('commands');
    type('>view');
    const rows = options();
    expect(rows.length).toBeGreaterThan(1);
    const count = rows.length;
    const input = screen.getByRole('combobox');

    const activeAfter = (key: string): number => {
      fireEvent.keyDown(input, { key });
      const id = input.getAttribute('aria-activedescendant');
      return Number(id!.slice('command-omnibar-option-'.length));
    };

    expect(activeAfter('ArrowDown')).toBe(1);
    expect(activeAfter('ArrowDown')).toBe(2);
    // Wrap past the end â†’ first option.
    for (let i = 2; i < count; i++) activeAfter('ArrowDown');
    expect(input.getAttribute('aria-activedescendant')).toBe('command-omnibar-option-0');
    // Wrap before the start â†’ last option.
    expect(activeAfter('ArrowUp')).toBe(count - 1);
  });

  it('auto-scrolls the active option into view', () => {
    render(<CommandOmnibar />);
    openOmnibar('commands');
    type('>view');
    const scrollSpy = Element.prototype.scrollIntoView as ReturnType<typeof vi.fn>;
    scrollSpy.mockClear();
    fireEvent.keyDown(screen.getByRole('combobox'), { key: 'ArrowDown' });
    expect(scrollSpy).toHaveBeenCalledWith({ block: 'nearest' });
  });

  it('Enter executes the active selection and closes the palette', () => {
    render(<CommandOmnibar />);
    openOmnibar('commands');
    type('pinned');
    fireEvent.keyDown(screen.getByRole('combobox'), { key: 'Enter' });
    expect(useUIStore.getState().viewMode).toBe('pinned');
    expect(useUIStore.getState().omnibarOpen).toBe(false);
  });

  it('Enter on a node result activates the node', () => {
    render(<CommandOmnibar />);
    openOmnibar('files');
    type('alpha');
    fireEvent.keyDown(screen.getByRole('combobox'), { key: 'Enter' });
    expect(useAgentNodeStore.getState().activeNodeId).toBe(node.id);
    expect(useUIStore.getState().omnibarOpen).toBe(false);
  });

  it('Escape dismisses and restores focus to the previously-focused element', () => {
    const trigger = document.createElement('button');
    document.body.appendChild(trigger);
    trigger.focus();
    render(<CommandOmnibar />);
    openOmnibar();
    expect(document.activeElement).toBe(screen.getByRole('combobox'));
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(useUIStore.getState().omnibarOpen).toBe(false);
    expect(document.activeElement).toBe(trigger);
    trigger.remove();
  });

  it('a backdrop click dismisses the palette', () => {
    render(<CommandOmnibar />);
    openOmnibar();
    fireEvent.click(screen.getByTestId('command-omnibar-backdrop'));
    expect(useUIStore.getState().omnibarOpen).toBe(false);
  });

  it('a click on an option row executes THE CLICKED row, not the keyboard-active one', () => {
    // Regression pin (issue #1411 review): the executor must receive the
    // clicked item. Move the keyboard highlight to row 2 first — if the
    // click executed the active item instead, viewMode would be row 2's.
    render(<CommandOmnibar />);
    openOmnibar('commands');
    type('>view');
    const rows = options().filter((r) => (r.textContent ?? '').includes('Switch view:'));
    expect(rows.length).toBeGreaterThan(2);
    const input = screen.getByRole('combobox');
    fireEvent.keyDown(input, { key: 'ArrowDown' });
    fireEvent.keyDown(input, { key: 'ArrowDown' });
    const target = rows[1];
    fireEvent.click(target);
    expect(useUIStore.getState().viewMode).toBe(viewModeForRow(target.textContent ?? ''));
  });

  it('clicking the LAST row executes that row (off-by-active regression pin)', () => {
    render(<CommandOmnibar />);
    openOmnibar('commands');
    type('>view');
    const rows = options().filter((r) => (r.textContent ?? '').includes('Switch view:'));
    const target = rows[rows.length - 1];
    fireEvent.click(target);
    expect(useUIStore.getState().viewMode).toBe(viewModeForRow(target.textContent ?? ''));
  });

  it('re-seeds the query when the mode changes while the palette is open', () => {
    // The openOmnibar contract: pressing the other chord on an open palette
    // re-seeds to that mode rather than being ignored (uiStore doc).
    render(<CommandOmnibar />);
    openOmnibar('files');
    type('alpha');
    act(() => {
      useUIStore.getState().openOmnibar('commands');
    });
    const input = screen.getByRole('combobox') as HTMLInputElement;
    expect(useUIStore.getState().omnibarMode).toBe('commands');
    expect(input.value).toBe('>');
    expect(options().length).toBeGreaterThan(0);
  });

  it('Shift+Tab falls through untouched (no completion, no focus trap)', () => {
    render(<CommandOmnibar />);
    openOmnibar('commands');
    type('>sync');
    const input = screen.getByRole('combobox') as HTMLInputElement;
    fireEvent.keyDown(input, { key: 'Tab', shiftKey: true });
    expect(input.value).toBe('>sync');
  });

  it('Tab on an empty result set does not preventDefault (no silent focus trap)', () => {
    render(<CommandOmnibar />);
    openOmnibar('files');
    type('zzzznope');
    const input = screen.getByRole('combobox') as HTMLInputElement;
    const event = fireEvent.keyDown(input, { key: 'Tab' });
    // fireEvent returns false when preventDefault was called.
    expect(event).toBe(true);
  });

  it('wraps the overlay in dialog semantics (role=dialog, aria-modal, labelled)', () => {
    render(<CommandOmnibar />);
    openOmnibar('commands');
    const dialog = screen.getByRole('dialog');
    expect(dialog.getAttribute('aria-modal')).toBe('true');
    expect(dialog.getAttribute('aria-label')).toMatch(/omnibar/i);
  });

  it('renders one badge per domain in the footer (no duplicate spawn badges)', () => {
    // `/` and `+` both scope spawning — they share ONE badge, so the
    // description text appears exactly once.
    const { container } = render(<CommandOmnibar />);
    openOmnibar('commands');
    const occurrences = container.textContent!.split('Spawning actions').length - 1;
    expect(occurrences).toBe(1);
  });

  it('Tab drills into the active result’s domain by applying its prefix filter', () => {
    render(<CommandOmnibar />);
    openOmnibar('files');
    type('settings');
    fireEvent.keyDown(screen.getByRole('combobox'), { key: 'Tab' });
    const input = screen.getByRole('combobox') as HTMLInputElement;
    // The first hit for "settings" is the "Open Settings" command — Tab
    // scopes the (unprefixed) query to the commands domain.
    expect(input.value.startsWith('>')).toBe(true);
    expect(input.value).toContain('settings');
    // The list is now scoped to commands only.
    const categories = options().map((o) => o.lastElementChild?.textContent);
    expect(categories.every((c) => c === 'command')).toBe(true);
  });

  it('Tab completes the query to the active result once the query is already scoped', () => {
    render(<CommandOmnibar />);
    openOmnibar('files');
    type('>sync');
    fireEvent.keyDown(screen.getByRole('combobox'), { key: 'Tab' });
    const input = screen.getByRole('combobox') as HTMLInputElement;
    expect(input.value).toBe('>Git sync');
  });
});

describe('CommandOmnibar — command execution routing', () => {
  it('activates a node, selects its mesh, and enters Mesh Grid', () => {
    executeOmnibarItem(`node:${node.id}`, {
      meshes: [mesh],
      spawnOptions: [],
      setViewMode: useUIStore.getState().setViewMode,
      openProbeTab: vi.fn(),
    });
    expect(useAgentNodeStore.getState().activeNodeId).toBe(node.id);
    expect(useMeshStore.getState().selectedMeshId).toBe(mesh.id);
    expect(useUIStore.getState().viewMode).toBe('mesh');
  });

  it('retargets an existing Single view without dropping back to Mesh Grid', () => {
    useUIStore.setState({ viewMode: 'single' });
    executeOmnibarItem(`node:${node.id}`, {
      meshes: [mesh],
      spawnOptions: [],
      setViewMode: useUIStore.getState().setViewMode,
      openProbeTab: vi.fn(),
    });
    expect(useMeshStore.getState().selectedMeshId).toBe(mesh.id);
    expect(useUIStore.getState().viewMode).toBe('single');
  });

  it('routes the view-filtered command to the Filtered view (#1609)', () => {
    executeOmnibarItem('command:view-filtered', {
      meshes: [mesh],
      spawnOptions: [],
      setViewMode: useUIStore.getState().setViewMode,
      openProbeTab: vi.fn(),
    });
    expect(useUIStore.getState().viewMode).toBe('filtered');
    expect(useUIStore.getState().lastNonSingleMode).toBe('filtered');
  });

  it('selects a mesh and aligns the canvas with Mesh Grid', () => {
    useUIStore.setState({ viewMode: 'pinned' });
    executeOmnibarItem(`mesh:${mesh.id}`, {
      meshes: [mesh],
      spawnOptions: [],
      setViewMode: useUIStore.getState().setViewMode,
      openProbeTab: vi.fn(),
    });
    expect(useMeshStore.getState().selectedMeshId).toBe(mesh.id);
    expect(useUIStore.getState().viewMode).toBe('mesh');
  });

  it('routes the show-cheatsheet command through uiStore (no window-event side channel)', () => {
    render(<CommandOmnibar />);
    openOmnibar('commands');
    type('cheatsheet');
    fireEvent.keyDown(screen.getByRole('combobox'), { key: 'Enter' });
    expect(useUIStore.getState().cheatsheetOpen).toBe(true);
    expect(useUIStore.getState().omnibarOpen).toBe(false);
  });

  it('routes the open-settings command through uiStore', () => {
    render(<CommandOmnibar />);
    openOmnibar('commands');
    type('settings');
    fireEvent.keyDown(screen.getByRole('combobox'), { key: 'Enter' });
    expect(useUIStore.getState().appSettingsOpen).toBe(true);
  });

  it('does not restore the pre-palette focus target after executing an action', () => {
    const trigger = document.createElement('button');
    document.body.appendChild(trigger);
    trigger.focus();
    render(<CommandOmnibar />);
    openOmnibar('commands');
    type('settings');
    fireEvent.keyDown(screen.getByRole('combobox'), { key: 'Enter' });
    expect(document.activeElement).not.toBe(trigger);
    trigger.remove();
  });

  it('runOmnibarCommand returns false for an unknown command id (catalog drift pin)', () => {
    const setViewMode = vi.fn();
    const openProbeTab = vi.fn();
    expect(
      runOmnibarCommand('no-such-command', {
        meshes: [],
        spawnOptions: [],
        setViewMode,
        openProbeTab,
      }),
    ).toBe(false);
    expect(setViewMode).not.toHaveBeenCalled();
    expect(openProbeTab).not.toHaveBeenCalled();
  });

  it('routes a spawn item through selectProviderForMesh with the right mesh and option', () => {
    const spy = vi
      .spyOn(useAgentNodeStore.getState(), 'selectProviderForMesh')
      .mockResolvedValue(node);
    executeOmnibarItem(`spawn:${spawnOption.id}:${mesh.id}`, {
      meshes: [mesh],
      spawnOptions: [spawnOption],
      setViewMode: vi.fn(),
      openProbeTab: vi.fn(),
    });
    expect(spy).toHaveBeenCalledWith(
      mesh.id,
      mesh.name,
      mesh.path,
      spawnOption.id,
      undefined,
      undefined,
    );
    spy.mockRestore();
  });

  it('forwards an initial prompt turn to selectProviderForMesh (issue #1413)', () => {
    const spy = vi
      .spyOn(useAgentNodeStore.getState(), 'selectProviderForMesh')
      .mockResolvedValue(node);
    executeOmnibarItem(`spawn:${spawnOption.id}:${mesh.id}`, {
      meshes: [mesh],
      spawnOptions: [spawnOption],
      setViewMode: vi.fn(),
      openProbeTab: vi.fn(),
      initialPrompt: 'fix the flaky test',
    });
    expect(spy).toHaveBeenCalledWith(
      mesh.id,
      mesh.name,
      mesh.path,
      spawnOption.id,
      undefined,
      'fix the flaky test',
    );
    spy.mockRestore();
  });

  it('routes an issue item to ITS mesh before opening the Issues tab', () => {
    // The palette can list issues from any mesh; the Probe's GitHub tabs
    // read the selected mesh, so the router must select the item's mesh
    // (2, not the currently selected 1) before opening the tab.
    useMeshStore.setState({ selectedMeshId: 1 });
    const openProbeTab = vi.fn();
    executeOmnibarItem('issue:2:7', {
      meshes: [{ ...mesh, id: 2, name: 'other' }],
      spawnOptions: [],
      setViewMode: vi.fn(),
      openProbeTab,
    });
    expect(useMeshStore.getState().selectedMeshId).toBe(2);
    expect(openProbeTab).toHaveBeenCalledWith('issues');
  });

  it('routes a pull item to its mesh and the Pull Requests tab', () => {
    useMeshStore.setState({ selectedMeshId: 1 });
    const openProbeTab = vi.fn();
    executeOmnibarItem('pull:3:42', {
      meshes: [{ ...mesh, id: 3, name: 'other-pr-mesh' }],
      spawnOptions: [],
      setViewMode: vi.fn(),
      openProbeTab,
    });
    expect(useMeshStore.getState().selectedMeshId).toBe(3);
    expect(openProbeTab).toHaveBeenCalledWith('pulls');
    expect(useUIStore.getState().activeDiffFile).toEqual({
      filePath: '',
      rootPath: mesh.path,
      nodeId: null,
      meshId: 3,
      source: 'pr',
      prNumber: 42,
    });
  });

  it('routes a mesh result to mesh selection', () => {
    render(<CommandOmnibar />);
    openOmnibar('files');
    type('buildmesh');
    fireEvent.keyDown(screen.getByRole('combobox'), { key: 'Enter' });
    expect(useMeshStore.getState().selectedMeshId).toBe(mesh.id);
  });
});

describe('CommandOmnibar — quick spawn prompt mode (issue #1413)', () => {
  beforeEach(() => {
    loadSpawnOptionsMock.mockResolvedValue([spawnOption]);
  });

  async function openSpawnResults(): Promise<HTMLInputElement> {
    render(<CommandOmnibar />);
    openOmnibar('files');
    await waitFor(() => expect(loadSpawnOptionsMock).toHaveBeenCalled());
    type('/claude');
    await waitFor(() => {
      expect(options().some((o) => (o.textContent ?? '').includes('Spawn Claude Code on buildmesh'))).toBe(true);
    });
    return screen.getByRole('combobox') as HTMLInputElement;
  }

  it('surfaces Spawn [Harness] on [Mesh] for `/` and `spawn` queries', async () => {
    const input = await openSpawnResults();
    expect(options()[0].textContent).toMatch(/Spawn Claude Code on buildmesh/);

    fireEvent.change(input, { target: { value: 'spawn' } });
    await waitFor(() => {
      expect(options().some((o) => (o.textContent ?? '').includes('Spawn Claude Code on buildmesh'))).toBe(true);
    });
  });

  it('Tab on a spawn result enters prompt mode with the recipe as context', async () => {
    const input = await openSpawnResults();
    fireEvent.keyDown(input, { key: 'Tab' });
    expect(input.getAttribute('aria-label')).toMatch(/initial prompt/i);
    expect(input.value).toBe('');
    expect(screen.getByTestId('command-omnibar-prompt-context').textContent).toBe(
      'Spawn Claude Code on buildmesh',
    );
    expect(screen.getByTestId('command-omnibar-prompt-hint')).toBeTruthy();
    expect(options()).toHaveLength(0);
  });

  it('does not fuzzy-search the draft prompt while prompt mode is active', async () => {
    const input = await openSpawnResults();
    fireEvent.keyDown(input, { key: 'Tab' });
    fireEvent.change(input, {
      target: { value: 'Refactor the auth middleware to support bearer tokens' },
    });
    expect(options()).toHaveLength(0);
    expect(screen.queryByRole('listbox')).toBeNull();
  });

  it('Enter in prompt mode dispatches selectProviderForMesh with the typed prompt and closes', async () => {
    const spy = vi
      .spyOn(useAgentNodeStore.getState(), 'selectProviderForMesh')
      .mockResolvedValue(node);
    const input = await openSpawnResults();
    fireEvent.keyDown(input, { key: 'Tab' });
    fireEvent.change(input, { target: { value: 'fix the flaky test' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(spy).toHaveBeenCalledWith(
      mesh.id,
      mesh.name,
      mesh.path,
      spawnOption.id,
      undefined,
      'fix the flaky test',
    );
    expect(useUIStore.getState().omnibarOpen).toBe(false);
    spy.mockRestore();
  });

  it('Enter on a spawn result without Tab spawns immediately with no prompt', async () => {
    const spy = vi
      .spyOn(useAgentNodeStore.getState(), 'selectProviderForMesh')
      .mockResolvedValue(node);
    const input = await openSpawnResults();
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(spy).toHaveBeenCalledWith(
      mesh.id,
      mesh.name,
      mesh.path,
      spawnOption.id,
      undefined,
      undefined,
    );
    expect(useUIStore.getState().omnibarOpen).toBe(false);
    spy.mockRestore();
  });

  it('Escape from prompt mode returns to the spawn results instead of closing', async () => {
    const input = await openSpawnResults();
    fireEvent.keyDown(input, { key: 'Tab' });
    expect(screen.getByTestId('command-omnibar-prompt-context')).toBeTruthy();
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(useUIStore.getState().omnibarOpen).toBe(true);
    expect(screen.queryByTestId('command-omnibar-prompt-context')).toBeNull();
    expect((screen.getByRole('combobox') as HTMLInputElement).value).toBe('/claude');
    await waitFor(() => {
      expect(options().some((o) => (o.textContent ?? '').includes('Spawn Claude Code on buildmesh'))).toBe(true);
    });
  });
});

describe('CommandOmnibar — tool discovery start screen (Option A)', () => {
  const TOOL_TABS = [
    'files',
    'review',
    'usage',
    'worktrees',
    'properties',
    'autopilot',
    'circuits',
    'issues',
    'pulls',
    'sessions',
    'scratchpad',
  ] as const;

  it('shows every probe destination as a grouped shortcut on first open', () => {
    render(<CommandOmnibar />);
    openOmnibar('files');
    expect(screen.getByTestId('command-omnibar-tool-groups')).toBeTruthy();
    for (const tab of TOOL_TABS) {
      expect(screen.getByTestId(`command-omnibar-tool-${tab}`)).toBeTruthy();
    }
  });

  it('renders the discovery grid below the search input, which stays anchored on top', () => {
    render(<CommandOmnibar />);
    openOmnibar('files');
    const input = screen.getByRole('combobox');
    const groups = screen.getByTestId('command-omnibar-tool-groups');
    // Input precedes the grid in document order — typing swaps the grid
    // for results in place instead of teleporting the input.
    expect(input.compareDocumentPosition(groups) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it('pairs GitHub Issues and Pull Requests in one group', () => {
    render(<CommandOmnibar />);
    openOmnibar('files');
    const github = screen.getByTestId('command-omnibar-tool-group-github');
    expect(github.textContent).toMatch(/GitHub Issues/);
    expect(github.textContent).toMatch(/Pull Requests/);
  });

  it('typing hides the groups and searches instead', () => {
    render(<CommandOmnibar />);
    openOmnibar('files');
    expect(screen.getByTestId('command-omnibar-tool-groups')).toBeTruthy();
    type('alpha');
    expect(screen.queryByTestId('command-omnibar-tool-groups')).toBeNull();
    expect(options().some((o) => (o.textContent ?? '').includes('alpha-node'))).toBe(true);
  });

  it('clicking a tool tile opens that probe destination and closes the palette', () => {
    render(<CommandOmnibar />);
    openOmnibar('files');
    fireEvent.click(screen.getByTestId('command-omnibar-tool-issues'));
    expect(useUIStore.getState().probeOpen).toBe(true);
    expect(useUIStore.getState().probeTab).toBe('issues');
    expect(useUIStore.getState().omnibarOpen).toBe(false);
  });

  it('covers every probe destination in exactly one group (exhaustiveness pin)', () => {
    const grouped = TOOL_DISCOVERY_GROUPS.flatMap((g) => g.tiles.map((t) => t.tab));
    expect([...grouped].sort()).toEqual([...PROBE_TAB_ORDER].sort());
  });

  it('ArrowDown highlights tiles without moving focus, and wraps around', () => {
    render(<CommandOmnibar />);
    openOmnibar('files');
    const input = screen.getByRole('combobox');
    fireEvent.keyDown(input, { key: 'ArrowDown' });
    // Focus never leaves the input — the highlight is virtual.
    expect(document.activeElement).toBe(input);
    expect(input.getAttribute('aria-activedescendant')).toBe('command-omnibar-tool-review');
    expect(screen.getByTestId('command-omnibar-tool-review').getAttribute('data-active')).toBe('true');
    // Wrap past the last tile (11 tiles) back to the first.
    for (let i = 0; i < 10; i++) fireEvent.keyDown(input, { key: 'ArrowDown' });
    expect(input.getAttribute('aria-activedescendant')).toBe('command-omnibar-tool-files');
    // ArrowUp from the first tile wraps to the last.
    fireEvent.keyDown(input, { key: 'ArrowUp' });
    expect(input.getAttribute('aria-activedescendant')).toBe('command-omnibar-tool-usage');
  });

  it('Enter opens the highlighted tile and typing afterwards still works', () => {
    render(<CommandOmnibar />);
    openOmnibar('files');
    const input = screen.getByRole('combobox') as HTMLInputElement;
    fireEvent.keyDown(input, { key: 'ArrowDown' });
    // No keystrokes were lost to a focus move: typing searches as usual.
    fireEvent.change(input, { target: { value: 'alpha' } });
    expect(screen.queryByTestId('command-omnibar-tool-groups')).toBeNull();
    expect(options().some((o) => (o.textContent ?? '').includes('alpha-node'))).toBe(true);
  });

  it('Enter on a highlighted tile opens that destination and closes the palette', () => {
    render(<CommandOmnibar />);
    openOmnibar('files');
    const input = screen.getByRole('combobox');
    fireEvent.keyDown(input, { key: 'ArrowDown' });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(useUIStore.getState().probeOpen).toBe(true);
    expect(useUIStore.getState().probeTab).toBe('review');
    expect(useUIStore.getState().omnibarOpen).toBe(false);
  });
});
