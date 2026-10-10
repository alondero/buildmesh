import type { GitHubPageCompleteness } from "../../types/generated/GitHubPageCompleteness";

/**
 * "This list is truncated" — the visible half of issue #1528 / #2024 rank 6.
 *
 * Every GitHub list read is paginated and bounded, so a bare `Vec` could not
 * distinguish "page 1" from "everything": the panel presented the first 100
 * rows as the repository's issues. The backend now reports completeness
 * alongside the items (see `services::github::pagination`), and this renders it
 * rather than letting the list imply a completeness it cannot prove.
 *
 * The wording names the *reason*, because the remedies differ: a safety cap or
 * a search ceiling is a permanent limit of the read, while upstream
 * incompleteness or a cancellation is transient and worth retrying.
 */
export function FeedCompletenessNote({
  completeness,
  label,
}: {
  completeness: GitHubPageCompleteness | null;
  /** Noun phrase for the list, e.g. "issues" or "changed files". */
  label: string;
}) {
  if (!completeness || completeness.complete) return null;

  const shown = completeness.returned;
  const of = completeness.reported_total;
  const reason = describeReason(completeness.incomplete_reason);

  return (
    <div
      data-testid="feed-completeness-note"
      role="status"
      style={{
        padding: "8px 12px",
        marginBottom: 8,
        borderRadius: 6,
        background: "var(--surface-2)",
        color: "var(--text-dim)",
        fontSize: 12,
        lineHeight: 1.4,
      }}
    >
      {of != null
        ? `Showing the first ${shown} of ${of} ${label}.`
        : `Showing the first ${shown} ${label}.`}{" "}
      {reason}
    </div>
  );
}

function describeReason(reason: GitHubPageCompleteness["incomplete_reason"]): string {
  switch (reason) {
    case "safety_cap":
      return "The read hit its page limit, so more exist.";
    case "search_ceiling":
      return "GitHub's search API cannot return more than 1,000 matches for this query.";
    case "upstream_incomplete":
      return "GitHub reported that its own index was still incomplete — try again.";
    case "cancelled":
      return "The read was cancelled before it finished.";
    default:
      return "More results may exist.";
  }
}