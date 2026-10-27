/**
 * The idle animation gate (issue #2599).
 *
 * The renderer Compositor thread produces a frame every vsync while ANY
 * infinite CSS animation runs. On a long-running hub that measured ~16 ms per
 * frame at 4K — the compositor stayed pinned near one core for hours while
 * the JavaScript main thread sat at 3%, because the page never went idle.
 *
 * The gate marks the root element as idle while the app is not the foreground
 * surface, and one rule in `App.css` pauses the decorative infinite
 * animations. Two things have to stay true, and this file pins both:
 *
 *   1. Every decorative infinite animation the desktop app actually uses is
 *      inside that rule. An animation nobody gated keeps the Compositor
 *      producing frames forever, and the regression is invisible until
 *      someone reads a CPU profile again.
 *   2. The rule reaches ONLY the infinite animations. The one-shot entrance
 *      animations (`animate-fade-in`, `animate-scale-in`,
 *      `animate-slide-in-right`) must keep running: a toast that fades in
 *      while the window is in the background would otherwise freeze at zero
 *      opacity and never be seen.
 *
 * The mobile SPA is deliberately out of scope: it is a separate application
 * with its own stylesheet (`src/mobile/styles.css`) and its own lifecycle,
 * and the issue's reproduction scenario is the desktop hub's renderer.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { renderHook, act } from '@testing-library/react';

const windowApi = vi.hoisted(() => ({
  isFocused: vi.fn(),
  onFocusChanged: vi.fn<
    (cb: (event: { payload: boolean }) => void) => Promise<() => void>
  >(),
}));

vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => windowApi }));

import { useIdleAnimationGate } from '../../src/hooks/useIdleAnimationGate';
import { IDLE_ROOT_ATTRIBUTE } from '../../src/lib/idleAnimationGate';

const APP_CSS = readFileSync(resolve(__dirname, '../../src/App.css'), 'utf8');
const APP_TSX = readFileSync(resolve(__dirname, '../../src/App.tsx'), 'utf8');

/** The stylesheet without its comments — comments are prose, not selectors. */
const CSS = APP_CSS.replace(/\/\*[\s\S]*?\*\//g, '');

// ---------------------------------------------------------------------------
// CSS parsing helpers
// ---------------------------------------------------------------------------

interface Rule {
  selectors: string[];
  body: string;
}

/** Every flat rule in a stylesheet, with comments stripped. */
function rules(css: string): Rule[] {
  const cleaned = css.replace(/\/\*[\s\S]*?\*\//g, '');
  const out: Rule[] = [];
  for (const match of cleaned.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const selectors = match[1].split(',').map((s) => s.trim()).filter(Boolean);
    out.push({ selectors, body: match[2] });
  }
  return out;
}

/** The selectors of the idle gate rule — the one keyed off the root marker. */
function gateSelectors(css: string): string[] {
  // Found by scanning rather than by position: a second gate rule earlier in
  // the file must not make this helper read the wrong one.
  const matches = [...css.matchAll(/([^{}]*data-window-idle[^{}]*)\{([^{}]*)\}/g)];
  expect(matches.length, 'App.css must contain exactly one idle gate rule').toBe(1);
  return matches[0][1].split(',').map((s) => s.trim()).filter(Boolean);
}

/** The declaration body of the idle gate rule. */
function gateBody(css: string): string {
  const open = css.indexOf('{', css.indexOf(`[${IDLE_ROOT_ATTRIBUTE}`));
  let depth = 0;
  let end = open;
  for (; end < css.length; end++) {
    if (css[end] === '{') depth++;
    if (css[end] === '}' && --depth === 0) break;
  }
  return css.slice(open + 1, end);
}

/** An infinite animation the gate has to account for. */
interface InfiniteAnimation {
  /** Every class the selector names — a rule may name more than one. */
  classes: string[];
  /** The pseudo-element it targets, when it targets one. */
  pseudo: string | null;
  /** The raw selector, for the failure message. */
  selector: string;
}

/** Every infinite animation a stylesheet declares. */
function infiniteAnimations(css: string): InfiniteAnimation[] {
  const found: InfiniteAnimation[] = [];
  for (const rule of rules(css)) {
    const shorthand = rule.body.match(/(?:^|;|\s)animation\s*:\s*([^;]+)/);
    // CSS keywords are case-insensitive, so `INFINITE` counts too.
    const infinite =
      (shorthand && /\binfinite\b/i.test(shorthand[1])) ||
      /animation-iteration-count\s*:\s*infinite/i.test(rule.body);
    if (!infinite) continue;
    for (const selector of rule.selectors) {
      // Any pseudo-element, not only ::before/::after: `.foo::marker` is just
      // as capable of keeping the Compositor busy as `.foo::after`.
      const pseudo = /::([a-z-]+)\s*$/i.exec(selector)?.[1] ?? null;
      const base = pseudo ? selector.slice(0, selector.indexOf('::')) : selector;
      const classes = [...base.matchAll(/\.([A-Za-z0-9_-]+)/g)].map((match) => match[1]);
      // A selector that names no class is reported too: the gate's class list
      // cannot reach it, so it must not pass in silence.
      found.push({ classes, pseudo, selector });
    }
  }
  return found;
}

/** Tailwind's infinite `animate-*` utilities. `animate-none` is not one. */
const TAILWIND_INFINITE_UTILITIES = ['spin', 'pulse', 'bounce', 'ping'];

/** Every stylesheet the desktop app ships, and the mobile SPA's (exempt). */
function stylesheets(srcRoot: string): { path: string; css: string }[] {
  const found: { path: string; css: string }[] = [];
  const walk = (dir: string) => {
    for (const entry of readdirSync(dir)) {
      const full = join(dir, entry);
      if (statSync(full).isDirectory()) {
        walk(full);
        continue;
      }
      if (!entry.endsWith('.css')) continue;
      found.push({ path: full, css: readFileSync(full, 'utf8') });
    }
  };
  walk(srcRoot);
  return found;
}

/** Every infinite Tailwind animation class the desktop app renders. */
function tailwindInfiniteClasses(srcRoot: string): string[] {
  const tokens = new Set<string>();
  const pattern = new RegExp(
    `(?<![\\w-])(?:[a-z0-9-]+:)*animate-(?:${TAILWIND_INFINITE_UTILITIES.join('|')})(?![\\w-])`,
    'g',
  );
  const walk = (dir: string) => {
    for (const entry of readdirSync(dir)) {
      const full = join(dir, entry);
      if (statSync(full).isDirectory()) {
        // The mobile SPA is a separate app with its own stylesheet.
        if (entry === 'mobile') continue;
        walk(full);
        continue;
      }
      if (!/\.tsx?$/.test(entry)) continue;
      for (const match of readFileSync(full, 'utf8').matchAll(pattern)) tokens.add(match[0]);
    }
  };
  walk(srcRoot);
  return [...tokens];
}

/** Whether one gate selector reaches the element carrying `token`. */
function gateCovers(selectors: string[], token: string): boolean {
  if (token.includes(':')) {
    // A Tailwind variant is a literal class token ("motion-safe:animate-pulse"),
    // so a plain `.animate-pulse` selector does NOT reach it — the gate has to
    // name the variant explicitly.
    return selectors.some((s) => s.includes(`[class~='${token}']`));
  }
  const escaped = token.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  // The trailing guard stops `.animate-pulse` from matching `.animate-pulse-fast`.
  const pattern = new RegExp(`\\.${escaped}(?![\\w-])`);
  return selectors.some((s) => pattern.test(s));
}

/** Whether one gate selector names `className` as a class of its own. */
function selectorNamesClass(selector: string, className: string): boolean {
  const escaped = className.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return new RegExp(`\\.${escaped}(?![\\w-])`).test(selector);
}

/** Whether the gate reaches one infinite animation, pseudo-element included. */
function gateCoversAnimation(selectors: string[], animation: InfiniteAnimation): boolean {
  if (animation.classes.length === 0) return false;
  return selectors.some((selector) => {
    // Every class of the rule has to be named: a rule on `.a.b` is not paused
    // by a gate that names only `.a`.
    if (!animation.classes.every((className) => selectorNamesClass(selector, className))) return false;
    if (!animation.pseudo) return true;
    // `.foo` does not pause `.foo::after` — the gate has to name the
    // pseudo-element too. Pseudo-element names are case-insensitive in CSS.
    return selector.toLowerCase().includes(`::${animation.pseudo.toLowerCase()}`);
  });
}

// ---------------------------------------------------------------------------
// Behaviour
// ---------------------------------------------------------------------------

let focusHandler: ((event: { payload: boolean }) => void) | null = null;

function emitFocus(focused: boolean) {
  act(() => {
    focusHandler!({ payload: focused });
  });
}

function setVisibility(state: 'visible' | 'hidden') {
  Object.defineProperty(document, 'visibilityState', { value: state, configurable: true });
  act(() => {
    document.dispatchEvent(new Event('visibilitychange'));
  });
}

function idleMarker(): string | null {
  return document.documentElement.getAttribute(IDLE_ROOT_ATTRIBUTE);
}

beforeEach(() => {
  focusHandler = null;
  document.documentElement.removeAttribute(IDLE_ROOT_ATTRIBUTE);
  Object.defineProperty(document, 'visibilityState', { value: 'visible', configurable: true });
  windowApi.isFocused.mockReset().mockResolvedValue(true);
  windowApi.onFocusChanged.mockReset().mockImplementation(
    (cb: (event: { payload: boolean }) => void) => {
      focusHandler = cb;
      return Promise.resolve(() => {});
    },
  );
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe('useIdleAnimationGate', () => {
  it('marks the root idle when the window loses focus', async () => {
    renderHook(() => useIdleAnimationGate());
    await act(async () => {});
    expect(idleMarker()).toBeNull();

    emitFocus(false);
    expect(idleMarker()).toBe('true');
  });

  it('clears the marker when the window regains focus', async () => {
    renderHook(() => useIdleAnimationGate());
    await act(async () => {});

    emitFocus(false);
    expect(idleMarker()).toBe('true');

    emitFocus(true);
    expect(idleMarker()).toBeNull();
  });

  it('marks the root idle when the document is hidden', async () => {
    renderHook(() => useIdleAnimationGate());
    await act(async () => {});
    expect(idleMarker()).toBeNull();

    setVisibility('hidden');
    expect(idleMarker()).toBe('true');

    setVisibility('visible');
    expect(idleMarker()).toBeNull();
  });

  it('asks the OS instead of assuming focus', async () => {
    // Launched behind another window: no focus-change event is ever delivered,
    // so the initial query has to carry the answer.
    windowApi.isFocused.mockResolvedValue(false);
    renderHook(() => useIdleAnimationGate());
    await act(async () => {});

    expect(idleMarker()).toBe('true');
  });

  it('keeps the animations running when the focus query fails', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    windowApi.isFocused.mockRejectedValue(new Error('focus unavailable'));
    renderHook(() => useIdleAnimationGate());
    await act(async () => {});

    // The safe direction: an app that cannot tell is an app that behaves
    // exactly like it does today.
    expect(idleMarker()).toBeNull();
    expect(warn).toHaveBeenCalled();
  });

  it('still engages after a failed focus query', async () => {
    // The initial query and the focus subscription are independent: a window
    // whose query fails can still report a focus change, and a gate that
    // switched itself off for the rest of the session over one failed query
    // would never do the job it exists for.
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    windowApi.isFocused.mockRejectedValue(new Error('focus unavailable'));
    renderHook(() => useIdleAnimationGate());
    await act(async () => {});
    expect(idleMarker()).toBeNull();

    emitFocus(false);
    expect(idleMarker()).toBe('true');
    expect(warn).toHaveBeenCalled();
  });

  it('does not let a stale focus query undo a focus change', async () => {
    // The query and the subscription run side by side, so a focus change can
    // land before the query's answer arrives. The event carries the later
    // state, so the stale answer must not overwrite it — that would switch
    // the gate off while the window is in the background, which is the bug
    // this whole change exists to fix.
    let resolveQuery: ((focused: boolean) => void) | null = null;
    windowApi.isFocused.mockImplementation(
      () => new Promise<boolean>((resolve) => { resolveQuery = resolve; }),
    );
    renderHook(() => useIdleAnimationGate());
    await act(async () => {});

    emitFocus(false);
    expect(idleMarker()).toBe('true');

    await act(async () => { resolveQuery!(true); });
    expect(idleMarker()).toBe('true');
  });

  it('removes the marker when it unmounts', async () => {
    const { unmount } = renderHook(() => useIdleAnimationGate());
    await act(async () => {});
    emitFocus(false);
    expect(idleMarker()).toBe('true');

    unmount();
    expect(idleMarker()).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// The stylesheet contract
// ---------------------------------------------------------------------------

describe('the idle gate rule', () => {
  it('pauses the animations instead of removing them', () => {
    // `animation: none` would jump the glyph back to its base style when the
    // window comes back; pausing keeps the phase the user left behind.
    expect(gateBody(CSS)).toMatch(/animation-play-state\s*:\s*paused/);
  });

  it('gates every infinite animation the app defines in a stylesheet', () => {
    // EVERY stylesheet under src/, not only App.css: an infinite animation
    // added to a component's own stylesheet would keep the Compositor
    // producing frames forever, and the gate would not be the thing that
    // stopped it. The mobile SPA is exempt — it is a separate application
    // with its own lifecycle, not the desktop WebView this issue is about.
    const selectors = gateSelectors(CSS);
    const sheets = stylesheets(resolve(__dirname, '../../src')).filter(
      (sheet) => !sheet.path.replace(/\\/g, '/').includes('/src/mobile/'),
    );
    expect(sheets.length).toBeGreaterThan(0);

    const gated = new Set<string>();
    for (const sheet of sheets) {
      for (const animation of infiniteAnimations(sheet.css.replace(/\/\*[\s\S]*?\*\//g, ''))) {
        expect(
          gateCoversAnimation(selectors, animation),
          `${animation.selector} (${sheet.path}) must be inside the idle gate`,
        ).toBe(true);
        gated.add(animation.selector);
      }
    }
    expect(gated.size).toBeGreaterThan(0);
  });

  it('gates every infinite Tailwind animation the desktop app renders', () => {
    const selectors = gateSelectors(CSS);
    const used = tailwindInfiniteClasses(resolve(__dirname, '../../src'));
    expect(used.length).toBeGreaterThan(0);

    for (const token of used) {
      expect(gateCovers(selectors, token), `${token} must be inside the idle gate`).toBe(true);
    }
  });

  it('leaves the one-shot entrance animations alone', () => {
    const selectors = gateSelectors(CSS);
    for (const entrance of ['animate-fade-in', 'animate-scale-in', 'animate-slide-in-right']) {
      expect(gateCovers(selectors, entrance)).toBe(false);
    }
    // A blanket selector would pause the entrance animations too, and with
    // them a toast that arrives while the window is in the background.
    for (const selector of selectors) {
      expect(selector).not.toContain('*');
    }
  });

  it('is mounted by the app shell', () => {
    // The gate is a root-level side effect: it has to run for the whole
    // session, not only for the screens that happen to animate. The match
    // requires the call to START the line, so a commented-out call does not
    // satisfy it.
    expect(APP_TSX).toMatch(/^\s*useIdleAnimationGate\(\);/m);
  });
});
