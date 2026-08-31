/**
 * Multi-domain indexers for the Command Omnibar (wayfinder #1371, task
 * #1410).
 *
 * Each indexer is a pure function that projects one slice of the app's
 * in-memory state (agent nodes, meshes, provider menu, cached GitHub issues
 * and PRs) into `IndexedItem`s for `searchItems` in `./fuzzySearch.ts`. The
 * indexers never touch stores or IPC — the UI layer feeds them the data and
 * re-runs them when that data changes.
 *
 * Domain contract (issue #1410 §1):
 *   - Agent Nodes:  name, branch, worktree name, provider/harness, session
 *                   status, and parent mesh name.
 *   - Meshes:       mesh name, repo path, and active branch.
 *   - App Commands: theme toggling, view mode switches (Single, Mesh,
 *                   Pinned, All), open Settings, open Remote Access, show
 *                   Cheatsheet, Git sync, and Probe tab shortcuts.
 *   - GitHub:       loaded issues and pull requests for active/cached meshes.
 *   - Spawning:     quick-spawn actions for all registered harnesses across
 *                   available meshes.
 */
import type { AgentNode } from '../../types/generated/AgentNode';
import type { Mesh } from '../../types/generated/Mesh';
import type { GitHubIssue } from '../../types/generated/GitHubIssue';
import type { GitHubPullRequest } from '../../types/generated/GitHubPullRequest';
import type { SpawnOption } from '../groups';
import type { ProbeTab } from '../../stores/uiStore';
import type { ViewMode } from '../../stores/uiStore';
import { getStatusConfig } from '../status';
import type { IndexedField, IndexedItem, FieldWeight } from './fuzzySearch';

/** Build a weighted field with its pre-folded text. Folding happens ONCE at
 *  index-build time (locale-invariant `toLowerCase()` — see the engine note
 *  in `fuzzySearch.ts`), so the per-keystroke search path never allocates. */
export function field(text: string, weight: FieldWeight): IndexedField {
  return { text, foldedText: text.toLowerCase(), weight };
}

/** Category ids the indexers emit — one per domain, stable for the UI. */
export const CATEGORY = {
  node: 'node',
  mesh: 'mesh',
  command: 'command',
  issue: 'issue',
  pullRequest: 'pull-request',
  spawn: 'spawn',
} as const;
export type Category = (typeof CATEGORY)[keyof typeof CATEGORY];

/**
 * The prefix characters that scope the palette to a domain (issue #1410 §2).
 * Maps each leading character to the categories its scope includes — `'/'`
 * and `'+'` both scope spawning, `'#'` spans GitHub issues and pull
 * requests, `'>'` is commands ONLY (the issue spec; meshes are intentionally
 * excluded — see the `filterByPrefix` doc). The UI layer can render the
 * prefix hints from this table and/or delegate to `filterByPrefix`.
 */
export const PREFIX_FILTERS: ReadonlyArray<{
  prefix: string;
  description: string;
  categories: readonly Category[];
}> = [
  { prefix: '>', description: 'Commands', categories: [CATEGORY.command] },
  { prefix: '@', description: 'Agent nodes', categories: [CATEGORY.node] },
  { prefix: '/', description: 'Spawning actions', categories: [CATEGORY.spawn] },
  { prefix: '+', description: 'Spawning actions', categories: [CATEGORY.spawn] },
  { prefix: '#', description: 'GitHub issues and pull requests', categories: [CATEGORY.issue, CATEGORY.pullRequest] },
];

/**
 * The searchable palette: every indexed item, regardless of domain. The UI
 * layer owns keeping this in sync with the stores (subscribe to the stores,
 * re-run the indexers, replace the array).
 */
export type OmnibarIndex = IndexedItem[];

