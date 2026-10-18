import type { NodeInputRequest } from '../../../lib/status';

export function NodeRequestPreview({ nodeId, request }: { nodeId: number; request: NodeInputRequest }) {
  return (
        <div
          data-testid={`attn-prompt-${nodeId}`}
          style={{
            fontSize: 12,
            color: "var(--amber)",
            marginTop: 8,
            textAlign: "left",
            display: "-webkit-box",
            WebkitLineClamp: 2,
            WebkitBoxOrient: "vertical",
            overflow: "hidden",
            overflowWrap: "anywhere",
          }}
        >
          {request.message ?? "Waiting for the agent's prompt…"}
        </div>
  );
}
