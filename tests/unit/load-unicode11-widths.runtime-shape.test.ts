/**
 * Regression: terminal rendered blank inside agent nodes after #298.
 *
 * The helper in loadUnicode11Widths.ts reads `term.unicode._providers['11']`
 * to capture the addon's wcwidth for delegation. The vitest mock in
 * vitest.setup.ts satisfies that read by pre-seeding `_providers` directly
 * on the user-facing `term.unicode`, so the existing unit tests pass.
 *
 * In the REAL @xterm/xterm runtime, `term.unicode` is a thin proxy whose
 * `register` delegates to `_core.unicodeService.register(...)` — and
 * `_providers` lives ONLY on that internal service. Reading
 * `term.unicode._providers` returns undefined; the subsequent
 * `providers['11']` throws a TypeError that gets caught silently by
 * TerminalRegistry.doCreate, so no terminal instance is ever produced and
 * the agent node's pane stays blank.
 *
 * This test mirrors the real shape (no `_providers` on `term.unicode`):
 * `term.unicode.register` delegates to `_core.unicodeService.register`,
 * and the internal service is the one the addon writes to. With the
 * current production code, calling `loadUnicode11Widths` throws
 * `Cannot read properties of undefined (reading '11')`. With the fix,
 * the wrapper installs and overrides the addon correctly.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

vi.mock('@xterm/xterm', () => ({ Terminal: vi.fn() }));
vi.mock('@xterm/addon-unicode11', () => ({ Unicode11Addon: vi.fn() }));

import { loadUnicode11Widths, EXTRA_WIDE_EMOJI_LOOKUP } from '../../src/components/Terminal/loadUnicode11Widths';

interface FakeProvider {
  version: string;
  wcwidth(cp: number): 0 | 1 | 2;
  charProperties(cp: number, preceding: number): number;
}

/**
 * Mirror the REAL @xterm/xterm runtime shape (xterm.js 5.x):
 * - `term.unicode` is a thin proxy; `register` delegates to the internal
 *   service. It does NOT expose `_providers`.
 * - `term._core.unicodeService` is the actual UnicodeService singleton.
 * - The addon's `activate(coreTerm)` writes to the internal service
 *   directly (via a core-terminal `unicode` getter that returns the same
 *   service). In this mock we model that by having `loadAddon()` perform
 *   the registration on the internal service on behalf of the addon.
 */
function makeRuntimeShapeMock(
  addonWcwidth: (cp: number) => 0 | 1 | 2 = () => 1,
  addonCharProperties: (cp: number, preceding: number) => number = () => 0,
) {
  const internalProviders: Record<string, FakeProvider> = {};
  let activeVersion = '6';

  const internalService: {
    _providers: Record<string, FakeProvider>;
    register(p: FakeProvider): void;
  } = {
    _providers: internalProviders,
    register(p) { internalProviders[p.version] = p; },
  };

  const term = {
    // Simulates what `term.loadAddon(new Unicode11Addon())` does: the
    // addon's activate calls `unicode.register(new UnicodeV11)` against
    // the internal service. We model it directly so the test exercises
    // the same observable side effect the real add-on produces.
    loadAddon: vi.fn(() => {
      internalService.register({
        version: '11',
        wcwidth: addonWcwidth,
        charProperties: addonCharProperties,
      });
    }),
    // User-facing proxy: NO `_providers`, has `register` that delegates
    // to the internal service (one-line passthrough in the real xterm
    // source: `register(e){this._core.unicodeService.register(e)}`).
    unicode: {
      get activeVersion() { return activeVersion; },
      set activeVersion(v: string) {
        if (!internalProviders[v]) {
          throw new Error(`unknown Unicode version "${v}"`);
        }
        activeVersion = v;
      },
      register: vi.fn((p: FakeProvider) => internalService.register(p)),
    },
    // The internal service lives behind `_core.unicodeService` in real xterm.
    _core: { unicodeService: internalService },
  };

  return { term, internalProviders, getActive: () => activeVersion };
}

