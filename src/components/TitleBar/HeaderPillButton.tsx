import type { ReactNode, Ref } from 'react';

/**
 * Shared skeleton for the right-hand utility cluster (Usage, Zoom, Settings,
 * Remote Access, More). Same style vocabulary as the ViewModeSwitcher segments
 * (issue #1609): borderless, card-hover, active cyan — the pills read as
 * part of the same toolbar instead of a separate bordered group.
 *
 * Extracted from `TitleBar.tsx` so the zoom control can reuse the pill
 * without importing the title bar itself (which would cycle: TitleBar
 * renders the zoom control).
 */
export function HeaderPillButton({ icon, label, onClick, title, ariaLabel, active = false, testId, ariaExpanded, ariaHasPopup, ariaControls, buttonRef }: {
  icon: ReactNode;
  label: string;
  onClick: () => void;
  title: string;
  ariaLabel: string;
  active?: boolean;
  testId?: string;
  ariaExpanded?: boolean;
  /** Present for trigger buttons that disclose a popover/menu; omitted for
      the plain navigation pills. */
  ariaHasPopup?: 'dialog' | 'menu' | 'listbox' | 'tree' | 'grid';
  /** Id of the disclosed surface, wired to `aria-controls` when open. */
  ariaControls?: string;
  /** Trigger ref, so a disclosing caller can restore focus on close. */
  buttonRef?: Ref<HTMLButtonElement>;
}) {
  return (
    <button
      ref={buttonRef}
      type="button"
      onClick={onClick}
      data-testid={testId}
      aria-label={ariaLabel}
      aria-expanded={ariaExpanded}
      aria-haspopup={ariaHasPopup}
      aria-controls={ariaControls}
      title={title}
      className={`inline-flex h-9 shrink-0 items-center gap-1.5 px-2 py-1.5 rounded-md text-sm font-sans font-medium transition-colors ${
        active
          ? 'bg-bg-card text-accent-cyan'
          : 'text-text-secondary hover:bg-bg-card hover:text-text-primary'
      }`}
    >
      {icon}
      {/* Always labelled (#2421). The bar no longer degrades pills to bare
          glyphs: below `UTILITIES_IN_BAR_MIN_WIDTH` the utilities fold into
          the More panel instead, and More — the one pill left — keeps its
          name because it is the route to them. */}
      <span className="whitespace-nowrap">{label}</span>
    </button>
  );
}
