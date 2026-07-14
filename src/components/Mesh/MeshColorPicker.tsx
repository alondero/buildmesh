import { MESH_PALETTE } from '../../lib/meshColors';

interface MeshColorPickerProps {
  /** Currently selected hex (`#rrggbb`). */
  value: string;
  onChange: (hex: string) => void;
  /** Optional id for the group's label association. */
  labelId?: string;
}

/**
 * The shared mesh colour picker: the eight built-in palette swatches plus a
 * native `<input type="color">` for an arbitrary hex. Used both in the
 * "New mesh" modal (choose a colour up front) and the sidebar swatch's
 * recolour popover (issue: mesh colour picker). Presentational — it owns no
 * persistence; the parent decides what to do with the chosen hex.
 */
export function MeshColorPicker({ value, onChange, labelId }: MeshColorPickerProps) {
  const selectedLower = value.toLowerCase();
  const isCustom = !MESH_PALETTE.some((p) => p.hex.toLowerCase() === selectedLower);

  return (
    <div className="flex flex-wrap items-center gap-2" role="group" aria-labelledby={labelId}>
      {MESH_PALETTE.map((p) => {
        const selected = p.hex.toLowerCase() === selectedLower;
        return (
          <button
            key={p.hex}
            type="button"
            onClick={() => onChange(p.hex)}
            aria-label={`Colour ${p.hex}`}
            aria-pressed={selected}
            title={p.hex}
            className={`h-6 w-6 rounded-full transition-transform hover:scale-110 ${
              selected ? 'ring-2 ring-offset-2 ring-offset-bg-overlay ring-white' : ''
            }`}
            style={{ backgroundColor: p.hex }}
          />
        );
      })}
      {/* Custom colour — the native picker. The wrapper swatch shows the
          current custom hex (or a neutral "+" affordance) and clicking it
          opens the OS colour picker via the overlaid transparent input. */}
      <label
        title="Custom colour"
        className={`relative h-6 w-6 cursor-pointer rounded-full border border-dashed border-border-default flex items-center justify-center overflow-hidden ${
          isCustom ? 'ring-2 ring-offset-2 ring-offset-bg-overlay ring-white' : ''
        }`}
        style={isCustom ? { backgroundColor: value } : undefined}
      >
        {!isCustom && <span className="text-text-muted text-xs leading-none">+</span>}
        <input
          type="color"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          aria-label="Custom colour"
          className="absolute inset-0 h-full w-full cursor-pointer opacity-0"
        />
      </label>
    </div>
  );
}