/** Merge the five domains into one palette array. */
export function buildOmnibarIndex(opts: {
  nodes: AgentNode[];
  meshes: Mesh[];
  commands: AppCommand[];
  spawnOptions: SpawnOption[];
  issues: { meshId: number; items: GitHubIssue[] }[];
  pullRequests: { meshId: number; items: GitHubPullRequest[] }[];
}): OmnibarIndex {
  const { nodes, meshes, commands, spawnOptions, issues, pullRequests } = opts;
  const nodeItems = indexAgentNodes(nodes, meshes);
  const meshItems = indexMeshes(meshes);
  const commandItems = indexCommands(commands);
  const spawnItems = indexSpawnOptions(spawnOptions, meshes);
  const issueItems = indexGitHub(issues, pullRequests, meshes);
  return [
    ...nodeItems,
    ...meshItems,
    ...commandItems,
    ...spawnItems,
    ...issueItems,
  ];
}

/** A built-in palette command — the "App Commands" domain (issue #1410 §1). */
export interface AppCommand {
  /** Stable id (e.g. `cycle-grid-modes`), also used for the item id. */
  id: string;
  /** Title shown in the palette row. */
  label: string;
  /** Short description / keyboard hint for the subtitle line. */
  subtitle?: string;
  /** Icon hint for the row. */
  icon?: string;
  /** Search aliases beyond the label (e.g. `theme`, `mode`). */
  keywords?: string[];
}

/** The full built-in command catalog (issue #1410 §1 — theme toggling, view
 *  mode switches, Settings, Remote Access, Cheatsheet, Git sync, Probe tabs).
 *  The UI layer maps the `id` to the actual store action when executed. */
export const APP_COMMANDS: readonly AppCommand[] = [
  {
    id: 'toggle-theme',
    label: 'Toggle theme (dark / light)',
    subtitle: 'Switch the app color scheme',
    icon: 'theme',
    keywords: ['dark', 'light', 'appearance'],
  },
  { id: 'view-single', label: 'Switch view: Single', subtitle: 'Solo the active node', icon: 'single', keywords: ['maximize', 'focus'] },
  { id: 'view-mesh', label: 'Switch view: Mesh Grid', subtitle: 'Scope to the selected mesh', icon: 'mesh', keywords: ['grid', 'scope'] },
  { id: 'view-pinned', label: 'Switch view: Pinned', subtitle: 'Pinned nodes across all meshes', icon: 'pinned', keywords: ['pin'] },
  { id: 'view-all', label: 'Switch view: All Nodes', subtitle: 'Every loaded node', icon: 'all', keywords: ['all', 'nodes'] },
  { id: 'open-settings', label: 'Open Settings', subtitle: 'App preferences', icon: 'settings', keywords: ['preferences', 'config'] },
  { id: 'open-remote-access', label: 'Open Remote Access', subtitle: 'Mobile QR pairing', icon: 'remote', keywords: ['mobile', 'qr', 'pair'] },
  { id: 'show-cheatsheet', label: 'Show Cheatsheet', subtitle: 'Keyboard shortcuts', icon: 'cheatsheet', keywords: ['shortcuts', 'keys', 'help', '?'] },
  { id: 'git-sync', label: 'Git sync', subtitle: 'Fetch and pull all meshes', icon: 'sync', keywords: ['fetch', 'pull', 'git'] },
  { id: 'probe-files', label: 'Probe tab: Files', subtitle: 'Open the Files tab', icon: 'files', keywords: ['project', 'files'] },
  { id: 'probe-review', label: 'Probe tab: Review', subtitle: 'Open the Review tab', icon: 'review', keywords: ['changes', 'diff'] },
  { id: 'probe-issues', label: 'Probe tab: Issues', subtitle: 'Open the Issues tab', icon: 'issues', keywords: ['github', 'issues'] },
  { id: 'probe-pulls', label: 'Probe tab: Pull Requests', subtitle: 'Open the Pull Requests tab', icon: 'pulls', keywords: ['github', 'pr'] },
  { id: 'probe-sessions', label: 'Probe tab: Sessions', subtitle: 'Open the Sessions tab', icon: 'sessions', keywords: ['agents'] },
  { id: 'probe-worktrees', label: 'Probe tab: Worktrees', subtitle: 'Open the Worktrees tab', icon: 'worktrees', keywords: ['git', 'worktree'] },
];

