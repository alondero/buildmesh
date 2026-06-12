import { Terminal } from '@xterm/xterm';
import { Unicode11Addon } from '@xterm/addon-unicode11';

/**
 * Codepoints in the BMP that have `Emoji_Presentation=Yes` in Unicode 11+ but
 * which `@xterm/addon-unicode11@0.9.0` ships with width=1 in its lookup table.
 * Without overrides here, those rows in CLI output (Claude Code status tables,
 * `npm`, `gh`, etc.) shear their right box-drawing border by exactly one cell:
 * modern CLIs pad assuming 2 cells, xterm undercounts.
 *
 * Compiled from `emoji-data.txt` (Unicode 13.0, a superset of the Unicode 11
 * table the addon targets) and verified against the addon's `wcwidth` in
 * `loadUnicode11Widths.test.ts`. Re-run the test's "is this still misclassified?"
 * block when bumping the addon — if it reports zero leftovers, drop this set
 * and the wrapper, and revert to the bare `Unicode11Addon` calls.
 */
const EXTRA_WIDE_EMOJI: ReadonlySet<number> = new Set<number>([
  0x23ED, 0x23EE, 0x23EF, 0x23F1, 0x23F2, 0x23F8, 0x23F9, 0x23FA,
  0x25B6, 0x25C0, 0x25FB, 0x25FC,
  0x26A0, 0x26C8, 0x26FB, 0x26FC,
  0x2708, 0x2709, 0x270C, 0x270F, 0x2712, 0x2714, 0x2716, 0x271D,
  0x2721, 0x2733, 0x2734, 0x2744, 0x2747, 0x2763, 0x2764,
  0x27A1, 0x2934, 0x2935, 0x2B05, 0x2B06, 0x2B07,
]);

/** Exported for tests only — the array form lets the test iterate in order. */
export const EXTRA_WIDE_EMOJI_LOOKUP: readonly number[] = Array.from(EXTRA_WIDE_EMOJI).sort(
  (a, b) => a - b,
);

interface UnicodeProvider {
  version: string;
  wcwidth(cp: number): 0 | 1 | 2;
  charProperties(cp: number, preceding: number): number;
}

/**
 * Activate xterm's Unicode 11 width table with a small patch layer that fills
 * in the BMP emoji the upstream table undercounts. Use this at every terminal
 * construction site (agent registry, build/run terminal, mobile screen)
 * instead of the bare `Unicode11Addon` activation.
 *
 * Why a wrapper and not a different addon: `@xterm/addon-unicode11` is the
 * only first-party option and it bundles a hardcoded `l[65536]` lookup table
 * that's incomplete. Replacing the whole provider means re-implementing that
 * table ourselves. The wrapper defers to the addon for everything it gets
 * right (CJK, fullwidth, ASCII) and overrides only the 38 codepoints it
 * misclassifies — the smallest possible diff to fix the alignment bug.
 */
export function loadUnicode11Widths(term: Terminal): void {
  // Loading the addon registers a `version: '11'` provider on the unicode
  // service. We need to capture that provider's `wcwidth` by reference so
  // our wrapper can call it directly — going through `term.unicode.wcwidth`
  // would recurse through whatever provider is currently active (us, after
  // we register below).
  //
  // `_providers` is a stable xterm internal: the unicode service has exposed
  // it as a `Record<versionString, provider>` since the addon API was added,
  // and the public `register(provider)` API itself is `_providers[v] = p`
  // — i.e. we're reading back the same map the addon just wrote into.
  const providers = (term.unicode as unknown as {
    _providers: Record<string, UnicodeProvider>;
  })._providers;

  term.loadAddon(new Unicode11Addon());
  const addonWcwidth = providers['11'].wcwidth;
  const addonCharProperties = providers['11'].charProperties;

  // `register` replaces any existing provider for the same version, so this
  // shadows the addon's provider at key `'11'`. The setter on
  // `activeVersion` re-activates the (now ours) provider.
  term.unicode.register({
    version: '11',
    wcwidth(cp: number): 0 | 1 | 2 {
      if (EXTRA_WIDE_EMOJI.has(cp)) return 2;
      return addonWcwidth(cp);
    },
    charProperties(cp: number, preceding: number): number {
      return addonCharProperties(cp, preceding);
    },
  });
  term.unicode.activeVersion = '11';
}
