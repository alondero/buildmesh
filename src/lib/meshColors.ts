// The built-in mesh accent palette. Used both as the deterministic fallback
// (keyed on mesh id) and as the swatch set offered by the mesh colour picker.
// `hex` is the source of truth a user's stored `Mesh.color` records; the
// Tailwind `border`/`bg`/`text` classes are convenience handles for palette
// entries (custom hexes render via inline styles instead).
export const MESH_PALETTE = [
  { border: 'border-l-sky-400', bg: 'bg-sky-400', text: 'text-sky-400', hex: '#38bdf8' },
  { border: 'border-l-amber-400', bg: 'bg-amber-400', text: 'text-amber-400', hex: '#fbbf24' },
  { border: 'border-l-emerald-400', bg: 'bg-emerald-400', text: 'text-emerald-400', hex: '#34d399' },
  { border: 'border-l-rose-400', bg: 'bg-rose-400', text: 'text-rose-400', hex: '#fb7185' },
  { border: 'border-l-violet-400', bg: 'bg-violet-400', text: 'text-violet-400', hex: '#a78bfa' },
  { border: 'border-l-orange-400', bg: 'bg-orange-400', text: 'text-orange-400', hex: '#fb923c' },
  { border: 'border-l-cyan-400', bg: 'bg-cyan-400', text: 'text-cyan-400', hex: '#22d3ee' },
  { border: 'border-l-pink-400', bg: 'bg-pink-400', text: 'text-pink-400', hex: '#f472b6' },
] as const;

export type MeshColor = {
  border: string;
  bg: string;
  text: string;
  hex: string;
};

/** Relative luminance of a 6-digit hex, per WCAG 2.x. */
function relativeLuminance(hex: string): number {
  const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex.trim());
  if (!m) throw new Error(`Not a 6-digit hex colour: ${hex}`);
  const channel = (raw: string) => {
    const c = parseInt(raw, 16) / 255;
    return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
  };
  return (
    0.2126 * channel(m[1]) + 0.7152 * channel(m[2]) + 0.0722 * channel(m[3])
  );
}

/**
 * The ink that stays readable ON a swatch of `hex` — the near-black step or
 * the white step, whichever has the higher contrast ratio.
 *
 * The palette's eight swatches are all light, so they all take the dark ink,
 * but a custom Mesh colour is whatever the user picked: #ffffff and #000000
 * are both legal. The checkmark that marks the selected swatch is painted
 * with this, so the marker keeps a readable contrast against the colour it
 * marks (issue #2775) — the better of the two inks is never worse than
 * 4.4:1, the floor where the two choices cross.
 *
 * The two inks are the extremes of the RGB cube and are deliberately NOT
 * theme tokens: a theme-relative ink (the light theme's `text-inverse` is
 * white) would flip with the theme and lose contrast against a swatch of
 * the same lightness. This file is one of the documented mirror files, so
 * the two literals belong here rather than in a component.
 */
export function swatchInk(hex: string): string {
  const luminance = relativeLuminance(hex);
  const inkLuminance = relativeLuminance('#0a0a0e');
  const onDark = (luminance + 0.05) / (inkLuminance + 0.05);
  const onLight = 1.05 / (luminance + 0.05);
  return onDark >= onLight ? '#0a0a0e' : '#ffffff';
}

/**
 * Resolve a mesh's display colour. An explicit user-picked `color` (a
 * `#rrggbb` hex, from the New-mesh modal or the sidebar swatch) wins; when
 * it matches a palette entry we return that entry so the Tailwind class
 * handles come along, otherwise a synthetic entry carrying just the hex
 * (consumers that need a class render the hex inline). With no stored colour
 * we fall back to the deterministic palette keyed on the mesh id — the
 * historical behaviour.
 */
export function getMeshColor(meshId: number, color?: string | null): MeshColor {
  if (color) {
    const lower = color.toLowerCase();
    const match = MESH_PALETTE.find((p) => p.hex.toLowerCase() === lower);
    if (match) return match;
    return { border: '', bg: '', text: '', hex: color };
  }
  return MESH_PALETTE[meshId % MESH_PALETTE.length];
}

/** The palette hex a new mesh defaults to, varied by how many meshes exist. */
export function defaultMeshColor(meshCount: number): string {
  return MESH_PALETTE[meshCount % MESH_PALETTE.length].hex;
}
