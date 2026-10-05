/**
 * TitleBar — the bespoke window chrome that replaced the native title bar
 * (frameless window, "decorations": false in tauri.conf.json). Pins the
 * spec shape (wordmark left, ViewModeSwitcher + settings/remote icons as
 * the in-bar toolbar, minimize/maximize/close right), the window-control
 * IPC wiring, the single-writer isMaximized contract (the onResized
 * listener owns the glyph state — no optimistic flip), the drag-region
 * placement (on the bar/spacer/wordmark, never on the interactive
 * clusters), and the modal open/close wiring for the two icons that moved
 * here from the Sidebar header.
 *
 * The two modals are stubbed: the test pins TitleBar's wiring, not the
 * modals' own behaviour (covered by their own suites). The window API
 * mock is file-local and overrides the global setup mock, which only
 * models focus tracking.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { act, fireEvent, render, renderHook, screen, within } from '@testing-library/react';

const windowApi = vi.hoisted(() => ({
  minimize: vi.fn(),
  toggleMaximize: vi.fn(),
  close: vi.fn(),
  isMaximized: vi.fn().mockResolvedValue(false),
  onResized: vi.fn<(cb: () => void) => Promise<() => void>>(),
  // Focus tracking (ADR-0035): drives the caption buttons' inactive state.
  isFocused: vi.fn().mockResolvedValue(true),
  onFocusChanged: vi.fn<
    (cb: (event: { payload: boolean }) => void) => Promise<() => void>
  >(),
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => windowApi,
}));

vi.mock('../../src/components/AppSettings/AppSettingsModal', () => ({
  AppSettingsModal: ({ onClose, initialTab }: { onClose: () => void; initialTab?: string }) => (
    <div role="dialog" aria-label="App settings" data-initial-tab={initialTab}>
      <button type="button" onClick={onClose}>stub-close-settings</button>
    </div>
  ),
}));

vi.mock('../../src/components/RemoteAccess/RemoteAccessModal', () => ({
  RemoteAccessModal: ({ onClose }: { onClose: () => void }) => (
    <div role="dialog" aria-label="Remote access">
      <button type="button" onClick={onClose}>stub-close-remote</button>
    </div>
  ),
}));

import { TitleBar } from '../../src/components/TitleBar/TitleBar';
import { useUIStore, type ViewMode } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { type AgentNode } from '../../src/stores/agentNodeStore';
import { useProbeContext } from '../../src/hooks/useProbeContext';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import {
  TERMINAL_FONT_SIZE_DEFAULT,
  setTerminalFontSize,
  terminalFontSize,
} from '../../src/components/Terminal/terminalConfig';

let resizeHandler: (() => void) | null = null;
let focusHandler: ((event: { payload: boolean }) => void) | null = null;

/** Render and flush the initial isMaximized sync (a promise that resolves
    after mount) so tests don't trip act() warnings on the settle. */
async function renderTitleBar() {
  const utils = render(<TitleBar />);
  await act(async () => {});
  return utils;
}

/** The class list on a caption button's glyph. */
function captionGlyphClass(name: string): string {
  return screen.getByRole('button', { name }).querySelector('svg')!.getAttribute('class') ?? '';
}

beforeEach(() => {
  resizeHandler = null;
  focusHandler = null;
  windowApi.isMaximized.mockResolvedValue(false);
  windowApi.isFocused.mockResolvedValue(true);
  windowApi.onResized.mockImplementation((cb: () => void) => {
    resizeHandler = cb;
    return Promise.resolve(() => {});
  });
  windowApi.onFocusChanged.mockImplementation(
    (cb: (event: { payload: boolean }) => void) => {
      focusHandler = cb;
      return Promise.resolve(() => {});
    },
  );
  useUIStore.setState({
    omnibarOpen: false,
    omnibarMode: 'files',
    probeOpen: false,
    probeTab: 'files',
    activeDiffFile: null,
    appSettingsOpen: false,
    appSettingsTab: 'general',
    remoteAccessOpen: false,
  });
  // The zoom slider is a view over the module-level terminal font size, which
  // persists to localStorage; reset it so slider assertions start from the
  // default regardless of test order.
  setTerminalFontSize(TERMINAL_FONT_SIZE_DEFAULT);
});

