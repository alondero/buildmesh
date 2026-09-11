import { describe, expect, it } from 'vitest';
import { brandFor } from '../../src/lib/brandRegistry';

describe('brandFor', () => {
  it('resolves a Proxied Spawn Option id to its provider brand', () => {
    expect(brandFor('claude:minimax')).toMatchObject({
      id: 'minimax',
      chipHex: '#6366f1',
      chipClass: 'bg-indigo-500',
    });
  });

  it('returns the same brand record for canonical and alias ids', () => {
    expect(brandFor('claude')).toBe(brandFor('anthropic'));
  });

  it('keeps Kimi identity stable across canonical and composite ids', () => {
    const kimi = brandFor('kimi');
    expect(brandFor('claude:kimi')).toBe(kimi);
    expect(kimi).toMatchObject({
      id: 'kimi',
      chipHex: '#00c4c4',
      chipClass: 'bg-cyan-500',
    });
  });

  it('registers Cursor with its official brand treatment', () => {
    expect(brandFor('cursor')).toMatchObject({
      id: 'cursor',
      chipHex: '#1B1913',
      chipClass: 'bg-neutral-900',
    });
  });

  it('registers DeepSeek with its official brand treatment', () => {
    // Issue #1127: the primary brand id is `deepseek` (matching the
    // First-class Model Provider account id), with `dsh` and
    // `deepseek-harness` kept as aliases so the harness-side brand chip
    // still resolves. All four ids (canonical + three aliases) must point
    // at the same Brand record.
    const ds = brandFor('deepseek');
    expect(brandFor('dsh')).toBe(ds);
    expect(brandFor('deepseek-harness')).toBe(ds);
    expect(brandFor('claude:deepseek')).toBe(ds);
    expect(ds).toMatchObject({
      id: 'deepseek',
      chipHex: '#1E88E5',
      chipClass: 'bg-blue-600',
    });
  });

  it('registers Command Code with its official brand treatment', () => {
    const cc = brandFor('commandcode');
    expect(brandFor('cmdc')).toBe(cc);
    expect(brandFor('command-code')).toBe(cc);
    expect(brandFor('claude:commandcode')).toBe(cc);
    expect(cc).toMatchObject({
      id: 'commandcode',
      chipHex: '#8C4EDD',
      chipClass: 'bg-purple-600',
    });
  });

  it('registers Muse Code with the Meta brand treatment', () => {
    const muse = brandFor('muse-code');
    expect(brandFor('muse')).toBe(muse);
    expect(muse).toMatchObject({
      id: 'muse-code',
      chipHex: '#0866FF',
      chipClass: 'bg-blue-600',
    });
    // The official Meta infinity-loop mark ships inline (monochrome via
    // currentColor), not as an image asset or the old placeholder glyph.
    expect(muse?.icon.kind).toBe('inline');
  });

  it('registers Freebuff with its official brand treatment', () => {
    const fb = brandFor('freebuff');
    expect(brandFor('claude:freebuff')).toBe(fb);
    expect(fb).toMatchObject({
      id: 'freebuff',
      chipHex: '#f97316',
      chipClass: 'bg-orange-500',
    });
  });

  it('returns undefined for an unregistered provider', () => {
    expect(brandFor('claude:custom-account')).toBeUndefined();
    expect(brandFor('mystery')).toBeUndefined();
  });

  // ----- Cross-runtime WSL profile ids (issue: Meta mark missing on WSL Meta Code row) -----
  //
  // The WSL auto-detector (`agent/detection.rs::detect_wsl_profiles`) builds
  // profile ids as `<harness>-wsl-<distrohash>` (e.g. `muse-wsl-5562756e7475`
  // for Ubuntu), and manual config may use `<harness>-wsl`. Native Spawn
  // `ProviderInfo` rows carry `id == profile.id` (no `:`), so the brand
  // lookup must strip the runtime suffix to resolve to the harness's brand.
  // Otherwise every WSL harness row renders the wire `meta.icon` fallback
  // glyph instead of the registered brand mark — meaning the WSL Meta Code
  // row shows the placeholder "M" instead of the Meta infinity-loop mark
  // that the native row already renders (issue closed in #1711).

  it('resolves a bare WSL profile id to its harness brand (muse-wsl -> muse-code)', () => {
    expect(brandFor('muse-wsl')).toBe(brandFor('muse'));
    expect(brandFor('muse-wsl')?.id).toBe('muse-code');
  });

  it('resolves a hashed WSL profile id to its harness brand (muse-wsl-5562756e7475 -> muse-code)', () => {
    // The distro hash is the lowercase hex of the WSL distribution name
    // (Ubuntu -> `5562756e7475`). The brand lookup must ignore the hash and
    // resolve to the same Brand record the bare WSL and native ids do.
    expect(brandFor('muse-wsl-5562756e7475')).toBe(brandFor('muse'));
    expect(brandFor('muse-wsl-5562756e7475')?.chipHex).toBe('#0866FF');
  });

  it('resolves other WSL harnesses (codex-wsl, codex-wsl-<hash>) to their harness brand', () => {
    // The fix must generalise across harnesses — Codex in WSL would
    // otherwise drop to its fallback glyph too.
    expect(brandFor('codex-wsl')).toBe(brandFor('codex'));
    expect(brandFor('codex-wsl-5562756e7475')).toBe(brandFor('codex'));
  });

  it('resolves a Proxied row whose harness is a WSL profile (muse-wsl:muse-code -> muse-code)', () => {
    // The colon-split path (`<harness>:<provider>`) composes with the
    // runtime-suffix strip: after splitting to `muse-code` the lookup
    // hits the brand directly, no suffix work needed.
    expect(brandFor('muse-wsl:muse-code')).toBe(brandFor('muse-code'));
  });

  it('does not strip `-wsl` from a registered harness id that happens to contain it', () => {
    // Pin the negative case: if a future harness id is registered as
    // `<name>-wsl` (no `:` in the id), `brandFor('<name>-wsl')` must still
    // resolve to that brand record directly, never fall through to
    // `<name>` (which may be a different brand).
    expect(brandFor('mystery-wsl')).toBeUndefined();
  });
});