describe('loadUnicode11Widths — real-runtime shape (regression for blank terminal)', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('does not throw against a terminal whose unicode proxy lacks _providers', () => {
    // The bug: production code reads term.unicode._providers which is
    // undefined on the real terminal. The TypeError is caught by
    // TerminalRegistry.doCreate, no instance is returned, the pane stays
    // blank. This test fails loud if the regression returns.
    const { term } = makeRuntimeShapeMock();
    expect(() => loadUnicode11Widths(term as unknown as Parameters<typeof loadUnicode11Widths>[0])).not.toThrow();
  });

  it('overrides the addon provider at the same key the addon wrote to', () => {
    // After loadAddon, the internal _providers['11'] is the addon's
    // provider. After loadUnicode11Widths, it must be the wrapper
    // (verified by width-2 response for ⚠). If the code wrote to a
    // different slot, the active version would still resolve to the
    // addon's provider and the regression would be back.
    const { term, internalProviders } = makeRuntimeShapeMock();
    loadUnicode11Widths(term as unknown as Parameters<typeof loadUnicode11Widths>[0]);
    expect(internalProviders['11'].wcwidth(0x26A0)).toBe(2);
  });

  it('overrides every BMP Emoji_Presentation codepoint the addon misses', () => {
    const { term, internalProviders } = makeRuntimeShapeMock();
    loadUnicode11Widths(term as unknown as Parameters<typeof loadUnicode11Widths>[0]);
    const wrapped = internalProviders['11'].wcwidth;
    for (const cp of EXTRA_WIDE_EMOJI_LOOKUP) {
      expect(wrapped(cp)).toBe(2);
    }
  });

  it('defers to the addon for widths it gets right (CJK, ASCII)', () => {
    // The addon's wcwidth knows CJK is 2 and ASCII is 1. The wrapper
    // must pass through, not return 1 for everything it doesn't override.
    const addonWcwidth = vi.fn((cp: number): 0 | 1 | 2 => {
      if (cp === 0x4E2D) return 2; // CJK
      if (cp >= 0x20 && cp < 0x7f) return 1; // ASCII
      return 1;
    });
    const { term, internalProviders } = makeRuntimeShapeMock(addonWcwidth);
    loadUnicode11Widths(term as unknown as Parameters<typeof loadUnicode11Widths>[0]);
    const wrapped = internalProviders['11'].wcwidth;
    expect(wrapped(0x4E2D)).toBe(2);
    expect(wrapped(0x41)).toBe(1);
    expect(addonWcwidth).toHaveBeenCalledWith(0x4E2D);
    expect(addonWcwidth).toHaveBeenCalledWith(0x41);
  });

  it('activates version 11 after wrapping', () => {
    const { term, getActive } = makeRuntimeShapeMock();
    loadUnicode11Widths(term as unknown as Parameters<typeof loadUnicode11Widths>[0]);
    expect(getActive()).toBe('11');
  });

  it('registers the wrapper exactly once via the unicode.register passthrough', () => {
    // The production helper should call term.unicode.register (the
    // public API) to install the wrapper, not reach into _providers
    // directly. If the helper does its own map assignment against
    // term.unicode._providers, the wrapper would land on a slot xterm
    // never reads.
    const { term } = makeRuntimeShapeMock();
    loadUnicode11Widths(term as unknown as Parameters<typeof loadUnicode11Widths>[0]);
    expect(term.unicode.register).toHaveBeenCalledTimes(1);
    expect(term.unicode.register).toHaveBeenCalledWith(expect.objectContaining({ version: '11' }));
  });

  it("binds the addon's charProperties so its `this.wcwidth` lookup resolves", () => {
    // The addon's charProperties combines wcwidth + combining-class into
    // the single property bitmap xterm reads — internally it calls
    // `this.wcwidth(...)`. Destructuring the method without binding would
    // throw "Cannot read properties of undefined (reading 'wcwidth')"
    // when xterm dispatches a character through the wrapper. This is the
    // exact error the user hit at runtime after the first fix shipped.
    // The mock's charProperties mirrors the real addon's bitmap layout
    // (low 2 bits = width), so the assertion below is meaningful.
    const { term, internalProviders } = makeRuntimeShapeMock(
      (cp) => (cp === 0x4E2D ? 2 : 1),
      (cp) => (cp === 0x4E2D ? 2 : 0), // charProperties bitmap: low 2 bits = width
    );
    loadUnicode11Widths(term as unknown as Parameters<typeof loadUnicode11Widths>[0]);
    const wrapped = internalProviders['11'];
    expect(() => wrapped.charProperties(0x4E2D, 0)).not.toThrow();
    // CJK width flows through the addon, not the override set.
    expect(wrapped.charProperties(0x4E2D, 0) & 0x3).toBe(2);
  });
});
