import { brandFor } from '../../lib/brandRegistry';

interface ProviderIconProps {
  providerId: string;
  /** Tailwind size class, e.g. "h-3.5 w-3.5" for 14px. Defaults to 12px. */
  className?: string;
  /** Title for accessibility; defaults to the provider id. */
  title?: string;
  /** Wrap the icon in a coloured chip. Unknown providers use neutral grey. */
  withBackground?: boolean;
  /** Pixel size of the chip wrapper when `withBackground` is set. */
  chipSize?: number;
  /** Live wire colour override; otherwise the registered brand colour is used. */
  backgroundColor?: string;
  /** Glyph used instead of the neutral dot when no registered brand exists. */
  fallbackGlyph?: string;
  /** `data-testid` applied to the optional chip wrapper. */
  chipTestId?: string;
}

export function ProviderIcon({
  providerId,
  className = 'h-3 w-3',
  title,
  withBackground,
  chipSize = 34,
  backgroundColor,
  fallbackGlyph,
  chipTestId,
}: ProviderIconProps) {
  const label = title ?? providerId;
  const brand = brandFor(providerId);

  const inner = (() => {
    if (brand?.icon.kind === 'inline') {
      const InlineIcon = brand.icon.component;
      return <InlineIcon className={className} title={label} />;
    }

    if (brand?.icon.kind === 'image') {
      return (
        <img
          src={brand.icon.src}
          alt=""
          title={label}
          className={className}
          draggable={false}
        />
      );
    }

    if (fallbackGlyph) {
      return (
        <span
          aria-hidden="true"
          title={label}
          style={{
            fontSize: Math.round(chipSize * 0.41),
            fontWeight: 700,
            lineHeight: 1,
          }}
        >
          {fallbackGlyph}
        </span>
      );
    }

    return (
      <span
        aria-hidden="true"
        title={label}
        className={`${className} bg-text-muted rounded-full inline-block shrink-0`}
      />
    );
  })();

  if (!withBackground) return inner;

  return (
    <div
      data-testid={chipTestId}
      style={{
        width: chipSize,
        height: chipSize,
        borderRadius: Math.round(chipSize / 4.5),
        background: backgroundColor ?? brand?.chipHex ?? '#555',
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
