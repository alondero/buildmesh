import { createDualKeyCache } from '../lib/pathInvalidatedCache';
import { nodeChangedFiles, type GitStatus } from '../lib/tauri';
import { usePathInvalidatedQuery } from './usePathInvalidatedQuery';

// Agent Changes is keyed by node id, but GIT_CHANGED events arrive with the
// node's working-directory path. The dual-key cache keeps the list shared
// across consumers while subscribing to the correct path for invalidation.
const agentChangedFilesClient = createDualKeyCache<number, GitStatus[]>({
  fetcher: nodeChangedFiles,
  name: 'agentChangedFiles',
  // A busy agent can emit several file events per second. The list is cheap
  // compared with a rendered diff, but it still only needs to converge once
  // per edit burst rather than start one git walk per event.
  minRefetchIntervalMs: 2_000,
});

/**
 * Files changed by an Agent Node since its Base Ref. This deliberately does
 * not fetch diff hunks; those are loaded lazily by the centre diff overlay
 * after a row is clicked.
 */
export function useAgentChangedFiles(
  nodeId: number | null,
  rootPath: string | null,
): {
  files: GitStatus[];
  loading: boolean;
  error: Error | null;
} {
  const { data, loading, error } = usePathInvalidatedQuery(
    agentChangedFilesClient,
    nodeId,
    rootPath,
  );
  return { files: data ?? [], loading, error };
}
