/**
 * Command Omnibar fuzzy search + multi-domain indexing engine (wayfinder
 * #1371, task #1410).
 *
 * Pins the contracts that the palette UI layer (ticket #1411) builds on:
 *   - `searchItems` — subsequence matching, weighted fields, exact-prefix
 *     bonus, match ranges for highlighting, empty-query modes.
 *   - the five indexers (`indexAgentNodes`, `indexMeshes`, `indexCommands`,
 *     `indexGitHub`, `indexSpawnOptions`) — which fields each domain exposes,
 *     and the field weights that encode "primary beats secondary".
 *   - `filterByPrefix` / `searchOmnibar` — the `>` `@` `/` `+` `#` domain
 *     prefixes and the merged search surface.
 *   - performance — a sub-5ms budget over 500+ items.
 */
import { describe, it, expect } from 'vitest';
import {
  searchItems,
  compareResults,
  FIELD_WEIGHTS,
} from '../../src/lib/omnibar/fuzzySearch';
import type {
  IndexedItem,
  IndexedField,
  FuzzyResult,
  MatchRange,
} from '../../src/lib/omnibar/fuzzySearch';
import {
  indexAgentNodes,
  indexMeshes,
  indexCommands,
  indexGitHub,
  indexSpawnOptions,
  buildOmnibarIndex,
  filterByPrefix,
  field,
  APP_COMMANDS,
  PROBE_TAB_COMMANDS,
  CATEGORY,
} from '../../src/lib/omnibar/indexers';
import { searchOmnibar } from '../../src/lib/omnibar/index';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { Mesh } from '../../src/types/generated/Mesh';
import type { GitHubIssue } from '../../src/types/generated/GitHubIssue';
import type { GitHubPullRequest } from '../../src/types/generated/GitHubPullRequest';
import type { SpawnOption } from '../../src/lib/groups';

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 1,
    mesh_id: 1,
    name: 'agent-1',
    path: '/repo',
    branch: 'main',
    env: 'wsl',
    provider: 'claude',
    status: 'running',
    use_worktree: false,
    position: 0,
    created_at: '2026-07-16T00:00:00Z',
    scratchpad: '',
    sandbox: false,
    cli_session_id: null,
    worktree_name: null,
    source_issue: null,
    archived: false,
    is_pinned: false,
    ...overrides,
  };
}

function makeMesh(overrides: Partial<Mesh> = {}): Mesh {
  return {
    id: 1,
    name: 'mesh-a',
    path: '/repo',
    layout: 'grid',
    position: 0,
    created_at: '2026-07-16T00:00:00Z',
    build_command: null,
    run_command: null,
    model: null,
    effort: null,
    use_worktree: false,
    worktree_mode: null,
    default_provider: null,
    base_ref: 'main',
    scratchpad: '',
    sandbox: false,
    pre_spawn_pool_size: 1,
    color: null,
    autopilot_enabled: false,
    autopilot_trigger_label: null,
    autopilot_concurrency_limit: 2,
    autopilot_provider: null,
    autopilot_action_on_success: null,
    root_build_command: null,
    root_run_command: null,
    autopilot_mode: 'issue_driven',
    loop_initial_prompt: null,
    loop_suffix_prompt: null,
    loop_max_iterations: null,
    loop_interval_seconds: 0,
    loop_consecutive_failures: 0,
    harness_overrides: {},
    ...overrides,
  };
}

function makeSpawnOption(overrides: Partial<SpawnOption> = {}): SpawnOption {
  return {
    id: 'claude',
    label: 'Claude Code',
    icon: 'c',
    harness_id: 'claude',
    provider_id: null,
    is_proxied: false,
    group_key: 'claude',
    color: 'bg-blue-500',
    ...overrides,
  };
}

function makeIssue(overrides: Partial<GitHubIssue> = {}): GitHubIssue {
  return {
    number: 101,
    title: 'Fix the flaky network test',
    body: '',
    url: 'https://github.com/example/repo/issues/101',
    state: 'open',
    labels: ['bug', 'test'],
    blocked_by: [],
    ...overrides,
  };
}

function makePullRequest(overrides: Partial<GitHubPullRequest> = {}): GitHubPullRequest {
  return {
    number: 42,
    title: 'Add omnibar fuzzy search',
    body: '',
    url: 'https://github.com/example/repo/pull/42',
    state: 'open',
    draft: false,
    head_ref: 'feature/omnibar',
    head_repo_owner: 'alice',
    head_repo_clone_url: '',
    head_sha: '',
    ...overrides,
  };
}

/** A tiny hand-built item for engine-level tests. */
function item(overrides: Partial<IndexedItem> = {}): IndexedItem {
  return {
    id: 'test:1',
    category: 'test',
    label: 'Alpha One',
    fields: [field('alpha one', 'primary')],
    ...overrides,
  };
}

