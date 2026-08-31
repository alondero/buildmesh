/**
 * Command Omnibar — fuzzy search + multi-domain indexing engine
 * (wayfinder #1371, task #1410).
 *
 * Facade over the two layers:
 *   - `fuzzySearch.ts` — the pure, zero-dependency fuzzy scoring engine
 *     (subsequence matching, weighted fields, exact-prefix bonus, match
 *     ranges for highlighting, sub-5ms over 500+ items).
 *   - `indexers.ts` — the multi-domain indexers that project Agent Nodes,
 *     Meshes, App Commands, GitHub issues/PRs, and Spawn Recipes into
 *     searchable `IndexedItem`s.
 *
 * The palette UI layer (ticket #1411) feeds stores into `buildOmnibarIndex`,
 * then calls `searchOmnibar` per keystroke with the raw (possibly prefixed)
 * query.
 */
export {
  searchItems,
  compareResults,
  FIELD_WEIGHTS,
} from './fuzzySearch';
export type {
  IndexedField,
  IndexedItem,
  MatchRange,
  FieldMatch,
  FuzzyResult,
  FieldWeight,
  EmptyQueryMode,
} from './fuzzySearch';
export {
  buildOmnibarIndex,
  indexAgentNodes,
  indexMeshes,
  indexCommands,
  indexGitHub,
  indexSpawnOptions,
  filterByPrefix,
  APP_COMMANDS,
  PROBE_TAB_COMMANDS,
  CATEGORY,
  PREFIX_FILTERS,
  viewModeCommandId,
} from './indexers';
export type {
  OmnibarIndex,
  AppCommand,
  Category,
} from './indexers';

import { searchItems } from './fuzzySearch';
import { filterByPrefix } from './indexers';
import type { FuzzyResult, IndexedItem } from './fuzzySearch';

/**
 * Search the full palette with prefix filtering (issue #1410 §2 and §3):
 * applies the leading `>` / `@` / `/` / `+` / `#` domain filter, then runs
 * the remaining query through the fuzzy engine. `limit` caps the returned
 * result count; an empty query returns `[]` unless `emptyMode` is set.
 */
export function searchOmnibar(
  items: readonly IndexedItem[],
  rawQuery: string,
  opts?: { limit?: number; emptyMode?: 'none' | 'all' | 'top' },
): FuzzyResult[] {
  const { items: scoped, query } = filterByPrefix(items, rawQuery);
  return searchItems(scoped, query, { limit: opts?.limit, emptyMode: opts?.emptyMode });
}
