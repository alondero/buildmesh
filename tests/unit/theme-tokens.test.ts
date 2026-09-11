import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

// Diff and status components colour added/removed/modified state with
// `text-accent-green`, `bg-accent-red/10`, `text-accent-amber`, etc. In
// Tailwind v4 those utilities only emit CSS when the matching `--color-accent-*`
// token exists in `@theme`; an undefined token renders *nothing* (no error, just
// invisible). The diff view shipped colourless for exactly this reason — the
// tokens were never defined. Guard the contract so it can't silently regress.
describe('theme accent colour tokens', () => {
  const css = readFileSync(resolve(__dirname, '../../src/App.css'), 'utf8');

  it.each(['accent-green', 'accent-red', 'accent-amber'])(
    'defines --color-%s (used by diff add/remove/modified styling)',
    (name) => {
      expect(css).toMatch(new RegExp(`--color-${name}\\s*:`));
    }
  );

  // The design-system pass (modernization) migrated components onto these
  // tokens; if any disappears from @theme, the utility classes silently stop
  // emitting and the UI regresses invisibly.
  it('defines --text-2xs (Tailwind v4 font-size namespace — the sub-xs step badges use)', () => {
    expect(css).toMatch(/--text-2xs\s*:/);
  });

  it.each(['fade-in', 'scale-in', 'slide-in-right'])(
    'defines --animate-%s and its keyframes (toast/modal/dropdown entrances)',
    (name) => {
      expect(css).toMatch(new RegExp(`--animate-${name}\\s*:`));
      expect(css).toMatch(new RegExp(`@keyframes ${name}\\b`));
    }
  );

  it('retimes all transition utilities with the snappy defaults', () => {
    expect(css).toMatch(/--default-transition-duration\s*:/);
    expect(css).toMatch(/--default-transition-timing-function\s*:/);
  });

  it('declares color-scheme so native form controls (select popups) follow the theme', () => {
    // A native <select> popup is OS/browser-rendered and follows the
    // document's `color-scheme`, not our background-color utilities. With
    // no declaration the dark theme still opened a light OS popup — light
    // text on white. Dark is the default; [data-theme="light"] re-points it.
    expect(css).toMatch(/html\s*\{[^}]*color-scheme:\s*dark/);
    expect(css).toMatch(/\[data-theme="light"\][\s\S]{0,200}color-scheme:\s*light/);
    // Scoped to the root only: an explicit value on body would outrank the
    // root's [data-theme="light"] override for everything below it, leaving
    // light theme with dark native controls.
    expect(css).not.toMatch(/\bbody\s*(?:,[^{]*)?\{[^}]*color-scheme/);
  });

  it('restores pointer cursor on buttons (Tailwind v4 preflight sets cursor: default)', () => {
    // Components rely on this base rule instead of per-button cursor-pointer.
    expect(css).toMatch(/button:not\(:disabled\)[\s\S]{0,200}cursor:\s*pointer/);
  });
});
