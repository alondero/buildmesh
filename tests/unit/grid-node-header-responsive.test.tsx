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
vi.mock('../../src/hooks/useProviderList', () => ({ useProviderList: () => [], __resetSharedProviderListForTests: () => {} }));
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
  invalidateOpenPrForNode: vi.fn(),
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
  return {
    ...actual,
    openInFileManager: openInFileManagerMock,
    toggleNodePinned: toggleNodePinnedMock,
    mergePr: vi.fn().mockResolvedValue('Merged'),
  };
});

// Imported AFTER mocks so it sees the stubbed BuildRunDropdown etc.
import { seedAgentNodes } from './helpers/seedAgentNodes';
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


describe('GridNodeHeader compact layout behaviour', () => {
  beforeEach(setupCommonState);
  it.each([200, 240, 300, 400, 600, 700])('keeps title, maximize and actions accessible at %ipx without metadata rows', width => {
    setupWithSummaryAndPr();
    const { root } = renderHeader(width);
    expect(root.textContent).toContain(NODE.name);
    expect(screen.getByRole('button', { name: 'Maximize agent node' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Agent node actions' })).toBeTruthy();
    expect(root.textContent).not.toContain('Repository root');
    expect(root.textContent).not.toContain('changed files');
    expect(screen.queryByText('PR #123') !== null).toBe(width >= 640);
    fireEvent.click(screen.getByRole('button', { name: 'Agent node actions' }));
    expect(screen.getByRole('menuitem', { name: /Close session/ })).toBeTruthy();
    expect(screen.getByRole('menuitem', { name: 'Session details' })).toBeTruthy();
  });

  it('keeps an open menu and its commands available when a pane changes width', () => {
    const { root } = renderHeader(700);
    fireEvent.click(screen.getByRole('button', { name: 'Agent node actions' }));
    fireResize(root, 240);
    fireEvent.click(screen.getByRole('menuitem', { name: 'Maximize (Alt+G)' }));
    expect(useUIStore.getState().viewMode).toBe('single');
    expect(screen.queryByRole('menu')).toBeNull();
  });

  it('supports keyboard navigation, disabled-item skipping and Escape focus return', async () => {
    renderHeader(240);
    const trigger = screen.getByRole('button', { name: 'Agent node actions' });
    fireEvent.click(trigger);
    expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'Open in file explorer' }));
    fireEvent.keyDown(document, { key: 'ArrowUp' });
    expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'View changes' }));
    fireEvent.keyDown(document, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'Open in file explorer' }));
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('menu')).toBeNull();
    await waitFor(() => expect(document.activeElement).toBe(trigger));
  });

  it('closes on an outside click', () => {
    renderHeader(240);
    fireEvent.click(screen.getByRole('button', { name: 'Agent node actions' }));
    fireEvent.mouseDown(document.body);
    expect(screen.queryByRole('menu')).toBeNull();
  });
});
