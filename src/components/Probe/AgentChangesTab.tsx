/**
 * AgentChangesTab — issue #376. The Probe Panel's 🔍 tab body.
 *
 * Wraps the existing `AgentReviewPanel` (ADR 0005 — stacked-diff review
 * surface) with the focused agent node's id and resolved path. By the time
 * this component mounts, `ProbeTabBody` has already guaranteed
 * `activeNodeId` is non-null (otherwise it would have shown the
 * "no active agent node" empty state), so the assertion below never
 * fires at runtime.
 *
 * The review surface itself is unchanged from PR #170 — every file the
 * agent changed since branching, sticky summary bar, jump-to-file index
 * built from the per-file sticky headers, and a collapsible FileTree for
 * opening any (even unchanged) file in the editor. The only thing this
 * component adds is the binding to the Probe context.
 */

import { AgentReviewPanel } from '../FileTree/AgentReviewPanel';
import { useProbeContext } from '../../hooks/useProbeContext';

export function AgentChangesTab() {
  const { activeNodeId, activePath } = useProbeContext();

  // ProbeTabBody gates on `activeNodeId !== null` before mounting this
  // component, so the assertion is a type-narrowing convenience rather
  // than a runtime guard.
  if (activeNodeId === null || activePath === null) return null;

  return <AgentReviewPanel nodeId={activeNodeId} rootPath={activePath} />;
}
