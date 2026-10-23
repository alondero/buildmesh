/**
 * Title-bar navigation hierarchy (#2421) — the width tiers that decide which
 * controls the bar renders, pinned through a stubbed `matchMedia` because the
 * tiers are chosen in JS (jsdom applies no media queries).
 *
 *   < 1440px     one labelled View selector (view + scope + Mesh picker);
 *                Usage, Zoom, Settings and Mobile live in the More panel.
 *   1440–1699px  the five labelled segments and the Scope picker return;
 *                the utilities stay folded (the Filtered centre cell cannot
 *                afford their labels until 1700px).
 *   >= 1700px    the utilities are back in the bar as labelled pills.
 *
 * The modals and window API are stubbed as in `title-bar.test.tsx`; this file
 * pins which surface owns each destination at each tier, and that every one
 * stays keyboard-reachable with the right pressed/expanded state.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';

const windowApi = vi.hoisted(() => ({
  minimize: vi.fn(),
  toggleMaximize: vi.fn(),
  close: vi.fn(),
  isMaximized: vi.fn().mockResolvedValue(false),
  onResized: vi.fn().mockResolvedValue(() => {}),
  isFocused: vi.fn().mockResolvedValue(true),
  onFocusChanged: vi.fn().mockResolvedValue(() => {}),
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => windowApi,
}));

/** What each modal saw focused when it mounted. The shared Modal records
    `document.activeElement` at mount and restores it on close, so this is the
    element focus would return to (#2421 review). */
const modalFocusAtMount = vi.hoisted(() => ({ settings: null as Element | null, remote: null as Element | null }));

vi.mock('../../src/components/AppSettings/AppSettingsModal', async () => {
  const { useLayoutEffect } = await import('react');
  return {
    AppSettingsModal: () => {
      useLayoutEffect(() => { modalFocusAtMount.settings = document.activeElement; }, []);
      return <div role="dialog" aria-label="App settings" />;
    },
  };
});

vi.mock('../../src/components/RemoteAccess/RemoteAccessModal', async () => {
  const { useLayoutEffect } = await import('react');
  return {
    RemoteAccessModal: () => {
      useLayoutEffect(() => { modalFocusAtMount.remote = document.activeElement; }, []);
      return <div role="dialog" aria-label="Remote access" />;
    },
  };
});

import { TitleBar } from '../../src/components/TitleBar/TitleBar';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useToastStore } from '../../src/stores/toastStore';
import { type AgentNode } from '../../src/stores/agentNodeStore';
import { enterMesh } from '../../src/lib/navigation';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import {
  TERMINAL_FONT_SIZE_DEFAULT,
  setTerminalFontSize,
  terminalFontSize,
} from '../../src/components/Terminal/terminalConfig';

const MESH_1: Mesh = {
  id: 1, name: 'demo-1', path: '/repo/1', branch: 'main', position: 0,
  color: null, layout: 'grid', use_worktree: false, created_at: '2026-01-01',
  github_owner: null, github_repo: null, github_last_synced: null, pre_spawn_pool_size: 0,
};
const MESH_2: Mesh = { ...MESH_1, id: 2, name: 'demo-2', path: '/repo/2', position: 1 };

const NODE_A: AgentNode = {
  id: 1, mesh_id: 1, name: 'agent-a', path: '/repo/1', branch: 'main',
  env: 'wsl', provider: 'claude', status: 'running', cli_session_id: null,
  use_worktree: false, source_issue: null, worktree_name: null,
  created_at: '2026-01-01', position: 0, scratchpad: '', sandbox: false,
  source_pr: null, head_repo_owner: null, head_repo_clone_url: null,
  source_pr_pinned_sha: null, is_pinned: false, archived: false,
};
const NODE_B: AgentNode = { ...NODE_A, id: 2, mesh_id: 2, name: 'agent-b' };

/** A `matchMedia` that answers `(min-width: Npx)` against a fixed viewport. */
function stubViewport(width: number) {
  vi.stubGlobal('matchMedia', (query: string) => {
    const min = /\(min-width:\s*(\d+)px\)/.exec(query);
    return {
      matches: min ? width >= Number(min[1]) : false,
      media: query,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    };
  });
}

