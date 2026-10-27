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
import { StrictMode } from 'react';
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
  // Found by scanning rather than by position, so the helper cannot read the
  // wrong rule if the block is ever moved. The assertion below then pins that
  // there is exactly ONE gate rule: a second one would be a second place to
  // forget an animation, so it is a failure rather than a silent choice.
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
  /** The full selector the animation runs on, for the coverage check. */
  selector: string;
}

/** Whether a declaration body declares an infinite animation. */
function declaresInfiniteAnimation(body: string): boolean {
  const shorthand = body.match(/(?:^|;|\s)animation\s*:\s*([^;]+)/);
  // CSS keywords are case-insensitive, so `INFINITE` counts too.
  return Boolean(
    (shorthand && /\binfinite\b/i.test(shorthand[1])) ||
    /animation-iteration-count\s*:\s*infinite/i.test(body),
  );
}

/** Every infinite animation a stylesheet declares in a flat rule. */
function infiniteAnimations(css: string): InfiniteAnimation[] {
  const found: InfiniteAnimation[] = [];
  for (const rule of rules(css)) {
    // An at-rule is not a class selector: `@utility animate-marquee { … }`
    // names its utility in the at-rule's own prelude, and the dedicated
    // scanners below read it from there.
    if (rule.selectors.some((selector) => selector.trim().startsWith('@'))) continue;
    if (!declaresInfiniteAnimation(rule.body)) continue;
    for (const selector of rule.selectors) {
      // Any pseudo-element, not only ::before/::after: `.foo::marker` is just
      // as capable of keeping the Compositor busy as `.foo::after`.
      found.push({ classes: selectorClasses(selector), selector });
    }
  }
  return found;
}

/** The body of every `@name { ... }` at-rule, brace-matched. */
function atRuleBodies(css: string, name: string): string[] {
  const bodies: string[] = [];
  for (const match of css.matchAll(new RegExp(`@${name}\\b`, 'g'))) {
    const open = css.indexOf('{', match.index);
    if (open < 0) break;
    let depth = 0;
    for (let index = open; index < css.length; index += 1) {
      if (css[index] === '{') depth += 1;
      else if (css[index] === '}' && --depth === 0) {
        bodies.push(css.slice(open + 1, index));
        break;
      }
    }
  }
  return bodies;
}

/**
 * The infinite Tailwind utilities a stylesheet's `@theme` block declares.
 *
 * Tailwind v4 turns a `--animate-<name>: <value>` token into the
 * `animate-<name>` utility, so an infinite token is an infinite animation the
 * gate has to cover. The flat-rule scanner above cannot see it: `@theme { … }`
 * nests its declarations, and the token's value never appears as an
 * `animation:` shorthand.
 */
function themeAnimationUtilities(css: string): InfiniteAnimation[] {
  const found: InfiniteAnimation[] = [];
  // Every `@theme` block, not only the first: a stylesheet may declare more
  // than one, and a token in the second one is as much an infinite animation
  // as a token in the first.
  for (const body of atRuleBodies(css, 'theme')) {
    for (const match of body.matchAll(/--animate-([\w-]+)\s*:\s*([^;]+);/g)) {
      if (!/\binfinite\b/i.test(match[2])) continue;
      found.push({ classes: [`animate-${match[1]}`], selector: `.animate-${match[1]}` });
    }
  }
  return found;
}

/**
 * The infinite animations a stylesheet's `@utility` at-rules declare.
 *
 * `@utility animate-marquee { animation: marquee 3s linear infinite; }` is a
 * Tailwind utility like any other, and the utility's NAME is the class the app
 * renders — so the gate has to name that name, and the guard has to read it
 * from the at-rule rather than report a rule with no class at all.
 */
