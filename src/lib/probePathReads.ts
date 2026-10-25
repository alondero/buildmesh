import { getGitStatus, nodeChangedFiles } from './tauri/git';
import type { ProbeLoadContext } from './probeResource';

// These reads are shared with path-invalidated clients. They must not import
// tab bodies: sidebar/file-tree consumers use the same cache before Probe opens.
export async function readProjectFiles({ activePath }: ProbeLoadContext) {
  return activePath ? getGitStatus(activePath) : null;
}

export async function readAgentChanges({ nodeId }: ProbeLoadContext) {
  return nodeId == null ? null : nodeChangedFiles(nodeId);
}
