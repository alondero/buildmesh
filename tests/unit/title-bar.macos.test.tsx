/**
 * TitleBar on macOS — pins the macOS-only branch of the bespoke window
 * chrome. The default `tests/unit/title-bar.test.tsx` covers the
 * Windows/Linux branch (where the right-side square controls stay and
 * the macOS traffic lights are absent); this file forces `isMac = true`
 * via a `vi.mock` factory hoisted over the platform module, then asserts
 * the platform-conditional rendering the knowledge primer calls out as
 * "macOS conventions": the three traffic lights sit on the LEFT in
 * close/minimize/maximize order, the right-side square controls are
 * suppressed (the lights replace them), the wordmark stays right after
 * the lights so it remains the visible "this is the app" affordance,
 * and the drag-region placement stays clean (lights are NOT drag regions,
 * wordmark + bar + spacer still are). The same single-writer isMaximized
 * invariant applies — the maximize traffic light's aria-label still
 * tracks the resize-derived state.
 *
 * The `vi.mock` on `lib/platform` is hoisted by Vitest, so the
 * `TitleBar.tsx` import of `isMac` resolves to the factory's `true`
 * before the component module is ever evaluated — a plain
 * `navigator.platform` patch wouldn't work because `isMac` is captured
 * at module-load time. The setup file's beforeEach note (issue #354
 * follow-up) is the same trap that this file deliberately avoids.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { act, fireEvent, render, screen } from '@testing-library/react';

vi.mock('../../src/lib/platform', () => ({
  isMac: true,
  isWindows: false,
}));

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

describe('TitleBar on macOS', () => {
  describe('spec shape', () => {
    it('renders three traffic lights on the left in close/minimize/maximize order and suppresses the right-side square controls', async () => {
      const { container } = await renderTitleBar();
      // The macOS traffic lights wrapper is the very first child of the header.
      const header = container.querySelector('header')!;
      const lightsWrapper = header.querySelector('[data-testid="macos-traffic-lights"]')!;
      expect(lightsWrapper).toBeTruthy();
      // The three lights in Apple's reading order.
      const lights = lightsWrapper.querySelectorAll('button');
      expect(lights.length).toBe(3);
      expect(lights[0].getAttribute('data-testid')).toBe('macos-traffic-close');
      expect(lights[1].getAttribute('data-testid')).toBe('macos-traffic-minimize');
      expect(lights[2].getAttribute('data-testid')).toBe('macos-traffic-maximize');
      // The right-side SQUARE controls are gone on macOS — the unique
      // `w-11` Tailwind class is the WindowControlButton affordance;
      // the traffic lights themselves are valid `button`s with the same
      // `Minimize window` / `Maximize window` / `Close window`
      // accessible names (Apple uses these for VoiceOver too), so the
      // right test for the suppression is "no w-11 squares", not
      // "no minimize button at all".
      expect(container.querySelectorAll('button.w-11').length).toBe(0);
      // The rest of the chrome is still present.
      expect(screen.getByAltText('Buildmesh')).toBeTruthy();
      expect(screen.getByRole('group', { name: /view mode/i })).toBeTruthy();
      expect(screen.getByRole('button', { name: 'Open settings' })).toBeTruthy();
      expect(screen.getByRole('button', { name: 'Open remote access' })).toBeTruthy();
    });

    it('wordmark still carries data-tauri-drag-region; the traffic lights themselves do NOT', async () => {
      await renderTitleBar();
      expect(screen.getByAltText('Buildmesh').hasAttribute('data-tauri-drag-region')).toBe(true);
      for (const kind of ['close', 'minimize', 'maximize']) {
        const button = screen.getByTestId(`macos-traffic-${kind}`);
        expect(button.hasAttribute('data-tauri-drag-region')).toBe(false);
        expect(button.querySelector('[data-tauri-drag-region]')).toBeNull();
      }
    });

    it('paints the traffic lights with the macOS system palette values', async () => {
      const { container } = await renderTitleBar();
      const close = screen.getByTestId('macos-traffic-close');
      const minimize = screen.getByTestId('macos-traffic-minimize');
      const maximize = screen.getByTestId('macos-traffic-maximize');
      // Pinning the exact macOS fill colours (the system palette, not
      // arbitrary reds/greens) is the bit that keeps the strip reading as
      // a real macOS title bar rather than a generic round-button set.
      expect(close.className).toContain('bg-[#FF5F57]');
      expect(minimize.className).toContain('bg-[#FEBC2E]');
      expect(maximize.className).toContain('bg-[#28C840]');
      // Sanity check: there are exactly three traffic-light buttons (the
      // selector has to scope to `button` — the wrapper div also carries
      // a `macos-traffic-*` testid (`macos-traffic-lights`), so the
      // attribute-only selector would over-count to 4).
      expect(container.querySelectorAll('button[data-testid^="macos-traffic-"]').length).toBe(3);
    });
  });

  describe('window controls', () => {
    it('wires the three macOS traffic lights to close / minimize / toggleMaximize', async () => {
      await renderTitleBar();
      fireEvent.click(screen.getByTestId('macos-traffic-close'));
      expect(windowApi.close).toHaveBeenCalledTimes(1);
      fireEvent.click(screen.getByTestId('macos-traffic-minimize'));
      expect(windowApi.minimize).toHaveBeenCalledTimes(1);
      fireEvent.click(screen.getByTestId('macos-traffic-maximize'));
      expect(windowApi.toggleMaximize).toHaveBeenCalledTimes(1);
    });

    it('swaps the maximize traffic light to Restore only when isMaximized re-syncs (single-writer)', async () => {
      await renderTitleBar();
      // Initial sync resolved false → Maximize.
      const maximize = screen.getByTestId('macos-traffic-maximize');
      expect(maximize.getAttribute('aria-label')).toBe('Maximize window');
      // Clicking toggles the window but must NOT flip the aria-label
      // itself — the onResized listener owns isMaximized.
      fireEvent.click(maximize);
      expect(windowApi.toggleMaximize).toHaveBeenCalledTimes(1);
      expect(maximize.getAttribute('aria-label')).toBe('Maximize window');
      // The resize arrives; the re-query reports maximized → Restore.
      windowApi.isMaximized.mockResolvedValue(true);
      await act(async () => { resizeHandler!(); });
      expect(screen.getByTestId('macos-traffic-maximize').getAttribute('aria-label')).toBe('Restore window');
      // And back again on restore.
      windowApi.isMaximized.mockResolvedValue(false);
      await act(async () => { resizeHandler!(); });
      expect(screen.getByTestId('macos-traffic-maximize').getAttribute('aria-label')).toBe('Maximize window');
    });
  });
});