/** The `ProbeTab` ids covered by the probe-tab commands above — keeps the
 *  catalog and the store's tab vocabulary from drifting. */
export const PROBE_TAB_COMMANDS: readonly ProbeTab[] = [
  'files',
  'review',
  'issues',
  'pulls',
  'sessions',
  'worktrees',
];

/** Map a `ViewMode` to its omnibar command id (shared with the UI layer). */
export function viewModeCommandId(mode: ViewMode): string {
  return `view-${mode}`;
}

/** Agent Nodes (issue #1410 §1 — name, branch, worktree name, provider /
 *  harness, session status, parent mesh name). The mesh lookup supplies the
 *  parent-mesh name field and subtitle. */
export function indexAgentNodes(nodes: AgentNode[], meshes: Mesh[]): IndexedItem[] {
  const meshNameById = new Map(meshes.map((m) => [m.id, m.name]));
  const items: IndexedItem[] = [];
  for (const node of nodes) {
    const statusLabel = getStatusConfig(node.status).label;
    const meshName = meshNameById.get(node.mesh_id) ?? '';
    const fields: IndexedItem['fields'] = [
      field(node.name, 'primary'),
      field(node.branch, 'secondary'),
      field(node.worktree_name ?? '', 'secondary'),
      field(node.provider, 'secondary'),
      field(statusLabel, 'secondary'),
      field(meshName, 'secondary'),
    ];
    const subtitle = [
      node.worktree_name ?? node.branch,
      meshName || undefined,
    ].filter(Boolean).join(' · ');
    items.push({
      id: `node:${node.id}`,
      category: CATEGORY.node,
      label: node.name,
      subtitle,
      icon: 'node',
      fields,
    });
  }
  return items;
}

/** Meshes (issue #1410 §1 — mesh name, repo path, active branch). The active
 *  branch is `base_ref` (the mesh's canonical base branch — see the generated
 *  `AgentNode.branch` doc for the overload note). */
export function indexMeshes(meshes: Mesh[]): IndexedItem[] {
  const items: IndexedItem[] = [];
  for (const mesh of meshes) {
    const fields: IndexedItem['fields'] = [
      field(mesh.name, 'primary'),
      field(mesh.path, 'secondary'),
      field(mesh.base_ref, 'secondary'),
    ];
    const subtitle = [mesh.path, mesh.base_ref || undefined].filter(Boolean).join(' · ');
    items.push({
      id: `mesh:${mesh.id}`,
      category: CATEGORY.mesh,
      label: mesh.name,
      subtitle,
      icon: 'mesh',
      fields,
    });
  }
  return items;
}

/** App Commands (issue #1410 §1). Each command's label is the primary field
 *  and its keywords the secondary field(s), so `settings` matches both the
 *  label (`Open Settings`) and the keyword alias. */
export function indexCommands(commands: readonly AppCommand[]): IndexedItem[] {
  const items: IndexedItem[] = [];
  for (const cmd of commands) {
    const fields: IndexedItem['fields'] = [
      field(cmd.label, 'primary'),
      ...(cmd.keywords ?? []).map((keyword) => field(keyword, 'secondary')),
    ];
    items.push({
      id: `command:${cmd.id}`,
      category: CATEGORY.command,
      label: cmd.label,
      subtitle: cmd.subtitle,
      icon: cmd.icon,
      fields,
    });
  }
  return items;
}

/** GitHub Probes (issue #1410 §1 — loaded issues and pull requests for
 *  active/cached meshes). Each item carries the mesh name as a secondary
 *  field so a `#mesh`-style query can scope results. */
