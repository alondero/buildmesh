import { isWindows } from './platform';

// `worktree_name` is `string | null` (the generated `AgentNode` shape — ts-rs
// emits Rust `Option<String>` as `string | null`, issue #359); `null` is falsy
// so the guard below handles it the same as the old `undefined`.
//
// The trim mirrors the canonical `env::worktree_segment` rule in
// `src-tauri/src/env/mod.rs`, and is paired with `node_internal_path` in
// `src-tauri/src/commands/file_watcher.rs`. The GIT_CHANGED `internal_path`
// Rust emits and the path this helper returns must be byte-identical — a
// divergence (even just whitespace) means the event never matches the
// subscription and the node's changed-files goes stale. Paired-constant
// pattern, not a single source of truth (issue #387, ADR-0010).
export function getNodeGitPath(node: {
  path: string;
  worktree_name?: string | null;
  worktree_path?: string | null;
  use_worktree?: boolean;
}): string {
  if (node.use_worktree !== false && node.worktree_name) {
    const trimmed = node.worktree_name.trim();
    if (trimmed) {
      const stored = node.worktree_path?.trim();
      if (stored) return stored;
      return `${node.path}/.claude/worktrees/${trimmed}`;
    }
  }
  return node.path;
}

/**
 * Resolve the directory containing a Mesh's Worktree Nodes for display and
 * path matching. The backend applies the same precedence (Mesh override,
 * app-wide default, legacy fallback); this paired helper keeps the settings
 * UI from showing a relative preference without its Mesh root context.
 */
export function getEffectiveWorktreeDirectory(
  meshPath: string,
  meshOverride?: string | null,
  appDefault?: string | null,
): string {
  const configured = meshOverride?.trim() || appDefault?.trim() || '.claude/worktrees';
  const isAbsolute =
    configured.startsWith('/') ||
    configured.startsWith('\\\\') ||
    configured.startsWith('//') ||
    /^[A-Za-z]:[\\/]/.test(configured);
  if (isAbsolute) return configured;
  const separator = meshPath.includes('\\') ? '\\' : '/';
  return `${meshPath.replace(/[\\/]+$/, '')}${separator}${configured.replace(/^[\\/]+/, '')}`;
}

/**
 * Normalize a filesystem path for cross-platform equality.
 *
 * Collapses forward and back slashes to `/`, strips trailing separators, and
 * (on Windows) lowercases. The OS file lookup is the source of truth for
 * case-sensitivity, so we only lowercase on Windows.
 */
function normalizePath(p: string): string {
  let n = p.replace(/\\/g, '/');
  n = n.replace(/\/+$/, '');
  if (isWindows) n = n.toLowerCase();
  return n;
}

/**
 * Does this `GIT_CHANGED` event refer to work happening inside `watchedPath`?
 *
 * `watchedPath` is the path the consumer cares about — either a mesh root
 * (then any worktree subdir of it counts as a match) or a specific worktree
 * path (then only an exact match counts). The two-tier match lets the same
 * helper serve:
 *
 *   - Mesh-level consumers (`useMeshHealth`, `useMeshGitStatus`,
 *     `useGitBranchStatus` in `MeshItem`) that subscribe with the mesh root
 *     and want to refresh on any worktree edit.
 *   - Node-level consumers (`useGitSummary`, `useOpenPr`,
 *     `AgentChangedFilesList`, `CenterDiffOverlay`) that subscribe with the
 *     worktree path itself.
 *
 * The helper also normalizes for cross-platform match: forward/back-slashes
 * are collapsed, trailing separators stripped, and (on Windows) the
 * comparison is case-insensitive. macOS/Linux keep case-sensitivity.
 *
 * Returns `false` for null/undefined/empty `watchedPath`.
 */
export function pathMatchesGitEvent(
  event: { path: string; internal_path?: string | undefined; mesh_path?: string | undefined },
  watchedPath: string | null | undefined,
): boolean {
  if (!watchedPath) return false;

  const watched = normalizePath(watchedPath);
  // The worktree-prefix form is what makes a mesh-root subscription catch
  // edits inside any of its worktree subdirs. It's a no-op for worktree
  // paths (a worktree is already `<root>/.claude/worktrees/<name>`, so a path
  // starting with `<worktree>/.claude/worktrees/` would only ever match a
  // *nested* worktree, which buildmesh never creates).
  const worktreePrefix = `${watched}/.claude/worktrees/`;
  const candidates = [event.path, event.internal_path, event.mesh_path].filter(
    (c): c is string => typeof c === 'string' && c.length > 0,
  );

  for (const candidate of candidates) {
    const norm = normalizePath(candidate);
    if (norm === watched) return true;
    if (norm.startsWith(worktreePrefix)) return true;
  }
  return false;
}
