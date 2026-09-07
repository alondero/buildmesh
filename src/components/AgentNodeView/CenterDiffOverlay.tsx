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

import { formatError } from '../../lib/errorUtils';
import { useCallback, useEffect, useRef, useState } from 'react';
import {
  diffFileAgainstHead,
  diffNodeFileAgainstBase,
  getGitStatus,
  nodeChangedFiles,
  stageFile,
  revertFile,
  type DiffResult,
  type FileDiff,
  type GitStatus,
} from '../../lib/tauri';
import { useUIStore, type DiffContext } from '../../stores/uiStore';
import { useMeshStore } from '../../stores/meshStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useToastStore } from '../../stores/toastStore';
import { useGitPathInvalidation } from '../../hooks/useGitPathInvalidation';
import { Diff } from '../Diff/Diff';
import { PrDiffView } from './PrDiffView';
import { DiffOverlayShell } from './DiffOverlayShell';
import { DiffFileNavDrawer } from './DiffFileNavDrawer';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
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
  // Issue #1384 — subscribe to the normalized `nodesById` directly. The
  // selector returns the same object reference for any other node's change,
  // so unrelated attention/rename events don't re-render this overlay.
  const node = useAgentNodeStore((s) =>
    diff.nodeId === null ? null : s.nodesById[diff.nodeId] ?? null,
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
            className="text-text-secondary text-xs font-mono font-medium shrink-0"
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
  // Issue #1264 — track the most recent BACKGROUND refresh failure so a
  // long-lived overlay doesn't keep showing a stale diff after a
  // transient git error (the previous shape was: "if the initial load
  // worked, the overlay keeps showing whatever files it last received,
  // silently". The first-load failure still routes through `error`
  // and replaces the body; the background chip is a non-blocking
  // signal that the live refresh is failing. Cleared on the next
  // successful refresh.
  const [backgroundError, setBackgroundError] = useState<string | null>(null);
  // Issue #1374 (item 3) — Quick File Navigation Drawer. Closed by
  // default so the diff gets the full center workspace on first paint;
  // the user opens it to hop between files without leaving the overlay.
  const [drawerOpen, setDrawerOpen] = useState(false);
  const openDiff = useUIStore((s) => s.openDiff);
  // Issue #1374 — destructive Revert needs an explicit confirmation
  // (Revert discards uncommitted work for tracked files, outright
  // deletes untracked files). Stage is non-destructive (the file's
  // working copy is unchanged) so it does NOT need a confirm gate.
  const [revertConfirmFilePath, setRevertConfirmFilePath] = useState<string | null>(null);
  // `version` bumps after a successful stage/revert so the drawer
  // refetches — the drawer's mount-time fetch is otherwise stale until
  // the user manually closes + reopens it (issue #1374 review feedback).
  const [drawerVersion, setDrawerVersion] = useState(0);

  // Issue #1181 — per-file diffs also go through `run_blocking`, and rapid
  // file switching in
  // the overlay + `git-changed` bursts can pile up the same way the
  // review panel does. Owning an AbortController + matching the
  // existing reqId pattern keeps the two consumers symmetric.
  // Monotonic token so an older in-flight fetch (rapid file switching,
  // or a burst of GIT_CHANGED events) can't overwrite the newest result.
  const reqId = useRef(0);
  const abortRef = useRef<AbortController | null>(null);

  const fetchDiff = useCallback(
    (opts?: { background?: boolean }) => {
      const myId = ++reqId.current;
      // Abort the prior fetch (if any) before kicking off a new one.
      abortRef.current?.abort();
      const controller = new AbortController();
      abortRef.current = controller;
      if (!opts?.background) {
        setFiles(null);
        // A new explicit load is the user's "I'm paying attention now"
        // signal — clear the stale background-error chip too, even if
        // the next refresh fails, so the chip is a one-failure-at-a-time
        // signal rather than sticky history.
        setBackgroundError(null);
      }
      setError(null);
      const promise: Promise<DiffResult> =
        diff.source === 'base' && diff.nodeId !== null
          ? diffNodeFileAgainstBase(diff.nodeId, diff.filePath, controller.signal)
          : diffFileAgainstHead(diff.rootPath, diff.filePath);
      promise
        .then((d) => {
          if (reqId.current !== myId) return;
          setFiles(d.files);
          // A successful refresh — background OR explicit — clears the
          // chip so a transient error doesn't haunt the overlay past
          // recovery.
          setBackgroundError(null);
        })
        .catch((e) => {
          if (reqId.current !== myId) return;
          // Cancellation is the expected end-state for any superseded
          // fetch — surface it as nothing so a background-refresh
          // abort doesn't masquerade as a real failure.
          if (controller.signal.aborted) return;
          if (opts?.background) {
            // The diff is still showing the last-successful result;
            // surface the failure as a non-blocking chip so the user
            // can tell the live refresh is failing without losing the
            // visible diff.
            setBackgroundError(formatError(e));
          } else {
            setError(formatError(e));
          }
        });
    },
    [diff.source, diff.nodeId, diff.filePath, diff.rootPath],
  );

  useEffect(() => {
    fetchDiff();
    // Abort pending fetches on unmount or dep change so a stale `.then` can't
    // setState.
    return () => {
      abortRef.current?.abort();
    };
  }, [fetchDiff]);

  // Live refresh: the agent keeps editing while the overlay is open, so re-pull
  // when the watcher reports a change in this worktree.
  // Issue #1165: same 2 s freshness window — a burst of `GIT_CHANGED` events
  // during a heavy edit session collapses to one trailing refetch instead of
  // one per emit (each refetch hits the libgit2 walk + 3× syntect pass).
  useGitPathInvalidation(diff.rootPath, () => fetchDiff({ background: true }), {
    minRefetchIntervalMs: 2_000,
  });

  // Issue #1374 — per-file quick actions. Stage and Revert surface
  // failures via the project's shared toast pipeline (the previous
  // shape `console.error` left the user with zero visual feedback on
  // a destructive operation — issue #1374 review feedback). Revert is
  // destructive (discards uncommitted work / deletes untracked
  // files), so it opens a `ConfirmDialog` first. Copy Diff serializes
  // the fetched `FileDiff[]` into a GitHub-style unified diff text
  // block; its only failure mode is clipboard access denied, which the
  // OS surfaces natively.
  const addToast = useToastStore((s) => s.addToast);
  const quickActions = {
    onStageFile: () => {
      stageFile(diff.rootPath, diff.filePath)
        .then(() => {
          setDrawerVersion((v) => v + 1);
        })
        .catch((e) => {
          addToast(
            'DiffOverlay',
            `Stage failed for ${diff.filePath}: ${formatError(e)}`,
            'error',
          );
        });
    },
    onRevertFile: () => {
      // Open the confirm modal; the actual revert happens on confirm.
      setRevertConfirmFilePath(diff.filePath);
    },
    onCopyDiff: () => {
      const text = buildDiffClipboardText(diff.filePath, files);
      navigator.clipboard.writeText(text).catch((e) => {
        addToast(
          'DiffOverlay',
          `Copy failed: ${formatError(e)}`,
          'error',
        );
      });
    },
  };

  const doRevert = useCallback(() => {
    const path = revertConfirmFilePath;
    if (!path) return;
    setRevertConfirmFilePath(null);
    revertFile(diff.rootPath, path)
      .then(() => {
        setDrawerVersion((v) => v + 1);
      })
      .catch((e) => {
        addToast(
          'DiffOverlay',
          `Revert failed for ${path}: ${formatError(e)}`,
          'error',
        );
      });
  }, [revertConfirmFilePath, diff.rootPath, addToast]);

  // Issue #1374 — bulk file-list fetch for the Quick File Navigation
  // Drawer. Base diffs use `node_changed_files` (per-node cancel-aware,
  // see `commands/diff.rs`); head diffs use `get_git_status` for the
  // session's working tree. `drawerVersion` is a dep so a successful
  // Stage/Revert bumps `drawerVersion` and forces the drawer to refetch
  // — the previous shape had the drawer showing stale M/A/D badges until
  // the user manually closed + reopened it (issue #1374 review feedback).
  // The refetch is driven by the consumer's effect (which depends on
  // `drawerVersion`); this callback closure only reads `diff.*`, so
  // listing `drawerVersion` here would re-allocate on every bump without
  // changing what the callback does. Issue #1542.
  const fetchAllFiles = useCallback((): Promise<GitStatus[]> => {
    if (diff.source === 'base' && diff.nodeId !== null) {
      return nodeChangedFiles(diff.nodeId);
    }
    return getGitStatus(diff.rootPath);
  }, [diff.source, diff.nodeId, diff.rootPath]);

  // Jump-to-file: update `activeDiffFile` in the UI store so the overlay
  // body re-fetches the new file's diff via the existing `fetchDiff`
  // path. Reuses the same context shape, only the `filePath` changes.
  const jumpToFile = useCallback(
    (path: string) => {
      openDiff({ ...diff, filePath: path });
    },
    [openDiff, diff],
  );

  return (
    <>
      <DiffOverlayShell
        diff={diff}
        onClose={closeDiff}
        quickActions={quickActions}
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
      {/* Issue #1374 — render-prop so the shell's Unified/Split mode is
          threaded into `<Diff>` as a single source of truth. The shell's
          toolbar toggle AND each `<FileDiffCard>`'s inline toggle both
          route through `setViewMode`, so they stay in sync without
          duplicating the localStorage write. */}
      {(viewMode, setViewMode) =>
        error ? (
          <div className="h-full flex items-center justify-center text-accent-red text-xs px-3 text-center">
            {error}
          </div>
        ) : files === null ? (
          <div className="h-full flex items-center justify-center">
            <LoadingState label="Loading diff…" />
          </div>
        ) : (
          <div className="h-full flex min-h-0">
            {/* Issue #1374 (item 3) — collapsible Quick File Navigation
                Drawer. Sits as a left-edge side panel inside the body;
                closed by default so the diff gets the full width. The
                toggle button sits at the top of the panel so the user
                always has a place to collapse it back. */}
            {drawerOpen && (
              <DiffFileNavDrawer
                fetchFiles={fetchAllFiles}
                currentFilePath={diff.filePath}
                onSelectFile={jumpToFile}
                refreshKey={drawerVersion}
              />
            )}
            <div className="flex-1 min-w-0 flex flex-col min-h-0">
              <div className="flex items-center gap-2 px-3 py-1.5 border-b border-border-subtle bg-bg-surface shrink-0">
                <button
                  type="button"
                  data-testid="diff-file-nav-toggle"
                  onClick={() => setDrawerOpen((v) => !v)}
                  aria-pressed={drawerOpen}
                  aria-label={drawerOpen ? 'Hide file navigation' : 'Show file navigation'}
                  className="inline-flex items-center gap-1 px-2 py-0.5 rounded-md text-2xs text-text-secondary hover:text-accent-cyan hover:bg-bg-card transition-colors border border-border-subtle"
                >
                  {drawerOpen ? '◂ Hide files' : '▸ Show files'}
                </button>
              </div>
              <div className="flex-1 min-h-0 overflow-auto">
                {backgroundError && (
                  // Issue #1264 — subtle "refresh failed" chip. The diff
                  // below is still the last-successful render; the chip
                  // tells the user the live refresh is failing so the
                  // visible diff is potentially stale. Non-blocking so a
                  // transient git hiccup doesn't tear down the overlay.
                  <div
                    role="status"
                    aria-live="polite"
                    data-testid="background-refresh-failed"
                    className="px-3 py-1.5 bg-status-warning/10 border-b border-status-warning/30 text-2xs text-status-warning flex items-center gap-2"
                  >
                    <span
                      aria-hidden="true"
                      className="inline-block h-1.5 w-1.5 rounded-full bg-status-warning"
                    />
                    Refresh failed — showing last known diff
                    <span className="text-text-muted truncate" title={backgroundError}>
                      ({backgroundError})
                    </span>
                  </div>
                )}
                <Diff files={files} mode={viewMode} onModeChange={setViewMode} />
              </div>
            </div>
          </div>
        )
      }
      </DiffOverlayShell>
      {revertConfirmFilePath && (
        <ConfirmDialog
          title="Revert file?"
          message={`Revert ${revertConfirmFilePath}? This discards any uncommitted edits to tracked files (or deletes the file outright if it's untracked). The change can't be undone.`}
          confirmLabel="Revert"
          onConfirm={doRevert}
          onCancel={() => setRevertConfirmFilePath(null)}
        />
      )}
    </>
  );
}

/** Serialize a single file's diff into GitHub-style unified diff text for
 *  clipboard copying (issue #1374 "Copy Diff"). `files` may be null while
 *  loading — fall back to a header-only stub so the button never silently
 *  no-ops. */
function buildDiffClipboardText(
  filePath: string,
  files: FileDiff[] | null,
): string {
  if (!files || files.length === 0) {
    return `diff --git a/${filePath} b/${filePath}\n(no diff data loaded)`;
  }
  const lines: string[] = [];
  for (const f of files) {
    lines.push(`diff --git a/${f.path} b/${f.path}`);
    if (f.old_path && f.old_path !== f.path) {
      lines.push(`rename from ${f.old_path}`);
      lines.push(`rename to ${f.path}`);
    }
    for (const h of f.hunks) {
      lines.push(`@@ -${h.old_start},${h.old_lines} +${h.new_start},${h.new_lines} @@`);
      for (const l of h.lines) {
        const marker = l.line_type === 'add' ? '+' : l.line_type === 'remove' ? '-' : ' ';
        lines.push(`${marker}${l.content}`);
      }
    }
  }
  return lines.join('\n');
}