export function indexGitHub(
  issues: { meshId: number; items: GitHubIssue[] }[],
  pullRequests: { meshId: number; items: GitHubPullRequest[] }[],
  meshes: Mesh[],
): IndexedItem[] {
  const meshNameById = new Map(meshes.map((m) => [m.id, m.name]));
  const items: IndexedItem[] = [];

  for (const { meshId, items: list } of issues) {
    const meshName = meshNameById.get(meshId) ?? '';
    for (const issue of list) {
      const fields: IndexedItem['fields'] = [
        field(issue.title, 'primary'),
        // Index BOTH the `#N` form (so `#101` starts at index 0 and wins the
        // prefix bonus) and the bare number (so `101` isn't penalised for
        // the leading `#`). Review #1425.
        field(`#${issue.number}`, 'secondary'),
        field(String(issue.number), 'secondary'),
        field(issue.labels.join(' '), 'secondary'),
        field(meshName, 'secondary'),
      ];
      items.push({
        id: `issue:${meshId}:${issue.number}`,
        category: CATEGORY.issue,
        label: `#${issue.number} ${issue.title}`,
        subtitle: meshName || undefined,
        icon: 'issue',
        fields,
      });
    }
  }

  for (const { meshId, items: list } of pullRequests) {
    const meshName = meshNameById.get(meshId) ?? '';
    for (const pr of list) {
      const fields: IndexedItem['fields'] = [
        field(pr.title, 'primary'),
        field(`#${pr.number}`, 'secondary'),
        field(String(pr.number), 'secondary'),
        field(pr.head_ref, 'secondary'),
        field(meshName, 'secondary'),
      ];
      items.push({
        id: `pull:${meshId}:${pr.number}`,
        category: CATEGORY.pullRequest,
        label: `#${pr.number} ${pr.title}`,
        subtitle: meshName || undefined,
        icon: 'pr',
        fields,
      });
    }
  }

  return items;
}

/** Spawning Recipes (issue #1410 §1 — quick-spawn actions for all registered
 *  harnesses across available meshes). One item per `SpawnOption`, with the
 *  mesh name as a secondary field so the `/spawn mesh` drill-down works.
 *
 *  Spawn options are intentionally mesh-bound (the action spawns INTO a
 *  specific mesh): with an empty `meshes` list the indexer emits nothing —
 *  the palette simply has no spawn entries until a mesh is loaded, which is
 *  the correct pre-boot state. Review #1425. */
export function indexSpawnOptions(
  spawnOptions: SpawnOption[],
  meshes: Mesh[],
): IndexedItem[] {
  const items: IndexedItem[] = [];
  for (const option of spawnOptions) {
    const label = `Spawn ${option.label}`;
    const fields: IndexedItem['fields'] = [
      field(label, 'primary'),
      field(option.label, 'secondary'),
      field(option.harness_id, 'secondary'),
    ];
    for (const mesh of meshes) {
      items.push({
        id: `spawn:${option.id}:${mesh.id}`,
        category: CATEGORY.spawn,
        label,
        subtitle: mesh.name,
        icon: option.icon || 'spawn',
        fields: [
          ...fields,
          field(mesh.name, 'secondary'),
        ],
      });
    }
  }
  return items;
}

/**
 * Apply an omnibar domain prefix to a raw query (issue #1410 §2). Returns
 * the filtered items plus the remaining search text. Prefixes:
 *   `>` → commands only · `@` → agent nodes · `/` or `+` → spawning actions ·
 *   `#` → GitHub issues and pull requests.
 *
 * `>` is commands ONLY per the issue spec (review #1425) — meshes are
 * deliberately excluded so an action-menu query never surfaces repo entities.
 * A bare prefix (e.g. just `>`) yields the scoped list with an empty search
 * query; callers can combine this with an `emptyMode` to show the whole
 * domain instead of a blank palette.
 */
export function filterByPrefix(
  items: readonly IndexedItem[],
  rawQuery: string,
): { items: IndexedItem[]; query: string } {
  const trimmed = rawQuery;
  const first = trimmed[0];
  const rest = trimmed.slice(1);
  switch (first) {
    case '>':
      return { items: items.filter((i) => i.category === CATEGORY.command), query: rest };
    case '@':
      return { items: items.filter((i) => i.category === CATEGORY.node), query: rest };
    case '/':
    case '+':
      return { items: items.filter((i) => i.category === CATEGORY.spawn), query: rest };
    case '#':
      return {
        items: items.filter((i) => i.category === CATEGORY.issue || i.category === CATEGORY.pullRequest),
        query: rest,
      };
    default:
      return { items: items.slice(), query: trimmed };
  }
}
