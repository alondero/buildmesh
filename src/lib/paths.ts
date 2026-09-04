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
//
// Issue #1519: Worktree Nodes created after the configurable directory
// landed carry the exact resolved dir in `worktree_path` (immutable).
// When present (trimmed, non-empty) it wins; `None` (Root Nodes +
// pre-#1519 rows) falls back to the legacy
// `<mesh>/.claude/worktrees/<name>` layout byte-for-byte. Mirrors
// `env::node_working_path` — keep the two in sync.
export function getNodeGitPath(node: {
  path: string;
  worktree_name?: string | null;
  use_worktree?: boolean;
  worktree_path?: string | null;
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
 * Effective worktree container dir (raw form) for a Mesh.
 * Precedence: Mesh override → app default → `.claude/worktrees` under root.
 * Relative values join from `meshPath` with `/`; absolute used verbatim.
 * Trimmed; blank collapses to inherit/default. No shell/`~` expansion.
 * Mirrors `env::effective_worktree_dir_raw` — keep the two in sync.
 */
export function getEffectiveWorktreeDir(
  meshPath: string,
  meshDirectory?: string | null,
  appDirectory?: string | null,
): string {
  const clean = (v?: string | null): string | null => {
    const t = v?.trim();
    return t ? t : null;
  };
  const chosen = clean(meshDirectory) ?? clean(appDirectory);
  if (!chosen) return `${meshPath}/.claude/worktrees`;
  // Mirrors the backend normalization (issue #1519): trailing separators
  // trimmed on absolute values; leading/trailing stripped on relative ones
  // (the backend additionally rejects `.`/`..`/forbidden segments at the
  // write boundary — this helper is display-only).
  if (isAbsoluteWorktreePath(chosen)) {
    const dir = chosen.replace(/[/\\]+$/, '');
    if (!dir) return `${meshPath}/.claude/worktrees`;
    return dir;
  }
  const dir = chosen.replace(/^[/\\]+/, '').replace(/[/\\]+$/, '');
  if (!dir) return `${meshPath}/.claude/worktrees`;
  return `${meshPath}/${dir}`;
}

function isAbsoluteWorktreePath(p: string): boolean {
  const t = p.trim();
  if (!t) return false;
  if (t.startsWith('/')) return true;
  if (t.startsWith('\\\\') || t.startsWith('//')) return true;
  if (/^[a-zA-Z]:[\\/]/.test(t)) return true;
  return false;
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
  event: { path: string; internal_path?: string | undefined },
  watchedPath: string | null | undefined,
  // Issue #1519: effective container dirs for mesh-root subscriptions with
  // custom locations (relative inside-mesh + absolute outside). When
  // supplied, a candidate under any of them also matches the mesh root.
  // Node-level subscriptions (watched == worktree path) don't need it —
  // exact match already covers the configured path via `getNodeGitPath`.
  effectiveDirs?: Array<string | null | undefined>,
): boolean {
  if (!watchedPath) return false;

  const watched = normalizePath(watchedPath);
  // The worktree-prefix form is what makes a mesh-root subscription catch
  // edits inside any of its worktree subdirs. It's a no-op for worktree
  // paths (a worktree is already `<root>/.claude/worktrees/<name>`, so a path
  // starting with `<worktree>/.claude/worktrees/` would only ever match a
  // *nested* worktree, which buildmesh never creates).
  // Issue #1519: also match any subdir under the mesh root (covers relative
  // custom dirs inside the mesh) plus the configured effective dirs
  // (covers absolute locations outside the root).
  const legacyPrefix = `${watched}/.claude/worktrees/`;
  const extraPrefixes = (effectiveDirs ?? [])
    .filter((d): d is string => typeof d === 'string' && d.trim().length > 0)
    .map((d) => `${normalizePath(d)}/`);
  const candidates = [event.path, event.internal_path].filter(
    (c): c is string => typeof c === 'string' && c.length > 0,
  );

  for (const candidate of candidates) {
    const norm = normalizePath(candidate);
    if (norm === watched) return true;
    if (norm.startsWith(legacyPrefix)) return true;
    for (const prefix of extraPrefixes) {
      if (norm === normalizePath(prefix.slice(0, -1))) return true;
      if (norm.startsWith(prefix)) return true;
    }
  }
  return false;
}