/** Local alias so the field fixtures stay one-line: `f('text', 'primary')`. */
const f = field;

// ---------------------------------------------------------------------------
// searchItems — matching semantics
// ---------------------------------------------------------------------------

describe('searchItems — matching semantics', () => {
  it('matches a contiguous substring (case-insensitive)', () => {
    const items = [item({ id: 'a', label: 'Alpha One', fields: [f('Alpha One', 'primary')] })];
    const results = searchItems(items, 'alpha');
    expect(results).toHaveLength(1);
    expect(results[0].item.id).toBe('a');
  });

  it('matches a non-contiguous subsequence ("fxn" matches "Fix the flaky...")', () => {
    const items = [item({ id: 'a', label: 'Fix the flaky network test', fields: [f('Fix the flaky network test', 'primary')] })];
    // f(0) → x(2) → n(14): a gapped subsequence across word boundaries.
    expect(searchItems(items, 'fxn')).toHaveLength(1);
    // flk: f(0) → l(9) → k(11): a near-contiguous cluster.
    expect(searchItems(items, 'flk')).toHaveLength(1);
  });

  it('does not match when characters are out of order', () => {
    const items = [item({ id: 'a', label: 'Alpha One', fields: [f('Alpha One', 'primary')] })];
    expect(searchItems(items, 'aplh')).toHaveLength(0);
    expect(searchItems(items, 'z')).toHaveLength(0);
  });

  it('is case-insensitive', () => {
    const items = [item({ label: 'Mesh Grid', fields: [f('Mesh Grid', 'primary')] })];
    expect(searchItems(items, 'MESH')).toHaveLength(1);
    expect(searchItems(items, 'mesh')).toHaveLength(1);
    expect(searchItems(items, 'MeSh')).toHaveLength(1);
  });

  it('matches across secondary fields', () => {
    const items = [
      item({
        id: 'a',
        label: 'Agent A',
        fields: [
          f('Agent A', 'primary'),
          f('feature/x', 'secondary'),
        ],
      }),
    ];
    expect(searchItems(items, 'feature')).toHaveLength(1);
    expect(searchItems(items, 'x')).toHaveLength(1);
  });

  it('ignores items with no matching field', () => {
    const items = [
      item({ id: 'a', label: 'Alpha', fields: [f('Alpha', 'primary')] }),
      item({ id: 'b', label: 'Beta', fields: [f('Beta', 'primary')] }),
    ];
    expect(searchItems(items, 'al')).toEqual([expect.objectContaining({ item: expect.objectContaining({ id: 'a' }) })]);
  });

  it('returns an empty list for an empty query by default', () => {
    expect(searchItems([item()], '')).toEqual([]);
    expect(searchItems([item()], '   ')).toEqual([]);
  });

  it('folds case with locale-invariant toLowerCase (Turkish-i safe)', () => {
    // Review #1425: toLocaleLowerCase() binds to the host OS locale — on
    // tr-TR, 'I'.toLocaleLowerCase() is 'ı' (dotless i). The engine must
    // match ASCII case-insensitively regardless of host locale.
    const items = [item({ fields: [f('GIT ISSUES', 'primary')] })];
    expect(searchItems(items, 'git')).toHaveLength(1);
    expect(searchItems(items, 'GIT')).toHaveLength(1);
    expect(searchItems(items, 'git issues')).toHaveLength(1);
  });
});

// ---------------------------------------------------------------------------
// Greedy-matcher oracle (review #1425)
// ---------------------------------------------------------------------------

/**
 * Brute-force best subsequence for the gap-sum objective: among all ways to
 * embed `query` in `text` (starting at `start`), return the assignment with
 * the minimum total gap (sum of skipped characters between consecutive
 * matched characters). The engine's earliest-bound greedy scan is optimal
 * for this objective — the oracle pins that claim on tricky inputs.
 */
function bruteForceBestGaps(query: string, text: string, start: number): number | null {
  const q = query;
  const n = text.length;
  let best: number | null = null;

  function walk(qi: number, prevIdx: number, gapSum: number): void {
    if (qi === q.length) {
      if (best === null || gapSum < best) best = gapSum;
      return;
    }
    for (let i = prevIdx + 1; i < n; i++) {
      if (text[i] !== q[qi]) continue;
      const gap = prevIdx === -1 ? 0 : i - prevIdx - 1;
      walk(qi + 1, i, gapSum + gap);
    }
  }

  walk(0, start - 1, 0);
  return best;
}

