interface PinIconProps {
  className?: string;
  size?: number;
  filled?: boolean;
  strokeWidth?: number;
}

/** Lucide `pin`, shared by the view switcher, node markers and menus. */
export function PinIcon({ className, size = 12, filled = false, strokeWidth = 2 }: PinIconProps) {
  return (
    <svg className={className} width={size} height={size} viewBox="0 0 24 24"
      fill={filled ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth={strokeWidth}
      strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M12 17v5" fill="none" />
      <path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z" />
    </svg>
  );
}

export function PinnedNodeMarker({ onMeshTint = false }: { onMeshTint?: boolean }) {
  return (
    <span role="img" aria-label="Pinned" title="Pinned"
      className={`inline-flex shrink-0 items-center justify-center text-text-secondary${onMeshTint ? ' size-4 rounded-sm bg-bg-card' : ''}`}>
      <PinIcon filled />
    </span>
  );
}
