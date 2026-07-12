/**
 * Issue #736 — Agent node title collapses first when the pane is narrow.
 * Make the header responsive.
 *
 * The header should prioritise showing the node name, status dot, and
 * provider icon. Lower-priority elements (mesh label → worktree chip →
 * summary → PR chip → inline actions) should hide progressively before
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
  BuildRunDropdown: () => <span data-testid="build-run">Build ▼</span>,
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
// kebab's new "Reveal in file explorer" item doesn't dispatch a real
// IPC during the responsive suite.
const { openInFileManagerMock } = vi.hoisted(() => ({
  openInFileManagerMock: vi.fn().mockResolvedValue(undefined),
}));
vi.mock('../../src/lib/tauri', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/lib/tauri')>();
  return { ...actual, openInFileManager: openInFileManagerMock };
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
  useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: NODE.id });
  useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
  useUIStore.setState({ maximizedNodeId: null });
  summaryMock.mockReset();
  prMock.mockReset();
  openUrlMock.mockClear();
}

function renderHeader(width: number) {
  const { getByTestId, ...rest } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
  const root = getByTestId('grid-node-header');
  fireResize(root, width);
  return { ...rest, root, getByTestId };
}

// ----- Tier classification pure-function tests --------------------------
import { getHeaderTier, HEADER_TIER_BREAKPOINTS } from '../../src/components/AgentNodeView/GridNodeHeader';

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

    it('opens on click and reveals Reveal + Maximize + Close items', () => {
      // The trio is rendered inline above the kebab threshold; below it,
      // all three fold into the menu so the title isn't squeezed out.
      // Kebab item order matches the inline layout by DOM position:
      // Open in file explorer (0), Maximize/Restore (1), Close (2) —
      // same alignment as the visible trio.
      const { root } = renderHeader(200);
      const trigger = screen.getByLabelText('Agent node actions');
      fireEvent.click(trigger);
      const menu = document.querySelector('[role="menu"]')!;
      expect(menu).toBeTruthy();
      expect(menu.querySelectorAll('[role="menuitem"]')).toHaveLength(3);
      const items = menu.querySelectorAll('[role="menuitem"]');
      expect(items[0].textContent).toContain('Open in file explorer');
      expect(items[1].textContent!.toLowerCase()).toMatch(/maximize|restore grid/);
      expect(items[2].textContent).toContain('Close agent node');
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
      render(<GridNodeHeader node={node} onBuildRun={() => {}} />);
      // Drive the responsive tier below the kebab threshold (compact =
      // < 280px) so the kebab, not the inline trio, is what renders.
      fireResize(screen.getByTestId('grid-node-header'), 200);
      fireEvent.click(screen.getByLabelText('Agent node actions'));
      const menu = document.querySelector('[role="menu"]')!;
      const items = menu.querySelectorAll('[role="menuitem"]');
      // Reveal is the first item in the kebab, mirroring the inline layout.
      fireEvent.click(items[0]);
      expect(openInFileManagerMock).toHaveBeenCalledWith('/repo/.claude/worktrees/kebab-feat');
    });

    it('toggles Maximize from the kebab item', () => {
      renderHeader(200);
      expect(useUIStore.getState().maximizedNodeId).toBeNull();
      fireEvent.click(screen.getByLabelText('Agent node actions'));
      // Click the first menuitem — note the label varies by platform (Alt vs ⌘).
      const maximizeItem = Array.from(document.querySelectorAll('[role="menuitem"]'))
        .find((el) => /maximize/i.test(el.textContent ?? '')) as HTMLElement | undefined;
      expect(maximizeItem).toBeTruthy();
      fireEvent.click(maximizeItem!);
      expect(useUIStore.getState().maximizedNodeId).toBe(NODE.id);
    });

    it('toggles Close from the kebab item (closing the node)', async () => {
      renderHeader(200);
      fireEvent.click(screen.getByLabelText('Agent node actions'));
      const closeItem = Array.from(document.querySelectorAll('[role="menuitem"]'))
        .find((el) => /^×?\s*Close/i.test((el.textContent ?? '').trim())) as HTMLElement | undefined;
      expect(closeItem).toBeTruthy();
      fireEvent.click(closeItem!);
      // deleteAgentNode flips the row into `closingNodeIds`; the header
      // unmounts during the teardown, so the kebab label vanishes.
      await waitFor(() => {
        expect(useAgentNodeStore.getState().closingNodeIds.has(NODE.id)).toBe(true);
      });
    });

    it('updates the kebab\'s Maximize label to "Restore grid" when already maximized', () => {
      useUIStore.setState({ maximizedNodeId: NODE.id });
      const { root } = renderHeader(200);
      fireEvent.click(screen.getByLabelText('Agent node actions'));
      const menu = document.querySelector('[role="menu"]')!;
      expect(menu.textContent?.toLowerCase()).toMatch(/restore grid/);
      expect(menu.textContent?.toLowerCase()).not.toMatch(/^.*maximize/);
      // Still a menu of three actions: Reveal + Maximize/Restore + Close
      // (the inline Reveal button folds into the kebab at this tier
      // too, so its count lives here as well as on the inline path).
      expect(menu.querySelectorAll('[role="menuitem"]')).toHaveLength(3);
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
      const { rerender } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
      const root = screen.getByTestId('grid-node-header');
      fireResize(root, 500);
      expect(screen.queryByLabelText('Agent node actions')).toBeNull();
      expect(screen.getByLabelText('Close agent node')).toBeTruthy();
      expect(screen.getByLabelText('Maximize agent node')).toBeTruthy();
      rerender(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
    });
  });
});
