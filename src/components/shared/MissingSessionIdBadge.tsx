/** Shared status badge for suspended nodes whose provider identity is absent. */
export function MissingSessionIdBadge() {
  return (
    <span
      className="text-status-warning text-xs shrink-0"
      title="This node has no saved session ID. Use Regenerate to start a new conversation in this node."
    >
      Missing session ID
    </span>
  );
}
