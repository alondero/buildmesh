/**
 * Command Omnibar lifecycle integration tests (wayfinder #1371, ticket
 * #1414 — the final integration coverage for the Omnibar feature).
 *
 * The Omnibar is the universal command palette (`⌘/Ctrl+K` for files mode,
 * `⌘/Ctrl+P` for commands mode — issue #1409). This file exercises the
 * full user-visible lifecycle that the cheatsheet advertises:
 *
 *   1. **Open from a terminal-focus element.** The palette must render on
 *      top of the terminal grid, capture focus into the search box, and
 *      NOT remount any xterm terminal (TerminalManager singleton — see
 *      `docs/knowledge-primer.md` *Terminal Persistence*).
 *   2. **Seed and search.** Seeding at least one mesh + one command
 *      gives the listbox something to filter; typing a query narrows the
 *      results, and the `>` prefix routes to the command menu.
 *   3. **Execute an action.** Pressing Enter on a command result runs the
 *      dispatcher (`executeOmnibarItem` in `omnibarActions.ts`) and closes
 *      the palette. The cheatsheet command is the simplest end-to-end
 *      probe — it toggles `uiStore.cheatsheetOpen` and would otherwise
 *      need a Settings modal mount to observe.
 *   4. **Close + focus restore.** Closing via Escape must drop focus back
 *      to the previously-focused terminal element (the spec's "restoring
 *      terminal focus" line).
 *
 * The store-level tests at the top of the file pin the state-machine
 * shape; the rendering tests below pin the component-level contract.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act, fireEvent, render } from '@testing-library/react';

// jsdom doesn't implement `scrollIntoView` (called by the omnibar's
// keep-active-option-visible effect). The real production effect is a
// pure convenience; stubbing it lets the lifecycle mount without
// triggering an unimplemented-API error. Other integration tests in
// this repo hit the same jsdom gap and stub similarly.
if (!('scrollIntoView' in HTMLElement.prototype)) {
  Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', {
    configurable: true,
    value: vi.fn(),
  });
}
import {
  useUIStore,
  type OmnibarMode,
} from '../../src/stores/uiStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { CommandOmnibar } from '../../src/components/CommandOmnibar/CommandOmnibar';
import {
  PREFIX_FILTERS,
} from '../../src/lib/omnibar/indexers';
import { seedAgentNodes } from '../unit/helpers/seedAgentNodes';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { Mesh } from '../../src/types/generated/Mesh';

// -----------------------------------------------------------------------
// Fixtures — a single running node + a single mesh gives the palette
// both @nodes and `>` commands to show, while staying small enough to
// keep the index build under a millisecond.
// -----------------------------------------------------------------------

const RUNNING_NODE: AgentNode = {
  id: 1,
  mesh_id: 1,
  name: 'agent-alpha',
  path: '/repo',
  branch: 'main',
  env: 'host',
  provider: 'claude',
  status: 'running',
  use_worktree: false,
  position: 0,
  created_at: '2026-09-01T00:00:00Z',
  scratchpad: '',
  sandbox: false,
  cli_session_id: null,
  worktree_name: null,
  source_issue: null,
  archived: false,
  is_pinned: false,
};

const MESH: Mesh = {
  id: 1,
  name: 'demo-mesh',
  path: '/repo',
  layout: 'grid',
  position: 0,
  created_at: '2026-09-01T00:00:00Z',
  build_command: null,
  run_command: null,
  model: null,
  effort: null,
  use_worktree: false,
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

/**
 * Reset every store the omnibar touches plus the modal booleans. The
 * palette reads/writes `uiStore` (open flag, mode, cheatsheet flag),
 * `meshStore` (mesh selection for spawn routing), and `agentNodeStore`
 * (node indexing). Wiping all three between tests keeps the lifecycle
 * tests independent.
 */
