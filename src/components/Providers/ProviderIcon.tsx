import minimaxLogo from '../../assets/providers/minimax.svg';
import antigravityLogo from '../../assets/providers/antigravity.png';

// Brand-coloured assets stay as <img> — the colour is baked in and would be
// distorted by `currentColor` inheritance. The dark SVG assets are inlined
// as components so their `fill="currentColor"` picks up the surrounding
// text colour (otherwise they render black-on-black on the dark UI).

function ClaudeCodeIcon({ className, title }: { className?: string; title?: string }) {
  return (
    <svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg" className={className} aria-hidden="true">
      <title>{title}</title>
      <path fill="currentColor" d="M21 10.5h3v3h-3v3h-1.5v3H18v-3h-1.5v3H15v-3H9v3H7.5v-3H6v3H4.5v-3H3v-3H0v-3h3v-6h18Zm-15 0h1.5v-3H6Zm10.5 0H18v-3h-1.5z" />
    </svg>
  );
}

function OpenAIIcon({ className, title }: { className?: string; title?: string }) {
  // ViewBox covers the full bounds of the swirl path extracted from the
  // 1180x320 wordmark (roughly 306x320). The earlier square viewBox was
  // clipping the top/bottom of the symbol.
  return (
    <svg viewBox="0 0 306 320" xmlns="http://www.w3.org/2000/svg" className={className} aria-hidden="true">
      <title>{title}</title>
      <path fill="currentColor" d="M123.2,118.3V85c0-2.2,0.6-3.8,2.9-5.1L187.9,44c8.3-4.8,18.9-7,29.2-7c39.1,0,63.8,30.1,63.8,62.5c0,2.6,0,6.1-0.6,9l-64.7-37.8c-3.2-1.9-6.7-2.2-10.6,0L123.2,118.3z M266.1,236.6v-74c0-4.2-1.6-7-5.4-9.3l-82-47.7l28.8-16.7c1.6-1,4.2-1,5.8,0l62.2,35.9c17.6,10.3,29.8,32.7,29.8,54.1C305.2,204.2,289.8,227.6,266.1,236.6z M106.2,172.8l-28.5-17c-2.2-1.3-2.9-2.9-2.9-5.1V79.3c0-34.9,26.6-61.2,62.8-61.2c14.1,0,27.6,4.8,38.4,13.5L111.7,69c-3.8,2.2-5.4,5.1-5.4,9.3V172.8z M162,204.9l-38.8-21.8v-46.1l38.8-21.8l38.4,21.8v46.1L162,204.9z M186,301.9c-14.1,0-27.6-4.8-38.4-13.5L212,251c3.8-2.2,5.4-5.1,5.4-9.3v-94.5l28.8,17c2.2,1.3,2.9,2.9,2.9,5.1v71.5C249.1,275.7,222.2,301.9,186,301.9z M110.4,231.1l-62.2-35.9c-17.6-10.3-29.8-32.7-29.8-54.1c0-25.6,15.7-48.7,39.4-57.7v74.3c0,4.2,1.6,7,5.4,9.3l81.7,47.4l-28.8,16.7C114.6,232.1,112,232.1,110.4,231.1z M106.5,283c-36.8,0-63.8-27.6-63.8-61.8c0-3.2,0.3-6.4,0.6-9.3l64.4,37.2c3.8,2.2,7,2.2,10.9,0l81.7-47.4V235c0,2.2-0.6,3.8-2.9,5.1L135.7,276C127.4,280.8,116.8,283,106.5,283z M186,319.2c38.4,0,70.5-27.6,77.5-64.1c35.9-9,59-42.3,59-76.3c0-22.4-9.6-43.9-27.2-59.6c1.6-6.7,2.9-13.8,2.9-20.5c0-45.2-36.8-79.1-79.1-79.1c-8.7,0-17.3,1.6-25.6,4.5C179,9.7,159.4,0.8,137.6,0.8c-38.4,0-70.5,27.6-77.5,64.1c-35.9,9-59,42.3-59,76.3c0,22.4,9.6,43.9,27.2,59.6c-1.6,6.7-2.9,13.8-2.9,20.5c0,45.2,36.8,79.1,79.1,79.1c8.7,0,17.3-1.6,25.6-4.5C144.7,310.3,164.2,319.2,186,319.2z" />
    </svg>
  );
}

function OpenCodeIcon({ className, title }: { className?: string; title?: string }) {
  // The original mark uses two greys; we keep that contrast with two
  // `currentColor` paths at different opacities.
  return (
    <svg viewBox="0 0 240 300" xmlns="http://www.w3.org/2000/svg" className={className} aria-hidden="true">
      <title>{title}</title>
      <path d="M180 240H60V120H180V240Z" fill="currentColor" fillOpacity="0.4" />
      <path d="M180 60H60V240H180V60ZM240 300H0V0H240V300Z" fill="currentColor" />
    </svg>
  );
}

