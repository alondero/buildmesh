/**
 * ProbeSection — the labelled-section wrapper for the destinations that
 * separate *configuration* from *maintenance* (issue #1460).
 *
 * Before the split, `MeshPropertiesTab` and `WorktreeManagerTab` were two
 * undifferentiated column stacks: a field appeared below another field with
 * nothing saying whether it was a preference, a strategy decision, a
 * recovery action, or a destructive one. This primitive gives both
 * destinations the same spine — a heading, an optional one-line explanation,
 * and a body — so "Project Settings has clear sections" and "Repository
 * distinguishes health/recovery from cleanup" are one rendering rule rather
 * than two hand-drawn arrangements.
 *
 * The dock's 240px minimum (`PROBE_PANEL_BOUNDS`) is why the heading uses
 * `break-words` and the description wraps: `truncate` here would hide the
 * tail that carries the scope (which path, which risk). See
 * `docs/development/probe-ui-checklist.md`.
 *
 * `tone="danger"` is the one visual escalation. It exists because the issue
 * requires Delete Mesh to be a *clearly labelled* danger zone rather than a
 * red button at the bottom of a form — the label is the affordance, and it
 * is asserted in the tests.
 */

import type { ReactNode } from 'react';

export interface ProbeSectionProps {
  /** Section heading. Read as the group's user-facing name, not a field label. */
  title: string;
  /** Optional one-line scope/risk explanation under the heading. */
  description?: ReactNode;
  /** `danger` marks a destructive group (Delete Mesh). */
  tone?: 'default' | 'danger';
  /** Test hook for the section wrapper; also the group a11y label. */
  testId?: string;
  children: ReactNode;
}

export function ProbeSection({
  title,
  description,
  tone = 'default',
  testId,
  children,
}: ProbeSectionProps) {
  const danger = tone === 'danger';
  return (
    <section
      aria-label={title}
      data-testid={testId}
      data-tone={tone}
      className={`rounded-md border p-3 space-y-3 ${
        danger ? 'border-status-error/40 bg-status-error/5' : 'border-border-subtle'
      }`}
    >
      <header className="space-y-1">
        <h3
          className={`text-2xs uppercase tracking-wide break-words ${
            danger ? 'text-status-error' : 'text-text-muted'
          }`}
        >
          {title}
        </h3>
        {description && (
          <p className="text-2xs text-text-muted break-words">{description}</p>
        )}
      </header>
      {children}
    </section>
  );
}

/**
 * A muted line stating WHICH path a destination acts on. Issue #1460
 * requires mesh-root versus active-node semantics to be explicit rather
 * than inferred: the Repository destination walks the project root, and
 * Project Settings edits the project root, so both say so in the body —
 * the inspector header's `detailLabel` covers the common case but is the
 * first thing lost when the dock is narrow.
 */
export function ProbeScopeNote({ children }: { children: ReactNode }) {
  return (
    <p className="text-2xs text-text-muted break-words" data-testid="probe-scope-note">
      {children}
    </p>
  );
}