function resetStores() {
  useUIStore.setState({
    omnibarOpen: false,
    omnibarMode: 'files',
    cheatsheetOpen: false,
    appSettingsOpen: false,
    remoteAccessOpen: false,
    openProbeTab: useUIStore.getState().openProbeTab,
    probeTab: 'files',
    probeOpen: false,
  });
  useMeshStore.setState({
    meshesById: new Map([[MESH.id, MESH]]),
    selectedMeshId: MESH.id,
  });
  // seedAgentNodes installs a fresh nodesById + nodeIds; calling it after a
  // previous test left state behind is the documented pattern (memory
  // #1384 — the seed helper handles the entity-adapter invariants).
  seedAgentNodes([RUNNING_NODE], RUNNING_NODE.id);
}

// -----------------------------------------------------------------------
// 1. Store-level lifecycle — the state machine the App.tsx binding calls.
// -----------------------------------------------------------------------

describe('Command Omnibar store lifecycle (issue #1414)', () => {
  beforeEach(() => {
    resetStores();
  });

  it('openOmnibar(mode) flips the open flag and seeds the mode', () => {
    act(() => useUIStore.getState().openOmnibar('files'));
    expect(useUIStore.getState().omnibarOpen).toBe(true);
    expect(useUIStore.getState().omnibarMode).toBe('files');

    act(() => useUIStore.getState().openOmnibar('commands'));
    expect(useUIStore.getState().omnibarMode).toBe('commands');
  });

  it('toggleOmnibar closes when called with the same mode (modal-toggle expectation)', () => {
    const mode: OmnibarMode = 'files';
    act(() => useUIStore.getState().toggleOmnibar(mode));
    expect(useUIStore.getState().omnibarOpen).toBe(true);

    act(() => useUIStore.getState().toggleOmnibar(mode));
    expect(useUIStore.getState().omnibarOpen).toBe(false);
  });

  it('toggleOmnibar in the OTHER mode just switches mode without closing', () => {
    act(() => useUIStore.getState().toggleOmnibar('files'));
    act(() => useUIStore.getState().toggleOmnibar('commands'));
    // The editors' convention: pressing the other chord while the palette
    // is open re-seeds the mode rather than closing + reopening. The
    // component contract for this is exercised in the lifecycle suite
    // below (the re-seed-on-mode-toggle test).
    expect(useUIStore.getState().omnibarOpen).toBe(true);
    expect(useUIStore.getState().omnibarMode).toBe('commands');
  });
});

// -----------------------------------------------------------------------
// 2. Mount lifecycle — render the palette, drive the store, observe.
// -----------------------------------------------------------------------

