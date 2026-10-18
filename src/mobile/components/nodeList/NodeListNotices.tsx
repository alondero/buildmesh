
import type { AgentNode } from '../../api';

export function NodeListNotices({ staleNotice, setStaleNotice, error, setError, committed, setCommitted, onOpenNode }: {
  staleNotice: boolean; setStaleNotice: (value: boolean) => void; error: string | null; setError: (value: string | null) => void;
  committed: AgentNode | null; setCommitted: (node: AgentNode | null) => void; onOpenNode: (node: AgentNode) => void;
}) {
  return <>
      {staleNotice && (
        <div className="toast" data-testid="stale-notice">
          <span style={{ flex: 1 }}>
            The desktop could not read its current state. Showing the last known list.
          </span>
          <button
            onClick={() => setStaleNotice(false)}
            aria-label="Dismiss"
            style={{
              background: "none",
              border: "none",
              color: "var(--text-faint)",
              cursor: "pointer",
              fontSize: 18,
              lineHeight: 1,
              padding: 0,
            }}
          >
            ×
          </button>
        </div>
      )}

      {error && (
        <div className="toast error" data-testid="create-error">
          <span style={{ flex: 1 }}>{error}</span>
          {/* The node exists but is not running: give the user a way through
              to it that does not depend on the toast still being there. */}
          {committed && (
            <button className="btn-ghost" onClick={() => onOpenNode(committed)}>
              Open
            </button>
          )}
          <button
            onClick={() => {
              setError(null);
              setCommitted(null);
            }}
            aria-label="Dismiss"
            style={{
              background: "transparent",
              border: "none",
              color: "inherit",
              fontSize: 18,
              cursor: "pointer",
              padding: "0 4px",
            }}
          >
            ×
          </button>
        </div>
      )}

  </>;
}
