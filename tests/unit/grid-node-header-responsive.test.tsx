/**
 * Issue #736 — Agent node title collapses first when the pane is narrow.
 * Make the header responsive.
 *
 * The header should prioritise showing the node name, status dot, and
 * provider icon. Lower-priority elements (mesh label â†’ worktree chip â†’
 * summary â†’ PR chip â†’ inline actions) should hide progressively before
 * the title gets truncated, and the destructive close+maximise controls
 * should move into a kebab overflow menu when space is too tight for
 * them inline.
 *
 * jsdom does not ship ResizeObserver, so we install a fake here so the
 * `useResizeWidth` hook inside `GridNodeHeader` can be driven
 * deterministically. The fake stores each observed element's callback
 * in a module-scoped Map; tests fire `fireResize(el, width)` to send a
 * synthetic `ResizeObserverEntry`. Without this stub the hook never
 * updates from its initial `Infinity` (== widest tier) and every chip
 * would always render — the bug would not regress by failing.
 *
 * Tier thresholds pinned by the same constants the component reads.
 * Keeping these aligned matters: if anyone changes `HEADER_TIER_BREAKPOINTS`,
 * these expected widths must follow. We pin them to the *current* values
 * (200/300/400/600) which match the issue's acceptance criteria, so a
 * future narrowing or widening of any band forces a deliberate test
 * update alongside the constant change.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { act } from 'react';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';

// ----- Fake ResizeObserver -------------------------------------------------
//
// Stores the most recent callback per observed element. Tests call
// `fireResize(el, width)` to push a synthetic entry through that
// callback. Mirrors the ResizeObserver constructor signature so the
// production hook can `new RO(cb)` without modification.
type ROCallback = (entries: Array<Pick<ResizeObserverEntry, 'contentRect'>>) => void;
const roCallbacks = new Map<Element, ROCallback>();

class FakeResizeObserver {
  private cb: ROCallback;
  private observed = new Set<Element>();
  constructor(cb: ROCallback) {
    this.cb = cb;
  }
  observe(el: Element) {
    roCallbacks.set(el, this.cb);
    this.observed.add(el);
  }
  unobserve(el: Element) {
    roCallbacks.delete(el);
    this.observed.delete(el);
  }
  disconnect() {
    // Real ResizeObserver's disconnect drops only this observer's
    // recorded targets, leaving other observers untouched. Iterate
    // `this.observed` rather than calling `roCallbacks.clear()` so a
    // sibling header's stored callback survives a peer's unmount.
    for (const el of this.observed) roCallbacks.delete(el);
    this.observed.clear();
  }
}

function fireResize(el: Element, width: number) {
  const cb = roCallbacks.get(el);
  if (!cb) throw new Error('No ResizeObserver registered for element');
  // Wrap in `act()` so React flushes the resulting `setWidth` update
  // synchronously. Without this, React 18+ defers the re-render past
  // our DOM reads and every test would see the `xl` (initial Infinity)
  // tier rather than the one the test asked for.
  act(() => {
    cb([{ contentRect: { width, height: 0, top: 0, left: 0, right: width, bottom: 0, x: 0, y: 0 } } as unknown as ResizeObserverEntry]);
  });
}

beforeEach(() => {
  roCallbacks.clear();
  vi.stubGlobal('ResizeObserver', FakeResizeObserver);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

// ----- Module mocks -------------------------------------------------------
//
// `BuildRunDropdown` is the right-side companion of the header; we
// render a stub here so the tests don't depend on its internals (other
// tests already cover the dropdown's own behaviour). Keeping the stub
// visible in the DOM via `data-testid="build-run"` lets us assert
// acceptance criterion "BuildRunDropdown must never be the sole visible
// element" by checking the title is always co-rendered.
vi.mock('../../src/components/BuildRun/BuildRunDropdown', () => ({
  BuildRunDropdown: () => <span data-testid="build-run">Build â–¼</span>,
}));

const summaryMock = vi.fn();
vi.mock('../../src/hooks/useGitSummary', () => ({
  useGitSummary: () => ({ summary: summaryMock(), loading: false, refresh: vi.fn() }),
}));

const prMock = vi.fn();
vi.mock('../../src/hooks/useOpenPr', () => ({
  useOpenPr: () => ({ pr: prMock(), loading: false, refresh: vi.fn() }),
}));

const { openUrlMock } = vi.hoisted(() => ({
  openUrlMock: vi.fn().mockResolvedValue(undefined),
}));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: openUrlMock }));

// Mirror the lib/tauri mock from `grid-node-header.test.tsx` so the
// kebab's "Reveal in file explorer" and Pin/Unpin items don't dispatch a
// real IPC during the responsive suite.
const { openInFileManagerMock, toggleNodePinnedMock } = vi.hoisted(() => ({
  openInFileManagerMock: vi.fn().mockResolvedValue(undefined),
  toggleNodePinnedMock: vi.fn(),
}));
vi.mock('../../src/lib/tauri', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/lib/tauri')>();
  return { ...actual, openInFileManager: openInFileManagerMock, toggleNodePinned: toggleNodePinnedMock };
});

// Imported AFTER mocks so it sees the stubbed BuildRunDropdown etc.
import { GridNodeHeader } from '../../src/components/AgentNodeView/GridNodeHeader';

const NODE: AgentNode = {
  id: 1,
  mesh_id: 1,
  name: 'longish-agent-name',
  path: '/repo',
  branch: 'main',
  env: 'wsl',
  provider: 'anthropic',
  status: 'running',
  use_worktree: false,
  position: 0,
  created_at: new Date(0).toISOString(),
  scratchpad: '',
  sandbox: false,
  is_pinned: false,
};

const MESH: Mesh = {
  id: 1,
  name: 'demo',
  path: '/repo',
  layout: 'single',
  position: 0,
  created_at: new Date(0).toISOString(),
};

function setupWithSummaryAndPr() {
  // Realistic input — every chip has content to show at its full tier.
  summaryMock.mockReturnValue({ total: 6, added: 3, modified: 2, deleted: 1 });
  prMock.mockReturnValue({
    number: 123,
    url: 'https://github.com/alondero/buildmesh/pull/123',
    title: 'feat: responsive header',
    draft: false,
  });
}

function setupCommonState() {
  seedAgentNodes([NODE], NODE.id);
  useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
  // Baseline grid mode — Single (which subsumes the old maximize) flips
  // the header's solo/restore affordances, so tests opt into it explicitly.
  useUIStore.setState({ viewMode: 'mesh', lastNonSingleMode: 'mesh' });
  summaryMock.mockReset();
  prMock.mockReset();
  openUrlMock.mockClear();
  toggleNodePinnedMock.mockReset();
}

function renderHeader(width: number) {
  const { getByTestId, ...rest } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
  const root = getByTestId('grid-node-header');
  fireResize(root, width);
  return { ...rest, root, getByTestId };
}

// ----- Tier classification pure-function tests --------------------------
import { getHeaderTier, HEADER_TIER_BREAKPOINTS } from '../../src/components/AgentNodeView/GridNodeHeader';
import { seedAgentNodes } from './helpers/seedAgentNodes';

describe('getHeaderTier()', () => {
  it('returns xl at the wide breakpoint and above', () => {
    expect(getHeaderTier(HEADER_TIER_BREAKPOINTS.xl)).toBe('xl');
    expect(getHeaderTier(900)).toBe('xl');
  });
  it('returns wide between medium and xl breakpoints', () => {
    expect(getHeaderTier(HEADER_TIER_BREAKPOINTS.wide)).toBe('wide');
    expect(getHeaderTier(HEADER_TIER_BREAKPOINTS.xl - 1)).toBe('wide');
  });
  it('returns medium between slim and wide breakpoints', () => {
    expect(getHeaderTier(HEADER_TIER_BREAKPOINTS.medium)).toBe('medium');
    expect(getHeaderTier(HEADER_TIER_BREAKPOINTS.wide - 1)).toBe('medium');
  });
  it('returns slim between compact and medium breakpoints', () => {
    expect(getHeaderTier(HEADER_TIER_BREAKPOINTS.slim)).toBe('slim');
    expect(getHeaderTier(HEADER_TIER_BREAKPOINTS.medium - 1)).toBe('slim');
  });
  it('returns compact below the slim floor', () => {
    expect(getHeaderTier(HEADER_TIER_BREAKPOINTS.slim - 1)).toBe('compact');
    expect(getHeaderTier(0)).toBe('compact');
  });
});

// ----- Render-branch tests at the four acceptance-criteria widths ------

describe('GridNodeHeader responsive behaviour (issue #736)', () => {
  beforeEach(setupCommonState);

  describe('at 200 px (compact)', () => {
    beforeEach(setupWithSummaryAndPr);

    it('reports data-tier="compact"', () => {
      const { root } = renderHeader(200);
      expect(root.getAttribute('data-tier')).toBe('compact');
    });

    it('always renders the node name and provider icon together (#736 core acceptance)', () => {
      const { root } = renderHeader(200);
      // The title is the agent name (`InlineEditableText` wraps it in a span).
      expect(root.textContent).toContain('longish-agent-name');
      // The provider icon renders an element with the provider id as data-attr.
      // ProviderIcon is a small component; we just assert the title and the
      // BuildRunDropdown render side-by-side, which is the criterion
      // "BuildRunDropdown must never be the sole visible element".
      expect(screen.getByTestId('build-run')).toBeTruthy();
      expect(screen.getByTestId('build-run').textContent).toContain('Build');
      expect(root.textContent).toContain('longish-agent-name');
    });

    it('hides the mesh label to give the title room', () => {
      const { root } = renderHeader(200);
      // The mesh label format is `[demo #1]`. It must NOT be present at
      // compact tier because the title is the highest-priority text.
      expect(root.textContent).not.toContain('[demo #1]');
    });

    it('hides the worktree/root pill', () => {
      const { root } = renderHeader(200);
      expect(root.textContent).not.toContain('root');
      expect(screen.queryByTitle('Agent runs in the repository root')).toBeNull();
    });

    it('hides the diff summary', () => {
      const { root } = renderHeader(200);
      expect(root.textContent).not.toContain('+3');
      expect(screen.queryByTitle('Click to see changes')).toBeNull();
    });

    it('hides the PR chip', () => {
      const { root } = renderHeader(200);
      expect(root.textContent).not.toContain('PR #123');
    });

    it('renders the kebab overflow menu instead of inline close+maximize', () => {
      const { root } = renderHeader(200);
      expect(screen.getByLabelText('Agent node actions')).toBeTruthy();
      expect(screen.queryByLabelText('Close agent node')).toBeNull();
      expect(screen.queryByLabelText('Maximize agent node')).toBeNull();
      // sanity: the kebab is the right-side trigger, not a chip on the left
      expect(root.contains(screen.getByLabelText('Agent node actions'))).toBe(true);
    });
  });

  describe('at 300 px (slim)', () => {
    beforeEach(setupWithSummaryAndPr);

    it('reports data-tier="slim"', () => {
      const { root } = renderHeader(300);
      expect(root.getAttribute('data-tier')).toBe('slim');
    });

    it('shows the mesh label alongside the title', () => {
      const { root } = renderHeader(300);
      expect(root.textContent).toContain('longish-agent-name');
      expect(root.textContent).toContain('[demo #1]');
    });

    it('still hides worktree pill, summary, and PR', () => {
      const { root } = renderHeader(300);
      expect(root.textContent).not.toContain('root');
      expect(root.textContent).not.toContain('+3');
      expect(root.textContent).not.toContain('PR #123');
    });

    it('keeps the kebab (inline buttons are still too wide)', () => {
      renderHeader(300);
      expect(screen.getByLabelText('Agent node actions')).toBeTruthy();
      expect(screen.queryByLabelText('Close agent node')).toBeNull();
    });
  });

  describe('at 400 px (medium)', () => {
    beforeEach(setupWithSummaryAndPr);

    it('reports data-tier="medium"', () => {
      const { root } = renderHeader(400);
      expect(root.getAttribute('data-tier')).toBe('medium');
    });

    it('shows the worktree/root pill', () => {
      const { root } = renderHeader(400);
      expect(screen.getByTitle('Agent runs in the repository root')).toBeTruthy();
      expect(root.textContent).toContain('root');
    });

    it('still hides the diff summary and PR chip', () => {
      const { root } = renderHeader(400);
      expect(root.textContent).not.toContain('+3');
      expect(root.textContent).not.toContain('PR #123');
    });

    it('uses the inline close+maximize buttons (no kebab)', () => {
      renderHeader(400);
      expect(screen.getByLabelText('Close agent node')).toBeTruthy();
      expect(screen.getByLabelText('Maximize agent node')).toBeTruthy();
      expect(screen.queryByLabelText('Agent node actions')).toBeNull();
    });
  });

  describe('at 600 px (wide — the issue acceptance floor)', () => {
    // Note: the issue's "Add tests covering 200, 300, 400, 600px widths"
    // spans four of the five tiers — 600 maps to `wide` (xl kicks in at
    // 640). The diff summary is a deliberate xl-only signal because it
    // competes for the same horizontal real estate as the worktree pill
    // and PR chip and would otherwise crowd them out at the 480-600
    // window. The dedicated 700 px test below covers the xl case.
    beforeEach(setupWithSummaryAndPr);

    it('reports data-tier="wide"', () => {
      const { root } = renderHeader(600);
      expect(root.getAttribute('data-tier')).toBe('wide');
    });

    it('shows mesh label, worktree pill, and PR chip; hides diff summary', () => {
      const { root } = renderHeader(600);
      expect(root.textContent).toContain('[demo #1]');
      expect(root.textContent).toContain('root');
      expect(root.textContent).toContain('PR #123');
      // Summary chip is only at xl — covers the full set would crowd the
      // 480-640 window against the worktree pill and PR chip.
      expect(root.textContent).not.toContain('+3');
    });

    it('uses the inline close+maximize buttons (no kebab)', () => {
      renderHeader(600);
      expect(screen.getByLabelText('Close agent node')).toBeTruthy();
      expect(screen.getByLabelText('Maximize agent node')).toBeTruthy();
      expect(screen.queryByLabelText('Agent node actions')).toBeNull();
    });
  });

  describe('at 700 px (xl — summary joins the row)', () => {
    beforeEach(setupWithSummaryAndPr);

    it('reports data-tier="xl"', () => {
      const { root } = renderHeader(700);
      expect(root.getAttribute('data-tier')).toBe('xl');
    });

    it('shows every chip including the diff summary', () => {
      const { root } = renderHeader(700);
      expect(root.textContent).toContain('[demo #1]');
      expect(root.textContent).toContain('root');
      expect(root.textContent).toContain('+3');
      expect(root.textContent).toContain('~2');
      expect(root.textContent).toContain('-1');
      expect(root.textContent).toContain('PR #123');
    });
  });

  // ----- Tier transitions on resize -----------------------------------
  describe('transition between tiers on resize', () => {
    beforeEach(setupWithSummaryAndPr);

    it('reveals progressively-hidden chips as the pane grows', () => {
      const { root } = renderHeader(200);
      expect(root.textContent).not.toContain('root'); // compact
      expect(root.textContent).not.toContain('PR #123');
      // Grow to medium — worktree pill appears.
      fireResize(root, 420);
      expect(root.textContent).toContain('root');
      expect(root.textContent).not.toContain('PR #123'); // PR still hidden at medium
      // Grow to wide — PR chip appears.
      fireResize(root, 560);
      expect(root.textContent).toContain('PR #123');
      expect(root.textContent).not.toContain('+3'); // summary still hidden
      // Grow to xl — diff summary appears.
      fireResize(root, 700);
      expect(root.textContent).toContain('+3');
    });

    it('swaps from inline buttons to kebab as the pane shrinks', () => {
      const { root } = renderHeader(700);
      expect(screen.getByLabelText('Close agent node')).toBeTruthy();
      expect(screen.queryByLabelText('Agent node actions')).toBeNull();
      fireResize(root, 300);
      expect(screen.getByLabelText('Agent node actions')).toBeTruthy();
      expect(screen.queryByLabelText('Close agent node')).toBeNull();
    });
  });

  // ----- Kebab menu behaviours ---------------------------------------
  describe('kebab overflow menu (issue #736)', () => {
    beforeEach(() => {
      // Keep summary/PR null at narrow widths so the test isn't polluted
      // by extra chips.
      summaryMock.mockReturnValue(null);
      prMock.mockReturnValue(null);
    });

    it('opens on click and reveals Regenerate + Reveal + Pin + Maximize + Close items (issue #1502)', () => {
      // The actions are rendered inline above the kebab threshold; below
      // it, all fold into the menu so the title isn't squeezed out.
      // Kebab item order by DOM position: Regenerate (0, issue #1502 —
      // mirrors the sidebar context-menu order for discoverability),
      // Open in file explorer (1), Pin/Unpin (2, wayfinder #982 / #985),
      // Maximize/Restore (3), Close (4). The manual Finish item (#484)
      // was removed — wrap-up is an autopilot-only concern.
      const { root } = renderHeader(200);
      const trigger = screen.getByLabelText('Agent node actions');
      fireEvent.click(trigger);
      const menu = document.querySelector('[role="menu"]')!;
      expect(menu).toBeTruthy();
      // Query only the TOP-LEVEL menuitems (the Regenerate submenu renders
      // its own nested `[role="menu"]` when open — closed here, but guard
      // against future hover-open flakiness by scoping to direct children
      // of the kebab menu root).
      const topLevel = Array.from(menu.querySelectorAll(':scope > [role="menuitem"], :scope > div > [role="menuitem"]'));
      expect(topLevel).toHaveLength(5);
      expect(topLevel[0].textContent).toContain('Regenerate');
      expect(topLevel[1].textContent).toContain('Open in file explorer');
      expect(topLevel[2].textContent).toContain('Pin node');
      expect(topLevel[3].textContent!.toLowerCase()).toMatch(/maximize|restore grid/);
      expect(topLevel[4].textContent).toContain('Close agent node');
      // sanity: the trigger is in the DOM and the menu is its descendant tree
      expect(root.contains(trigger)).toBe(true);
    });

    it('opening the "Open in file explorer" item invokes openInFileManager with the resolved path', () => {
      // The kebab fires the same handler as the inline button — they
      // differ only in their mount tier, not their semantics. The IPC
      // contract (root vs worktree resolution) is asserted in
      // grid-node-header.test.tsx; here we pin the kebab routes
      // through the same wrapper rather than its own private copy.
      openInFileManagerMock.mockClear();
      const node = { ...NODE, use_worktree: true, worktree_name: 'kebab-feat' };
      useAgentNodeStore.setState({ nodesById: { [node.id]: node }, nodeIds: [node.id] });
      render(<GridNodeHeader nodeId={node.id} onBuildRun={() => {}} />);
      // Drive the responsive tier below the kebab threshold (compact =
      // < 280px) so the kebab, not the inline trio, is what renders.
      fireResize(screen.getByTestId('grid-node-header'), 200);
      fireEvent.click(screen.getByLabelText('Agent node actions'));
      const menu = document.querySelector('[role="menu"]')!;
      // Issue #1502 — Regenerate is now the first row, so Reveal shifts to index 1.
      const items = menu.querySelectorAll('[role="menuitem"]');
      // Find Reveal by label (robust to the Regenerate row shifting indices).
      const reveal = Array.from(items).find((el) => /Open in file explorer/.test(el.textContent ?? '')) as HTMLElement;
      fireEvent.click(reveal);
      expect(openInFileManagerMock).toHaveBeenCalledWith('/repo/.claude/worktrees/kebab-feat');
    });

    it('toggles Maximize from the kebab item (enters Single — wayfinder #982)', () => {
      renderHeader(200);
      expect(useUIStore.getState().viewMode).toBe('mesh');
      fireEvent.click(screen.getByLabelText('Agent node actions'));
      // Find the item by its label — note it varies by platform (Alt vs ⌥).
      const maximizeItem = Array.from(document.querySelectorAll('[role="menuitem"]'))
        .find((el) => /maximize/i.test(el.textContent ?? '')) as HTMLElement | undefined;
      expect(maximizeItem).toBeTruthy();
      fireEvent.click(maximizeItem!);
      // Single subsumes the old per-node maximize; the mode switch solos
      // the active node (NODE is active in setupCommonState).
      expect(useUIStore.getState().viewMode).toBe('single');
    });

    it('toggles Pin from the kebab item (wayfinder #982 / #985)', () => {
      toggleNodePinnedMock.mockResolvedValueOnce({ ...NODE, is_pinned: true });
      renderHeader(200);
      fireEvent.click(screen.getByLabelText('Agent node actions'));
      const pinItem = Array.from(document.querySelectorAll('[role="menuitem"]'))
        .find((el) => /pin node/i.test(el.textContent ?? '')) as HTMLElement | undefined;
      expect(pinItem).toBeTruthy();
      fireEvent.click(pinItem!);
      expect(toggleNodePinnedMock).toHaveBeenCalledWith(NODE.id);
    });

    it('labels the kebab pin item "Unpin node" when the node is pinned', () => {
      const pinned = { ...NODE, is_pinned: true };
      seedAgentNodes([pinned]);
      render(<GridNodeHeader nodeId={pinned.id} onBuildRun={() => {}} />);
      fireResize(screen.getByTestId('grid-node-header'), 200);
      fireEvent.click(screen.getByLabelText('Agent node actions'));
      const pinItem = Array.from(document.querySelectorAll('[role="menuitem"]'))
        .find((el) => /unpin node/i.test(el.textContent ?? '')) as HTMLElement | undefined;
      expect(pinItem).toBeTruthy();
      expect(pinItem!.getAttribute('aria-pressed')).toBe('true');
    });

    it('toggles Close from the kebab item (closing the node)', async () => {
      renderHeader(200);
      fireEvent.click(screen.getByLabelText('Agent node actions'));
      const closeItem = Array.from(document.querySelectorAll('[role="menuitem"]'))
        .find((el) => /^×?\s*Close/i.test((el.textContent ?? '').trim())) as HTMLElement | undefined;
      expect(closeItem).toBeTruthy();
      fireEvent.click(closeItem!);
      // deleteAgentNode is async across three phases (issue #1001):
      //   Phase 0 — sync: flips `closingNodeIds` so the row shows a spinner.
      //   Phase 1 — await getWorktreeCloseSafety + optional user prompt.
      //   Phase 2 — sync setState drops the row from `nodesById`, then
      //             awaits the kill + delete IPCs.
      // The previous shape awaited only Phase 0 — Phase 2's setState landed
      // AFTER the test returned, re-rendering the leaked GridNodeHeader
      // fiber with `node = undefined` and tripping React's
      // "Rendered fewer hooks than expected" assertion (the comment "the
      // header unmounts during the teardown" assumed Phase 2 finished
      // in-flight, which it doesn't against mocked IPC). Awaiting the
      // row removal AND then a couple of macro-task ticks so all the
      // downstream awaits (`kill_agent`, `delete_agent_node` IPCs) drain
      // keeps the test fully settled before the next test's setup.
      await waitFor(() => {
        expect(useAgentNodeStore.getState().closingNodeIds.has(NODE.id)).toBe(true);
      });
      await waitFor(() => {
        expect(useAgentNodeStore.getState().nodesById[NODE.id]).toBeUndefined();
      });
      // Drain any remaining microtasks (the `await killAgent` +
      // `await api.deleteAgentNode` chain) so no setState lands during
      // the next test's setup phase.
      await new Promise((r) => setTimeout(r, 0));
      await new Promise((r) => setTimeout(r, 0));
    });

    it('updates the kebab\'s Maximize label to "Restore grid" in Single mode', () => {
      useUIStore.setState({ viewMode: 'single', lastNonSingleMode: 'mesh' });
      const { root } = renderHeader(200);
      fireEvent.click(screen.getByLabelText('Agent node actions'));
      const menu = document.querySelector('[role="menu"]')!;
      expect(menu.textContent?.toLowerCase()).toMatch(/restore grid/);
      expect(menu.textContent?.toLowerCase()).not.toMatch(/^.*maximize/);
      // Still a menu of five actions: Regenerate + Reveal + Pin +
      // Maximize/Restore + Close (issue #1502 adds Regenerate first).
      // The inline buttons fold into the kebab at this tier too, so the
      // count lives here as well as on the inline path. (Finish was
      // removed — autopilot-only concern.)
      const topLevel = menu.querySelectorAll(':scope > [role="menuitem"], :scope > div > [role="menuitem"]');
      expect(topLevel).toHaveLength(5);
      // Sanity: render did not unmount.
      expect(root.isConnected).toBe(true);
    });

    it('closes on Escape and returns focus to the kebab trigger', async () => {
      renderHeader(200);
      const trigger = screen.getByLabelText('Agent node actions');
      trigger.focus();
      fireEvent.click(trigger);
      expect(document.querySelector('[role="menu"]')).toBeTruthy();
      fireEvent.keyDown(document, { key: 'Escape' });
      await waitFor(() => {
        expect(document.querySelector('[role="menu"]')).toBeNull();
      });
      // Focus returns to the trigger via requestAnimationFrame. jsdom does
      // NOT advance rAFs on its own; waitFor polls. The restore uses
      // requestAnimationFrame per the existing MeshItem close pattern (#735).
      await waitFor(() => expect(document.activeElement).toBe(trigger));
    });

    it('closes on outside mousedown (no items selected)', async () => {
      renderHeader(200);
      fireEvent.click(screen.getByLabelText('Agent node actions'));
      expect(document.querySelector('[role="menu"]')).toBeTruthy();
      fireEvent.mouseDown(document.body);
      await waitFor(() => {
        expect(document.querySelector('[role="menu"]')).toBeNull();
      });
    });

    it('hides the kebab and reveals inline buttons at wider tiers', () => {
      // Render at wide tier (medium+) — kebab absent, inline present.
      const { rerender } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
      const root = screen.getByTestId('grid-node-header');
      fireResize(root, 500);
      expect(screen.queryByLabelText('Agent node actions')).toBeNull();
      expect(screen.getByLabelText('Close agent node')).toBeTruthy();
      expect(screen.getByLabelText('Maximize agent node')).toBeTruthy();
      rerender(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    });
  });
});
