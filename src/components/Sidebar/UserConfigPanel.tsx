/**
 * UserConfigPanel — issue #60. Right-dock panel that lists the resolved
 * ~/.claude tree for browsing, with files opening in the user's default
 * editor instead of an inline diff.
 *
 * Why it is NOT a Probe tab
 * -------------------------
 * The Probe Panel anchors on `useProbeContext()` (mesh-scoped); the
 * `useUIStore` already gates the `usage` tab as the lone host-scoped tab
 * (no mesh required), but that tab is a glanceable surface, not a tree
 * browser. User Config deserves its own panel because it owns its
 * visibility (`userConfigOpen`), its resolved path is fetched exactly
 * once on mount (the ~/.claude location is stable for the process), and
 * clicking through to an external editor doesn't fit the Probe's
 * review/diff flow.
 *
 * Why `showGitStatus={false}` is enough to suppress M badges
 * ---------------------------------------------------------
 * `FileTree` gates the `useChangedFiles` fetch on `showGitStatus` — when
 * false, the hook is called with `null` and the shared cache stays cold.
 * Acceptance criterion #5 ("Full ~/.claude tree renders, no M badges") is
 * therefore satisfied without any extra plumbing here.
 */

import { useState } from 'react';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { formatError } from '../../lib/errorUtils';
import { getUserConfigDir, openInEditor } from '../../lib/tauri';
import { useUIStore } from '../../stores/uiStore';
import { FileTree } from '../FileTree/FileTree';

// User Config panel width (issue #60). Pinned at 360 — same default as
// the Probe Panel's `PROBE_PANEL_BOUNDS.DEFAULT_WIDTH` in `useProbeResize`,
// so the two docks feel consistent across surfaces. We deliberately omit
// `useProbeResize` here: User Config opens ad-hoc off the sidebar (no
// mesh/grid context) and the 360px default is a sensible trade-off
// between reading long paths and leaving room for the AgentNodeView grid.
const USER_CONFIG_PANEL_WIDTH = 360;

export function UserConfigPanel() {
  const userConfigOpen = useUIStore((s) => s.userConfigOpen);
  const setUserConfigOpen = useUIStore((s) => s.setUserConfigOpen);

  // ~/.claude resolves to a stable path for the lifetime of the process —
  // one fetch on mount is enough. A null `path` means "still resolving" or
  // "fetch failed"; the body renders the appropriate one of three states
  // (loading, error, tree) on top of the header.
  const [path, setPath] = useState<string | null>(null);
  const [pathError, setPathError] = useState<string | null>(null);

  useAsyncEffect((signal) => {
    getUserConfigDir()
      .then((resolved) => {
        if (signal.aborted) return;
        setPath(resolved);
      })
      .catch((e) => {
        if (signal.aborted) return;
        setPathError(formatError(e));
      });
  }, []);

  // Don't render the panel when closed — the previous `FileExplorerPanel`
  // mounted unconditionally and burned a row of vertical space inside the
  // sidebar column even when collapsed. Mount-on-open matches what the
  // Probe Panel already does (`{probeOpen && (...)}` at line 121).
  if (!userConfigOpen) return null;

  const closePanel = () => setUserConfigOpen(false);

  return (
    <div className="flex h-full shrink-0 bg-bg-surface">
      <section
        role="region"
        aria-label="User config"
        className="flex flex-col h-full shrink-0 border-l border-border-subtle"
        style={{ width: USER_CONFIG_PANEL_WIDTH }}
      >
        {/* Header — mirrors the Probe Panel's two-line chrome (title +
            subheading) so the user always knows which context the dock
            is anchored to. The subheading here is the resolved ~/.claude
            path itself (mono-font, like PathHeader), since there is no
            mesh / node to name — render-only, no copy-to-clipboard here. */}
        <div className="flex items-center justify-between gap-2 px-3 py-2 border-b border-border-subtle min-h-[56px]">
          <div className="flex flex-col min-w-0 flex-1">
            <span className="text-sm text-text-primary font-medium truncate flex items-center gap-1.5">
              <span aria-hidden="true">📁</span>
              <span className="truncate">User Config</span>
            </span>
            {path && (
              <span
                className="text-2xs text-text-secondary truncate font-mono"
                title={path}
              >
                {path}
              </span>
            )}
          </div>
          <button
            type="button"
            onClick={closePanel}
            aria-label="Close user config panel"
            title="Close"
            className="text-text-muted hover:text-text-secondary transition-colors shrink-0 ml-2"
          >
            <svg
              width="14"
              height="14"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <path d="M18 6 6 18M6 6l12 12" />
            </svg>
          </button>
        </div>

        {/* Body — either the resolved FileTree, a loading spinner, or a
            surfaced error. We don't pre-flight `pathError` here because
            the FileTree itself reports a clean error for the missing-dir
            case (e.g. ~/.claude doesn't exist on a fresh install — the
            backend's `list_directory` returns "Path does not exist"). */}
        <div className="flex-1 overflow-auto">
          {pathError && !path ? (
            <div className="flex items-center justify-center h-32 px-4 text-accent-red text-xs text-center">
              {pathError}
            </div>
          ) : !path ? (
            <div className="flex items-center justify-center h-32 text-text-muted text-xs">
              Loading user config…
            </div>
          ) : (
            <FileTree
              rootPath={path}
              showGitStatus={false}
              selectedFile={null}
              // Contract seam: `FileTree.onFileSelect` is required by
              // type but never fires on this surface. The `showGitStatus`
              // = false invariant at FileTree.tsx:134-136 short-circuits
              // the `useChangedFiles` fetch, so `handleFileClick`'s
              // `isChanged` branch is unreachable — only `onUnchanged-
              // FileSelect` ever receives a click. Routing that lands
              // on the Rust `open_in_editor` command (acceptance #6).
              onFileSelect={() => {}}
              onUnchangedFileSelect={async (p) => {
                try {
                  await openInEditor(p);
                } catch (e) {
                  console.error('Failed to open file in editor:', e);
                }
              }}
            />
          )}
        </div>
      </section>
    </div>
  );
}
