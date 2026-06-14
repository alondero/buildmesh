/**
 * The mobile `NodeList` renders per-node avatar badges from a hard-coded
 * `PROVIDER_BADGE` table that mirrors the backend's `UiMeta` (#295). The
 * table is the only synchronous lookup available at render time — the
 * live `listProviders()` IPC payload drives the picker, not the badge.
 *
 * `EXPECTED_BADGES` is the canonical pairing: if you add a backend
 * provider, you must add it to BOTH `PROVIDER_BADGE` (in NodeList.tsx)
 * AND this table. A loop-driven test fails fast if either side drifts.
 *
 * Long-term: replace the hard-coded table with a live lookup against the
 * IPC payload (follow-up #328).
 */
import { describe, it, expect } from 'vitest';
import { providerIcon, providerColor } from '../../src/mobile/screens/NodeList';

const EXPECTED_BADGES: Record<string, { icon: string; color: string }> = {
  anthropic: { icon: 'A', color: '#1d7cfc' },
  minimax:   { icon: 'M', color: '#6366f1' },
  kimi:      { icon: 'K', color: '#00c4c4' },
  agy:       { icon: 'G', color: '#10b981' },
  opencode:  { icon: 'O', color: '#f59e0b' },
  // Mirror Provider::Terminal::ui() in src-tauri/.../adapters/terminal.rs
  terminal:  { icon: 'T', color: '#9ca3af' },
  // Mirror Provider::Codex::ui() in src-tauri/.../adapters/codex.rs
  codex:     { icon: 'X', color: '#10a37f' },
};

describe('mobile NodeList providerIcon / providerColor', () => {
  for (const [id, { icon, color }] of Object.entries(EXPECTED_BADGES)) {
    it(`"${id}" returns glyph "${icon}" on background "${color}"`, () => {
      expect(providerIcon(id)).toBe(icon);
      expect(providerColor(id)).toBe(color);
      // Avatar badges are 34x34px — keep glyphs to one printable char so
      // they fit. Spread counts UTF-16 code units (good enough for the
      // current ASCII set; emoji are intentionally disallowed).
      expect([...icon].length).toBe(1);
    });
  }

  it('falls back to "?" / "#555" for unknown providers (unchanged behaviour)', () => {
    expect(providerIcon('mystery')).toBe('?');
    expect(providerColor('mystery')).toBe('#555');
  });
});