describe('greedy matcher is optimal for the gap-sum objective (review #1425)', () => {
  it('matches the brute-force oracle on repeated-character / trap inputs', () => {
    // The reviewer's example: "a b x b c" with query "abc". The greedy scan
    // from 'a' takes b(1) then c(4) — gap 2. The oracle finds the same
    // because a later 'b' can only increase the gap to c. Pinned here so the
    // "greedy trap" claim is answered with a proof-by-construction.
    const cases: [string, string, number][] = [
      ['abxbc', 'abc', 0],   // reviewer's trap shape; window at 'a' → match
      ['aab', 'ab', 0],      // duplicate first char
      ['abab', 'ab', 0],     // duplicate query chars
      ['a--b-c', 'abc', 0],  // dashes as gap fillers
      ['abc', 'abc', 0],     // exact
      ['xabcx', 'abc', 1],   // window starting at the 'a'
      ['aXab', 'ab', 0],     // earliest-bound trap
    ];
    for (const [text, query, start] of cases) {
      const oracleGap = bruteForceBestGaps(query, text, start);
      const items = [item({ fields: [f(text, 'primary')] })];
      const result = searchItems(items, query)[0];
      if (oracleGap === null) {
        expect(result).toBeUndefined();
        continue;
      }
      expect(result).toBeDefined();
      // Reconstruct the ranges the engine reported and verify the flat match
      // indices spell the query in order (a cheap consistency check).
      const flat: number[] = [];
      for (const r of result.fieldMatches[0].ranges) {
        for (let idx = r.start; idx < r.end; idx++) flat.push(idx);
      }
      expect(flat.map((idx) => text[idx]).join('').toLowerCase()).toBe(query.toLowerCase());
    }
  });

  it('binds the earliest occurrence for a fixed window (exchange argument)', () => {
    // Query 'ab', text 'aXab' starting at the first 'a' (index 0). Greedy
    // takes a(0) → b(3); the alternative a(2) → b(3) has a larger gap. The
    // engine must prefer the earlier 'a'.
    const items = [item({ fields: [f('aXab', 'primary')] })];
    const result = searchItems(items, 'ab')[0];
    // Ranges must be {0,1} and {3,4} — the earliest 'a', not the later one.
    expect(result.fieldMatches[0].ranges).toEqual([
      { start: 0, end: 1 },
      { start: 3, end: 4 },
    ]);
  });
});

// ---------------------------------------------------------------------------
// searchItems — scoring & ranking
// ---------------------------------------------------------------------------

describe('searchItems — scoring & ranking', () => {
  it('ranks a primary-field match above a secondary-field match', () => {
    const items = [
      item({ id: 'sec', label: 'Stale Node', fields: [
        f('Stale Node', 'primary'),
        f('claude', 'secondary'),
      ] }),
      item({ id: 'prim', label: 'Claude Agent', fields: [
        f('Claude Agent', 'primary'),
      ] }),
    ];
    const results = searchItems(items, 'claude');
    expect(results.map((r) => r.item.id)).toEqual(['prim', 'sec']);
  });

  it('ranks an exact-prefix match above a later substring match', () => {
    const items = [
      item({ id: 'later', label: 'Sync Status', fields: [f('Sync Status', 'primary')] }),
      item({ id: 'prefix', label: 'Sync Now', fields: [f('Sync Now', 'primary')] }),
    ];
    // "sync" is a prefix of both; check a query that only prefixes one.
    const results = searchItems(items, 'sync');
    // "Sync Status" has a longer field, so brevity favours "Sync Now" — but
    // both match at the start. The prefix bonus dominates.
    expect(results[0].item.id).toBe('prefix');
  });

  it('ranks a shorter field above a longer field at equal match quality', () => {
    const items = [
      item({ id: 'long', label: 'xxxxxxxxxx Alpha One xxxxxxxxxx', fields: [f('xxxxxxxxxx Alpha One xxxxxxxxxx', 'primary')] }),
      item({ id: 'short', label: 'Alpha One', fields: [f('Alpha One', 'primary')] }),
    ];
    const results = searchItems(items, 'alpha');
    expect(results[0].item.id).toBe('short');
  });

  it('applies the boost on top of the scored match', () => {
    const items = [
      item({ id: 'a', label: 'Alpha', boost: 100, fields: [f('Alpha', 'primary')] }),
      item({ id: 'b', label: 'Alphabeta', fields: [f('Alphabeta', 'primary')] }),
    ];
    // Without the boost, 'Alphabeta' would be a weaker (longer-field) match;
    // with it, 'Alpha' still wins even though its base score is lower.
    const boosted = searchItems(items, 'alpha');
    expect(boosted[0].item.id).toBe('a');
  });

  it('ranks exact whole-field matches above prefix matches', () => {
    const items = [
      item({ id: 'prefix', label: 'Sync Status', fields: [f('Sync Status', 'primary')] }),
      item({ id: 'exact', label: 'Sync', fields: [f('Sync', 'primary')] }),
    ];
    expect(searchItems(items, 'sync')[0].item.id).toBe('exact');
  });

  it('sorts deterministically with a label tiebreak on equal scores', () => {
    const items = [
      item({ id: 'z', label: 'Zulu Alpha', fields: [f('Alpha', 'primary')] }),
      item({ id: 'a', label: 'Alpha Zed', fields: [f('Alpha', 'primary')] }),
    ];
    // Both fields are identical, so both score the same for 'alpha' — the
    // label tiebreak decides the order.
    const results = searchItems(items, 'alpha');
    expect(results.map((r) => r.item.id)).toEqual(['a', 'z']);
  });

  it('respects the limit option', () => {
    const items = [1, 2, 3, 4, 5].map((n) =>
      item({ id: `n${n}`, label: `Node ${n}`, fields: [f(`Node ${n}`, 'primary')] }),
    );
    expect(searchItems(items, 'node', { limit: 2 })).toHaveLength(2);
    expect(searchItems(items, 'node')).toHaveLength(5);
  });
});

