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
  field,
  APP_COMMANDS,
  PROBE_DESTINATION_COMMANDS,
  PROBE_TAB_COMMANDS,
  CATEGORY,
  PREFIX_FILTERS,
  CATEGORY_PREFIX,
  viewModeCommandId,
} from './indexers';
export type {
  OmnibarIndex,
  AppCommand,
  Category,
} from './indexers';

import { searchItems, type EmptyQueryMode } from './fuzzySearch';
import { filterByPrefix } from './indexers';
import type { FuzzyResult, IndexedItem } from './fuzzySearch';

/**
 * Search the full palette with prefix filtering (issue #1410 §2 and §3):
 * applies the leading `>` (commands + spawn) / `@` / `/` / `+` / `#`
 * domain filter, then runs
 * the remaining query through the fuzzy engine. `limit` caps the returned
 * result count.
 *
 * Bare-prefix behaviour (review #1425): typing just a prefix (`>`, `@`,
 * `#`…) strips to an empty query. The engine's default `emptyMode: 'none'`
 * would return a blank palette, so `searchOmnibar` defaults a BARE prefix to
 * `'all'` — the user immediately sees the whole scoped domain (the standard
 * VS Code / Raycast palette behaviour) instead of a blank menu until they
 * type a second character. A genuinely empty raw query (no prefix, nothing
 * typed) still returns `[]` unless `emptyMode` is passed explicitly.
 */
export function searchOmnibar(
  items: readonly IndexedItem[],
  rawQuery: string,
  opts?: { limit?: number; emptyMode?: EmptyQueryMode },
): FuzzyResult[] {
  const { items: scoped, query } = filterByPrefix(items, rawQuery);
  const isBarePrefix = rawQuery.trim() !== '' && query.trim() === '';
  const emptyMode: EmptyQueryMode = isBarePrefix
    ? (opts?.emptyMode ?? 'all')
    : (opts?.emptyMode ?? 'none');
  return searchItems(scoped, query, { limit: opts?.limit, emptyMode });
}
