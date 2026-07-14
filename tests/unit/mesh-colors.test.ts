import { describe, it, expect } from 'vitest';
import { getMeshColor, defaultMeshColor, MESH_PALETTE } from '../../src/lib/meshColors';

describe('getMeshColor', () => {
  it('falls back to the deterministic palette keyed on id when no color is stored', () => {
    expect(getMeshColor(0)).toBe(MESH_PALETTE[0]);
    expect(getMeshColor(1)).toBe(MESH_PALETTE[1]);
    // wraps around the palette length
    expect(getMeshColor(MESH_PALETTE.length)).toBe(MESH_PALETTE[0]);
  });

  it('ignores the id and returns the palette entry when a stored color matches a swatch', () => {
    const entry = MESH_PALETTE[2];
    // id 0 would otherwise map to palette[0]; the stored color wins.
    expect(getMeshColor(0, entry.hex)).toBe(entry);
  });

  it('matches stored palette colors case-insensitively', () => {
    const entry = MESH_PALETTE[4];
    expect(getMeshColor(0, entry.hex.toUpperCase())).toBe(entry);
  });

  it('returns a synthetic entry carrying the hex for a custom (non-palette) color', () => {
    const resolved = getMeshColor(0, '#123456');
    expect(resolved.hex).toBe('#123456');
    expect(resolved.border).toBe('');
    expect(resolved.bg).toBe('');
    expect(resolved.text).toBe('');
  });
});

describe('defaultMeshColor', () => {
  it('varies by mesh count and wraps the palette', () => {
    expect(defaultMeshColor(0)).toBe(MESH_PALETTE[0].hex);
    expect(defaultMeshColor(3)).toBe(MESH_PALETTE[3].hex);
    expect(defaultMeshColor(MESH_PALETTE.length + 1)).toBe(MESH_PALETTE[1].hex);
  });
});