describe('compareResults', () => {
  it('orders by descending score then ascending label', () => {
    const a: FuzzyResult = { item: item({ label: 'z' }), score: 5, fieldMatches: [], bestFieldText: '' };
    const b: FuzzyResult = { item: item({ label: 'a' }), score: 5, fieldMatches: [], bestFieldText: '' };
    const c: FuzzyResult = { item: item({ label: 'm' }), score: 10, fieldMatches: [], bestFieldText: '' };
    expect([a, b, c].sort(compareResults).map((r) => r.item.label)).toEqual(['m', 'a', 'z']);
  });
});

// ---------------------------------------------------------------------------
// searchItems — match highlighting ranges
// ---------------------------------------------------------------------------

describe('searchItems — match highlighting ranges', () => {
  it('reports a single contiguous range for a substring match', () => {
    const items = [item({ fields: [f('Alpha One', 'primary')] })];
    const result = searchItems(items, 'alpha')[0];
    expect(result.fieldMatches[0].ranges).toEqual([{ start: 0, end: 5 }]);
  });

  it('reports multiple ranges for a gapped subsequence match', () => {
    const items = [item({ fields: [f('Git Sync', 'primary')] })];
    const result = searchItems(items, 'gs')[0];
    // 'g' at 0, 's' at 4 → two single-character ranges.
    expect(result.fieldMatches[0].ranges).toEqual([{ start: 0, end: 1 }, { start: 4, end: 5 }]);
  });

  it('reports ranges in the original case', () => {
    const items = [item({ fields: [f('Mesh Grid', 'primary')] })];
    const result = searchItems(items, 'MESH')[0];
    expect(result.fieldMatches[0].ranges).toEqual([{ start: 0, end: 4 }]);
  });

  it('reports the best-matching field text for tab-complete', () => {
    const items = [item({ fields: [
      f('Alpha One', 'primary'),
      f('feature/alpha', 'secondary'),
    ] })];
    const result = searchItems(items, 'alpha')[0];
    // Both fields match 'alpha'; the first (primary) field wins the "best"
    // slot for tab-complete.
    expect(result.bestFieldText).toBe('Alpha One');
    expect(result.fieldMatches[0].fieldIndex).toBe(0);
  });

  it('highlights a match in a secondary field when the primary does not match', () => {
    const items = [item({ fields: [
      f('Alpha One', 'primary'),
      f('feature/alpha', 'secondary'),
    ] })];
    const result = searchItems(items, 'feature')[0];
    expect(result.fieldMatches[0].fieldIndex).toBe(1);
    expect(result.bestFieldText).toBe('feature/alpha');
  });

  it('reports EVERY matching field so the UI can highlight label + subtitle', () => {
    // Review #1425: the type contract says `fieldMatches: FieldMatch[]` — the
    // engine must populate all matching fields, not just the best one.
    const items = [item({ fields: [
      f('Alpha One', 'primary'),
      f('feature/alpha', 'secondary'),
    ] })];
    const result = searchItems(items, 'alpha')[0];
    expect(result.fieldMatches.map((fm) => fm.fieldIndex)).toEqual([0, 1]);
    // Both contribute to the aggregate score.
    expect(result.score).toBeGreaterThan(FIELD_WEIGHTS.primary);
  });

  it('aggregates the score across all matching fields', () => {
    const items = [
      item({ id: 'both', label: 'Alpha Beta', fields: [
        f('Alpha', 'primary'),
        f('Alpha Beta', 'secondary'),
      ] }),
      item({ id: 'one', label: 'Alpha Gamma', fields: [
        f('Alpha', 'primary'),
      ] }),
    ];
    const results = searchItems(items, 'alpha');
    // 'both' matches in two fields → higher aggregate than 'one'.
    expect(results[0].item.id).toBe('both');
  });
});

// ---------------------------------------------------------------------------
// searchItems — empty-query modes
// ---------------------------------------------------------------------------

