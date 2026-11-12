
import type { Mesh } from '../../api';

import { Sheet } from '../../ui';

export function MeshActionsSheet({
  mesh,
  onClose,
  onOpenAgentNodes,
  onOpenIssues,
  onOpenPulls,
}: {
  mesh: Mesh;
  onClose: () => void;
  onOpenAgentNodes: () => void;
  onOpenIssues: () => void;
  onOpenPulls?: () => void;
}) {
  return (
    // The sheet's `label` becomes the dialog's accessible name. Without it
    // the dialog announced no name, so a screen-reader user could not tell
    // which mesh the actions belonged to (issue #2776). Naming it with the
    // mesh identity is the supported Sheet API — `aria-labelledby` on the
    // visible heading would announce the bare mesh name instead.
    <Sheet
      onClose={onClose}
      testId="mesh-actions-sheet"
      label={`Actions for ${mesh.name}`}
    >
      <h3
        style={{
          fontSize: 13,
          fontWeight: 600,
          color: "var(--text-dim)",
          textTransform: "uppercase",
          letterSpacing: "0.05em",
          margin: 0,
          marginBottom: 12,
        }}
      >
        {mesh.name}
      </h3>
      {onOpenPulls && <SheetButton onClick={onOpenPulls} testId="mesh-sheet-pulls" label="Pull requests" hint="See open PR status" />}
      <SheetButton
        onClick={onOpenAgentNodes}
        testId="mesh-sheet-discovered-nodes"
        label="Archive"
        hint="Resume an existing CLI session"
      />
      <SheetButton
        onClick={onOpenIssues}
        testId="mesh-sheet-issues"
        label="GitHub Issues"
        hint="Spawn an agent prefilled with an issue"
      />
    </Sheet>
  );
}

function SheetButton({
  onClick,
  testId,
  label,
  hint,
}: {
  onClick: () => void;
  testId: string;
  label: string;
  hint: string;
}) {
  return (
    <button
      onClick={onClick}
      data-testid={testId}
      className="card"
      style={{ display: "block", background: "var(--surface-2)" }}
    >
      <div style={{ fontSize: 14, fontWeight: 500, color: "var(--text)" }}>
        {label}
      </div>
      <div style={{ fontSize: 12, color: "var(--text-dim)", marginTop: 2 }}>
        {hint}
      </div>
    </button>
  );
}
