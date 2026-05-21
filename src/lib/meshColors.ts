const MESH_PALETTE = [
  { border: 'border-l-sky-400', bg: 'bg-sky-400', text: 'text-sky-400', hex: '#38bdf8' },
  { border: 'border-l-amber-400', bg: 'bg-amber-400', text: 'text-amber-400', hex: '#fbbf24' },
  { border: 'border-l-emerald-400', bg: 'bg-emerald-400', text: 'text-emerald-400', hex: '#34d399' },
  { border: 'border-l-rose-400', bg: 'bg-rose-400', text: 'text-rose-400', hex: '#fb7185' },
  { border: 'border-l-violet-400', bg: 'bg-violet-400', text: 'text-violet-400', hex: '#a78bfa' },
  { border: 'border-l-orange-400', bg: 'bg-orange-400', text: 'text-orange-400', hex: '#fb923c' },
  { border: 'border-l-cyan-400', bg: 'bg-cyan-400', text: 'text-cyan-400', hex: '#22d3ee' },
  { border: 'border-l-pink-400', bg: 'bg-pink-400', text: 'text-pink-400', hex: '#f472b6' },
] as const;

export function getMeshColor(meshId: number) {
  return MESH_PALETTE[meshId % MESH_PALETTE.length];
}