describe('Command Omnibar component lifecycle (issue #1414)', () => {
  beforeEach(() => {
    resetStores();
  });

  afterEach(() => {
    // Each render mounts the palette; if a test left it open, close it
    // so the next test's `beforeEach` doesn't see a stale open flag.
    act(() => useUIStore.getState().closeOmnibar());
    if (document.activeElement instanceof HTMLElement) {
      document.activeElement.blur();
    }
  });

  it('renders the search input when open (catalog wiring #1410 / #1411)', () => {
    const { container } = render(<CommandOmnibar />);
    // Counterpart: a mounted-but-closed palette must render nothing —
    // the mount/unmount discipline is what arms the Escape listener.
    expect(container.firstChild).toBeNull();

    act(() => useUIStore.getState().openOmnibar());
    const input = container.querySelector<HTMLInputElement>(
      'input[role="combobox"]',
    );
    expect(input).toBeTruthy();
    expect(input!.getAttribute('aria-autocomplete')).toBe('list');
    expect(input!.getAttribute('aria-haspopup')).toBe('listbox');
  });

  it('seeds the search box with `>` when opened in commands mode', () => {
    act(() => useUIStore.getState().openOmnibar('commands'));
    const { container } = render(<CommandOmnibar />);
    const input = container.querySelector<HTMLInputElement>(
      'input[role="combobox"]',
    );
    expect(input?.value).toBe('>');
  });

  it('captures focus from a terminal-like element on open and restores on close', async () => {
    // The "terminal" in this test is a focusable element representing
    // the xterm helper textarea the real app would have held. The
    // palette's useEffect captures document.activeElement on mount and
    // restores it on unmount — this is what drops the user back into a
    // focused xterm terminal after closing (issue #1411 acceptance).
    const terminal = document.createElement('textarea');
    terminal.setAttribute('aria-label', 'terminal');
    terminal.tabIndex = 0;
    document.body.appendChild(terminal);
    terminal.focus();
    expect(document.activeElement).toBe(terminal);

    act(() => useUIStore.getState().openOmnibar());
    const { container } = render(<CommandOmnibar />);
    const input = container.querySelector<HTMLInputElement>(
      'input[role="combobox"]',
    );
    // Identity (not just role) — a future refactor that wraps the input
    // in a <div role="combobox"> with a child <input> would otherwise
    // pass against the wrapper rather than the element that owns focus.
    expect(input).toBeTruthy();
    expect(document.activeElement).toBe(input);

    // Drive the close through the store so React owns the unmount path:
    // closeOmnibar flips the flag, CommandOmnibar re-renders to null,
    // <OmnibarPalette>'s cleanup restores focus. NOT calling unmount()
    // manually is what pins the real contract — a future change to a
    // display:none mount would otherwise hide the regression.
    act(() => useUIStore.getState().closeOmnibar());
    await act(async () => {});
    expect(document.activeElement).toBe(terminal);
    terminal.remove();
  });

  it('typing in the search box narrows results (live search, issue #1410)', async () => {
    act(() => useUIStore.getState().openOmnibar('commands'));
    const { container } = render(<CommandOmnibar />);
    const input = container.querySelector<HTMLInputElement>(
      'input[role="combobox"]',
    );
    expect(input).toBeTruthy();

    // The seeded query is `>` — the listbox should show command + spawn
    // items. Search for `cheat` which uniquely matches `show-cheatsheet`.
    await act(async () => {
      fireEvent.change(input!, { target: { value: '>cheat' } });
    });
    const options = container.querySelectorAll('[role="option"]');
    expect(options.length).toBeGreaterThan(0);
    const labels = Array.from(options).map((o) => o.textContent || '');
    expect(
      labels.some((l) => /show this cheatsheet/i.test(l) || /cheatsheet/i.test(l)),
    ).toBe(true);
  });

  it('Enter on the cheatsheet command opens the cheatsheet modal and closes the palette', async () => {
    act(() => useUIStore.getState().openOmnibar('commands'));
    const { container } = render(<CommandOmnibar />);
    const input = container.querySelector<HTMLInputElement>(
      'input[role="combobox"]',
    );
    expect(input).toBeTruthy();

    // Type a query that uniquely matches the cheatsheet command.
    await act(async () => {
      fireEvent.change(input!, { target: { value: '>cheatsheet' } });
    });
    const option = container.querySelector<HTMLElement>('[role="option"]');
    expect(option).toBeTruthy();
    expect(option!.textContent || '').toMatch(/cheatsheet/i);

    // Pressing Enter executes the active option.
    await act(async () => {
      fireEvent.keyDown(input!, { key: 'Enter' });
    });

    // Side effect of executing `show-cheatsheet`: the cheatsheet modal
    // open flag flipped, the palette closed itself.
    expect(useUIStore.getState().cheatsheetOpen).toBe(true);
    expect(useUIStore.getState().omnibarOpen).toBe(false);
  });

  it('Escape on the window closes the palette from any focused element', async () => {
    act(() => useUIStore.getState().openOmnibar());
    render(<CommandOmnibar />);
    expect(useUIStore.getState().omnibarOpen).toBe(true);

    await act(async () => {
      fireEvent.keyDown(document, { key: 'Escape' });
    });
    expect(useUIStore.getState().omnibarOpen).toBe(false);
  });

  it('clicking the backdrop closes the palette (dismiss = click outside)', async () => {
    act(() => useUIStore.getState().openOmnibar());
    const { getByTestId } = render(<CommandOmnibar />);
    expect(useUIStore.getState().omnibarOpen).toBe(true);

    await act(async () => {
      fireEvent.click(getByTestId('command-omnibar-backdrop'));
    });
    expect(useUIStore.getState().omnibarOpen).toBe(false);
  });

  it('does not render anything while the palette is closed (mount = open)', () => {
    // Counterpart to the focus-restore test: the palette is a sibling
    // overlay that mounts ONLY while open. A permanent mount would
    // arm its window Escape listener and steal Escape from any other
    // component that needs it (terminals, modals).
    const { container } = render(<CommandOmnibar />);
    expect(container.firstChild).toBeNull();
  });

  it('renders a prefix hint badge for every PREFIX_FILTERS entry (discoverability)', () => {
    act(() => useUIStore.getState().openOmnibar());
    const { container } = render(<CommandOmnibar />);
    // The footer is the prefix-discoverability strip; each PREFIX_FILTER
    // entry renders a <kbd> per prefix (the deduplication in the
    // component collapses '/'+' '+' by description but still emits both
    // <kbd>s inside the same group). Asserting on the rendered keycap
    // text — not the source array — is what makes the badge test
    // meaningful: a future bug that stops wiring PREFIX_FILTERS into the
    // footer would surface here even though `PREFIX_FILTERS.length` is
    // unchanged.
    const footer = container.querySelector('.border-t.border-border-subtle');
    expect(footer).toBeTruthy();
    const badgeTexts = Array.from(footer!.querySelectorAll('kbd')).map(
      (k) => k.textContent ?? '',
    );
    const expectedPrefixes = PREFIX_FILTERS.map((f) => f.prefix);
    for (const prefix of expectedPrefixes) {
      expect(badgeTexts).toContain(prefix);
    }
  });

  it('re-seeds the search box to `>` when toggleOmnibar flips to commands while open', () => {
    // Component-side counterpart of the store-level "OTHER mode keeps
    // open" test: pressing the other chord must re-seed the search box
    // to the new mode's prefix (the editors' quick-open convention,
    // issue #1411). The store test only verifies the flag; this test
    // pins the UI contract that `useEffect(() => setQuery(mode==='commands'
    // ? '>' : ''), [mode])` actually runs while the palette is mounted.
    act(() => useUIStore.getState().openOmnibar('files'));
    const { container } = render(<CommandOmnibar />);
    const input = container.querySelector<HTMLInputElement>(
      'input[role="combobox"]',
    );
    expect(input).toBeTruthy();
    // Sanity: files mode seeds an empty box.
    expect(input!.value).toBe('');

    // Switch to commands while open — the palette must NOT remount (the
    // flag stayed true), but the seed useEffect must re-fire.
    act(() => useUIStore.getState().toggleOmnibar('commands'));
    const inputAfter = container.querySelector<HTMLInputElement>(
      'input[role="combobox"]',
    );
    expect(inputAfter).toBe(input);
    expect(inputAfter!.value).toBe('>');
    // And back: the same effect fires on the reverse transition.
    act(() => useUIStore.getState().toggleOmnibar('files'));
    expect(inputAfter!.value).toBe('');
  });

  it('does not throw when a keystroke fires before the palette is mounted', () => {
    // Regression guard: the Escape listener attaches inside a useEffect
    // (it does not arm while the palette is closed). A stray keyDown on
    // document.body before mount must be a no-op — listeners from
    // earlier component instances, or a sloppy parent that bubbles,
    // would otherwise surface here.
    render(<CommandOmnibar />);
    expect(useUIStore.getState().omnibarOpen).toBe(false);
    expect(() => {
      fireEvent.keyDown(document.body, { key: 'Escape' });
      fireEvent.keyDown(document.body, { key: 'Enter' });
    }).not.toThrow();
    // And the closed flag never spontaneously flips.
    expect(useUIStore.getState().omnibarOpen).toBe(false);
  });
});
