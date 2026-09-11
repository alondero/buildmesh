import type { ReactNode } from 'react';
import { InfoTip } from '../shared/InfoTip';

interface SettingsRowProps {
  /** Setting name. Rendered as the row's label. */
  label: string;
  /** Short one-line description shown under the label; full text goes in `details`. */
  summary?: string;
  /** Full explanation, revealed behind the ⓘ affordance. Omit for a self-evident row. */
  details?: ReactNode;
  /** When set, the label is associated with this control id (label↔control a11y). */
  htmlFor?: string;
  /** The control. */
  children: ReactNode;
  /** `inline` puts the control in a fixed right column; `stacked` places it under the label. */
  layout?: 'inline' | 'stacked';
  /** Override the inline control column width. */
  controlClassName?: string;
}

/**
 * One compact settings row: label (+ optional ⓘ help) and a one-line summary on
 * the left, control on the right. Replaces the old three-block pattern (label →
 * multi-line paragraph → control) that forced heavy scrolling in the Settings
 * modal — long help lives behind the InfoTip instead.
 */
export function SettingsRow({
  label,
  summary,
  details,
  htmlFor,
  children,
  layout = 'inline',
  controlClassName = 'w-72 shrink-0',
}: SettingsRowProps) {
  const header = (
    <div className="min-w-0">
      <div className="flex items-center gap-1.5">
        {htmlFor ? (
          <label htmlFor={htmlFor} className="text-base font-medium text-text-secondary">
            {label}
          </label>
        ) : (
          <span className="text-base font-medium text-text-secondary">{label}</span>
        )}
        {details && <InfoTip label={label}>{details}</InfoTip>}
      </div>
      {summary && <p className="mt-0.5 text-sm text-text-muted line-clamp-2">{summary}</p>}
    </div>
  );

  if (layout === 'stacked') {
    return (
      <div className="py-3.5">
        {header}
        <div className="mt-3">{children}</div>
      </div>
    );
  }

  return (
    <div className="flex items-start justify-between gap-6 py-3.5">
      {header}
      <div className={controlClassName}>{children}</div>
    </div>
  );
}

/**
 * Groups related rows under a heading, separated from the previous section by a
 * rule. The optional `description` is surfaced through the same ⓘ affordance so
 * section intros don't eat vertical space.
 */
export function SettingsSection({
  title,
  description,
  testId,
  children,
}: {
  title: string;
  description?: ReactNode;
  testId?: string;
  children: ReactNode;
}) {
  return (
    <section
      className="border-t border-border-subtle pt-6 first:border-t-0 first:pt-0"
      data-testid={testId}
    >
      <div className="mb-1 flex items-center gap-1.5">
        <h3 className="text-lg font-semibold text-text-primary">{title}</h3>
        {description && <InfoTip label={title}>{description}</InfoTip>}
      </div>
      {children}
    </section>
  );
}
