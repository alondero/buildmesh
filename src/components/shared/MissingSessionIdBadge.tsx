/** Shared status badge for suspended nodes whose provider identity is absent. */
export function MissingSessionIdBadge({ compact = false }: { compact?: boolean }) {
  return (
    <span
      className="text-status-warning text-xs shrink-0"
      aria-label={compact ? 'Missing session ID' : undefined}
      title="This node has no saved session ID. Use Regenerate to start a new conversation in this node."
    >
      {compact ? '⚠' : 'Missing session ID'}
    </span>
  );
}
