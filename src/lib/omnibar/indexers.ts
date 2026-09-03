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
 *                   Cheatsheet, Git sync, and the inspector destinations
 *                   (issue #1375).
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
import { PROBE_TAB_ORDER } from '../probeContext';
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
 * requests, `'>'` is the action menu (commands + spawn; meshes are
 * intentionally excluded — see the `filterByPrefix` doc). The UI layer
 * can render the prefix hints from this table and/or delegate to
 * `filterByPrefix`.
 */
export const PREFIX_FILTERS: ReadonlyArray<{
  prefix: string;
  description: string;
  categories: readonly Category[];
}> = [
  // `>` is the action menu (issue #1413): built-in commands PLUS quick-spawn
  // recipes. Repo entities (meshes, nodes, issues) stay out so an action
  // query never surfaces them — the #1410 / review #1425 exclusion, now
  // with spawn included as an action rather than a repo entity.
  { prefix: '>', description: 'Commands', categories: [CATEGORY.command, CATEGORY.spawn] },
  { prefix: '@', description: 'Agent nodes', categories: [CATEGORY.node] },
  { prefix: '/', description: 'Spawning actions', categories: [CATEGORY.spawn] },
  { prefix: '+', description: 'Spawning actions', categories: [CATEGORY.spawn] },
  { prefix: '#', description: 'GitHub issues and pull requests', categories: [CATEGORY.issue, CATEGORY.pullRequest] },
];

/**
 * The inverse of `PREFIX_FILTERS`: one canonical prefix per category, for
 * consumers that need to drill a category INTO its domain (the omnibar's
 * Tab gesture, issue #1411). Spawn has two legal prefixes (`/` and `+`);
 * `/` is the canonical drill-in target. Categories absent from the map
 * (meshes) have no prefix domain.
 */
export const CATEGORY_PREFIX: Partial<Record<Category, string>> = {
  [CATEGORY.command]: '>',
  [CATEGORY.node]: '@',
  [CATEGORY.spawn]: '/',
  [CATEGORY.issue]: '#',
  [CATEGORY.pullRequest]: '#',
};

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

/**
 * Inspector-destination palette commands (issue #1375). The Record is
 * keyed by the `probe-<tab>` routing id and typed as
 * `Record<\`probe-${ProbeTab}\`, AppCommand>` so adding a value to
 * `PROBE_TAB_ORDER` in `probeContext.ts:155` forces a matching entry
 * here at compile time. The previous `PROBE_TAB_COMMANDS =
 * PROBE_TAB_ORDER` alias was tautological — it never read from the
 * AppCommand array, so a missing entry on this side compiled cleanly
 * (PR #1489 review #1B). The flat `APP_COMMANDS` array below merges
 * non-probe entries with `Object.values` of this map; consumers stay
 * ignorant of the split.
 *
 * Exported (not just spread into `APP_COMMANDS`) so the palette's tool
 * discovery screen can read the same labels instead of re-declaring copy
 * — one catalog, two readers.
 */
export const PROBE_DESTINATION_COMMANDS: Record<`probe-${ProbeTab}`, AppCommand> = {
  'probe-files': {
    id: 'probe-files',
    label: 'Open Files',
    subtitle: 'Browse the project explorer',
    icon: 'files',
    keywords: ['project', 'explorer', 'file tree'],
  },
  'probe-review': {
    id: 'probe-review',
    label: 'Open Agent Changes',
    subtitle: 'Review what your agent changed',
    icon: 'review',
    keywords: ['changes', 'diff', 'review'],
  },
  'probe-usage': {
    id: 'probe-usage',
    label: 'Open Usage',
    subtitle: 'Check provider usage and limits',
    icon: 'usage',
    keywords: ['meters', 'limits', 'quota', 'balance'],
  },
  'probe-worktrees': {
    id: 'probe-worktrees',
    label: 'Open Worktrees',
    subtitle: 'Switch branches and working folders',
    icon: 'worktrees',
    keywords: ['git', 'worktree', 'branch'],
  },
  'probe-properties': {
    id: 'probe-properties',
    label: 'Open Project Settings',
    subtitle: 'Configure the selected project',
    icon: 'properties',
    keywords: ['project', 'properties', 'build', 'run'],
  },
  'probe-autopilot': {
    // ADR-0030 "one name per destination" — the palette label carries the
    // inspector header (`Autopilot`) under the established `Open <Header>`
    // pattern (Open Project Settings, Open GitHub Issues, ...). The previous
    // `Open Automation: Autopilot` form added a namespace the inspector
    // header did not share, breaking the palette ↔ header parity rule.
    id: 'probe-autopilot',
    label: 'Open Autopilot',
    subtitle: 'Keep recurring work moving',
    icon: 'autopilot',
    keywords: ['automation', 'policies', 'loops', 'issue-driven'],
  },
  'probe-circuits': {
    // See probe-autopilot above — the `Automation:` prefix was a one-off
    // namespace that did not appear in the inspector header, so the palette
    // entry was the only surface using two words for the same destination.
    id: 'probe-circuits',
    label: 'Open Circuits',
    subtitle: 'Inspect connected automations',
    icon: 'circuits',
    keywords: ['automation', 'flows', 'graphs'],
  },
  'probe-issues': {
    id: 'probe-issues',
    label: 'Open GitHub Issues',
    subtitle: 'Find work to pick up',
    icon: 'issues',
    keywords: ['github', 'bugs', 'backlog'],
  },
  'probe-pulls': {
    id: 'probe-pulls',
    label: 'Open GitHub Pull Requests',
    subtitle: 'See what is ready to merge',
    icon: 'pulls',
    keywords: ['github', 'pr', 'merge'],
  },
  'probe-sessions': {
    id: 'probe-sessions',
    label: 'Open Agent History',
    subtitle: 'Return to previous agent work',
    icon: 'sessions',
    keywords: ['archive', 'resume', 'history', 'completed', 'failed'],
  },
  'probe-scratchpad': {
    id: 'probe-scratchpad',
    label: 'Open Notes',
    subtitle: 'Keep notes beside the work',
    icon: 'scratchpad',
    keywords: ['scratch pad', 'notes', 'memo'],
  },
};

/**
 * The task-oriented palette destinations (issue #1375). The palette is the
 * primary navigation surface, so these entries use user-facing task names
 * and descriptions — never internal vocabulary like "Probe tab" or the
 * Host/Mesh/Agent lens headings. Every inspector destination is reachable
 * here (via the typed `PROBE_DESTINATION_COMMANDS` map above); Usage
 * additionally has a dedicated title-bar action. The UI layer maps the
 * `id` to the actual store action when executed.
 *
 * Probe destinations are spread from the type-checked Record so a missing
 * entry fails the build, not just the omnibar search test.
 */
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
  // 'settings' is an explicit alias so the dedicated command outranks the
  // "Open Project Settings" destination (whose label also matches the
  // query) when the user types a bare `settings` (issue #1375).
  { id: 'open-settings', label: 'Open Settings', subtitle: 'App preferences', icon: 'settings', keywords: ['settings', 'preferences', 'config'] },
  { id: 'open-remote-access', label: 'Open Remote Access', subtitle: 'Mobile QR pairing', icon: 'remote', keywords: ['mobile', 'qr', 'pair'] },
  { id: 'show-cheatsheet', label: 'Show Cheatsheet', subtitle: 'Keyboard shortcuts', icon: 'cheatsheet', keywords: ['shortcuts', 'keys', 'help', '?'] },
  { id: 'git-sync', label: 'Git sync', subtitle: 'Fetch and pull all meshes', icon: 'sync', keywords: ['fetch', 'pull', 'git'] },
  ...Object.values(PROBE_DESTINATION_COMMANDS),
];