function utilityAnimations(css: string): InfiniteAnimation[] {
  const found: InfiniteAnimation[] = [];
  for (const match of css.matchAll(/@utility\s+([^\s{]+)\s*\{([^{}]*)\}/g)) {
    if (!declaresInfiniteAnimation(match[2])) continue;
    found.push({ classes: [match[1]], selector: `.${match[1]}` });
  }
  return found;
}

/** Every infinite animation a stylesheet declares, in any of Tailwind's forms. */
function everyInfiniteAnimation(css: string): InfiniteAnimation[] {
  const source = css.replace(/\/\*[\s\S]*?\*\//g, '');
  return [...infiniteAnimations(source), ...themeAnimationUtilities(source), ...utilityAnimations(source)];
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

/**
 * The characters a class token can carry in front of the utility name: a
 * variant prefix (`hover:`, `data-[state=open]:`, `[&>*]:`) or the important
 * marker (`!`). The prefix is part of the token the DOM holds.
 */
const VARIANT_PREFIX = /[\w:[\]./%#()!@&*>~=-]*/;

/**
 * The source without its comments.
 *
 * A comment may mention a class name — "the variant `hover:animate-pulse` is
 * not the bare utility" — and the token scanner reads raw text, so a comment
 * that merely names a variant would otherwise turn into a token the gate has
 * to cover. Strings are left alone, so a class inside one still counts.
 */
function stripComments(source: string): string {
  let out = '';
  let index = 0;
  let quote: string | null = null;
  while (index < source.length) {
    const char = source[index];
    const next = source[index + 1];
    if (quote) {
      out += char;
      if (char === '\\') {
        out += next ?? '';
        index += 2;
        continue;
      }
      if (char === quote) quote = null;
      index += 1;
      continue;
    }
    // An apostrophe opens a string only when it does not follow a word
    // character: in JSX text ("don't stop") it is prose, and treating it as a
    // string opener would leave the rest of the region unstripped.
    if (char === '"' || char === '`' || (char === "'" && !/[\w$]/.test(source[index - 1] ?? ''))) {
      quote = char;
      out += char;
      index += 1;
      continue;
    }
    // An escaped character outside a string — a regex literal's `\/`, say —
    // is copied with the character it escapes, so the `//` inside it is not
    // mistaken for the start of a comment.
    if (char === '\\') {
      out += char + (next ?? '');
      index += 2;
      continue;
    }
    if (char === '/' && next === '/') {
      while (index < source.length && source[index] !== '\n') index += 1;
      continue;
    }
    if (char === '/' && next === '*') {
      index += 2;
      while (index < source.length && !(source[index] === '*' && source[index + 1] === '/')) index += 1;
      index += 2;
      continue;
    }
    out += char;
    index += 1;
  }
  return out;
}

/**
 * Every infinite Tailwind animation class the desktop app renders.
 *
 * The token is captured IN FULL — variant prefix and important marker
 * included. A variant is a different class token: the DOM holds
 * "hover:animate-pulse", which a plain `.animate-pulse` selector does not
 * match. Capturing the bare utility instead would let the guard report a
 * coverage the gate does not have, so the variant is recorded as itself.
 */
function tailwindInfiniteClasses(srcRoot: string): string[] {
  const tokens = new Set<string>();
  const utility = `animate-(?:${TAILWIND_INFINITE_UTILITIES.join('|')})`;
  const pattern = new RegExp(`(?<![\\w-])${VARIANT_PREFIX.source}${utility}(?![\\w-])!?`, 'g');
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
      for (const match of stripComments(readFileSync(full, 'utf8')).matchAll(pattern)) tokens.add(match[0]);
    }
  };
  walk(srcRoot);
  return [...tokens];
}

/** Whether a token is a bare Tailwind utility — no variant, no important marker. */
function isBareUtility(token: string): boolean {
  return new RegExp(`^animate-(?:${TAILWIND_INFINITE_UTILITIES.join('|')})$`).test(token);
}

/** Whether one gate selector reaches the element carrying `token`. */
function gateCovers(selectors: string[], token: string): boolean {
  // Only the selectors that carry the marker are the gate: a selector without
  // it would pause its animations in the foreground too.
  const gated = selectors.filter((selector) => normaliseSelector(selector).startsWith(ROOT_SELECTOR));
  if (!isBareUtility(token)) {
    // A variant (`hover:animate-pulse`) or the important marker
    // (`animate-pulse!`) is a DIFFERENT class token, so a plain
    // `.animate-pulse` selector does NOT reach it — the gate has to name the
    // token in full, as `[class~='hover:animate-pulse']`.
    return gated.some((s) => s.includes(`[class~='${token}']`));
  }
  const escaped = token.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  // The trailing guard stops `.animate-pulse` from matching `.animate-pulse-fast`.
  const pattern = new RegExp(`\\.${escaped}(?![\\w-])`);
  return gated.some((s) => pattern.test(s));
}

/** The classes a selector names, for the failure message. */
function selectorClasses(selector: string): string[] {
  return [...selector.matchAll(/\.([A-Za-z0-9_-]+)/g)].map((match) => match[1]);
}

/**
 * The idle marker every gate selector has to carry.
 *
 * A selector without it would pause its animations in the FOREGROUND too —
 * the gate would stop being a gate — so the guard has to insist on it. The
 * marker is on the root element, so it always comes first.
 */
const ROOT_SELECTOR = normaliseSelector(`[${IDLE_ROOT_ATTRIBUTE}='true']`);

/**
 * Whether one gate selector reaches the element the animation runs on.
 *
 * The gate has to select the SAME element, not merely mention the same class.
 * `animation-play-state` is not inherited, so pausing a parent does not pause
 * a child's animation — the child's own `animation` shorthand resets the play
 * state — and a state pseudo-class (`.foo:hover`) carries its own animation
 * while the state lasts. A gate naming `.pulse-dots` therefore does NOT cover
 * `.pulse-dots > span`, and the guard must not report that it does.
 *
 * So the gate selector has to END with the animation's own selector, which
 * makes the requirement satisfiable: the gate can name the same target. The
 * root element is the one exception — it carries the marker itself, so the
 * bare `[data-window-idle='true']` selector reaches it.
 */
function gateCoversAnimation(selectors: string[], animation: InfiniteAnimation): boolean {
  const target = normaliseSelector(animation.selector);
  return selectors.some((selector) => {
    const normalised = normaliseSelector(selector);
    // The marker comes first: a selector without it is not the gate.
    if (!normalised.startsWith(ROOT_SELECTOR)) return false;
    if (normalised === target || normalised.endsWith(` ${target}`)) return true;
    return target === 'html' && normalised === ROOT_SELECTOR;
  });
}

/** A selector with its whitespace and quotes normalised, for comparison. */
function normaliseSelector(selector: string): string {
  return selector.replace(/\s+/g, ' ').trim().replace(/['"]/g, '');
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

  it('keeps one live subscription after a StrictMode double mount', async () => {
    // `src/main.tsx` mounts the app under React's StrictMode, which mounts
    // every effect, tears it down and mounts it again. The first subscription
    // must not outlive its effect: a listener that survives would deliver
    // every focus change a second time, and the marker would follow a
    // teardown that already ran.
    const live = new Set<() => void>();
    windowApi.onFocusChanged.mockImplementation((cb: (event: { payload: boolean }) => void) => {
      focusHandler = cb;
      const stop = vi.fn();
      live.add(stop);
      return Promise.resolve(() => {
        live.delete(stop);
        stop();
      });
    });

    const { unmount } = renderHook(() => useIdleAnimationGate(), { wrapper: StrictMode });
    await act(async () => {});

    expect(live.size).toBe(1);
    emitFocus(false);
    expect(idleMarker()).toBe('true');

    unmount();
    expect(live.size).toBe(0);
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
      for (const animation of everyInfiniteAnimation(sheet.css)) {
        expect(
          gateCoversAnimation(selectors, animation),
          `${animation.selector} (${sheet.path}) must be inside the idle gate`,
        ).toBe(true);
        gated.add(animation.selector);
      }
    }
    expect(gated.size).toBeGreaterThan(0);
  });

  it('sees an infinite animation declared as a Tailwind theme token', () => {
    // Tailwind v4 turns `--animate-<name>` into the `animate-<name>` utility.
    // An infinite token is an infinite animation, and `@theme { … }` nests its
    // declarations where the flat-rule scanner cannot reach them — so the
    // guard reads the token block itself.
    const css = '@theme { --animate-guard-probe: marquee 3s linear infinite; }';
    const found = everyInfiniteAnimation(css);
    expect(found.map((animation) => animation.classes[0])).toContain('animate-guard-probe');

    // And the guard has teeth: the current gate does not name it, so the
    // failure is reported rather than passed in silence.
    expect(gateCoversAnimation(gateSelectors(CSS), found[0])).toBe(false);
  });

  it('sees an infinite animation declared as a Tailwind utility', () => {
    // `@utility animate-marquee { … }` is a utility whose NAME is the class the
    // app renders. The guard reads the name out of the at-rule, so adding the
    // utility to the gate can satisfy it.
    const css = '@utility animate-guard-probe { animation: marquee 3s linear infinite; }';
    const found = everyInfiniteAnimation(css);
    expect(found).toHaveLength(1);
    expect(found[0].classes).toEqual(['animate-guard-probe']);

    // The gate can satisfy it by naming the class.
    expect(
      gateCoversAnimation(
        [...gateSelectors(CSS), "[data-window-idle='true'] .animate-guard-probe"],
        found[0],
      ),
    ).toBe(true);
  });

  it('does not mistake a Tailwind variant for the bare utility', () => {
    // `hover:animate-pulse` and `animate-pulse!` are DIFFERENT class tokens
    // from `animate-pulse`, and a plain `.animate-pulse` selector does not
    // reach them. The guard has to name the token in full, or it would report
    // a coverage the gate does not have.
    const selectors = gateSelectors(CSS);
    expect(gateCovers(selectors, 'animate-pulse')).toBe(true);
    for (const variant of ['hover:animate-pulse', 'data-[state=open]:animate-pulse', 'animate-pulse!']) {
      expect(gateCovers(selectors, variant), `${variant} must be gated in full`).toBe(false);
    }
  });

  it('does not let a comment name a class the app never renders', () => {
    // The token scanner reads source text, and a comment may mention a class
    // name. A comment that merely says "hover:animate-pulse is not the bare
    // utility" must not turn into a token the gate has to cover.
    const source = [
      '// the variant hover:animate-pulse is not the bare utility',
      '/* and neither is data-[state=open]:animate-pulse */',
      'const label = "animate-pulse";',
    ].join('\n');
    expect(stripComments(source)).not.toContain('hover:animate-pulse');
    // A class inside a string is code, not prose, so it survives.
    expect(stripComments(source)).toContain('"animate-pulse"');
  });

  it('strips a comment that follows JSX prose', () => {
    // An apostrophe in JSX text is prose, not a string opener. Treating it as
    // one would leave the rest of the region unstripped, and a comment inside
    // it would still reach the token scanner.
    const source = [
      "      <p>Don't stop the animation</p>",
      '      // hover:animate-pulse is not the bare utility',
      "      const label = 'animate-pulse';",
    ].join('\n');
    const stripped = stripComments(source);
    expect(stripped).not.toContain('hover:animate-pulse');
    // The class inside the string is still there.
    expect(stripped).toContain("'animate-pulse'");
  });

  it('keeps a regex literal with a slash in it intact', () => {
    // `/a\/\//` contains a `//`, which must not be read as the start of a
    // comment — that would swallow the rest of the line, including a real
    // class name.
    const source = ['const pattern = /a\\/\\//;', "const label = 'animate-pulse';"].join('\n');
    const stripped = stripComments(source);
    expect(stripped).toContain("'animate-pulse'");
  });

  it('requires the gate selector to carry the idle marker', () => {
    // A selector without the marker would pause its animations in the
    // foreground too — the gate would stop being a gate — so the guard must
    // not count it as coverage.
    const animation = { classes: ['animate-pulse'], selector: '.animate-pulse' };
    expect(gateCoversAnimation(gateSelectors(CSS), animation)).toBe(true);
    expect(gateCoversAnimation(['.animate-pulse'], animation)).toBe(false);
    expect(gateCovers(['.animate-pulse'], 'animate-pulse')).toBe(false);
    expect(gateCovers(["[class~='motion-safe:animate-pulse']"], 'motion-safe:animate-pulse')).toBe(false);
  });

  it('requires the gate to select the element the animation runs on', () => {
    // `animation-play-state` is not inherited, so pausing a parent does not
    // pause a child's animation, and a state pseudo-class carries its own
    // animation while the state lasts. A gate naming the parent is not a
    // cover for the child.
    const selectors = gateSelectors(CSS);
    for (const selector of ['.pulse-dots > span', '.foo:hover', '.foo .bar']) {
      const animation = { classes: selectorClasses(selector), selector };
      expect(gateCoversAnimation(selectors, animation), `${selector} must be gated on its own element`).toBe(false);
    }

    // And the requirement is satisfiable: the gate can name the same target.
    const animation = { classes: ['pulse-dots'], selector: '.pulse-dots > span' };
    expect(
      gateCoversAnimation([...selectors, "[data-window-idle='true'] .pulse-dots > span"], animation),
    ).toBe(true);
  });

  it('lets the root element be gated by the marker alone', () => {
    // The marker lives on the root element, so the bare attribute selector
    // reaches it — a classless rule on the root is satisfiable, and one on
    // another element is not.
    const selectors = gateSelectors(CSS);
    expect(gateCoversAnimation(selectors, { classes: [], selector: 'html' })).toBe(true);
    expect(gateCoversAnimation(selectors, { classes: [], selector: 'body' })).toBe(false);
    expect(
      gateCoversAnimation([...selectors, "[data-window-idle='true'] body"], { classes: [], selector: 'body' }),
    ).toBe(true);
  });

  it('gates every infinite Tailwind animation the desktop app renders', () => {
    const selectors = gateSelectors(CSS);
    const used = tailwindInfiniteClasses(resolve(__dirname, '../../src'));
    expect(used.length).toBeGreaterThan(0);

    // The scanner must not MISS a real token: a token it fails to see is a
    // hole no assertion above would catch, because the loop only sees what
    // the scanner found. These three are the utilities the desktop app
    // renders today.
    expect(used).toEqual(expect.arrayContaining(['animate-pulse', 'animate-spin', 'motion-safe:animate-pulse']));

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
