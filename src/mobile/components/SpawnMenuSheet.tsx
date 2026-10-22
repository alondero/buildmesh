/**
 * The Spawn Menu, as a phone bottom sheet (issue #2385).
 *
 * The sheet is a list of the backend's menu rows in the backend's own order
 * (ADR-0016). Picking a row starts the launch straight away: the split
 * button *is* the confirmation, as on the desktop and the Android app.
 */
import { Sheet } from "../ui";
import type { LaunchChoice } from "../spawnMenu";

export function SpawnMenuSheet({
  label,
  choices,
  error,
  notice,
  onRetry,
  onClose,
  onChoose,
  dismissible = true,
  testId = "spawn-menu",
}: {
  /** The sheet's heading and accessible name. */
  label: string;
  /** The menu rows, or `null` while the read is in flight. */
  choices: LaunchChoice[] | null;
  /** Why the read failed, if it did. */
  error: string | null;
  /**
   * A launch that failed. The sheet stays open so the user can pick again,
   * so the failure has to be reported *here* — behind the sheet's backdrop
   * the screen's own error line would be unreadable.
   */
  notice?: string | null;
  /** Retries a failed read. */
  onRetry?: () => void;
  /** Closes the sheet. */
  onClose: () => void;
  /** Starts the launch for the picked row. */
  onChoose: (choice: LaunchChoice) => void;
  /**
   * Whether the sheet can be dismissed right now. A launch is in flight once
   * a row is picked, and the sheet must not be dismissed out from under it.
   */
  dismissible?: boolean;
  /** Test id for the sheet. */
  testId?: string;
}) {
  return (
    <Sheet label={label} testId={testId} onClose={onClose} dismissible={dismissible}>
      <h3>{label}</h3>
      {notice && <p role="alert" className="inline-error">{notice}</p>}
      {choices === null && !error && (
        <p role="status">Loading launch configurations…</p>
      )}
      {error && (
        <p role="alert">
          {error}
          {onRetry && (
            <button className="btn-ghost" onClick={onRetry}>
              Retry
            </button>
          )}
        </p>
      )}
      {choices !== null && choices.length === 0 && !error && (
        <p>No launch configuration can take this prompt.</p>
      )}
      {choices !== null && choices.length > 0 && (
        <div className="spawn-menu">
          {choices.map((choice) => (
            <button
              key={choice.id}
              type="button"
              className="spawn-menu-item"
              data-testid={`${testId}-${choice.id}`}
              onClick={() => onChoose(choice)}
            >
              {choice.label}
            </button>
          ))}
        </div>
      )}
    </Sheet>
  );
}
