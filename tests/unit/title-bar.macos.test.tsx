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
 * A final block pins the lights' emulated `NSWindow` states (ADR-0036): the
 * native 12px geometry, the cluster-wide glyph reveal, the darker pressed
 * fill, and the flat grey an unfocused window takes (restored under the
 * pointer while hovered).
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
  // Focus tracking (ADR-0035) — the macOS branch deliberately does not dim the
  // traffic lights, but the hook still runs, so the mock has to provide it.
  isFocused: vi.fn().mockResolvedValue(true),
  onFocusChanged: vi.fn<
    (cb: (event: { payload: boolean }) => void) => Promise<() => void>
  >(),
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
let focusHandler: ((event: { payload: boolean }) => void) | null = null;

async function renderTitleBar() {
  const utils = render(<TitleBar />);
  await act(async () => {});
  return utils;
}

/** The ClassList of one traffic light, so a token assertion can say "this
    exact class" rather than "some class containing this substring" — the
    latter would let `bg-mac-close` match inside `active:bg-mac-close-pressed`
    and silently pass on the wrong light. */
function lightClasses(kind: 'close' | 'minimize' | 'maximize'): string[] {
  return screen.getByTestId(`macos-traffic-${kind}`).className.split(/\s+/);
}

/** The class list on a traffic light's glyph. */
function lightGlyphClass(kind: 'close' | 'minimize' | 'maximize'): string {
  return screen.getByTestId(`macos-traffic-${kind}`).querySelector('svg')!.getAttribute('class') ?? '';
}

/** The App.css token stem per light. The zoom light is `mac-zoom`, not
    `mac-maximize` — macOS calls the green button Zoom, and the token follows
    the system's vocabulary rather than our button's `kind`. */
