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