describe('TitleBar (bespoke window chrome)', () => {
  describe('spec shape', () => {
    it('renders wordmark, view-mode toolbar, navigation cluster, settings/remote icons and the three window controls', async () => {
      await renderTitleBar();
      expect(screen.getByRole('img', { name: 'Buildmesh' })).toBeTruthy();
      expect(screen.getByRole('group', { name: /view mode/i })).toBeTruthy();
      // Issue #1375 — labelled navigation cluster.
      expect(screen.getByRole('button', { name: 'Search or open' })).toBeTruthy();
      expect(screen.getByRole('button', { name: 'Open Usage' })).toBeTruthy();
      expect(screen.getByRole('button', { name: 'Open settings' })).toBeTruthy();
      expect(screen.getByRole('button', { name: 'Open mobile remote access' })).toBeTruthy();
      expect(screen.getByRole('button', { name: 'Minimize window' })).toBeTruthy();
      expect(screen.getByRole('button', { name: 'Maximize window' })).toBeTruthy();
      expect(screen.getByRole('button', { name: 'Close window' })).toBeTruthy();
    });

    it('marks the header, wordmark and side grid cells as drag regions but never the interactive clusters', async () => {
      const { container } = await renderTitleBar();
      const header = container.querySelector('header')!;
      expect(header.hasAttribute('data-tauri-drag-region')).toBe(true);
      expect(screen.getByRole('img', { name: 'Buildmesh' }).hasAttribute('data-tauri-drag-region')).toBe(true);
      // The header is a 1fr/auto/1fr grid; the two side cells carry the
      // drag region so their empty space grabs the window (the centre cell
      // is the palette field and must not).
      const cells = Array.from(header.children) as HTMLElement[];
      expect(cells).toHaveLength(3);
      expect(cells[0].hasAttribute('data-tauri-drag-region')).toBe(true);
      expect(cells[1].hasAttribute('data-tauri-drag-region')).toBe(false);
      expect(cells[2].hasAttribute('data-tauri-drag-region')).toBe(true);
      // Buttons (and their glyphs) must stay the mousedown target — if any
      // carried the attribute, Tauri's drag script would eat the click.
      for (const label of ['Search or open', 'Open Usage', 'Open settings', 'Open mobile remote access', 'Minimize window', 'Maximize window', 'Close window']) {
        const button = screen.getByRole('button', { name: label });
        expect(button.hasAttribute('data-tauri-drag-region')).toBe(false);
        expect(button.querySelector('[data-tauri-drag-region]')).toBeNull();
      }
    });
  });

  describe('window controls', () => {
    it('wires minimize / toggleMaximize / close to the current window', async () => {
      await renderTitleBar();
      fireEvent.click(screen.getByRole('button', { name: 'Minimize window' }));
      expect(windowApi.minimize).toHaveBeenCalledTimes(1);
      fireEvent.click(screen.getByRole('button', { name: 'Maximize window' }));
      expect(windowApi.toggleMaximize).toHaveBeenCalledTimes(1);
      fireEvent.click(screen.getByRole('button', { name: 'Close window' }));
      expect(windowApi.close).toHaveBeenCalledTimes(1);
    });

    it('swaps the maximize glyph for restore only when isMaximized re-syncs (single-writer)', async () => {
      await renderTitleBar();
      // Initial sync resolved false → Maximize.
      expect(screen.getByRole('button', { name: 'Maximize window' })).toBeTruthy();
      // Clicking toggles the window but must NOT flip the glyph itself —
      // the onResized listener owns isMaximized.
      fireEvent.click(screen.getByRole('button', { name: 'Maximize window' }));
      expect(windowApi.toggleMaximize).toHaveBeenCalledTimes(1);
      expect(screen.getByRole('button', { name: 'Maximize window' })).toBeTruthy();
      // The resize arrives; the re-query reports maximized → Restore.
      windowApi.isMaximized.mockResolvedValue(true);
      await act(async () => { resizeHandler!(); });
      expect(screen.getByRole('button', { name: 'Restore window' })).toBeTruthy();
      // And back again on restore.
      windowApi.isMaximized.mockResolvedValue(false);
      await act(async () => { resizeHandler!(); });
      expect(screen.getByRole('button', { name: 'Maximize window' })).toBeTruthy();
    });
  });

  describe('caption buttons (ADR-0035)', () => {
    it('renders all three as 46px backplates marked with data-window-control', async () => {
      const { container } = await renderTitleBar();
      const controls = Array.from(container.querySelectorAll('[data-window-control]'));
      expect(controls.map((control) => control.getAttribute('data-window-control'))).toEqual([
        'minimize',
        'maximize',
        'close',
      ]);
      for (const control of controls) {
        // 46px is the native caption width, so the three make up the standard
        // 138px cluster flush to the right edge — and it is also the width the
        // measured snap-overlay box has to line up with (the overlay follows
        // the DOM, so this stays honest if it ever changes).
        expect(control.className).toContain('w-[46px]');
        expect(control.className).toContain('shrink-0');
      }
    });

    it('runs the caption cluster full-bleed down the bar', async () => {
      const { container } = await renderTitleBar();
      const cluster = container.querySelector('[data-window-control]')!.parentElement!;
      // A real caption strip fills the height of its title bar rather than
      // floating as three centred chips.
      expect(cluster.className).toContain('self-stretch');
      expect(cluster.className).toContain('shrink-0');
    });

    it('carries the VS Code hover and pressed fills as class literals', async () => {
      await renderTitleBar();
      for (const name of ['Minimize window', 'Maximize window']) {
        const button = screen.getByRole('button', { name });
        expect(button.className).toContain('hover:bg-caption-hover');
        expect(button.className).toContain('active:bg-caption-pressed');
        // The old generic card/status hovers are gone — a caption backplate is
        // a translucent tint, not an opaque chip.
        expect(button.className).not.toContain('hover:bg-bg-card');
        expect(button.className).not.toContain('bg-status-error');
      }
      const close = screen.getByRole('button', { name: 'Close window' });
      expect(close.className).toContain('hover:bg-caption-close-hover');
      expect(close.className).toContain('active:bg-caption-close-pressed');
      expect(close.className).toContain('hover:text-white');
    });

    it('drops the HTML tooltip that native caption buttons do not have', async () => {
      await renderTitleBar();
      // `title` also used to be the source of the accessible name; it now has
      // to come from `aria-label` alone, so pin both halves of that move.
      for (const name of ['Minimize window', 'Maximize window', 'Close window']) {
        const button = screen.getByRole('button', { name });
        expect(button.hasAttribute('title')).toBe(false);
        expect(button.getAttribute('aria-label')).toBe(name);
      }
    });

    it('paints the caption glyphs as filled 16x16 codicon shapes', async () => {
      const { container } = await renderTitleBar();
      const glyphs = Array.from(container.querySelectorAll('[data-window-control] svg'));
      expect(glyphs).toHaveLength(3);
      for (const glyph of glyphs) {
        expect(glyph.getAttribute('viewBox')).toBe('0 0 16 16');
        expect(glyph.getAttribute('fill')).toBe('currentColor');
        // Filled outlines, not the 18px stroke-2 figures they replaced — the
        // stroke weight was most of what read as "not quite Windows".
        expect(glyph.getAttribute('stroke')).toBeNull();
        // 16px as a literal: the app's root font is 13px, so `h-4` would be 13.
        expect(glyph.getAttribute('class')).toContain('h-[16px]');
        expect(glyph.getAttribute('class')).toContain('w-[16px]');
      }
    });

    it('dims the caption glyphs while the window is inactive', async () => {
      await renderTitleBar();
      // Focused on mount → full strength.
      expect(captionGlyphClass('Minimize window')).not.toContain('opacity-60');

      // A focus change the OS drives (alt-tab, taskbar click) dims every
      // caption glyph, not just one of them.
      await act(async () => { focusHandler!({ payload: false }); });
      expect(captionGlyphClass('Minimize window')).toContain('opacity-60');
      expect(captionGlyphClass('Maximize window')).toContain('opacity-60');
      expect(captionGlyphClass('Close window')).toContain('opacity-60');
      // Only the glyph dims — the backplate must stay full-strength so hovering
      // an inactive window's button still reads as a live control.
      const button = screen.getByRole('button', { name: 'Minimize window' });
      expect(button.className).not.toContain('opacity-60');

      await act(async () => { focusHandler!({ payload: true }); });
      expect(captionGlyphClass('Minimize window')).not.toContain('opacity-60');
    });

    it('dims the caption glyphs when the window is already unfocused at mount', async () => {
      // The initial query covers launching behind another window, where no
      // focus-change event is ever delivered to this webview.
      windowApi.isFocused.mockResolvedValue(false);
      await renderTitleBar();
      expect(captionGlyphClass('Minimize window')).toContain('opacity-60');
    });

    it('swaps the maximise glyph for the restore outline on the resize re-query', async () => {
      await renderTitleBar();
      const maximizePath = screen
        .getByRole('button', { name: 'Maximize window' })
        .querySelector('path')!
        .getAttribute('d');
      windowApi.isMaximized.mockResolvedValue(true);
      await act(async () => { resizeHandler!(); });
      const restorePath = screen
        .getByRole('button', { name: 'Restore window' })
        .querySelector('path')!
        .getAttribute('d');
      expect(restorePath).not.toBe(maximizePath);
      // Still the same single-writer rule: the glyph follows the re-query, and
      // the two codicon figures are genuinely different shapes (the restore
      // one is the overlapped pair).
      expect(maximizePath).toBeTruthy();
      expect(restorePath).toBeTruthy();
    });
  });

  describe('modal wiring (icons moved from the Sidebar header)', () => {
    it('passes the requested Settings pane through and resets normal opens to General', async () => {
      await renderTitleBar();
      act(() => useUIStore.getState().openAppSettings('providers'));
      expect(screen.getByRole('dialog', { name: 'App settings' }).getAttribute('data-initial-tab')).toBe('providers');
      fireEvent.click(screen.getByRole('button', { name: 'stub-close-settings' }));
      fireEvent.click(screen.getByRole('button', { name: 'Open settings' }));
      expect(screen.getByRole('dialog', { name: 'App settings' }).getAttribute('data-initial-tab')).toBe('general');
    });
    it('opens and closes the App Settings modal', async () => {
      await renderTitleBar();
      expect(screen.queryByRole('dialog')).toBeNull();
      fireEvent.click(screen.getByRole('button', { name: 'Open settings' }));
      expect(screen.getByRole('dialog', { name: 'App settings' })).toBeTruthy();
      fireEvent.click(screen.getByRole('button', { name: 'stub-close-settings' }));
      expect(screen.queryByRole('dialog')).toBeNull();
    });

    it('opens and closes the Remote Access modal', async () => {
      await renderTitleBar();
      fireEvent.click(screen.getByRole('button', { name: 'Open mobile remote access' }));
      expect(screen.getByRole('dialog', { name: 'Remote access' })).toBeTruthy();
      fireEvent.click(screen.getByRole('button', { name: 'stub-close-remote' }));
      expect(screen.queryByRole('dialog')).toBeNull();
    });
  });

  describe('navigation cluster (issue #1375; Filtered search #1609)', () => {
    it('the labelled search field opens the command palette in files mode', async () => {
      await renderTitleBar();
      expect(useUIStore.getState().omnibarOpen).toBe(false);
      fireEvent.click(screen.getByRole('button', { name: 'Search or open' }));
      expect(useUIStore.getState().omnibarOpen).toBe(true);
      expect(useUIStore.getState().omnibarMode).toBe('files');
    });

    it('the Usage action opens the inspector on the host-global Usage destination', async () => {
      await renderTitleBar();
      expect(useUIStore.getState().probeOpen).toBe(false);
      fireEvent.click(screen.getByRole('button', { name: 'Open Usage' }));
      expect(useUIStore.getState().probeOpen).toBe(true);
      expect(useUIStore.getState().probeTab).toBe('usage');
    });

    it('mirrors the Usage surface state in aria-expanded (closed by default, open when active)', async () => {
      await renderTitleBar();
      const usage = screen.getByRole('button', { name: 'Open Usage' });
      expect(usage.getAttribute('aria-expanded')).toBe('false');
      act(() => {
        useUIStore.setState({ probeOpen: true, probeTab: 'usage' });
      });
      expect(usage.getAttribute('aria-expanded')).toBe('true');
    });

    it('keeps visible labels inside accessible names (WCAG 2.5.3) on the utility pills', async () => {
      await renderTitleBar();
      // SC 2.5.3 Label in Name: the accessible name must contain the
      // visible text, or voice dictation ("click Mobile") can't find the
      // control. Pin it for every pill.
      for (const [name, visible] of [
        ['Open Usage', 'Usage'],
        ['Open settings', 'Settings'],
        ['Open mobile remote access', 'Mobile'],
      ] as const) {
        const button = screen.getByRole('button', { name });
        const labelSpan = button.querySelector('span');
        expect(labelSpan?.textContent).toBe(visible);
        expect(name.toLowerCase()).toContain(visible.toLowerCase());
      }
    });

    // Tools share one disclosure and take no permanent space while it is closed.
    describe('tools overflow disclosure', () => {
      it('renders nothing until the trigger is clicked', async () => {
        await renderTitleBar();
        expect(screen.queryByTestId('titlebar-overflow-panel')).toBeNull();
        expect(screen.getByRole('button', { name: 'More tools' }).getAttribute('aria-expanded')).toBe('false');
      });

      it('lists every inspector tool except Usage, which has its own title-bar button', async () => {
        await renderTitleBar();
        fireEvent.click(screen.getByRole('button', { name: 'More tools' }));

        const panel = screen.getByRole('dialog', { name: 'Tools' });
        const buttons = within(panel).getAllByRole('button');
        expect(buttons.map((button) => button.querySelector('.font-medium')?.textContent)).toEqual([
          'Project Files', 'Agent Changes', 'Project Settings', 'Repository',
          'GitHub Issues', 'Pull Requests', 'Circuits', 'Agent History', 'Notes',
        ]);
        for (const button of buttons) {
          expect(button.querySelector('svg')?.getAttribute('aria-hidden')).toBe('true');
        }
        expect(panel.textContent).toContain('Review what the focused agent changed');
        expect(panel.textContent).toContain('Check health, recover, and clean up branches');
        expect(within(panel).queryByText('Usage')).toBeNull();
        expect(screen.getByRole('button', { name: 'Open Usage' })).toBeTruthy();
      });

      it.each([
        'files', 'review', 'properties', 'worktrees', 'issues', 'pulls',
        'circuits', 'sessions', 'scratchpad',
      ])('opens %s in the inspector, closes the disclosure, and restores focus', async (tab) => {
        const raf = vi.spyOn(window, 'requestAnimationFrame').mockImplementation((cb: FrameRequestCallback) => {
          cb(0);
          return 1;
        });
        try {
          await renderTitleBar();
          const trigger = screen.getByRole('button', { name: 'More tools' });
          fireEvent.click(trigger);
          fireEvent.click(screen.getByTestId(`titlebar-overflow-${tab}`));

          expect(useUIStore.getState().probeOpen).toBe(true);
          expect(useUIStore.getState().probeTab).toBe(tab);
          expect(screen.queryByTestId('titlebar-overflow-panel')).toBeNull();
          expect(document.activeElement).toBe(trigger);
        } finally {
          raf.mockRestore();
        }
      });

      it('toggles closed and closes on Escape', async () => {
        await renderTitleBar();
        const trigger = screen.getByRole('button', { name: 'More tools' });
        fireEvent.click(trigger);
        expect(screen.getByTestId('titlebar-overflow-panel')).toBeTruthy();
        fireEvent.click(trigger);
        expect(screen.queryByTestId('titlebar-overflow-panel')).toBeNull();

        fireEvent.click(trigger);
        expect(screen.getByTestId('titlebar-overflow-panel')).toBeTruthy();
        fireEvent.keyDown(document, { key: 'Escape' });
        expect(screen.queryByTestId('titlebar-overflow-panel')).toBeNull();
      });

      it('closes on an outside click', async () => {
        await renderTitleBar();
        fireEvent.click(screen.getByRole('button', { name: 'More tools' }));
        fireEvent.mouseDown(document.body);
        expect(screen.queryByTestId('titlebar-overflow-panel')).toBeNull();
      });

      it('names the active overflow tool in its tooltip', async () => {
        await renderTitleBar();
        act(() => useUIStore.getState().openProbeTab('review'));
        expect(screen.getByRole('button', { name: 'More tools' }).title).toBe('Agent Changes is open in the inspector');
        act(() => useUIStore.getState().openProbeTab('usage'));
        expect(screen.getByRole('button', { name: 'More tools' }).title).toBe('Open more tools');
      });
    });

    it('carries the responsive degradation classes (labels, chip, flex floors)', async () => {
      const { container } = await renderTitleBar();
      // Pill and switcher labels drop to icon-only below the SAME tier
      // (1400px) since #1609 and PR #1623 — one toolbar, one ladder;
      // the threshold moved from 1300px to 1400px to avoid a 2px clip on
      // the rightmost ViewModeSwitcher segment ("Filtered") at exactly
      // 1300px viewport (where labels become visible but the centre's
      // `w-80` 260px + side clusters' min-content can't coexist). The
      // kbd chip hides FIRST when narrowing at 1399px — user-facing
      // affordances outlast the decorative keyboard hint. Class
      // literals are the contract — they MUST stay as literal strings
      // (not template literals) so Tailwind v4's source scanner
      // compiles them. The media queries themselves are
      // browser-rendered.
      const remotePill = screen.getByRole('button', { name: 'Open mobile remote access' });
      expect(remotePill.querySelector('span')?.className).toContain('max-[1399px]:hidden');
      const usagePill = screen.getByRole('button', { name: 'Open Usage' });
      expect(usagePill.querySelector('span')?.className).toContain('max-[1399px]:hidden');
      const switcherGroup = screen.getByRole('group', { name: /view mode/i });
      const switcherLabel = switcherGroup.querySelector('span');
      expect(switcherLabel?.className).toContain('max-[1399px]:hidden');
      const chip = container.querySelector('kbd');
      expect(chip?.className).toContain('max-[1399px]:hidden');
      // Responsive palette width (PR #1623 review): the field is
      // `w-80` (260px at the 13px root) below 1786px viewport, and
      // bumps to its VS Code-parity `w-[640px]` at >=1786px where
      // the side clusters can afford it. Pin the breakpoint class so a
      // future rebase that drops the bump fails the test loudly.
      const searchButton = screen.getByTestId('titlebar-command-search');
      expect(searchButton.className).toContain('w-80');
      expect(searchButton.className).toContain('min-[1786px]:w-[640px]');
      // Flex floor: the palette field's wrapper must never collapse below
      // its yield-first floor.
      const searchWrapper = searchButton.parentElement!;
      expect(searchWrapper.className).toContain('min-w-0');
    });

    it('keeps the utility pills borderless like the switcher segments (#1609)', async () => {
      await renderTitleBar();
      for (const name of ['Open Usage', 'Open settings', 'Open mobile remote access']) {
        const pill = screen.getByRole('button', { name });
        expect(pill.className).not.toContain('border');
        expect(pill.className).toContain('hover:bg-bg-card');
      }
    });

    it('hides the Search Nodes bar outside the Filtered view (#1609)', async () => {
      await renderTitleBar();
      // Default boot mode (all) → no search input, no placeholder competing
      // with the wordmark/switcher for width.
      expect(screen.queryByTestId('grid-controls')).toBeNull();
    });

    it('mounts the Search Nodes bar beside the switcher only in the Filtered view (#1609)', async () => {
      await renderTitleBar();
      act(() => {
        useUIStore.setState({ viewMode: 'filtered', lastNonSingleMode: 'filtered' });
      });
      const controls = screen.getByTestId('grid-controls');
      // The search mounts in the LEFT cell (wordmark → switcher → search):
      // its nearest drag-region ancestor is the same cell that holds the
      // switcher group, and that cell precedes the palette field.
      const cell = controls.closest('[data-tauri-drag-region]')!;
      expect(cell.contains(screen.getByRole('group', { name: /view mode/i }))).toBe(true);
      const paletteField = screen.getByTestId('titlebar-command-search');
      expect(cell.contains(paletteField)).toBe(false);
    });

    it('clears the search input on view switches away from Filtered without wiping the stored query', async () => {
      // The query persists in the store/localStorage (#988 contract), so
      // re-entering Filtered restores the previous search. Only the input
      // unmounts — the store value is never reset by a mode change.
      await renderTitleBar();
      act(() => {
        useUIStore.setState({ viewMode: 'filtered', gridSearchQuery: 'alpha' });
      });
      expect((screen.getByTestId('grid-search-input') as HTMLInputElement).value).toBe('alpha');
      act(() => {
        useUIStore.setState({ viewMode: 'all' });
      });
      expect(screen.queryByTestId('grid-search-input')).toBeNull();
      expect(useUIStore.getState().gridSearchQuery).toBe('alpha');
    });
  });

  // #2074 — the scope indicator is the surface that renders the one derived
  // scope (#2071), and the caller that passes `meshes` so `deriveScope` can
  // resolve the Mesh name. These tests seed the Mesh list and the node store
  // directly; the indicator is a pure reader of both.
  describe('scope indicator (#2074)', () => {
    const MESH_A: Mesh = {
      id: 1, name: 'demo-1', path: '/repo/1', branch: 'main', position: 0,
      color: null, layout: 'grid', use_worktree: false, created_at: '2026-01-01',
      github_owner: null, github_repo: null, github_last_synced: null, pre_spawn_pool_size: 0,
    };
    const MESH_B: Mesh = { ...MESH_A, id: 2, name: 'demo-2', path: '/repo/2', position: 1 };
    const NODE: AgentNode = {
      id: 1, mesh_id: 1, name: 'agent-a', path: '/repo/1', branch: 'main',
      env: 'wsl', provider: 'claude', status: 'running', cli_session_id: null,
      use_worktree: false, source_issue: null, worktree_name: null,
      created_at: '2026-01-01', position: 0, scratchpad: '', sandbox: false,
      source_pr: null, head_repo_owner: null, head_repo_clone_url: null,
      source_pr_pinned_sha: null, is_pinned: true, archived: false,
    };
    const NODE_OTHER_MESH: AgentNode = { ...NODE, id: 2, mesh_id: 2, name: 'agent-b', is_pinned: false };
    const NODE_SAME_MESH: AgentNode = { ...NODE, id: 3, mesh_id: 1, name: 'agent-c', position: 1, is_pinned: false };

    beforeEach(() => {
      // meshStore FIRST: the uiStore mesh→mode subscription fires
      // synchronously and would clobber the viewMode set after it.
      useMeshStore.setState({
        meshes: [MESH_A, MESH_B],
        meshesById: new Map([[1, MESH_A], [2, MESH_B]]),
        selectedMeshId: null,
        loading: false,
        error: null,
      });
      useUIStore.setState({
        viewMode: 'all',
        lastNonSingleMode: 'all',
        gridSearchQuery: '',
        gridProviderFilter: null,
        gridStatusFilter: null,
        openScopePickerRequest: 0,
      });
      seedAgentNodes([NODE, NODE_OTHER_MESH, NODE_SAME_MESH]);
    });

    it('mounts in the left cell between the switcher and the Filtered search bar', async () => {
      act(() => {
        useUIStore.setState({ viewMode: 'filtered', lastNonSingleMode: 'filtered' });
      });
      await renderTitleBar();
      const cell = screen.getByTestId('scope-indicator').closest('[data-tauri-drag-region]')!;
      // Same toolbar cluster as the switcher, and ahead of the Filtered
      // view's own search bar — one left cluster, one degradation curve.
      expect(cell.contains(screen.getByRole('group', { name: /view mode/i }))).toBe(true);
      const children = Array.from(cell.children);
      const switcherIndex = children.indexOf(screen.getByRole('group', { name: /view mode/i }));
      const indicatorIndex = children.indexOf(screen.getByTestId('scope-indicator').parentElement!);
      const controlsIndex = children.indexOf(screen.getByTestId('grid-controls'));
      expect(switcherIndex).toBeGreaterThanOrEqual(0);
      expect(indicatorIndex).toBeGreaterThan(switcherIndex);
      expect(controlsIndex).toBeGreaterThan(indicatorIndex);
    });

    it('names the derived Mesh when the scope is Mesh-scoped, with a glyph beside the text', async () => {
      act(() => {
        useMeshStore.getState().selectMesh(1);
      });
      await renderTitleBar();
      const trigger = screen.getByTestId('scope-indicator');
      expect(trigger.getAttribute('aria-label')).toBe('demo-1');
      expect(trigger.textContent).toBe('demo-1');
      // Colour is never the only signal: the glyph carries the shape and is
      // hidden from assistive tech, which reads the text label instead.
      expect(trigger.querySelector('svg')?.getAttribute('aria-hidden')).toBe('true');
      // The Mesh-scoped label is the Mesh name and nothing else — the other
      // Mesh must not leak into it.
      expect(trigger.textContent).not.toContain('demo-2');
    });

    it('names the Mesh id when the scoped Mesh is not in the loaded list', async () => {
      // `deriveScope` reports a null name for a Mesh it has never seen
      // (deleted mid-session, or still loading); the indicator shows the id
      // rather than inventing a label.
      act(() => {
        useMeshStore.setState({ meshes: [MESH_B], meshesById: new Map([[2, MESH_B]]) });
        useMeshStore.getState().selectMesh(1);
      });
      await renderTitleBar();
      expect(screen.getByTestId('scope-indicator').getAttribute('aria-label')).toBe('Mesh #1');
    });

    it.each([
      ['all', null, '', 'All meshes · 3 nodes'],
      ['pinned', 1, '', 'Pinned across meshes · 1 node'],
      ['filtered', 1, 'agent-c', 'Filtered across meshes · 1 of 3 nodes'],
    ] as [ViewMode, number | null, string, string][])(
      'renders the honest cross-Mesh label and count in the %s view (%#)',
      async (viewMode, selectedMeshId, query, label) => {
        act(() => {
          if (selectedMeshId !== null) useMeshStore.setState({ selectedMeshId });
          useUIStore.setState({ viewMode, lastNonSingleMode: viewMode, gridSearchQuery: query });
        });
        await renderTitleBar();
        const trigger = screen.getByTestId('scope-indicator');
        // Visible text and accessible name are the same string, so the name
        // survives the label collapse and satisfies WCAG 2.5.3 (Label in Name).
        expect(trigger.getAttribute('aria-label')).toBe(label);
        expect(trigger.textContent).toBe(label);
        // A cross-Mesh mode must never name a Mesh — pinned and filtered
        // legitimately keep a sidebar selection, and that Mesh is NOT the
        // scope the grid is showing.
        expect(trigger.textContent).not.toContain('demo-1');
      },
    );

    it('names the narrowed count against the scope total in Filtered, so an empty result is distinguishable', async () => {
      act(() => {
        useUIStore.setState({ viewMode: 'filtered', lastNonSingleMode: 'filtered', gridSearchQuery: 'no-such-node' });
      });
      await renderTitleBar();
      // 0 of 3 — "the filters excluded everything", not "the app is broken".
      expect(screen.getByTestId('scope-indicator').getAttribute('aria-label')).toBe('Filtered across meshes · 0 of 3 nodes');
    });

    it('says so honestly in Mesh Grid with no Mesh selected', async () => {
      act(() => {
        useUIStore.setState({ viewMode: 'mesh', lastNonSingleMode: 'mesh' });
      });
      await renderTitleBar();
      expect(screen.getByTestId('scope-indicator').getAttribute('aria-label')).toBe('No mesh selected');
    });

    it('keeps the accessible name after the visible label collapses, truncating the name first', async () => {
      act(() => {
        useMeshStore.getState().selectMesh(1);
      });
      await renderTitleBar();
      const label = screen.getByTestId('scope-indicator').querySelector('span');
      const className = label?.className ?? '';
      // Truncate BEFORE the collapse: a bounded, ellipsised label at every
      // width, and the same 1400px hide tier the switcher segments and the
      // utility pills already use.
      expect(className).toContain('truncate');
      expect(className).toContain('max-w-[10rem]');
      expect(className).toContain('max-[1399px]:hidden');
      // The accessible name is the label string, so it survives the collapse.
      expect(screen.getByTestId('scope-indicator').getAttribute('aria-label')).toBe('demo-1');
    });

    it('is borderless and reuses the switcher segment vocabulary', async () => {
      await renderTitleBar();
      const crossMesh = screen.getByTestId('scope-indicator');
      expect(crossMesh.className).not.toContain('border');
      expect(crossMesh.className).toContain('hover:bg-bg-card');
      expect(crossMesh.className).toContain('text-text-secondary');
      expect(crossMesh.className).toContain('rounded-md');

      // A Mesh-scoped indicator takes the active accent — the same class pair
      // the switcher segment uses for its selected mode.
      act(() => {
        useMeshStore.getState().selectMesh(1);
      });
      const meshScoped = screen.getByTestId('scope-indicator');
      expect(meshScoped.className).toContain('bg-bg-card');
      expect(meshScoped.className).toContain('text-accent-cyan');
    });

    describe('Mesh picker', () => {
      /** rAF made synchronous so the focus return is observable, matching
          the tools-overflow disclosure's convention. */
      function mockRaf() {
        return vi.spyOn(window, 'requestAnimationFrame').mockImplementation((cb: FrameRequestCallback) => {
          cb(0);
          return 1;
        });
      }

      it('activation opens the picker and exposes its expanded state and target', async () => {
        await renderTitleBar();
        const trigger = screen.getByTestId('scope-indicator');
        expect(trigger.getAttribute('aria-expanded')).toBe('false');
        expect(trigger.getAttribute('aria-haspopup')).toBe('dialog');
        expect(trigger.getAttribute('aria-controls')).toBeNull();

        fireEvent.click(trigger);
        const picker = screen.getByRole('dialog', { name: 'Select a Mesh' });
        expect(trigger.getAttribute('aria-expanded')).toBe('true');
        expect(trigger.getAttribute('aria-controls')).toBe(picker.id);
        expect(within(picker).getByRole('button', { name: 'demo-1' })).toBeTruthy();
        expect(within(picker).getByRole('button', { name: 'demo-2' })).toBeTruthy();
        // Nothing is selected in the All Nodes view, so no row claims to be
        // the current scope.
        expect(within(picker).getByRole('button', { name: 'demo-1' }).getAttribute('aria-current')).toBeNull();
      });

      // #2076 — the Mesh Grid segment cannot reach into this component, so
      // it bumps `uiStore.openScopePickerRequest` and the indicator reacts
      // in a layout effect: the same request-counter channel
      // `focusGridSearchRequest` already uses for `GridControls`.
      it('opens on an open request, and opening never chooses a Mesh', async () => {
        await renderTitleBar();
        const trigger = screen.getByTestId('scope-indicator');
        expect(trigger.getAttribute('aria-expanded')).toBe('false');

        act(() => {
          useUIStore.getState().requestOpenScopePicker();
        });

        // Asking is not choosing: the panel is open with both Meshes listed
        // and the selection untouched, so the canvas keeps its honest
        // "no Mesh selected" state behind the panel.
        expect(screen.getByRole('dialog', { name: 'Select a Mesh' })).toBeTruthy();
        expect(useMeshStore.getState().selectedMeshId).toBeNull();
      });

      it('re-opens on the next request after a dismissal', async () => {
        // No idempotency guard on the counter: a second Mesh Grid press must
        // bump again, or a user who dismissed the panel could never get it
        // back without a mode change.
        await renderTitleBar();
        act(() => {
          useUIStore.getState().requestOpenScopePicker();
        });
        expect(screen.queryByRole('dialog', { name: 'Select a Mesh' })).toBeTruthy();
        fireEvent.keyDown(document, { key: 'Escape' });
        expect(screen.queryByRole('dialog', { name: 'Select a Mesh' })).toBeNull();

        act(() => {
          useUIStore.getState().requestOpenScopePicker();
        });

        expect(screen.queryByRole('dialog', { name: 'Select a Mesh' })).toBeTruthy();
        expect(useUIStore.getState().openScopePickerRequest).toBe(2);
      });

      it('stays closed on mount — the request counter starts at zero', async () => {
        await renderTitleBar();
        expect(screen.queryByRole('dialog', { name: 'Select a Mesh' })).toBeNull();
      });

      it('marks the selected Mesh as the current scope in the picker', async () => {
        act(() => {
          useMeshStore.getState().selectMesh(1);
        });
        await renderTitleBar();
        fireEvent.click(screen.getByTestId('scope-indicator'));
        const picker = screen.getByRole('dialog', { name: 'Select a Mesh' });
        expect(within(picker).getByRole('button', { name: 'demo-1' }).getAttribute('aria-current')).toBe('true');
        expect(within(picker).getByRole('button', { name: 'demo-2' }).getAttribute('aria-current')).toBeNull();
      });

      it('Escape closes the picker without changing scope and returns focus to the trigger', async () => {
        const raf = mockRaf();
        try {
          act(() => {
            useMeshStore.getState().selectMesh(1);
          });
          await renderTitleBar();
          const trigger = screen.getByTestId('scope-indicator');
          fireEvent.click(trigger);
          expect(screen.queryByRole('dialog', { name: 'Select a Mesh' })).toBeTruthy();

          fireEvent.keyDown(document, { key: 'Escape' });
          expect(screen.queryByRole('dialog', { name: 'Select a Mesh' })).toBeNull();
          expect(trigger.getAttribute('aria-expanded')).toBe('false');
          expect(document.activeElement).toBe(trigger);
          // Escape is dismissal, not a decision: scope is untouched.
          expect(useMeshStore.getState().selectedMeshId).toBe(1);
          expect(useUIStore.getState().viewMode).toBe('mesh');
        } finally {
          raf.mockRestore();
        }
      });

      it('closes on an outside mousedown without changing scope', async () => {
        await renderTitleBar();
        fireEvent.click(screen.getByTestId('scope-indicator'));
        fireEvent.mouseDown(document.body);
        expect(screen.queryByRole('dialog', { name: 'Select a Mesh' })).toBeNull();
        expect(useMeshStore.getState().selectedMeshId).toBeNull();
      });

      it('choosing a Mesh moves the canvas and the Probe destinations together', async () => {
        // #2073 removed the Probe Context Pins, so the Probe destinations
        // follow `selectedMeshId` — one write moves both, and they cannot
        // disagree afterwards.
        const raf = mockRaf();
        try {
          await renderTitleBar();
          const probe = renderHook(() => useProbeContext());
          expect(probe.result.current.subjectLabel).toBe('Mesh');
          const trigger = screen.getByTestId('scope-indicator');
          fireEvent.click(trigger);
          fireEvent.click(screen.getByRole('button', { name: 'demo-2' }));

          expect(useMeshStore.getState().selectedMeshId).toBe(2);
          expect(useUIStore.getState().viewMode).toBe('mesh');
          expect(probe.result.current.subjectLabel).toBe('Mesh: demo-2');
          expect(probe.result.current.mode).toBe('following');
          // The picker closes, focus returns to the trigger, and the control
          // now names the Mesh it moved to.
          expect(screen.queryByRole('dialog', { name: 'Select a Mesh' })).toBeNull();
          expect(document.activeElement).toBe(trigger);
          expect(trigger.getAttribute('aria-label')).toBe('demo-2');
        } finally {
          raf.mockRestore();
        }
      });

      it('reports an empty Mesh list rather than an empty picker', async () => {
        act(() => {
          useMeshStore.setState({ meshes: [], meshesById: new Map() });
        });
        await renderTitleBar();
        fireEvent.click(screen.getByTestId('scope-indicator'));
        const picker = screen.getByRole('dialog', { name: 'Select a Mesh' });
        expect(picker.textContent).toContain('No meshes yet');
        expect(within(picker).queryAllByRole('button')).toHaveLength(0);
      });
    });
  });

  describe('zoom slider (terminal text size)', () => {
    it('opens a text-size popover from the Zoom pill immediately left of Usage', async () => {
      await renderTitleBar();
      const zoom = screen.getByRole('button', { name: 'Zoom terminal text size' });
      expect(zoom.getAttribute('aria-expanded')).toBe('false');
      expect(screen.queryByTestId('zoom-panel')).toBeNull();

      fireEvent.click(zoom);
      expect(screen.getByTestId('zoom-panel')).toBeTruthy();
      expect(zoom.getAttribute('aria-expanded')).toBe('true');

      // Ordering contract: the zoom trigger precedes the Usage pill in the
      // right-hand utility cluster.
      const cluster = zoom.parentElement!.parentElement!;
      const labels = Array.from(cluster.querySelectorAll('button')).map(
        (button) => button.getAttribute('aria-label'),
      );
      expect(labels.indexOf('Zoom terminal text size')).toBeLessThan(
        labels.indexOf('Open Usage'),
      );
    });

    it('shows the current size and drives it from the slider', async () => {
      await renderTitleBar();
      fireEvent.click(screen.getByRole('button', { name: 'Zoom terminal text size' }));

      const slider = screen.getByTestId('zoom-slider') as HTMLInputElement;
      expect(Number(slider.value)).toBe(TERMINAL_FONT_SIZE_DEFAULT);
      expect(screen.getByTestId('zoom-value').textContent).toBe(`${TERMINAL_FONT_SIZE_DEFAULT}px`);

      fireEvent.change(slider, { target: { value: '16' } });
      expect(terminalFontSize()).toBe(16);
      expect(screen.getByTestId('zoom-value').textContent).toBe('16px');
    });

    it('reflects zoom changes made outside the control (keyboard / wheel)', async () => {
      await renderTitleBar();
      fireEvent.click(screen.getByRole('button', { name: 'Zoom terminal text size' }));

      act(() => {
        setTerminalFontSize(14);
      });

      expect((screen.getByTestId('zoom-slider') as HTMLInputElement).value).toBe('14');
      expect(screen.getByTestId('zoom-value').textContent).toBe('14px');
    });

    it('resets to the default size', async () => {
      await renderTitleBar();
      act(() => {
        setTerminalFontSize(18);
      });
      fireEvent.click(screen.getByRole('button', { name: 'Zoom terminal text size' }));

      fireEvent.click(screen.getByTestId('zoom-reset'));

      expect(terminalFontSize()).toBe(TERMINAL_FONT_SIZE_DEFAULT);
      expect(screen.getByTestId('zoom-value').textContent).toBe(`${TERMINAL_FONT_SIZE_DEFAULT}px`);
    });

    it('closes on Escape and on outside mousedown', async () => {
      await renderTitleBar();
      const zoom = screen.getByRole('button', { name: 'Zoom terminal text size' });

      fireEvent.click(zoom);
      fireEvent.keyDown(document, { key: 'Escape' });
      expect(screen.queryByTestId('zoom-panel')).toBeNull();

      fireEvent.click(zoom);
      expect(screen.getByTestId('zoom-panel')).toBeTruthy();
      fireEvent.mouseDown(document.body);
      expect(screen.queryByTestId('zoom-panel')).toBeNull();
    });

    it('keeps the trigger, panel and slider free of drag regions', async () => {
      await renderTitleBar();
      const zoom = screen.getByRole('button', { name: 'Zoom terminal text size' });
      expect(zoom.hasAttribute('data-tauri-drag-region')).toBe(false);
      expect(zoom.querySelector('[data-tauri-drag-region]')).toBeNull();

      fireEvent.click(zoom);
      expect(screen.getByTestId('zoom-panel').hasAttribute('data-tauri-drag-region')).toBe(false);
      expect(screen.getByTestId('zoom-slider').hasAttribute('data-tauri-drag-region')).toBe(false);
    });

    it('returns focus to the trigger when dismissed with Escape', async () => {
      // The focus restore runs in requestAnimationFrame (mirroring
      // BuildRunDropdown); run it synchronously in jsdom.
      const raf = vi.spyOn(window, 'requestAnimationFrame').mockImplementation((cb: FrameRequestCallback) => {
        cb(0);
        return 1;
      });
      try {
        await renderTitleBar();
        const zoom = screen.getByRole('button', { name: 'Zoom terminal text size' });
        zoom.focus();
        fireEvent.click(zoom);
        expect(screen.getByTestId('zoom-panel')).toBeTruthy();

        fireEvent.keyDown(document, { key: 'Escape' });

        expect(screen.queryByTestId('zoom-panel')).toBeNull();
        expect(document.activeElement).toBe(zoom);
      } finally {
        raf.mockRestore();
      }
    });

    it('wires the disclosure contract: aria-controls, aria-valuetext, hidden glyphs', async () => {
      await renderTitleBar();
      const zoom = screen.getByRole('button', { name: 'Zoom terminal text size' });
      // Closed: no aria-controls pointing at an absent panel.
      expect(zoom.getAttribute('aria-controls')).toBeNull();

      fireEvent.click(zoom);
      const panel = screen.getByTestId('zoom-panel');
      expect(zoom.getAttribute('aria-controls')).toBe(panel.id);
      expect(panel.id).not.toBe('');

      const slider = screen.getByTestId('zoom-slider');
      expect(slider.getAttribute('aria-valuetext')).toBe(`${TERMINAL_FONT_SIZE_DEFAULT}px`);

      // The decorative A/A swatches must not be announced.
      const glyphs = Array.from(panel.querySelectorAll('span')).filter(
        (span) => span.textContent === 'A',
      );
      expect(glyphs).toHaveLength(2);
      for (const glyph of glyphs) expect(glyph.getAttribute('aria-hidden')).toBe('true');
    });

    it('disables Reset at the default size and re-enables it after a change', async () => {
      await renderTitleBar();
      fireEvent.click(screen.getByRole('button', { name: 'Zoom terminal text size' }));

      const reset = screen.getByTestId('zoom-reset') as HTMLButtonElement;
      expect(reset.disabled).toBe(true);
      expect(reset.className).toContain('disabled:text-text-muted');

      fireEvent.change(screen.getByTestId('zoom-slider'), { target: { value: '15' } });
      expect(reset.disabled).toBe(false);

      fireEvent.click(reset);
      expect(terminalFontSize()).toBe(TERMINAL_FONT_SIZE_DEFAULT);
      expect(reset.disabled).toBe(true);
    });
  });
});
