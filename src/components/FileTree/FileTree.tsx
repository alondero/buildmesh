import { formatError } from '../../lib/errorUtils';
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from 'react';
import {
  listDirectory,
  type FileNode,
  diffFileAgainstHead,
  type DiffResult,
} from '../../lib/tauri';
import { useChangedFiles } from '../../hooks/useChangedFiles';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { fileDiffStatusMeta as statusMeta } from '../../lib/status';
import { LoadingState } from '../shared/Spinner';
import { addToast } from '../../stores/toastStore';

/** Lucide-style glyphs for the tree rows. */
function ChevronRightIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <polyline points="9 18 15 12 9 6" />
    </svg>
  );
}

function FolderIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.75"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
    </svg>
  );
}

function FileIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.75"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
      <polyline points="14 2 14 8 20 8" />
    </svg>
  );
}

interface FileTreeProps {
  rootPath: string;
  /** Show M badges on changed files */
  showGitStatus: boolean;
  /** Called when a changed file is selected (for inline diff) */
  onChangedFileSelect?: (path: string, diff: DiffResult) => void;
  /** Called when an unchanged file is selected (to open in editor) */
  onUnchangedFileSelect?: (path: string) => void;
  /** Currently selected file path (to show diff inline) */
  selectedFile: string | null;
  /** Callback to set the selected file */
  onFileSelect: (path: string | null) => void;
}

/** A flat row entry — one per currently-visible node, in DOM/visual order. */
interface VisibleRow {
  node: FileNode;
  depth: number;
}

