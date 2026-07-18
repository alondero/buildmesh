/**
 * CenterDiffOverlay — issue #379 (PRD #372).
 *
 * A full-height, full-width diff viewer that temporarily overlays the center
 * terminal grid when the user clicks a changed file in the Probe. Where the
 * Probe's 🔍 review tab stacks *every* changed file into its narrow 360px body,
 * this overlay gives one file the whole center workspace for a spacious read —
 * "playlist-style" review: the Probe stays open and interactive on the right,
 * so clicking another file just swaps the diff in place (the parent
 * `SessionView` re-mounts us with the new `diff` prop).
 *
 * The terminal grid behind the overlay is only *covered*, never torn down — the
 * `TerminalManager` singleton keeps every PTY alive, so "Back to Terminals"
 * returns the user to exactly the grid they left (hard rule: never dispose).
 *
 * Auto-close (acceptance criterion): the diff belongs to a specific lens — the
 * focused node and selected mesh captured in `diff.nodeId` / `diff.meshId` at
 * open time. If the user focuses a *different* background node card, or selects
 * a *different* project in the sidebar, the diff no longer matches what they're
 * looking at, so we close it rather than show a mismatched diff. The shared
 * `DiffOverlayShell` owns the toolbar + Esc + auto-close; this file owns the
 * per-source body (head/base local-git fetch vs `source: 'pr'` GitHub fetch).
 *
 * Source dispatch: `source: 'head' | 'base'` fetches from the local git
 * commands the existing review surface uses; `source: 'pr'` (issue #421)
 * routes to `PrDiffView`, which talks to GitHub's `/pulls/{n}/files` and
 * doesn't need a local checkout of the PR's head branch.
 *
 * Rules-of-Hooks discipline (issue #803): the two source bodies call a
 * *different number* of hooks (the head/base body owns the local-git fetch
 * state; the PR body delegates that to `PrDiffView`). The Probe stays
 * interactive, so `diff.source` can flip base→pr on the *same* mounted
 * overlay (open a base diff, then click a PR in the Probe). To keep the hook
 * order stable, this component is a thin dispatcher that always calls the
 * same three store hooks, then renders a *different child component* per
 * source — each child owns its own hooks unconditionally, and a source flip
 * swaps the component type (a clean remount) instead of changing the hook
 * count of one fiber.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import {
  diffFileAgainstHead,
  diffNodeFileAgainstBase,
  type DiffResult,
  type FileDiff,
} from '../../lib/tauri';
import { useUIStore, type DiffContext } from '../../stores/uiStore';
import { useMeshStore } from '../../stores/meshStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useGitPathInvalidation } from '../../hooks/useGitPathInvalidation';
import { Diff } from '../Diff/Diff';
import { PrDiffView } from './PrDiffView';
import { DiffOverlayShell } from './DiffOverlayShell';
import { LoadingState } from '../shared/Spinner';

interface CenterDiffOverlayProps {
  /** The file + lens to render. The parent only mounts us when this is
   *  non-null, so we never have to handle the empty case. */
  diff: DiffContext;
}

export function CenterDiffOverlay({ diff }: CenterDiffOverlayProps) {
  const closeDiff = useUIStore((s) => s.closeDiff);

  // Toolbar label — the file's "parent". Prefer the owning agent node's name;
  // a mesh-scoped diff (Project Files with no node focused) falls back to the
  // mesh name. Looked up live (not stored in the context) so a rename shows
  // through immediately. Used by both head/base and PR breadcrumb paths.
  const node = useAgentNodeStore((s) =>
    diff.nodeId === null ? null : s.agentNodes.find((n) => n.id === diff.nodeId) ?? null,
  );
  const meshName = useMeshStore((s) => s.meshesById.get(diff.meshId)?.name ?? null);
  const parentLabel = node?.name ?? meshName ?? 'Workspace';

  // These three hooks run every render, in the same order, regardless of
  // source. The per-source bodies (which call a differing number of hooks)
  // live in their own components below — see the Rules-of-Hooks note above.
  if (diff.source === 'pr') {
    return <CenterPrDiff diff={diff} closeDiff={closeDiff} parentLabel={parentLabel} />;
  }
  return <CenterHeadBaseDiff diff={diff} closeDiff={closeDiff} parentLabel={parentLabel} />;
}

