/**
 * Box with an arrow leaving it. The mark for a control that opens the
 * system browser (`openUrl`). Drawn as an SVG so the stroke fills its
 * box — the `↗` character spends a full em on empty side bearings and
 * reads as a trend arrow.
 *
 * Decorative. The surrounding control carries the accessible name.
 * `width` and `height` are 12 so the mobile SPA, which does not load
 * Tailwind, still sizes the glyph. A desktop `className` overrides that.
 */
export function OpenExternalIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      width={12}
      height={12}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
      <polyline points="15 3 21 3 21 9" />
      <line x1="10" y1="14" x2="21" y2="3" />
    </svg>
  );
}
