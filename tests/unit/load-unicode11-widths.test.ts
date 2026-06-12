/**
 * Regression: xterm's @xterm/addon-unicode11@0.9.0 ships an incomplete
 * Emoji_Presentation table. Without the wrapper in loadUnicode11Widths, any
 * CLI table row containing a BMP emoji from `EXTRA_WIDE_EMOJI` shears its
 * right box-drawing border by exactly one cell — the source pads assuming
 * width 2, xterm undercounts.
 *
 * The original report: Claude Code's disk-usage summary aligned `✅`/`❌` rows
 * correctly but `⚠️` rows, because ⚠ (U+26A0) was the one codepoint Adam
 * happened to hit. The wrapper is comprehensive because any
 * Emoji_Presentation=Yes codepoint triggers the same bug; the test asserts
 * the property for the full set.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

vi.mock('@xterm/xterm', () => ({ Terminal: vi.fn() }));
vi.mock('@xterm/addon-unicode11', () => ({ Unicode11Addon: vi.fn() }));

import { loadUnicode11Widths, EXTRA_WIDE_EMOJI_LOOKUP } from '../../src/components/Terminal/loadUnicode11Widths';

interface FakeProvider {
  version: string;
  wcwidth(cp: number): number;
  // The addon's `charProperties` calls `this.wcwidth(...)` internally, so
  // the production code now `.bind`s it to the provider object. Mocks
  // that omit `charProperties` will crash the helper at the bind call
  // — see the runtime-shape test for the real-xterm contract.
  charProperties(cp: number, preceding: number): number;
}

function makeMockTerm(addonWcwidth: (cp: number) => number = () => 1) {
  // Mirror the real @xterm/xterm shape: the user-facing `unicode` is a
  // thin proxy whose `register` delegates to the internal
  // `UnicodeService`; the addon writes to the internal service during
  // activate. See tests/unit/load-unicode11-widths.runtime-shape.test.ts
  // for the full rationale and the exact API contract.
  const internalProviders: Record<string, FakeProvider> = {};
  let activeVersion = '6';
  const addonProvider: FakeProvider = {
    version: '11',
    wcwidth: addonWcwidth,
    charProperties: () => 0,
  };
  internalProviders['11'] = addonProvider;

  const internalService = {
    _providers: internalProviders,
    register: vi.fn((p: FakeProvider) => {
      internalProviders[p.version] = p;
    }),
  };
  // Simulate what `loadAddon(new Unicode11Addon())` does: the addon's
  // activate calls the internal service's register(). We do it via a
  // loadAddon mock so the test exercises the same side effect.
  const loadAddon = vi.fn(() => {
    internalService.register(addonProvider);
  });
  // The user-facing register is a passthrough to the internal service —
  // mirrors the one-liner in real xterm source:
  //   register(e) { this._core.unicodeService.register(e) }
  const registerSpy = vi.fn((p: FakeProvider) => internalService.register(p));

  const term = {
    loadAddon,
    unicode: {
      get activeVersion() {
        return activeVersion;
      },
      set activeVersion(v: string) {
        if (!internalProviders[v]) throw new Error(`unknown Unicode version "${v}"`);
        activeVersion = v;
      },
      register: registerSpy,
    },
    _core: { unicodeService: internalService },
  };
  return {
    term,
    providers: internalProviders,
    activeVersionFn: () => activeVersion,
    registerSpy,
  };
}

describe('loadUnicode11Widths', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("regression: Adam's ⚠ (U+26A0) row renders as 2 cells", () => {
    // Without the wrapper, the addon's wcwidth(0x26A0) === 1 → row misaligns
    // by 1 cell on the right border. With the wrapper, width is 2 → the
    // source's column padding matches xterm's rendering.
    const { term, providers } = makeMockTerm();
    loadUnicode11Widths(term as unknown as Parameters<typeof loadUnicode11Widths>[0]);
    expect(providers['11'].wcwidth(0x26A0)).toBe(2);
  });

  it('overrides every Emoji_Presentation=Yes BMP codepoint the addon misses', () => {
    const { term, providers } = makeMockTerm();
    loadUnicode11Widths(term as unknown as Parameters<typeof loadUnicode11Widths>[0]);
    const wrapped = providers['11'].wcwidth;
    for (const cp of EXTRA_WIDE_EMOJI_LOOKUP) {
      expect(wrapped(cp)).toBe(2);
    }
  });

  it('defers to the addon for widths it gets right', () => {
    // Addon claims ✅, ❌, ⭐ are width 2 and ASCII is width 1 — none of
    // these are in the override set, so the wrapper must pass them through.
    const addonWcwidth = vi.fn((cp: number) => {
      if (cp === 0x2705 || cp === 0x274c || cp === 0x2b50) return 2;
      if (cp >= 0x20 && cp < 0x7f) return 1;
      return 1;
    });
    const { term, providers } = makeMockTerm(addonWcwidth);
    loadUnicode11Widths(term as unknown as Parameters<typeof loadUnicode11Widths>[0]);
    const wrapped = providers['11'].wcwidth;

    expect(wrapped(0x2705)).toBe(2); // ✅
    expect(wrapped(0x274c)).toBe(2); // ❌
    expect(wrapped(0x2b50)).toBe(2); // ⭐
    expect(wrapped(0x41)).toBe(1);   // A
    expect(addonWcwidth).toHaveBeenCalledWith(0x2705);
    expect(addonWcwidth).toHaveBeenCalledWith(0x41);
  });

  it('activates the version-11 provider after wrapping', () => {
    const { term, activeVersionFn } = makeMockTerm();
    loadUnicode11Widths(term as unknown as Parameters<typeof loadUnicode11Widths>[0]);
    expect(activeVersionFn()).toBe('11');
  });

  it('replaces the addon provider at version "11" (not appends)', () => {
    // xterm's UnicodeService.register does `this._providers[v] = p` — verify
    // the wrapper took the addon's slot, not a separate one.
    const { term, providers, registerSpy } = makeMockTerm();
    loadUnicode11Widths(term as unknown as Parameters<typeof loadUnicode11Widths>[0]);
    // Only our register call is observable through registerSpy (the mock
    // pre-seeds _providers['11'] by direct map assignment, not via
    // unicode.register). To prove the wrapper REPLACED rather than appended,
    // we read providers['11'].wcwidth back out and confirm it returns 2 for
    // the emoji it overrides — i.e. the wrapper's function, not the seeded
    // stub that would have returned 1.
    const wrapped = providers['11'].wcwidth;
    expect(wrapped(0x26a0)).toBe(2);
    expect(registerSpy).toHaveBeenCalledTimes(1);
  });

  it('the upstream addon alone misclassifies ⚠ as 1 (proves the test catches the bug)', () => {
    // Sanity check against the actual @xterm/addon-unicode11@0.9.0 binary:
    // if a future addon release fills in Emoji_Presentation for U+26A0 this
    // assertion will start failing — that's the signal to drop the wrapper
    // and the EXTRA_WIDE_EMOJI set. We use vi.importActual to bypass the
    // top-of-file mock (which would otherwise hand back a vi.fn()).
    return vi.importActual<typeof import('@xterm/addon-unicode11')>('@xterm/addon-unicode11').then(({ Unicode11Addon }) => {
      let addonWcwidth: ((cp: number) => number) | null = null;
      const fakeTerm = {
        registerCharacterJoiner: () => {},
        unicode: {
          activeVersion: '6',
          register: (p: { wcwidth: (cp: number) => number }) => {
            addonWcwidth = p.wcwidth;
          },
        },
      };
      // Unicode11Addon's activate calls registerCharacterJoiner + unicode.register
      new Unicode11Addon().activate(fakeTerm as unknown as { unicode: { register: (p: unknown) => void }; registerCharacterJoiner: () => void });
      expect(addonWcwidth).not.toBeNull();
      // The whole point: the addon's wcwidth is 1 for U+26A0 today. If this
      // flips to 2, the bug is gone upstream and the wrapper is dead weight.
      expect(addonWcwidth!(0x26a0)).toBe(1);
    });
  });
});