async function renderAt(width: number) {
  stubViewport(width);
  const utils = render(<TitleBar />);
  await act(async () => {});
  return utils;
}

function selector() {
  return screen.getByTestId('view-selector');
}

function openSelector() {
  fireEvent.click(selector());
  return screen.getByRole('dialog', { name: 'View and scope' });
}

function openMore() {
  fireEvent.click(screen.getByRole('button', { name: 'More tools' }));
  return screen.getByTestId('titlebar-overflow-panel');
}

beforeEach(() => {
  modalFocusAtMount.settings = null;
  modalFocusAtMount.remote = null;
  useMeshStore.setState({
    meshes: [MESH_1, MESH_2],
    meshesById: new Map([[1, MESH_1], [2, MESH_2]]),
    selectedMeshId: null,
    loading: false,
    error: null,
  });
  seedAgentNodes([NODE_A, NODE_B]);
  useUIStore.setState({
    viewMode: 'all',
    lastNonSingleMode: 'all',
    focusGridSearchRequest: 0,
    openScopePickerRequest: 0,
    probeOpen: false,
    probeTab: 'files',
    appSettingsOpen: false,
    remoteAccessOpen: false,
    gridSearchQuery: '',
    gridProviderFilter: null,
    gridStatusFilter: null,
  });
  useToastStore.setState({ toasts: [], notifications: [] });
  setTerminalFontSize(TERMINAL_FONT_SIZE_DEFAULT);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('below 1440px: one labelled View selector', () => {
  it('names the active view and its scope without hover, in place of the segments and scope icon', async () => {
    await renderAt(1280);
    act(() => { enterMesh(MESH_1.id); });
    expect(useUIStore.getState().viewMode).toBe('mesh');

    // The visible text — not a tooltip — names both the view and the Mesh.
    expect(selector().textContent).toContain('Mesh Grid');
    expect(selector().textContent).toContain('demo-1');
    expect(selector().getAttribute('aria-expanded')).toBe('false');
    expect(selector().getAttribute('aria-haspopup')).toBe('dialog');
    // The five-segment group and the separate scope icon are gone, so no
    // unlabelled glyph can read as a sixth view mode.
    expect(screen.queryByRole('group', { name: /view mode/i })).toBeNull();
    expect(screen.queryByTestId('scope-indicator')).toBeNull();
  });

  it('keeps cross-Mesh scope explicit in the selector label', async () => {
    await renderAt(900);
    expect(selector().textContent).toContain('All Nodes');
    expect(selector().textContent).toContain('all meshes');
  });

  it('lists every view mode with the active one pressed, and switching closes and returns focus', async () => {
    await renderAt(1280);
    const panel = openSelector();
    expect(selector().getAttribute('aria-expanded')).toBe('true');
    const views = within(panel).getByRole('group', { name: 'View' });
    const rows = within(views).getAllByRole('button');
    expect(rows.map(row => row.textContent)).toEqual(['Single', 'Mesh Grid', 'Pinned', 'All Nodes', 'Filtered']);
    expect(within(views).getByRole('button', { name: 'All Nodes' }).getAttribute('aria-pressed')).toBe('true');
    expect(within(views).getByRole('button', { name: 'Pinned' }).getAttribute('aria-pressed')).toBe('false');
    // Colour is never the only signal: the active row carries a check glyph
    // beside its mode glyph; the others carry the mode glyph alone.
    expect(within(views).getByRole('button', { name: 'All Nodes' }).querySelectorAll('svg')).toHaveLength(2);
    expect(within(views).getByRole('button', { name: 'Pinned' }).querySelectorAll('svg')).toHaveLength(1);

    vi.useFakeTimers({ toFake: ['requestAnimationFrame'] });
    try {
      fireEvent.click(within(views).getByRole('button', { name: 'Pinned' }));
      expect(useUIStore.getState().viewMode).toBe('pinned');
      expect(screen.queryByRole('dialog', { name: 'View and scope' })).toBeNull();
      act(() => { vi.advanceTimersToNextFrame(); });
      expect(document.activeElement).toBe(selector());
    } finally {
      vi.useRealTimers();
    }
    expect(selector().textContent).toContain('Pinned');
  });

  it('asks for a Mesh when Mesh Grid has none, then enters the picked Mesh', async () => {
    await renderAt(1280);
    const panel = openSelector();
    fireEvent.click(within(panel).getByRole('button', { name: 'Mesh Grid' }));
    expect(useUIStore.getState().viewMode).toBe('mesh');
    expect(useMeshStore.getState().selectedMeshId).toBeNull();
    // The panel stays open on the Mesh list — choosing is the user's act.
    const reopened = screen.getByRole('dialog', { name: 'View and scope' });
    const meshes = within(reopened).getByRole('group', { name: 'Mesh' });
    fireEvent.click(within(meshes).getByRole('button', { name: 'demo-2' }));
    expect(useMeshStore.getState().selectedMeshId).toBe(MESH_2.id);
    expect(screen.queryByRole('dialog', { name: 'View and scope' })).toBeNull();
    expect(selector().textContent).toContain('demo-2');
  });

  it('marks the current Mesh in the picker', async () => {
    await renderAt(1280);
    act(() => { enterMesh(MESH_2.id); });
    const panel = openSelector();
    const meshes = within(panel).getByRole('group', { name: 'Mesh' });
    expect(within(meshes).getByRole('button', { name: 'demo-2' }).getAttribute('aria-current')).toBe('true');
    expect(within(meshes).getByRole('button', { name: 'demo-1' }).getAttribute('aria-current')).toBeNull();
  });

  it('hands focus to the node search when Filtered is chosen', async () => {
    await renderAt(1280);
    const panel = openSelector();
    vi.useFakeTimers({ toFake: ['requestAnimationFrame'] });
    try {
      fireEvent.click(within(panel).getByRole('button', { name: 'Filtered' }));
      act(() => { vi.advanceTimersToNextFrame(); });
    } finally {
      vi.useRealTimers();
    }
    expect(useUIStore.getState().viewMode).toBe('filtered');
    expect(document.activeElement).toBe(screen.getByTestId('grid-search-input'));
    expect(selector().textContent).toContain('Filtered');
  });

  it('closes on an outside click without taking focus back from what the user clicked (#2421 review)', async () => {
    await renderAt(1280);
    openSelector();
    vi.useFakeTimers({ toFake: ['requestAnimationFrame'] });
    try {
      // Outside clicks dispatch mousedown on the target; focus moves there first.
      const other = document.createElement('button');
      document.body.appendChild(other);
      other.focus();
      fireEvent.mouseDown(other);
      act(() => { vi.advanceTimersToNextFrame(); });
      expect(screen.queryByRole('dialog', { name: 'View and scope' })).toBeNull();
      expect(document.activeElement).toBe(other);
      other.remove();
    } finally {
      vi.useRealTimers();
    }
  });

  it('closes on Escape without changing the view and returns focus to the selector', async () => {
    await renderAt(1280);
    openSelector();
    vi.useFakeTimers({ toFake: ['requestAnimationFrame'] });
    try {
      fireEvent.keyDown(document, { key: 'Escape' });
      act(() => { vi.advanceTimersToNextFrame(); });
    } finally {
      vi.useRealTimers();
    }
    expect(screen.queryByRole('dialog', { name: 'View and scope' })).toBeNull();
    expect(useUIStore.getState().viewMode).toBe('all');
    expect(document.activeElement).toBe(selector());
  });
});

describe('folded utilities return focus to More when their modal closes (#2421 review)', () => {
  it('records the More trigger as the focus target when Settings opens from the panel', async () => {
    await renderAt(1280);
    const more = screen.getByRole('button', { name: 'More tools' });
    fireEvent.click(more);
    fireEvent.click(within(screen.getByTestId('titlebar-overflow-panel')).getByTestId('titlebar-overflow-settings'));
    expect(useUIStore.getState().appSettingsOpen).toBe(true);
    expect(modalFocusAtMount.settings).toBe(more);
  });

  it('records the More trigger as the focus target when Mobile remote access opens from the panel', async () => {
    await renderAt(1280);
    const more = screen.getByRole('button', { name: 'More tools' });
    fireEvent.click(more);
    fireEvent.click(within(screen.getByTestId('titlebar-overflow-panel')).getByTestId('titlebar-overflow-remote-access'));
    expect(useUIStore.getState().remoteAccessOpen).toBe(true);
    expect(modalFocusAtMount.remote).toBe(more);
  });
});

describe('crossing a tier remounts the picker without reopening it', () => {
  // The tiers MOUNT a different picker, so a picker can mount long after the
  // last Mesh Grid request (#2076) was answered. Only a request made while it
  // is mounted may open it — a stale counter must not pop a panel on resize.
  it('does not open the compact selector for a request made before it mounted', async () => {
    useUIStore.setState({ openScopePickerRequest: 3 });
    await renderAt(1280);
    expect(screen.queryByRole('dialog', { name: 'View and scope' })).toBeNull();
    act(() => useUIStore.getState().requestOpenScopePicker());
    expect(screen.getByRole('dialog', { name: 'View and scope' })).toBeTruthy();
  });

  it('does not open the scope picker for a request made before it mounted', async () => {
    useUIStore.setState({ openScopePickerRequest: 3 });
    await renderAt(1440);
    expect(screen.queryByTestId('scope-picker')).toBeNull();
    act(() => useUIStore.getState().requestOpenScopePicker());
    expect(screen.getByTestId('scope-picker')).toBeTruthy();
  });
});

describe('utilities fold into More below 1700px', () => {
  it('keeps search, notifications and More in the bar and moves the four utilities into the panel', async () => {
    await renderAt(1280);
    expect(screen.getByRole('button', { name: 'Search or open' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Open Notifications' })).toBeTruthy();
    for (const testId of ['titlebar-usage', 'titlebar-zoom']) {
      expect(screen.queryByTestId(testId)).toBeNull();
    }
    expect(screen.queryByRole('button', { name: 'Open settings' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Open mobile remote access' })).toBeNull();
    // More keeps its visible label at compact widths: it is now the only
    // route to four destinations, so it must not collapse to a bare glyph.
    expect(screen.getByRole('button', { name: 'More tools' }).textContent).toContain('More');

    const panel = openMore();
    expect(within(panel).getByRole('button', { name: /Usage/ })).toBeTruthy();
    expect(within(panel).getByRole('button', { name: /^Settings/ })).toBeTruthy();
    expect(within(panel).getByRole('button', { name: /Mobile remote access/ })).toBeTruthy();
    expect(within(panel).getByRole('slider', { name: 'Terminal text size' })).toBeTruthy();
  });

  it('opens each folded utility from the More panel', async () => {
    await renderAt(1280);
    fireEvent.click(within(openMore()).getByRole('button', { name: /Usage/ }));
    expect(useUIStore.getState()).toMatchObject({ probeOpen: true, probeTab: 'usage' });
    expect(screen.queryByTestId('titlebar-overflow-panel')).toBeNull();

    fireEvent.click(within(openMore()).getByRole('button', { name: /^Settings/ }));
    expect(useUIStore.getState().appSettingsOpen).toBe(true);

    fireEvent.click(within(openMore()).getByRole('button', { name: /Mobile remote access/ }));
    expect(useUIStore.getState().remoteAccessOpen).toBe(true);
  });

  it('drives the shared terminal text size from the folded zoom slider', async () => {
    await renderAt(1280);
    const panel = openMore();
    fireEvent.change(within(panel).getByRole('slider', { name: 'Terminal text size' }), {
      target: { value: String(TERMINAL_FONT_SIZE_DEFAULT + 2) },
    });
    expect(terminalFontSize()).toBe(TERMINAL_FONT_SIZE_DEFAULT + 2);
    // The panel stays open: adjusting a slider is not a navigation.
    expect(screen.getByTestId('titlebar-overflow-panel')).toBeTruthy();
  });

  it('names an open folded Usage in the More tooltip (#2421 round 4)', async () => {
    await renderAt(1280);
    act(() => useUIStore.getState().openProbeTab('usage'));
    expect(screen.getByRole('button', { name: 'More tools' }).title).toBe('Usage is open in the inspector');
  });

  it('marks the open inspector tool as current in the More panel, not only by colour (#2421 review)', async () => {
    await renderAt(1280);
    act(() => useUIStore.getState().openProbeTab('review'));
    const panel = openMore();
    expect(within(panel).getByTestId('titlebar-overflow-review').getAttribute('aria-current')).toBe('true');
    expect(within(panel).getByTestId('titlebar-overflow-circuits').getAttribute('aria-current')).toBeNull();
  });

  it('marks Usage as current in the More panel while it is open in the inspector', async () => {
    await renderAt(1280);
    act(() => useUIStore.getState().openProbeTab('usage'));
    const panel = openMore();
    expect(within(panel).getByRole('button', { name: /Usage/ }).getAttribute('aria-current')).toBe('true');
  });
});

describe('1440–1699px: labelled segments, utilities still folded', () => {
  it('renders the five segments and the scope picker, with the utilities in More', async () => {
    await renderAt(1440);
    expect(screen.getByRole('group', { name: /view mode/i })).toBeTruthy();
    expect(screen.getByTestId('scope-indicator')).toBeTruthy();
    expect(screen.queryByTestId('view-selector')).toBeNull();
    expect(screen.queryByTestId('titlebar-usage')).toBeNull();
    expect(within(openMore()).getByRole('button', { name: /Usage/ })).toBeTruthy();
  });

  it('marks the scope control as a dropdown so it cannot read as a sixth view mode', async () => {
    await renderAt(1440);
    const scope = screen.getByTestId('scope-indicator');
    expect(scope.querySelector('[data-testid="scope-indicator-chevron"]')).toBeTruthy();
  });
});

describe('scope picker and tier edges (#2421 round 3)', () => {
  it('closes the scope picker on an outside click without taking focus back from what was clicked', async () => {
    await renderAt(1440);
    fireEvent.click(screen.getByTestId('scope-indicator'));
    expect(screen.getByTestId('scope-picker')).toBeTruthy();
    vi.useFakeTimers({ toFake: ['requestAnimationFrame'] });
    try {
      const other = document.createElement('button');
      document.body.appendChild(other);
      other.focus();
      fireEvent.mouseDown(other);
      act(() => { vi.advanceTimersToNextFrame(); });
      expect(screen.queryByTestId('scope-picker')).toBeNull();
      expect(document.activeElement).toBe(other);
      other.remove();
    } finally {
      vi.useRealTimers();
    }
  });

  it('shows the compact View selector at 1439px and the labelled segments at 1440px', async () => {
    await renderAt(1439);
    expect(screen.getByTestId('view-selector')).toBeTruthy();
    expect(screen.queryByRole('group', { name: /view mode/i })).toBeNull();
    cleanup();
    await renderAt(1440);
    expect(screen.getByRole('group', { name: /view mode/i })).toBeTruthy();
    expect(screen.queryByTestId('view-selector')).toBeNull();
  });

  it('shows the short Filtered label in the bar so the command field keeps its room, with the full name in aria-label (#2421 round 4)', async () => {
    useUIStore.setState({ viewMode: 'filtered' });
    await renderAt(1920);
    const scope = screen.getByTestId('scope-indicator');
    const visible = scope.querySelector('span')?.textContent ?? '';
    expect(visible).toMatch(/^all meshes · \d+ of \d+$/);
    expect(scope.getAttribute('aria-label')).toMatch(/^Filtered across meshes · \d+ of \d+ nodes$/);
  });

  it('folds the utilities at 1699px and returns them at 1700px', async () => {
    await renderAt(1699);
    expect(screen.queryByTestId('titlebar-usage')).toBeNull();
    expect(within(openMore()).getByTestId('titlebar-overflow-usage')).toBeTruthy();
    cleanup();
    await renderAt(1700);
    expect(screen.getByTestId('titlebar-usage')).toBeTruthy();
  });
});

describe('>= 1700px: utilities return to the bar', () => {
  it('renders the labelled utility pills and keeps them out of the More panel', async () => {
    await renderAt(1920);
    expect(screen.getByTestId('titlebar-usage')).toBeTruthy();
    expect(screen.getByTestId('titlebar-zoom')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Open settings' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Open mobile remote access' })).toBeTruthy();
    const panel = openMore();
    expect(within(panel).queryByRole('button', { name: /^Settings/ })).toBeNull();
    expect(within(panel).queryByRole('slider')).toBeNull();
  });
});
