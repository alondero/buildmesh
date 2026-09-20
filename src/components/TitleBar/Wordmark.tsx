/**
 * The Buildmesh lockup — Relay badge plus wordmark.
 *
 * Inline SVG and real text rather than a baked raster, so the wordmark follows
 * the active theme through the same `--color-*` tokens as the rest of the
 * chrome. A raster cannot: this bar sits on `bg-bg-surface`, which is `#ffffff`
 * in the light theme, and the previous near-white artwork disappeared into it.
 * Because the tokens resolve in CSS, a theme flip costs no JavaScript — there
 * is no subscription to clean up and nothing to keep in sync.
 *
 * Sizes are the lockup's own proportions (`docs/brand/b3-relay-lockup-*.svg`)
 * scaled to the title bar's `h-10` (32.5px at the 13px root): the mark is 88
 * units of the 96-unit box, and the 42px wordmark is 42/96 of the same box.
 */

/** Relay mark. Baked colours, not tokens: the badge carries its own dark plate
    so the neon wires stay legible against either theme's chrome. */
function RelayMark({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 96 96" className={className} fill="none" aria-hidden="true">
      <rect x="4" y="4" width="88" height="88" rx="25" fill="#16161d" />
      <path
        d="M48 5.25 H29 A23.75 23.75 0 0 0 5.25 29 V67 A23.75 23.75 0 0 0 29 90.75 H48"
        stroke="#00d4ff"
        strokeWidth="2.5"
      />
      <path
        d="M48 5.25 H67 A23.75 23.75 0 0 1 90.75 29 V67 A23.75 23.75 0 0 1 67 90.75 H48"
        stroke="#22c55e"
        strokeWidth="2.5"
      />
      <g stroke="#00d4ff" strokeWidth="5.5" strokeLinecap="round">
        <path d="M21 71 L30 28" />
        <path d="M30 28 L48 53" />
      </g>
      <g stroke="#22c55e" strokeWidth="5.5" strokeLinecap="round">
        <path d="M48 53 L66 28" />
        <path d="M66 28 L75 71" />
      </g>
      <circle cx="21" cy="71" r="6.5" fill="#00d4ff" />
      <circle cx="30" cy="28" r="8.5" fill="#00d4ff" />
      <circle cx="48" cy="53" r="7.5" fill="#8b5cf6" />
      <circle cx="66" cy="28" r="8.5" fill="#22c55e" />
      <circle cx="75" cy="71" r="6.5" fill="#22c55e" />
    </svg>
  );
}

export function Wordmark() {
  return (
    // The children are `pointer-events-none` so this wrapper stays the element
    // under the pointer. Tauri's drag script only checks the target
    // (`e.target.hasAttribute('data-tauri-drag-region')`), and the SVG's
    // painted nodes or the wordmark span would otherwise claim that role and
    // silently stop the bar dragging over the logo.
    <span
      data-tauri-drag-region
      role="img"
      aria-label="Buildmesh"
      className="flex h-10 shrink-0 select-none items-center gap-[9px]"
    >
      <RelayMark className="pointer-events-none h-[30px] w-[30px]" />
      <span className="pointer-events-none text-[14px] font-extrabold leading-none tracking-[-0.03em]">
        <span className="text-text-primary">build</span>
        <span className="text-accent-cyan">mesh</span>
      </span>
    </span>
  );
}
