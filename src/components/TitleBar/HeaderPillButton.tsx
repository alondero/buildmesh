import type { ReactNode, Ref } from 'react';

/**
 * Shared skeleton for the right-hand utility cluster (Usage, Zoom, Settings,
 * Remote Access). Same style vocabulary as the ViewModeSwitcher segments
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
      {/* Icon-only below 1400px window width — unified with the
          switcher's ladder (issue #1609; previously the pills dropped at
          1150px, so between the two tiers the bar mixed labelled
          segments with icon pills). The threshold moved from 1300px to
          1400px to avoid a 2px clip on the rightmost ViewModeSwitcher
          segment ("Filtered") at exactly 1300px — at 1300px the labels
          become visible but the centre's `w-80` (260px at the 13px root)
          plus the side clusters' min-content (~565px each) overflows
          the side tracks (PR #1623 review). The aria-label above keeps
          the accessible name stable. */}
      <span className="max-[1399px]:hidden">{label}</span>
    </button>
  );
}
