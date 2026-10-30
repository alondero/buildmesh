/**
 * The desktop's spawn split button, on the phone (issue #2385).
 *
 * The desktop's `SpawnButtonCluster` pairs a quick action with a chevron that
 * opens the Spawn Menu; the phone renders the same pair. The main half runs
 * the surface's default launch, and the chevron half opens the menu so the
 * user can pick a different Spawn Option or saved Launch Configuration.
 *
 * The main half carries the **default provider's brand mark** beside its
 * label, the same mark the desktop's quick-spawn half shows (issue #2040): the
 * icon is how the user tells *which* provider the default half will run before
 * they tap it. The mark is drawn from the mesh's pinned default Spawn Option
 * only — an app-wide default is invisible to the phone, so an unpinned mesh
 * shows the **Mesh default** label with no mark rather than a wrong one.
 *
 * The menu itself is *not* rendered here: it is a bottom sheet, and the
 * sheet is anchored to the app root rather than to the card the button sits
 * in. The surface that owns the button therefore owns the sheet too, and
 * passes the open state down so the chevron can advertise it.
 */
import type { Provider } from "../api";
import { ProviderIcon } from "../../components/Providers/ProviderIcon";

export function SpawnSplitButton({
  label,
  busyLabel,
  provider,
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
  /**
   * The mesh's pinned default Spawn Option, when the phone can resolve one.
   * Its brand mark is drawn beside the label; `null` leaves the label alone.
   */
  provider?: Provider | null;
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
        {busy ? (
          <span className="spawn-split-label">{busyLabel ?? "Starting…"}</span>
        ) : (
          <>
            {provider && (
              <span className="spawn-split-icon">
                <ProviderIcon
                  // A proxied row's mark is the account's, a native row's the
                  // harness's — the same key the desktop's cluster and the
                  // phone's provider picker use.
                  providerId={provider.provider_id ?? provider.harness_id}
                  // The wire glyph stands in for a harness with no registered
                  // brand mark, the way the phone's node rows and provider
                  // picker do.
                  fallbackGlyph={provider.icon}
                  title={provider.label}
                  className="spawn-split-brand"
                />
              </span>
            )}
            <span className="spawn-split-label">{label}</span>
          </>
        )}
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
