/**
 * App theme state — issue #734.
 *
 * Two states: 'dark' (default, preserved for users who never touch the
 * toggle) and 'light'. The choice is persisted in localStorage and
 * applied to `<html data-theme="...">` so the CSS cascade re-points
 * every --color-* custom property declared in App.css (see the
 * [data-theme="light"] block). xterm.js terminals, which take literal
 * colour strings rather than CSS variables, are kept in step via a
 * module-level pub/sub that TerminalRegistry's ThemeManager subscribes
 * to (mirrors the FontSizeManager pattern — see FontSizeManager.ts).
 *
 * The localStorage read is SYNCHRONOUS at module load. main.tsx imports
 * applyTheme() before React mounts so the [data-theme] attribute is on
 * <html> before the first paint, eliminating a flash of dark theme on
 * reloads where the user picked light.
 */

export type ThemeName = 'dark' | 'light';

const STORAGE_KEY = 'buildmesh.theme';

function loadTheme(): ThemeName {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored === 'light') return 'light';
  } catch {
    // localStorage unavailable (test env, private-mode Safari before
    // user interaction) — fall through to the dark default.
  }
  return 'dark';
}

let _currentTheme: ThemeName = loadTheme();

type ThemeListener = (theme: ThemeName) => void;
const listeners = new Set<ThemeListener>();

/** The active theme. Read-only; mutate via setTheme(). */
export function currentTheme(): ThemeName {
  return _currentTheme;
}

/**
 * Apply the active theme to <html data-theme> AND notify subscribers
 * (ThemeManager, which pushes the new xterm.js palette to every live
 * terminal). Setting the attribute to anything other than 'light' is a
 * no-op so the dark default doesn't litter the DOM with
 * data-theme="dark".
 *
 * Always syncs the DOM and localStorage with `next` — idempotent in
 * the sense that re-applying the active value is harmless, not
 * short-circuited. Listeners are only fired when the in-memory state
 * actually flips, so a no-op call doesn't wake xterm.
 */
export function setTheme(next: ThemeName): void {
  const changed = next !== _currentTheme;
  _currentTheme = next;
  try {
    if (next === 'light') {
      document.documentElement.setAttribute('data-theme', 'light');
      // Persist the active theme so a future boot picks it up at
      // module-load. Without this write, the in-memory value flips but
      // a reload reverts to the dark default — the user would have to
      // re-pick light every restart.
      localStorage.setItem(STORAGE_KEY, 'light');
    } else {
      document.documentElement.removeAttribute('data-theme');
      // localStorage entry removed on revert-to-default — keeps the
      // storage clean for users who tried light once and went back.
      localStorage.removeItem(STORAGE_KEY);
    }
  } catch {
    // document / localStorage unavailable — listeners still fire so
    // in-memory state (xterm themes) stays correct, even if the DOM
    // attribute can't be written (jsdom without document.documentElement
    // writes, etc.).
  }
  if (changed) listeners.forEach(cb => cb(next));
}

/**
 * Subscribe to theme flips. Returns an unsubscribe function. The
 * subscriber is NOT called on registration — only on subsequent
 * setTheme() calls — so callers don't need to seed themselves with the
 * current value (they can read currentTheme() if they need it).
 */
export function onThemeChange(cb: ThemeListener): () => void {
  listeners.add(cb);
  return () => { listeners.delete(cb); };
}

/**
 * Apply the current theme to <html data-theme>. Called once at app
 * boot from main.tsx (before React mounts) so the cascade is in effect
 * before the first paint. Safe to call repeatedly.
 */
export function applyTheme(): void {
  if (_currentTheme === 'light') {
    document.documentElement.setAttribute('data-theme', 'light');
  } else {
    document.documentElement.removeAttribute('data-theme');
  }
}