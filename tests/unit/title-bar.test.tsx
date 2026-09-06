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
import { act, fireEvent, render, screen } from '@testing-library/react';

const windowApi = vi.hoisted(() => ({
  minimize: vi.fn(),
  toggleMaximize: vi.fn(),
  close: vi.fn(),
  isMaximized: vi.fn().mockResolvedValue(false),
  onResized: vi.fn<(cb: () => void) => Promise<() => void>>(),
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => windowApi,
}));

vi.mock('../../src/components/AppSettings/AppSettingsModal', () => ({
  AppSettingsModal: ({ onClose }: { onClose: () => void }) => (
    <div role="dialog" aria-label="App settings">
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
import { useUIStore } from '../../src/stores/uiStore';

let resizeHandler: (() => void) | null = null;

/** Render and flush the initial isMaximized sync (a promise that resolves
    after mount) so tests don't trip act() warnings on the settle. */
async function renderTitleBar() {
  const utils = render(<TitleBar />);
  await act(async () => {});
  return utils;
}

beforeEach(() => {
  resizeHandler = null;
  windowApi.isMaximized.mockResolvedValue(false);
  windowApi.onResized.mockImplementation((cb: () => void) => {
    resizeHandler = cb;
    return Promise.resolve(() => {});
  });
  useUIStore.setState({
    omnibarOpen: false,
    omnibarMode: 'files',
    probeOpen: false,
    probeTab: 'files',
    activeDiffFile: null,
    probeContextPins: {},
  });
});

describe('TitleBar (bespoke window chrome)', () => {
  describe('spec shape', () => {
    it('renders wordmark, view-mode toolbar, navigation cluster, settings/remote icons and the three window controls', async () => {
      await renderTitleBar();
      expect(screen.getByAltText('Buildmesh')).toBeTruthy();
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
      expect(screen.getByAltText('Buildmesh').hasAttribute('data-tauri-drag-region')).toBe(true);
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

  describe('modal wiring (icons moved from the Sidebar header)', () => {
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
      expect(searchWrapper.className).toContain('min-w-44');
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
});