function KimiIcon({ className, title }: { className?: string; title?: string }) {
  return (
    <svg viewBox="0 0 24 25" xmlns="http://www.w3.org/2000/svg" className={className} aria-hidden="true">
      <title>{title}</title>
      <path fill="currentColor" d="M21.72 0.94C22.95 0.94 23.95 1.94 23.95 3.17C23.95 4.4 22.95 5.4 21.72 5.4H19.75C19.6 5.4 19.49 5.28 19.49 5.14V3.17C19.49 1.94 20.49 0.94 21.72 0.94Z" />
      <path fill="currentColor" d="M9.39 13.95L17.82 5.59C17.98 5.43 17.89 5.12 17.68 5.12H13.14C13.14 5.12 13.04 5.14 13 5.18L3.92 14.19C3.78 14.33 3.57 14.21 3.57 13.98V5.39C3.57 5.24 3.47 5.12 3.35 5.12H0.22C0.1 5.12 0 5.24 0 5.39V23.92C0 24.07 0.1 24.19 0.22 24.19H3.35C3.47 24.19 3.57 24.07 3.57 23.92V20.14C3.57 20.06 3.6 19.98 3.65 19.93L6.47 17.14C6.54 17.07 6.63 17.06 6.71 17.11L14.24 22.65C15.47 23.48 16.85 23.99 18.25 24.14C18.37 24.15 18.48 24.03 18.48 23.87V20.31C18.48 20.17 18.4 20.06 18.29 20.05C17.47 19.92 16.66 19.6 15.94 19.11L9.42 14.39C9.28 14.3 9.27 14.07 9.39 13.95Z" />
    </svg>
  );
}

function TerminalIcon({ className, title }: { className?: string; title?: string }) {
  // A filled `>` chevron + a small `_` cursor block, both placed so the
  // cursor sits directly to the right of the chevron tip (the prior
  // version dropped the cursor in the viewBox's bottom-right corner
  // where it rendered as a sub-pixel smudge at the 12-14px sizes used
  // in the sidebar/dropdown). Monochrome via `currentColor` so it picks
  // up the surrounding text colour like the other inline icons.
  return (
    <svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg" className={className} aria-hidden="true">
      <title>{title}</title>
      <path fill="currentColor" d="M3 5l9 7-9 7V5z" />
      <path fill="currentColor" d="M14 17h7v3h-7z" />
    </svg>
  );
}

type InlineIconProps = { className?: string; title?: string };
const INLINE_ICONS: Record<string, (props: InlineIconProps) => React.JSX.Element> = {
  anthropic: ClaudeCodeIcon,
  // Startup auto-detection (detection.rs) registers Claude Code with the
  // profile id "claude", so the harness-profile rows and any node spawned with
  // it key off "claude" — not the legacy "anthropic" id. Both must resolve to
  // the Claude Code mark or the row falls through to the gray-dot fallback
  // (#534). Custom Claude-compatible providers (their own slug id) still fall
  // back today — branded icons for those land with the provider redesign.
  claude: ClaudeCodeIcon,
  codex: OpenAIIcon,
  opencode: OpenCodeIcon,
  kimi: KimiIcon,
  terminal: TerminalIcon,
};

const COLORED_IMAGES: Record<string, string> = {
  minimax: minimaxLogo,
  agy: antigravityLogo,
};

// Single source of truth for the avatar-chip background colour. Mirrors
// the backend's `UiMeta::color` field in src-tauri/.../adapters/*.rs#ui().
// Used by the mobile `NodeRow` (via the `withBackground` prop) and by
// any other surface that needs the provider's brand colour as a fill.
const PROVIDER_CHIP_COLORS: Record<string, string> = {
  anthropic: '#1d7cfc',
  claude:    '#1d7cfc', // Detected Claude Code profile id (#534) — same brand blue.
  minimax:   '#6366f1',
  kimi:      '#00c4c4',
  agy:       '#10b981',
  opencode:  '#f59e0b',
  terminal:  '#9ca3af',
  codex:     '#10a37f',
};

interface ProviderIconProps {
  providerId: string;
  /** Tailwind size class, e.g. "h-3.5 w-3.5" for 14px. Defaults to 12px. */
  className?: string;
  /** Title for accessibility; defaults to the provider id. */
  title?: string;
  /**
   * Wrap the icon in a 34×34 colored chip. The mobile `NodeRow` uses
   * this for the per-node avatar; the desktop sidebar/grid/header/
   * settings uses the bare icon (no chip). Unknown providers get a
   * neutral gray chip so the row's left edge still has consistent
   * rhythm.
   */
  withBackground?: boolean;
  /**
   * `data-testid` applied to the chip wrapper when `withBackground` is
   * set. Optional — the chip itself is purely presentational.
   */
  chipTestId?: string;
}

export function ProviderIcon({
  providerId,
  className = 'h-3 w-3',
  title,
  withBackground,
  chipTestId,
}: ProviderIconProps) {
  const label = title ?? providerId;

  const InlineIcon = INLINE_ICONS[providerId];
  const inner = (() => {
    if (InlineIcon) {
      return <InlineIcon className={className} title={label} />;
    }
    const src = COLORED_IMAGES[providerId];
    if (src) {
      return (
        <img
          src={src}
          alt=""
          title={label}
          className={className}
          draggable={false}
        />
      );
    }
    // Fallback: a neutral dot for unknown providers.
    return <span aria-hidden="true" title={label} className={`${className} bg-gray-500 rounded-full inline-block shrink-0`} />;
  })();

  if (!withBackground) return inner;

  return (
    <div
      data-testid={chipTestId}
      style={{
        width: 34,
        height: 34,
        borderRadius: 8,
        background: PROVIDER_CHIP_COLORS[providerId] ?? '#555',
        color: '#fff',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        flexShrink: 0,
      }}
    >
      {inner}
    </div>
  );
}
