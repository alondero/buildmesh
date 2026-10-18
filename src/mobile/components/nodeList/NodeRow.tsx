
import type { AgentNode, NodeCircuitRun, Provider } from '../../api';

import { circuitStatusLine, needsYou } from '../../circuitRun';
import { ProviderIcon } from '../../../components/Providers/ProviderIcon';

import { getNodeStatusConfig } from '../../../lib/status';

export function NodeRow({
  node,
  onClick,
  providers,
  meshName,
  circuitRun,
}: {
  node: AgentNode;
  onClick: () => void;
  providers?: Provider[];
  meshName?: string;
  circuitRun?: NodeCircuitRun;
}) {
  // getNodeStatusConfig (src/lib/status.ts) is total over the SessionStatus union
  // — `archived` included (#788) — and safely falls back to idle on unknown
  // or missing statuses.
  const meta = getNodeStatusConfig(node);
  const needsInput = node.status === "awaiting_input";
  // Single source of truth for the badge + label: the live `listProviders()`
  // payload (issue #328). The fallback (`'?' / '#555'` + raw id) fires when:
  //   * the fetch hasn't resolved yet (`providers === []` initially), or
  //   * the node's provider id isn't in the live list (e.g. a since-removed
  //     harness profile). Both cases get a deterministic grey badge so the
  //     row's left edge still has consistent rhythm.
  // `providerMeta` carries color + icon for the chip and the same `.label`
  // drives the row subtitle — keeps the badge and the label in lockstep.
  const providerMeta = providers?.find(
    (p) => p.id === (node.launch_configuration?.id ?? node.provider),
  );
  const providerLabel =
    node.launch_configuration?.name ?? providerMeta?.label ?? node.provider;
  return (
    <button
      onClick={onClick}
      data-testid={`node-${node.id}`}
      className="card"
      style={needsInput ? { borderColor: "var(--attention-ring)" } : undefined}
    >
      <ProviderIcon
        // `backgroundColor` drives the chip from the live `meta.color`
        // (issue #328). When the fetch hasn't resolved, `ProviderIcon`
        // falls back to its unknown-provider gray (`'#555'`). Icon size
        // `h-4 w-4` keeps the full-bleed brand marks visually balanced
        // with the sparse monochrome glyphs at the established mobile
        // rhythm (16px icon inside the 34×34 chip).
        // `fallbackGlyph` feeds the live `meta.icon` letter back in for a
        // custom Claude-compatible Proxied account (issue #948): its slug
        // has no brand mark, so without this the row shows a bare dot
        // where the pre-#328 badge showed the wire letter.
        providerId={node.provider}
        withBackground
        backgroundColor={providerMeta?.color}
        fallbackGlyph={providerMeta?.icon}
        chipTestId="node-avatar"
        title={providerLabel}
        className="h-4 w-4"
      />
      <div style={{ flex: 1, minWidth: 0 }}>
        <div
          style={{
            fontSize: 14,
            fontWeight: 500,
            color: "var(--text)",
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {node.name}
        </div>
        <div className="node-mesh-name">Mesh: {meshName ?? "Unknown"}</div>
        <div
          style={{
            fontSize: 11,
            color: "var(--text-faint)",
            marginTop: 2,
            display: "flex",
            alignItems: "center",
            gap: 5,
            overflow: "hidden",
            whiteSpace: "nowrap",
          }}
        >
          {providerLabel} ·
          <span
            style={{
              width: 7,
              height: 7,
              borderRadius: "50%",
              background: meta.hex,
              flexShrink: 0,
              display: "inline-block",
            }}
          />
          <span style={{ color: meta.hex }}>{meta.label}</span>
        </div>
        {circuitRun && (
          <div
            className="node-circuit-line"
            data-testid={`node-circuit-${node.id}`}
            data-needs-you={needsYou(circuitRun)}
          >
            {circuitStatusLine(circuitRun)}
          </div>
        )}
      </div>
      <div
        style={{
          width: 3,
          height: 34,
          borderRadius: 2,
          background: meta.hex,
          flexShrink: 0,
        }}
      />
    </button>
  );
}
