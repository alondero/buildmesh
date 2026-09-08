/** Shared status badge for suspended nodes whose provider identity is absent.
 *
 * Mirrors `SignalHealthBadge`'s shape: inline-flex glyph + short label, with
 * the full explanation in the tooltip. The accessible name stays descriptive
 * in both tiers ("Missing session ID" — it explains *why* resume failed);
 * only the visible label is shortened, because a missing session ID is a
 * resume-failure reason, not a headline, and does not need prominent pixels.
 */
export function MissingSessionIdBadge({ compact = false }: { compact?: boolean }) {
  return (
    <span
      role="img"
      aria-label="Missing session ID"
      title="This node has no saved session ID. Use Regenerate to start a new conversation in this node."
      className="inline-flex shrink-0 items-center gap-1 text-xs text-status-warning"
    >
      <span aria-hidden="true">⚠</span>
      {!compact && <span>No session</span>}
    </span>
  );
}
