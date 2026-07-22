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
});

describe('TitleBar (bespoke window chrome)', () => {
  describe('spec shape', () => {
    it('renders wordmark, view-mode toolbar, settings/remote icons and the three window controls', async () => {
      await renderTitleBar();
      expect(screen.getByAltText('Buildmesh')).toBeTruthy();
      expect(screen.getByRole('group', { name: /view mode/i })).toBeTruthy();
      expect(screen.getByRole('button', { name: 'Open settings' })).toBeTruthy();
      expect(screen.getByRole('button', { name: 'Open remote access' })).toBeTruthy();
      expect(screen.getByRole('button', { name: 'Minimize window' })).toBeTruthy();
      expect(screen.getByRole('button', { name: 'Maximize window' })).toBeTruthy();
      expect(screen.getByRole('button', { name: 'Close window' })).toBeTruthy();
    });

    it('marks the header, wordmark and spacer as drag regions but never the interactive clusters', async () => {
      const { container } = await renderTitleBar();
      const header = container.querySelector('header')!;
      expect(header.hasAttribute('data-tauri-drag-region')).toBe(true);
      expect(screen.getByAltText('Buildmesh').hasAttribute('data-tauri-drag-region')).toBe(true);
      const spacer = header.querySelector('div.flex-1')!;
      expect(spacer.hasAttribute('data-tauri-drag-region')).toBe(true);
      // Buttons (and their glyphs) must stay the mousedown target — if any
      // carried the attribute, Tauri's drag script would eat the click.
      for (const label of ['Open settings', 'Open remote access', 'Minimize window', 'Maximize window', 'Close window']) {
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
      fireEvent.click(screen.getByRole('button', { name: 'Open remote access' }));
      expect(screen.getByRole('dialog', { name: 'Remote access' })).toBeTruthy();
      fireEvent.click(screen.getByRole('button', { name: 'stub-close-remote' }));
      expect(screen.queryByRole('dialog')).toBeNull();
    });
  });
});