/**
 * The `ProbeTab` ids covered by the destination commands above. Derived
 * from the typed `PROBE_DESTINATION_COMMANDS` map (issue #1375) so the
 * palette catalog and the inspector's destination vocabulary cannot
 * drift — every inspector destination is reachable from the palette by
 * construction. The `as ProbeTab[]` cast is safe: the Record key is
 * `probe-${ProbeTab}`, so stripping the prefix round-trips into
 * `PROBE_TAB_ORDER` membership.
 */
export const PROBE_TAB_COMMANDS: readonly ProbeTab[] = PROBE_TAB_ORDER;

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

/** Spawning Recipes (issue #1410 §1 / #1413 — quick-spawn actions for all
 *  registered harnesses across available meshes). One item per
 *  `(option, mesh)` pair. The label is `Spawn [Harness] on [Mesh]` so a
 *  `>` or `spawn` query surfaces the full recipe (issue #1413 §1); mesh
 *  name also stays a secondary field so `/spawn mesh` still drills down.
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
    for (const mesh of meshes) {
      const label = `Spawn ${option.label} on ${mesh.name}`;
      items.push({
        id: `spawn:${option.id}:${mesh.id}`,
        category: CATEGORY.spawn,
        label,
        subtitle: mesh.name,
        icon: option.icon || 'spawn',
        fields: [
          field(label, 'primary'),
          field(option.label, 'secondary'),
          field(option.harness_id, 'secondary'),
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
 *   `>` → commands + spawn actions · `@` → agent nodes · `/` or `+` →
 *   spawning actions · `#` → GitHub issues and pull requests.
 *
 * `>` is the action menu (issue #1413): commands AND spawn recipes.
 * Meshes / nodes / issues stay out so an action-menu query never
 * surfaces repo entities (review #1425).
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
      return {
        items: items.filter((i) => i.category === CATEGORY.command || i.category === CATEGORY.spawn),
        query: rest,
      };
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