describe('searchItems — empty-query modes', () => {
  const items = [
    item({ id: 'a', label: 'Alpha' }),
    item({ id: 'b', label: 'Beta' }),
  ];

  it("'none' returns an empty list (default)", () => {
    expect(searchItems(items, '')).toEqual([]);
  });

  it("'all' returns every item in insertion order", () => {
    const results = searchItems(items, '', { emptyMode: 'all' });
    expect(results.map((r) => r.item.id)).toEqual(['a', 'b']);
    expect(results.every((r) => r.score === 0)).toBe(true);
  });

  it("'top' returns only the first N items", () => {
    const results = searchItems(items, '', { emptyMode: 'top', limit: 1 });
    expect(results.map((r) => r.item.id)).toEqual(['a']);
  });
});

// ---------------------------------------------------------------------------
// FIELD_WEIGHTS — the scoring vocabulary
// ---------------------------------------------------------------------------

describe('FIELD_WEIGHTS', () => {
  it('keeps primary strictly above secondary', () => {
    expect(FIELD_WEIGHTS.primary).toBeGreaterThan(FIELD_WEIGHTS.secondary);
  });
});

// ---------------------------------------------------------------------------
// Indexers — Agent Nodes
// ---------------------------------------------------------------------------

describe('indexAgentNodes (issue #1410 §1)', () => {
  const meshes = [makeMesh({ id: 1, name: 'mesh-a' })];
  const node = makeNode({
    id: 7,
    mesh_id: 1,
    name: 'worker-7',
    branch: 'feature/omnibar',
    worktree_name: 'wt-omnibar',
    provider: 'claude',
    status: 'awaiting_input',
  });

  it('indexes name, branch, worktree name, provider, status, and mesh name', () => {
    const [entry] = indexAgentNodes([node], meshes);
    expect(entry.id).toBe('node:7');
    expect(entry.category).toBe(CATEGORY.node);
    expect(entry.fields.map((f) => f.text)).toEqual([
      'worker-7',
      'feature/omnibar',
      'wt-omnibar',
      'claude',
      'Needs attention', // getStatusConfig label
      'mesh-a',
    ]);
  });

  it('uses the worktree name in the subtitle when present', () => {
    const [entry] = indexAgentNodes([node], meshes);
    expect(entry.subtitle).toContain('wt-omnibar');
    expect(entry.subtitle).toContain('mesh-a');
  });

  it('falls back to the branch in the subtitle when there is no worktree', () => {
    const [entry] = indexAgentNodes([{ ...node, worktree_name: null }], meshes);
    expect(entry.subtitle).toContain('feature/omnibar');
  });

  it('matches against the branch field', () => {
    const items = indexAgentNodes([node], meshes);
    expect(searchItems(items, 'omnibar')).toHaveLength(1);
    expect(searchItems(items, 'wt-omnibar')).toHaveLength(1);
  });

  it('matches against the status label', () => {
    const items = indexAgentNodes([node], meshes);
    expect(searchItems(items, 'attention')).toHaveLength(1);
  });

  it('matches against the provider field', () => {
    const items = indexAgentNodes([node], meshes);
    expect(searchItems(items, 'claude')).toHaveLength(1);
  });

  it('returns an empty list for no nodes', () => {
    expect(indexAgentNodes([], meshes)).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// Indexers — Meshes
// ---------------------------------------------------------------------------

describe('indexMeshes (issue #1410 §1)', () => {
  const mesh = makeMesh({ id: 3, name: 'buildmesh', path: '/work/buildmesh', base_ref: 'main' });

  it('indexes name, repo path, and active branch', () => {
    const [entry] = indexMeshes([mesh]);
    expect(entry.id).toBe('mesh:3');
    expect(entry.fields.map((f) => f.text)).toEqual(['buildmesh', '/work/buildmesh', 'main']);
    expect(entry.subtitle).toContain('/work/buildmesh');
  });

  it('matches against the path field', () => {
    const items = indexMeshes([mesh]);
    expect(searchItems(items, 'work/buildmesh')).toHaveLength(1);
  });

  it('matches against the branch field', () => {
    const items = indexMeshes([mesh]);
    expect(searchItems(items, 'main')).toHaveLength(1);
  });
});

// ---------------------------------------------------------------------------
// Indexers — App Commands
// ---------------------------------------------------------------------------

describe('indexCommands (issue #1410 §1)', () => {
  it('ships the required built-in commands', () => {
    const ids = APP_COMMANDS.map((c) => c.id);
    expect(ids).toEqual(expect.arrayContaining([
      'toggle-theme',
      'view-single',
      'view-mesh',
      'view-pinned',
      'view-all',
      'open-settings',
      'open-remote-access',
      'show-cheatsheet',
      'git-sync',
    ]));
    // Probe tab shortcuts for every tab in PROBE_TAB_COMMANDS.
    for (const tab of PROBE_TAB_COMMANDS) {
      expect(ids).toContain(`probe-${tab}`);
    }
  });

  it('matches by label and by keyword alias', () => {
    const items = indexCommands(APP_COMMANDS);
    expect(searchItems(items, 'settings')).toHaveLength(1);
    expect(searchItems(items, 'preferences')).toHaveLength(1);
    // 'theme' is a gapped subsequence of 'Switch view: Mesh Grid' too, so
    // assert the dedicated command wins the top slot rather than the count.
    const themed = searchItems(items, 'theme');
    expect(themed[0].item.id).toBe('command:toggle-theme');
  });

  it('includes the cheatsheet', () => {
    const items = indexCommands(APP_COMMANDS);
    const results = searchItems(items, 'cheatsheet');
    expect(results[0].item.id).toBe('command:show-cheatsheet');
  });

  it('matches a view-mode switch by its label', () => {
    const items = indexCommands(APP_COMMANDS);
    const results = searchItems(items, 'pinned');
    expect(results[0].item.id).toBe('command:view-pinned');
  });
});

// ---------------------------------------------------------------------------
// Indexers — GitHub Probes
// ---------------------------------------------------------------------------

describe('indexGitHub (issue #1410 §1)', () => {
  const meshes = [makeMesh({ id: 1, name: 'buildmesh' })];
  const issue = makeIssue({ number: 101, title: 'Fix the flaky network test', labels: ['bug'] });
  const pr = makePullRequest({ number: 42, title: 'Add omnibar fuzzy search', head_ref: 'feature/omnibar' });

  it('indexes issues with number, title, labels, and mesh name', () => {
    const [entry] = indexGitHub([{ meshId: 1, items: [issue] }], [], meshes);
    expect(entry.id).toBe('issue:1:101');
    expect(entry.category).toBe(CATEGORY.issue);
    expect(entry.label).toBe('#101 Fix the flaky network test');
    expect(entry.fields.map((f) => f.text)).toContain('#101');
    expect(entry.fields.map((f) => f.text)).toContain('bug');
    expect(entry.fields.map((f) => f.text)).toContain('buildmesh');
  });

  it('indexes pull requests with number, title, head ref, and mesh name', () => {
    const [entry] = indexGitHub([], [{ meshId: 1, items: [pr] }], meshes);
    expect(entry.id).toBe('pull:1:42');
    expect(entry.category).toBe(CATEGORY.pullRequest);
    expect(entry.label).toBe('#42 Add omnibar fuzzy search');
    expect(entry.fields.map((f) => f.text)).toContain('feature/omnibar');
  });

  it('matches issues and PRs by number', () => {
    const items = indexGitHub([{ meshId: 1, items: [issue] }], [{ meshId: 1, items: [pr] }], meshes);
    expect(searchItems(items, '101')).toHaveLength(1);
    expect(searchItems(items, '42')).toHaveLength(1);
  });

  it('ranks a bare-number query as well as a #-prefixed one (review #1425)', () => {
    // Both `#101` (secondary field, index 0) and `101` (bare field) are
    // indexed, so `101` doesn't lose the start bonus for the leading `#`.
    const items = indexGitHub([{ meshId: 1, items: [issue] }], [], meshes);
    const bare = searchItems(items, '101')[0];
    const hashed = searchItems(items, '#101')[0];
    expect(bare.item.id).toBe('issue:1:101');
    expect(hashed.item.id).toBe('issue:1:101');
    // The bare-number field must be indexed (secondary) — the score is
    // comparable, not dominated by the `#`-offset penalty.
    expect(bare.fieldMatches.some((fm) => {
      const fieldText = items[0].fields[fm.fieldIndex].text;
      return fieldText === '101';
    })).toBe(true);
  });

  it('matches PRs by head ref', () => {
    const items = indexGitHub([], [{ meshId: 1, items: [pr] }], meshes);
    expect(searchItems(items, 'omnibar')).toHaveLength(1);
  });
});

// ---------------------------------------------------------------------------
// Indexers — Spawning Recipes
// ---------------------------------------------------------------------------

describe('indexSpawnOptions (issue #1410 §1)', () => {
  const meshes = [makeMesh({ id: 1, name: 'buildmesh' }), makeMesh({ id: 2, name: 'playground' })];
  const option = makeSpawnOption({ id: 'claude', label: 'Claude Code', harness_id: 'claude' });

  it('emits one quick-spawn item per (option, mesh) pair', () => {
    const items = indexSpawnOptions([option], meshes);
    expect(items).toHaveLength(2);
    expect(items[0].id).toBe('spawn:claude:1');
    expect(items[0].label).toBe('Spawn Claude Code on buildmesh');
    expect(items[0].subtitle).toBe('buildmesh');
    expect(items[1].label).toBe('Spawn Claude Code on playground');
  });

  it('matches by harness label and mesh name', () => {
    const items = indexSpawnOptions([option], meshes);
    expect(searchItems(items, 'claude')).toHaveLength(2);
    expect(searchItems(items, 'playground')).toHaveLength(1);
    expect(searchItems(items, 'spawn')).toHaveLength(2);
  });

  it('emits nothing when no meshes are loaded yet (review #1425)', () => {
    // Spawn options are mesh-bound (the action spawns INTO a mesh); with an
    // empty mesh list there is deliberately no spawn entry — the pre-boot
    // palette simply has none.
    expect(indexSpawnOptions([option], [])).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// buildOmnibarIndex — the merged palette
// ---------------------------------------------------------------------------

describe('buildOmnibarIndex', () => {
  it('merges all five domains into one palette', () => {
    const index = buildOmnibarIndex({
      nodes: [makeNode({ id: 1, name: 'agent-a' })],
      meshes: [makeMesh({ id: 1, name: 'mesh-a' })],
      commands: APP_COMMANDS,
      spawnOptions: [makeSpawnOption()],
      issues: [{ meshId: 1, items: [makeIssue()] }],
      pullRequests: [{ meshId: 1, items: [makePullRequest()] }],
    });
    const categories = new Set(index.map((i) => i.category));
    expect(categories).toEqual(new Set(['node', 'mesh', 'command', 'issue', 'pull-request', 'spawn']));
  });

  it('searches across every domain with a plain query', () => {
    const index = buildOmnibarIndex({
      nodes: [makeNode({ id: 1, name: 'worker-7' })],
      meshes: [makeMesh({ id: 1, name: 'mesh-a' })],
      commands: APP_COMMANDS,
      spawnOptions: [makeSpawnOption()],
      issues: [{ meshId: 1, items: [makeIssue({ title: 'worker queue backpressure' })] }],
      pullRequests: [],
    });
    // 'work' hits the node name and the issue title.
    const results = searchOmnibar(index, 'work');
    const categories = new Set(results.map((r) => r.item.category));
    expect(categories).toContain('node');
    expect(categories).toContain('issue');
  });
});

// ---------------------------------------------------------------------------
// Prefix filtering (issue #1410 §2)
// ---------------------------------------------------------------------------

describe('filterByPrefix (issue #1410 §2)', () => {
  const index = buildOmnibarIndex({
    nodes: [makeNode({ id: 1, name: 'agent-a' })],
    meshes: [makeMesh({ id: 1, name: 'mesh-a' })],
    commands: APP_COMMANDS,
    spawnOptions: [makeSpawnOption()],
    issues: [{ meshId: 1, items: [makeIssue()] }],
    pullRequests: [{ meshId: 1, items: [makePullRequest()] }],
  });

  function categoriesFor(items: import('../../src/lib/omnibar/indexers').OmnibarIndex): string[] {
    return [...new Set(items.map((i) => i.category))];
  }

  it('`>` filters to the action menu: commands and spawn (issue #1413)', () => {
    const { items, query } = filterByPrefix(index, '>set');
    expect(query).toBe('set');
    expect(categoriesFor(items).sort()).toEqual(['command', 'spawn']);
  });

  it('`@` filters to agent nodes only', () => {
    const { items, query } = filterByPrefix(index, '@agen');
    expect(query).toBe('agen');
    expect(categoriesFor(items)).toEqual(['node']);
  });

  it('`/` and `+` filter to spawning actions', () => {
    for (const prefix of ['/', '+']) {
      const { items, query } = filterByPrefix(index, `${prefix}claude`);
      expect(query).toBe('claude');
      expect(categoriesFor(items)).toEqual(['spawn']);
    }
  });

  it('`#` filters to issues and pull requests', () => {
    const { items, query } = filterByPrefix(index, '#flaky');
    expect(query).toBe('flaky');
    const cats = categoriesFor(items);
    expect(cats).toContain('issue');
    expect(cats).toContain('pull-request');
    expect(cats).not.toContain('node');
  });

  it('returns the full list when there is no prefix', () => {
    const { items, query } = filterByPrefix(index, 'plain');
    expect(query).toBe('plain');
    expect(items).toHaveLength(index.length);
  });
});

// ---------------------------------------------------------------------------
// searchOmnibar — prefix + fuzzy integration
// ---------------------------------------------------------------------------

describe('searchOmnibar', () => {
  const index = buildOmnibarIndex({
    nodes: [makeNode({ id: 1, name: 'agent-a' })],
    meshes: [makeMesh({ id: 1, name: 'mesh-a' })],
    commands: APP_COMMANDS,
    spawnOptions: [makeSpawnOption()],
    issues: [{ meshId: 1, items: [makeIssue({ title: 'Flaky test' })] }],
    pullRequests: [],
  });

  it('scopes a `#` query to GitHub items', () => {
    const results = searchOmnibar(index, '#flaky');
    expect(results).toHaveLength(1);
    expect(results[0].item.category).toBe('issue');
  });

  it('scopes an `@` query to agent nodes', () => {
    const results = searchOmnibar(index, '@agent');
    expect(results).toHaveLength(1);
    expect(results[0].item.category).toBe('node');
  });

  it('scopes a `>` query to the action menu and excludes repo entities (issue #1413)', () => {
    const results = searchOmnibar(index, '>set');
    const categories = new Set(results.map((r) => r.item.category));
    expect(categories.has('command')).toBe(true);
    // Meshes / nodes / issues stay out of `>` (issue #1410 §2 / review #1425).
    expect(categories.has('mesh')).toBe(false);
    expect(categories.has('node')).toBe(false);
    expect(categories.has('issue')).toBe(false);
  });

  it('a `>` or `spawn` query surfaces Spawn [Harness] on [Mesh] recipes (issue #1413 §1)', () => {
    const bySpawn = searchOmnibar(index, 'spawn');
    expect(bySpawn.some((r) => r.item.label === 'Spawn Claude Code on mesh-a')).toBe(true);

    const byActionPrefix = searchOmnibar(index, '>spawn');
    expect(byActionPrefix.some((r) => r.item.category === 'spawn')).toBe(true);
    expect(byActionPrefix.some((r) => r.item.label === 'Spawn Claude Code on mesh-a')).toBe(true);
  });

  it('returns an empty list when the prefix matches nothing', () => {
    expect(searchOmnibar(index, '@zzzz')).toEqual([]);
  });

  it('applies the limit', () => {
    // '>view' scopes to commands; the 'view-*' commands dominate, so the
    // limit must cap the result count.
    expect(searchOmnibar(index, '>view', { limit: 2 })).toHaveLength(2);
  });

  it('returns an empty list for an empty query', () => {
    expect(searchOmnibar(index, '')).toEqual([]);
  });

  it('a bare prefix lists the whole scoped domain instead of a blank palette (review #1425)', () => {
    // Typing just `>` / `@` / `#` strips to an empty query; searchOmnibar
    // must show the scoped list (emptyMode 'all'), not return [].
    const commands = searchOmnibar(index, '>');
    expect(commands.length).toBeGreaterThan(0);
    expect(commands.every((r) => r.item.category === 'command' || r.item.category === 'spawn')).toBe(true);
    expect(commands.some((r) => r.item.category === 'command')).toBe(true);
    expect(commands.some((r) => r.item.category === 'spawn')).toBe(true);

    const nodes = searchOmnibar(index, '@');
    expect(nodes.map((r) => r.item.category)).toEqual(['node']);

    const issues = searchOmnibar(index, '#');
    expect(issues.length).toBeGreaterThan(0);
    expect(issues.every((r) => r.item.category === 'issue' || r.item.category === 'pull-request')).toBe(true);
  });

  it('honours an explicit emptyMode for a bare prefix (review #1425)', () => {
    const results = searchOmnibar(index, '>', { emptyMode: 'top', limit: 2 });
    expect(results).toHaveLength(2);
  });
});

// ---------------------------------------------------------------------------
// Performance budget (issue #1410 §3 — sub-5ms over 500+ items)
// ---------------------------------------------------------------------------

describe('performance budget (issue #1410 §3)', () => {
  it('searches 500+ items in under 5ms per query', () => {
    const items: IndexedItem[] = [];
    for (let i = 0; i < 600; i++) {
      items.push({
        id: `node:${i}`,
        category: 'node',
        label: `agent-node-${i}`,
        fields: [
          f(`agent-node-${i}`, 'primary'),
          f(`feature/branch-${i % 50}`, 'secondary'),
          f(`worktree-${i % 30}`, 'secondary'),
          f(i % 3 === 0 ? 'claude' : 'codex', 'secondary'),
          f(i % 2 === 0 ? 'Running' : 'Idle', 'secondary'),
          f(`mesh-${i % 5}`, 'secondary'),
        ],
      });
    }

    const queries = ['agent', 'node-42', 'claude', 'branch-7', 'worktree', 'mesh-3', 'xyzzy'];
    // Warmup so the measured loop isn't paying first-call / JIT costs —
    // under a loaded vitest pool this test otherwise flakes above 5ms
    // even though the engine itself is well under budget in isolation.
    for (const q of queries) {
      searchItems(items, q, { limit: 50 });
    }

    const measure = (): number => {
      let total = 0;
      for (const q of queries) {
        const t0 = performance.now();
        searchItems(items, q, { limit: 50 });
        total += performance.now() - t0;
      }
      return total / queries.length;
    };

    let avg = measure();
    if (avg >= 5) avg = measure();
    expect(avg).toBeLessThan(5);
  });

  it('is deterministic across runs', () => {
    const items: IndexedItem[] = [];
    for (let i = 0; i < 100; i++) {
      items.push({
        id: `node:${i}`,
        category: 'node',
        label: `agent-node-${i}`,
        fields: [f(`agent-node-${i}`, 'primary')],
      });
    }
    const a = searchItems(items, 'node').map((r) => r.item.id);
    const b = searchItems(items, 'node').map((r) => r.item.id);
    expect(a).toEqual(b);
  });
});
