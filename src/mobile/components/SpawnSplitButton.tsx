/**
 * The desktop's spawn split button, on the phone (issue #2385).
 *
 * The desktop's `SpawnButtonCluster` pairs a quick action with a chevron that
 * opens the Spawn Menu; the phone renders the same pair. The main half runs
 * the surface's default launch, and the chevron half opens the menu so the
 * user can pick a different Spawn Option or saved Launch Configuration.
 *
 * The menu itself is *not* rendered here: it is a bottom sheet, and the
 * sheet is anchored to the app root rather than to the card the button sits
 * in. The surface that owns the button therefore owns the sheet too, and
 * passes the open state down so the chevron can advertise it.
 */
export function SpawnSplitButton({
  label,
  busyLabel,
  onStart,
  onToggleMenu,
  menuOpen,
  disabled,
  busy,
  testId,
  ariaLabel,
  menuLabel,
  compact,
}: {
  /** The main half's label — the mesh's default launch, or the action. */
  label: string;
  /** The main half's label while its launch is in flight. */
  busyLabel?: string;
  /** Runs the surface's default launch. */
  onStart: () => void;
  /** Opens or closes the Spawn Menu sheet. */
  onToggleMenu: () => void;
  /** Whether the Spawn Menu sheet is open. */
  menuOpen: boolean;
  /** Disables both halves. */
  disabled?: boolean;
  /** This button's own launch is in flight. */
  busy?: boolean;
  /** Test id for the main half. */
  testId: string;
  /** Accessible name for the main half. */
  ariaLabel?: string;
  /** Accessible name for the chevron half. */
  menuLabel?: string;
  /**
   * Keeps the pair at its natural width instead of filling the row — used
   * where the button sits beside other, equally-weighted controls.
   */
  compact?: boolean;
}) {
  const inert = Boolean(disabled || busy);
  return (
    <div className={compact ? "spawn-split spawn-split-compact" : "spawn-split"}>
      <button
        type="button"
        className="spawn-split-main"
        data-testid={testId}
        onClick={onStart}
        disabled={inert}
        aria-label={ariaLabel}
      >
        {busy ? (busyLabel ?? "Starting…") : label}
      </button>
      <button
        type="button"
        className="spawn-split-toggle"
        data-testid={`${testId}-menu`}
        onClick={onToggleMenu}
        disabled={inert}
        aria-haspopup="dialog"
        aria-expanded={menuOpen}
        aria-label={menuLabel ?? "Choose a launch configuration"}
      >
        <span aria-hidden="true">▾</span>
      </button>
    </div>
  );
}