interface DiffBranchProps {
  diff: DiffContext;
  closeDiff: () => void;
  parentLabel: string;
}

// PR diffs (`source: 'pr'`) don't use the local-git fetch path at all —
// PrDiffView handles its own state. The shell is the same; only the
// breadcrumb + mode label + body differ.
function CenterPrDiff({ diff, closeDiff, parentLabel }: DiffBranchProps) {
  const prLabel = diff.prNumber !== undefined ? `PR #${diff.prNumber}` : 'PR';
  return (
    <DiffOverlayShell
      diff={diff}
      onClose={closeDiff}
      modeLabel={{
        text: diff.filePath === '' ? 'PR list' : 'PR file',
        title:
          diff.filePath === ''
            ? 'Files changed in this pull request'
            : 'Diff for one file in this pull request',
      }}
      breadcrumb={
        <>
          {diff.filePath !== '' && (
            <>
              <span
                className="font-mono text-xs text-text-primary truncate"
                title={diff.filePath}
              >
                {diff.filePath}
              </span>
              <span className="text-text-muted text-xs shrink-0">in</span>
            </>
          )}
          <span
            className="text-accent-cyan text-xs font-mono font-medium shrink-0"
            title={prLabel}
          >
            {prLabel}
          </span>
          <span className="text-text-muted text-xs shrink-0">in</span>
          <span
            className="text-text-secondary text-xs font-medium truncate"
            title={parentLabel}
          >
            {parentLabel}
          </span>
        </>
      }
    >
      <PrDiffView diff={diff} />
    </DiffOverlayShell>
  );
}

// Head/base diffs fetch from the local git commands the existing review
// surface uses, and live-refresh while the agent keeps editing.
function CenterHeadBaseDiff({ diff, closeDiff, parentLabel }: DiffBranchProps) {
  const [files, setFiles] = useState<FileDiff[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Monotonic token so an older in-flight fetch (rapid file switching, or a
  // burst of GIT_CHANGED events) can't overwrite the newest result.
  const reqId = useRef(0);

  const fetchDiff = useCallback(
    (opts?: { background?: boolean }) => {
      const myId = ++reqId.current;
      if (!opts?.background) setFiles(null);
      setError(null);
      const promise: Promise<DiffResult> =
        diff.source === 'base' && diff.nodeId !== null
          ? diffNodeFileAgainstBase(diff.nodeId, diff.filePath)
          : diffFileAgainstHead(diff.rootPath, diff.filePath);
      promise
        .then((d) => {
          if (reqId.current !== myId) return;
          setFiles(d.files);
        })
        .catch((e) => {
          if (reqId.current !== myId) return;
          if (!opts?.background) setError(String(e));
        });
    },
    [diff.source, diff.nodeId, diff.filePath, diff.rootPath],
  );

  useEffect(() => {
    fetchDiff();
  }, [fetchDiff]);

  // Live refresh: the agent keeps editing while the overlay is open, so re-pull
  // when the watcher reports a change in this worktree. Mirrors AgentReviewPanel.
  useGitPathInvalidation(diff.rootPath, () => fetchDiff({ background: true }));

  return (
    <DiffOverlayShell
      diff={diff}
      onClose={closeDiff}
      modeLabel={{
        text: diff.source === 'base' ? 'vs base' : 'vs HEAD',
        title:
          diff.source === 'base'
            ? 'Changes since this agent branched from its base'
            : 'Uncommitted changes vs HEAD',
      }}
      breadcrumb={
        <>
          <span
            className="font-mono text-xs text-text-primary truncate"
            title={diff.filePath}
          >
            {diff.filePath}
          </span>
          <span className="text-text-muted text-xs shrink-0">in</span>
          <span
            className="text-text-secondary text-xs font-medium truncate"
            title={parentLabel}
          >
            {parentLabel}
          </span>
        </>
      }
    >
      {error ? (
        <div className="h-full flex items-center justify-center text-accent-red text-xs px-3 text-center">
          {error}
        </div>
      ) : files === null ? (
        <div className="h-full flex items-center justify-center">
          <LoadingState label="Loading diff…" />
        </div>
      ) : (
        <Diff files={files} />
      )}
    </DiffOverlayShell>
  );
}