const LIGHT_TOKEN = { close: 'close', minimize: 'minimize', maximize: 'zoom' } as const;

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
      // The right-side caption controls are gone on macOS. The marker is
      // the `data-window-control` attribute the three buttons carry — NOT
      // a Tailwind class. The old `w-11` sentinel stopped matching the
      // moment the controls moved to 46px backplates, which silently made
      // this assertion pass on a render that still had them (ADR-0035).
      // The traffic lights themselves are valid `button`s with the same
      // `Minimize window` / `Maximize window` / `Close window` accessible
      // names (Apple uses these for VoiceOver too), so the right test for
      // the suppression is "no marked caption controls", not "no minimize
      // button at all".
      expect(container.querySelectorAll('[data-window-control]').length).toBe(0);
      // The rest of the chrome is still present.
      expect(screen.getByRole('img', { name: 'Buildmesh' })).toBeTruthy();
      expect(screen.getByRole('group', { name: /view mode/i })).toBeTruthy();
      // Issue #1375 — the navigation cluster renders on macOS too, with the
      // macOS chord from the shortcut catalog (`⌘+K`, the open-omnibar row)
      // as the palette hint.
      const search = screen.getByRole('button', { name: 'Search or open' });
      expect(search).toBeTruthy();
      expect(search.textContent).toContain('⌘+K');
      expect(screen.getByRole('button', { name: 'Open Usage' })).toBeTruthy();
      expect(screen.getByRole('button', { name: 'Open settings' })).toBeTruthy();
      expect(screen.getByRole('button', { name: 'Open mobile remote access' })).toBeTruthy();
    });

    it('uses the macOS chord in the zoom trigger tooltip (⌘, not Ctrl)', async () => {
      await renderTitleBar();
      const zoom = screen.getByRole('button', { name: 'Zoom terminal text size' });
      const title = zoom.getAttribute('title') ?? '';
      // Sourced from the shortcut catalog's macKey, so the tooltip can't
      // drift from the real binding on this platform.
      expect(title).toContain('⌘+=');
      expect(title).toContain('⌘+-');
      expect(title).not.toContain('Ctrl');
    });

    it('wordmark still carries data-tauri-drag-region; the traffic lights themselves do NOT', async () => {
      await renderTitleBar();
      expect(screen.getByRole('img', { name: 'Buildmesh' }).hasAttribute('data-tauri-drag-region')).toBe(true);
      for (const kind of ['close', 'minimize', 'maximize']) {
        const button = screen.getByTestId(`macos-traffic-${kind}`);
        expect(button.hasAttribute('data-tauri-drag-region')).toBe(false);
        expect(button.querySelector('[data-tauri-drag-region]')).toBeNull();
      }
    });

    it('paints the traffic lights with the macOS system palette tokens', async () => {
      const { container } = await renderTitleBar();
      // The system fills (ADR-0036) are pinned by token name, not by hex: the
      // values live in App.css next to the pressed and inactive variants, so a
      // changed fill can't leave the states behind. A close button in any
      // other red is most of what makes a hand-drawn light read as generic.
      expect(lightClasses('close')).toContain('bg-mac-close');
      expect(lightClasses('minimize')).toContain('bg-mac-minimize');
      expect(lightClasses('maximize')).toContain('bg-mac-zoom');
      // Sanity check: there are exactly three traffic-light buttons (the
      // selector has to scope to `button` — the wrapper div also carries
      // a `macos-traffic-*` testid (`macos-traffic-lights`), so the
      // attribute-only selector would over-count to 4).
      expect(container.querySelectorAll('button[data-testid^="macos-traffic-"]').length).toBe(3);
    });

    it('draws the native 12px circle, not the 13px-root rem step', async () => {
      await renderTitleBar();
      for (const kind of ['close', 'minimize', 'maximize'] as const) {
        const classes = lightClasses(kind);
        // `w-3` / `h-3` compile to 0.75rem — 9.75px at this app's 13px root,
        // visibly smaller than every other macOS window's lights — which is
        // why the box is a pixel literal, exactly as the caption glyphs are.
        expect(classes).toContain('h-[12px]');
        expect(classes).toContain('w-[12px]');
        expect(classes).not.toContain('h-3');
        expect(classes).not.toContain('w-3');
      }
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

  describe('traffic-light states (ADR-0036)', () => {
    it('reveals all three glyphs from the cluster, never one at a time', async () => {
      const { container } = await renderTitleBar();
      // `group` has to sit on the strip: macOS shows the × / − / + together
      // the moment the pointer enters it, so a per-button group would reveal
      // exactly one symbol and read as a web widget rather than the platform.
      const cluster = container.querySelector('[data-testid="macos-traffic-lights"]')!;
      expect(cluster.className.split(/\s+/)).toContain('group');
      for (const kind of ['close', 'minimize', 'maximize'] as const) {
        expect(lightClasses(kind)).not.toContain('group');
        const glyph = lightGlyphClass(kind);
        expect(glyph).toContain('group-hover:opacity-100');
        // Hidden at rest, and tinted toward its own fill rather than a
        // generic near-black.
        expect(glyph).toContain('opacity-0');
        expect(glyph).toContain(`text-mac-${LIGHT_TOKEN[kind]}-glyph`);
      }
    });

    it('darkens the fill while a light is pressed, with no hover brightening', async () => {
      await renderTitleBar();
      for (const kind of ['close', 'minimize', 'maximize'] as const) {
        // AppKit's mouse-down fill: the lights darken while held.
        expect(lightClasses(kind)).toContain(`active:bg-mac-${LIGHT_TOKEN[kind]}-pressed`);
        // The old `hover:brightness-[0.92]` was never a macOS behaviour — the
        // glyph fading in is the hover cue — and a lingering brightness filter
        // would tint the glyph with it.
        expect(lightClasses(kind)).not.toContain('hover:brightness-[0.92]');
      }
    });

    it('greys all three lights while the window is inactive and restores colour on cluster hover', async () => {
      await renderTitleBar();
      // Focused on mount → each light paints its own colour.
      expect(lightClasses('close')).toContain('bg-mac-close');
      expect(lightClasses('close')).not.toContain('bg-mac-traffic-inactive');

      // An OS-driven focus change (alt-tab, a click in another app) greys the
      // whole strip — the at-a-glance cue for which window is taking keys.
      await act(async () => { focusHandler!({ payload: false }); });

      for (const kind of ['close', 'minimize', 'maximize'] as const) {
        const classes = lightClasses(kind);
        expect(classes).toContain('bg-mac-traffic-inactive');
        // The colour is not gone, only deferred: hovering the strip brings
        // back the light under the pointer, which is how you know what you are
        // about to click before the window is even focused.
        expect(classes).not.toContain(`bg-mac-${LIGHT_TOKEN[kind]}`);
        expect(classes).toContain(`group-hover:bg-mac-${LIGHT_TOKEN[kind]}`);
      }

      await act(async () => { focusHandler!({ payload: true }); });
      expect(lightClasses('close')).toContain('bg-mac-close');
      expect(lightClasses('close')).not.toContain('bg-mac-traffic-inactive');
    });

    it('greys the lights when the window is already unfocused at mount', async () => {
      // Launching behind another window never delivers a focus-change event to
      // this webview, so the initial `isFocused()` query has to carry it.
      windowApi.isFocused.mockResolvedValue(false);
      await renderTitleBar();
      for (const kind of ['close', 'minimize', 'maximize'] as const) {
        expect(lightClasses(kind)).toContain('bg-mac-traffic-inactive');
      }
    });

    it('drops the HTML tooltip that native traffic lights do not have', async () => {
      await renderTitleBar();
      for (const kind of ['close', 'minimize', 'maximize'] as const) {
        const light = screen.getByTestId(`macos-traffic-${kind}`);
        // `aria-label` (pinned above) is the only accessible name now, exactly
        // as the caption buttons settled in ADR-0035.
        expect(light.hasAttribute('title')).toBe(false);
      }
    });

    it('draws the glyphs in a 12-unit box at their native proportions', async () => {
      await renderTitleBar();
      for (const kind of ['close', 'minimize', 'maximize'] as const) {
        const glyph = screen.getByTestId(`macos-traffic-${kind}`).querySelector('svg')!;
        // A 12-unit viewBox rendered at 12px, so the coordinates are real
        // pixels (each symbol spans 3→9) instead of the old 0 0 8 8 box scaled
        // down to a 6.5px glyph.
        expect(glyph.getAttribute('viewBox')).toBe('0 0 12 12');
        expect(glyph.getAttribute('stroke')).toBe('currentColor');
        expect(glyph.getAttribute('stroke-width')).toBe('1.5');
        expect(glyph.getAttribute('class')).toContain('h-[12px]');
      }
    });
  });
});