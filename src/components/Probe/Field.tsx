/**
 * Field — label/control rhythm wrapper for Probe tabs.
 *
 * Lifted from `MeshPropertiesTab` (wayfinder #990 ticket #994) when a
 * second form tab (`AutopilotProbeTab`) needed the same `htmlFor`/`id`
 * wiring — the same lift pattern as `ProbeTabBody` (issue #842),
 * `ProbeToolbar` (issue #813), and `SaveIndicator` (issue #729).
 * The `htmlFor`↔`id` association is what lets `getByLabelText`
 * resolve the control in tests and click-to-focus the label work for
 * keyboard users. The hint ships as a dimmed suffix on the label line
 * so the rhythm stays single-line — long hints go in a paragraph
 * after the control, not in this wrapper.
 */
import type { ReactNode } from 'react';

export interface FieldProps {
  label: string;
  htmlFor: string;
  hint?: string;
  children: ReactNode;
}

export function Field({ label, htmlFor, hint, children }: FieldProps) {
  return (
    <div>
      <label htmlFor={htmlFor} className="block text-xs text-text-muted mb-1">
        {label}
        {hint && <span className="text-text-muted/60"> ({hint})</span>}
      </label>
      {children}
    </div>
  );
}
