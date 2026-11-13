import { MESH_PALETTE, swatchInk } from '../../lib/meshColors';

interface MeshColorPickerProps {
  /** Currently selected hex (`#rrggbb`). */
  value: string;
  onChange: (hex: string) => void;
  /** Optional id for the group's label association. */
  labelId?: string;
}

/**
 * The selection indicator on a swatch — issue #2775.
 *
 * The ring used to be `ring-white`, which resolved to white in BOTH themes:
 * in the light theme the ring and its offset were both white, so the selected
 * swatch dissolved into the dialog it sat in. `ring-text-primary` is the
 * theme-aware step — near-white on the dark dialog, near-black on the light
 * one — and the `ring-offset-bg-overlay` gap keeps the ring from touching the
 * swatch it marks, so the ring reads against the dialog either way.
 *
 * The checkmark is the NON-COLOUR cue: it names the selection without relying
 * on the ring's hue, and its ink is picked for contrast against the swatch
 * itself (`swatchInk`), so a very light custom colour still shows it.
 */
const SELECTED_SWATCH = 'ring-2 ring-offset-2 ring-offset-bg-overlay ring-text-primary';

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
            className={`relative flex h-6 w-6 items-center justify-center rounded-full transition-transform hover:scale-110 ${
              selected ? SELECTED_SWATCH : ''
            }`}
            style={{ backgroundColor: p.hex }}
          >
            {selected && (
              <span
                aria-hidden
                className="text-2xs font-bold leading-none"
                style={{ color: swatchInk(p.hex) }}
              >
                ✓
              </span>
            )}
          </button>
        );
      })}
      {/* Custom colour — the native picker. The wrapper swatch shows the
          current custom hex (or a neutral "+" affordance) and clicking it
          opens the OS colour picker via the overlaid transparent input.

          The focus indicator lives on this wrapper, not on the input: the
          input is `opacity-0`, so its own native focus ring is painted
          transparent, and the wrapper's `overflow-hidden` would clip a ring
          drawn on the input anyway (issue #2776). `has-[:focus-visible]`
          keeps the indicator keyboard-only — a mouse click on the swatch
          focuses the input without claiming a keyboard focus — and the
          accent-cyan outline stays distinguishable from the theme-aware
          selection ring used for the chosen colour (issue #2775). */}
      <label
        title="Custom colour"
        className={`relative h-6 w-6 cursor-pointer rounded-full border border-dashed border-border-default flex items-center justify-center overflow-hidden has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-accent-cyan has-[:focus-visible]:outline-offset-2 ${
          isCustom ? SELECTED_SWATCH : ''
        }`}
        style={isCustom ? { backgroundColor: value } : undefined}
      >
        {isCustom ? (
          <span
            aria-hidden
            className="text-2xs font-bold leading-none"
            style={{ color: swatchInk(value) }}
          >
            ✓
          </span>
        ) : (
          <span className="text-text-muted text-xs leading-none">+</span>
        )}
        <input
          type="color"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          // The custom swatch is the selected one whenever the current colour
          // is not a palette entry, so its accessible name says so — the
          // checkmark is a visual cue and does not reach a screen reader.
          aria-label={isCustom ? 'Custom colour (selected)' : 'Custom colour'}
          className="absolute inset-0 h-full w-full cursor-pointer opacity-0"
        />
      </label>
    </div>
  );
}