export function FileTree({
  rootPath,
  showGitStatus,
  onChangedFileSelect,
  onUnchangedFileSelect,
  selectedFile,
  onFileSelect,
}: FileTreeProps) {
  const [treeState, setTreeState] = useState<FileNode | null>(null);
  const [loadingState, setLoadingState] = useState(true);
  const [errorState, setErrorState] = useState<string | null>(null);
  // Lifted from per-TreeNode state (issue #728) so the keyboard handler
  // can compute a stable visible-order index space without walking the DOM.
  const [expandedPaths, setExpandedPaths] = useState<Set<string>>(() => new Set());
  // Roving tabindex — exactly one row is `tabIndex={0}` at a time.
  const [activeIndex, setActiveIndex] = useState(0);

  useAsyncEffect((signal) => {
    if (!rootPath) return;
    setLoadingState(true);
    setErrorState(null);
    setTreeState(null);

    listDirectory(rootPath, 4)
      .then((treeData) => {
        if (signal.aborted) return;
        setTreeState(treeData);
        setLoadingState(false);
      })
      .catch((e) => {
        if (signal.aborted) return;
        setErrorState(formatError(e));
        setLoadingState(false);
      });
  }, [rootPath]);

  // Git status badges come from the shared cache (dedupes with
  // ChangedFilesSection + useMeshGitStatus, and refreshes on GIT_CHANGED — the
  // old combined fetch did neither). Gate the loading view on both so a file's
  // changed/unchanged state is settled before the user can click it.
  const { files: gitFiles, loading: gitLoading } = useChangedFiles(
    showGitStatus ? rootPath || null : null,
  );

  // list_directory returns absolute node paths; get_git_status returns paths
  // relative to the repo root. Key the map by the relative path and reconcile
  // each node by stripping rootPath, so changed files are badged correctly.
  // Declared before `handleKeyDown` so the keyboard handler's closure can
  // read it on every render without tripping the TDZ (issue #728).
  // The map is intentionally rebuilt on every render — `gitFiles` is
  // the upstream source of truth, and the size here is bounded by the
  // repo's changed-file count (always small). Wrapping in useMemo
  // would add a cache to invalidate without changing what the keyboard
  // handler observes.
  //
  // The lint rule fires on this line because `gitStatusMap` is consumed
  // inside a useCallback's deps at L342 — the rule warns "object
  // construction makes deps change every render". Wrapping in useMemo
  // would replace one re-allocation with two (the memo + the Map
  // construction); the simpler fix is to suppress the warning here.
  // eslint-disable-next-line react-hooks/exhaustive-deps -- rebuilt every render on purpose; useMemo would replace one allocation with two without changing the rendered output.
  const gitStatusMap = new Map(gitFiles.map((s) => [normalizePath(s.path), s.status]));

  const handleFileClick = useCallback(
    async (path: string, relPath: string, isChanged: boolean) => {
      if (isChanged && onChangedFileSelect) {
        // Capture the previous selection so the failure branch can roll
        // it back. A bare `onFileSelect(null)` would clear any pre-existing
        // highlight — the user might have had a different file open before
        // clicking, and clobbering that one too looks like two dead clicks
        // (issue #1245).
        const previousSelection = selectedFile;
        onFileSelect(path);
        try {
          // diff_file_against_head joins session_path + file_path, so the
          // file_path must be relative to the repo root.
          const diff = await diffFileAgainstHead(rootPath, relPath);
          onChangedFileSelect(path, diff);
        } catch (e) {
          // Issue #1245 — surface the failure AND undo the optimistic
          // highlight. Without the rollback the row sits selected as if a
          // diff had opened, while nothing actually did — the highlight
          // would lie about an open diff for the rest of the session.
          // Toast mirrors the convention used by NodeItem.tsx and the
          // sibling ChangedFilesSection call site so all diff-load
          // failures dedup under one 'Review' provider slot.
          addToast(
            'Review',
            `Failed to load diff for ${relPath}: ${formatError(e)}`,
            'error',
          );
          onFileSelect(previousSelection);
        }
      } else if (onUnchangedFileSelect) {
        onUnchangedFileSelect(path);
      }
    },
    [rootPath, onChangedFileSelect, onUnchangedFileSelect, onFileSelect, selectedFile]
  );

  // Flatten the tree to the rows the user can see right now. The keyboard
  // handler reads this list to index ArrowDown/Up/Home/End and to compute
  // parent/child hops for ArrowLeft/Right — keeping it a memoised array
  // (not a DOM walk) means nav math is O(visible.length).
  const visible = useMemo<VisibleRow[]>(() => {
    if (!treeState) return [];
    const result: VisibleRow[] = [];
    const walk = (node: FileNode, depth: number) => {
      result.push({ node, depth });
      if (node.is_dir && expandedPaths.has(node.path)) {
        for (const child of node.children) {
          walk(child, depth + 1);
        }
      }
    };
    for (const child of treeState.children) {
      walk(child, 0);
    }
    return result;
  }, [treeState, expandedPaths]);

  // Clamp the roving tabindex when the visible list shrinks beneath it
  // (e.g. a parent folder was collapsed, hiding the focused descendant).
  useEffect(() => {
    if (visible.length === 0) {
      if (activeIndex !== 0) setActiveIndex(0);
      return;
    }
    if (activeIndex >= visible.length) {
      setActiveIndex(visible.length - 1);
    }
  }, [visible.length, activeIndex]);

  const toggleExpanded = useCallback((path: string) => {
    setExpandedPaths((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }, []);

  const treeRef = useRef<HTMLDivElement>(null);

  // Sync the roving tabindex to whichever row currently holds focus.
  // Without this, focusing a non-active row via mouse click or tab leaves
  // `activeIndex` pointing at the old row — the next ArrowDown would jump
  // from the OLD activeIndex, not the row the user is actually on. This
  // mirrors how `useAriaMenu` keeps its menuitem index in step with focus.
  const handleFocus = useCallback((e: React.FocusEvent<HTMLDivElement>) => {
    const target = e.target as HTMLElement;
    if (target.getAttribute('role') !== 'treeitem') return;
    const root = treeRef.current;
    if (!root) return;
    const items = root.querySelectorAll<HTMLElement>('[role="treeitem"]');
    const idx = Array.from(items).indexOf(target);
    if (idx >= 0) setActiveIndex(idx);
  }, []);

  // WAI-ARIA tree keyboard contract (issue #728). Delegated on the tree
  // container so a single handler covers every row, regardless of how
  // many rows the flat render emits. Reads the actually-focused row on
  // entry — NOT `activeIndex` from state — because `setActiveIndex` from
  // a just-fired focus event hasn't propagated through React's batch
  // boundary by the time the next synchronous event (e.g. Enter right
  // after `element.focus()`) arrives.
  const handleKeyDown = useCallback(
    (e: ReactKeyboardEvent<HTMLDivElement>) => {
      if (visible.length === 0) return;
      const root = treeRef.current;
      if (!root) return;

      const items = root.querySelectorAll<HTMLElement>('[role="treeitem"]');
      const focused = document.activeElement;
      let currentIndex = activeIndex;
      if (focused instanceof HTMLElement) {
        const idx = Array.from(items).indexOf(focused);
        if (idx >= 0) currentIndex = idx;
      }
      const current = visible[currentIndex];
      if (!current) return;

      let nextIndex: number | null = null;
      let action: 'expand' | 'collapse' | 'activate' | null = null;

      switch (e.key) {
        case 'ArrowDown':
          nextIndex = Math.min(currentIndex + 1, visible.length - 1);
          break;
        case 'ArrowUp':
          nextIndex = Math.max(currentIndex - 1, 0);
          break;
        case 'Home':
          nextIndex = 0;
          break;
        case 'End':
          nextIndex = visible.length - 1;
          break;
        case 'ArrowRight':
          if (current.node.is_dir) {
            if (!expandedPaths.has(current.node.path)) {
              action = 'expand';
            } else {
              // Already open — ArrowRight descends to the first child.
              const childIdx = currentIndex + 1;
              if (childIdx < visible.length && visible[childIdx].depth === current.depth + 1) {
                nextIndex = childIdx;
              }
            }
          }
          break;
        case 'ArrowLeft':
          if (current.node.is_dir && expandedPaths.has(current.node.path)) {
            action = 'collapse';
          } else {
            // Leaf or already-collapsed folder — ArrowLeft ascends to the
            // nearest visible ancestor.
            const parentDepth = current.depth - 1;
            for (let i = currentIndex - 1; i >= 0; i--) {
              if (visible[i].depth === parentDepth) {
                nextIndex = i;
                break;
              }
            }
          }
          break;
        case 'Enter':
        case ' ':
          action = 'activate';
          break;
        default:
          return;
      }

      e.preventDefault();
      if (action === 'expand' || action === 'collapse') {
        toggleExpanded(current.node.path);
      } else if (action === 'activate') {
        if (current.node.is_dir) {
          toggleExpanded(current.node.path);
        } else {
          const relPath = relativeToRoot(rootPath, current.node.path);
          const status = relPath != null ? gitStatusMap.get(relPath) : undefined;
          const isChanged = status !== undefined;
          handleFileClick(current.node.path, relPath ?? current.node.path, isChanged);
        }
      }
      if (nextIndex !== null && nextIndex !== currentIndex) {
        setActiveIndex(nextIndex);
        items[nextIndex]?.focus();
      }
    },
    [visible, activeIndex, expandedPaths, toggleExpanded, rootPath, gitStatusMap, handleFileClick]
  );

  // Block on the initial file-listing fetch, and on the initial git-status
  // fetch only while the tree hasn't rendered yet (so a file's changed/
  // unchanged state is settled before it's first clickable). Once
  // `treeState` exists, a background GIT_CHANGED refresh must NOT blank
  // the tree — re-mounting the whole list would reset the lifted
  // `expandedPaths` set and snap shut every folder the user had opened
  // (issue #804). `gitFiles` keeps its last value during the refresh
  // (see `pathInvalidatedCache.ts`), so badges just update in place once
  // the new status resolves.
  if (loadingState || (gitLoading && !treeState)) {
    return (
      <div className="flex items-center justify-center h-20">
        <LoadingState label="Loading files…" />
      </div>
    );
  }

  if (errorState) {
    return (
      <div className="flex items-center justify-center h-20 text-accent-red text-xs">
        Error: {errorState}
      </div>
    );
  }

  if (!treeState) {
    return (
      <div className="flex items-center justify-center h-20 text-text-muted text-xs">
        No files found
      </div>
    );
  }

  return (
    <div
      ref={treeRef}
      role="tree"
      aria-label={rootPath ? `Files in ${rootPath}` : 'Files'}
      onKeyDown={handleKeyDown}
      onFocus={handleFocus}
      className="text-xs font-mono"
    >
      {visible.map((row, index) => {
        const expanded = expandedPaths.has(row.node.path);
        const relPath = relativeToRoot(rootPath, row.node.path);
        const status = relPath != null ? gitStatusMap.get(relPath) : undefined;
        const isChanged = status !== undefined;
        const isSelected = selectedFile === row.node.path;
        const isActive = index === activeIndex;
        return (
          <TreeRow
            key={row.node.path}
            node={row.node}
            depth={row.depth}
            expanded={expanded}
            isActive={isActive}
            isChanged={isChanged}
            isSelected={isSelected}
            showGitStatus={showGitStatus}
            status={status}
            onToggleExpanded={toggleExpanded}
            onFileClick={handleFileClick}
            relPath={relPath}
          />
        );
      })}
    </div>
  );
}

/** Normalize separators to `/` and drop any trailing slash. */
function normalizePath(p: string): string {
  return p.replace(/\\/g, '/').replace(/\/+$/, '');
}

/** Path of `abs` relative to `root`, or null if `abs` is not under `root`. */
function relativeToRoot(root: string, abs: string): string | null {
  const nr = normalizePath(root);
  const na = normalizePath(abs);
  if (na === nr) return '';
  const prefix = nr + '/';
  return na.startsWith(prefix) ? na.slice(prefix.length) : null;
}

interface TreeRowProps {
  node: FileNode;
  depth: number;
  expanded: boolean;
  isActive: boolean;
  isChanged: boolean;
  isSelected: boolean;
  showGitStatus: boolean;
  status: string | undefined;
  onToggleExpanded: (path: string) => void;
  onFileClick: (path: string, relPath: string, isChanged: boolean) => void;
  relPath: string | null;
}

function TreeRow({
  node,
  depth,
  expanded,
  isActive,
  isChanged,
  isSelected,
  showGitStatus,
  status,
  onToggleExpanded,
  onFileClick,
  relPath,
}: TreeRowProps) {
  return (
    <div
      role="treeitem"
      aria-level={depth + 1}
      // Only folder rows carry aria-expanded — it's invalid on leaf treeitems.
      aria-expanded={node.is_dir ? expanded : undefined}
      tabIndex={isActive ? 0 : -1}
      className={`
        flex items-center gap-1 px-2 py-0.5 rounded-md cursor-pointer
        hover:bg-bg-card transition-colors outline-none
        focus-visible:ring-2 focus-visible:ring-accent-blue/60
        ${isSelected ? 'bg-bg-overlay' : ''}
      `}
      style={{ paddingLeft: `${depth * 16 + 8}px` }}
      onClick={() => {
        if (node.is_dir) {
          onToggleExpanded(node.path);
        } else {
          onFileClick(node.path, relPath ?? node.path, isChanged);
        }
      }}
    >
      {node.is_dir ? (
        <span
          className={`w-3 h-3 flex items-center justify-center text-text-muted transition-transform ${
            expanded ? 'rotate-90' : ''
          }`}
          // Decorative — the row itself owns expand on click. The
          // rotate-on-state is the cheapest way to flip the chevron
          // direction without swapping the SVG.
          data-testid={expanded ? 'folder-expanded' : 'folder-collapsed'}
        >
          <ChevronRightIcon className="w-3 h-3" />
        </span>
      ) : (
        <span className="w-3 h-3" />
      )}
      <span className="w-3 h-3 flex items-center justify-center text-text-muted">
        {node.is_dir ? (
          <FolderIcon className="w-3 h-3" />
        ) : (
          <FileIcon className="w-3 h-3" />
        )}
      </span>
      <span
        className={`flex-1 truncate ${
          node.is_dir ? 'text-text-secondary' : 'text-text-muted'
        }`}
      >
        {node.name}
      </span>
      {showGitStatus && status && (
        <span
          className={`font-bold ${statusMeta(status as Parameters<typeof statusMeta>[0]).color}`}
          title={statusMeta(status as Parameters<typeof statusMeta>[0]).label}
        >
          {statusMeta(status as Parameters<typeof statusMeta>[0]).letter}
        </span>
      )}
    </div>
  );
}
