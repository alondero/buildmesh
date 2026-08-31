/**
 * Pure fuzzy search engine for the Command Omnibar (wayfinder #1371, task
 * #1410).
 *
 * Zero-dependency, in-memory, sub-5ms over 500+ items. Everything here is a
 * pure function of its inputs — no DOM, no stores, no timers — so the whole
 * module is unit-testable in isolation and trivially cheap to re-run on every
 * keystroke. The indexers in `./indexers.ts` hand this engine flat text
 * strings plus per-item weights; the engine never sees the domain model.
 *
 * Hot-path note: matching runs against PRE-FOLDED text. Each `IndexedField`
 * carries its own `foldedText` (computed once when the index is built); the
 * per-keystroke path allocates nothing on the indexed strings. Case folding
 * uses `toLowerCase()` (locale-invariant — `toLocaleLowerCase()` binds to the
 * host OS locale, which breaks ASCII matching on Turkish/Azeri locales).
 */

/**
 * Per-field weight for the fields a domain item contributes to its search
 * text. The exact weight values encode the "primary field beats secondary
 * field" ranking contract (issue #1410: "weighted score algorithm with exact
 * prefix bonus"). They are deliberately small integers so score totals stay
 * in a legible range and the bonuses below can outrank a whole extra field.
 *
 * The weight is an **additive bias**: a field's match contributes
 * `matchQuality + weight`. It is NOT a multiplier — the numbers are chosen so
 * that a full-quality secondary match (30 + 12 + 18 + brevity + 60 ≈ 132)
 * can outrank a degraded primary match (gap-heavy quality + 100), which is
 * the behaviour the field hierarchy wants.
 */
export type FieldWeight = 'primary' | 'secondary';

/** The weights for `'primary'` / `'secondary'` fields. */
export const FIELD_WEIGHTS: Record<FieldWeight, number> = {
  primary: 100,
  secondary: 60,
};

/** A single weighted field of an indexed item. */
export interface IndexedField {
  /** The raw text the matcher searches and the UI highlights against. */
  text: string;
  /**
   * The pre-folded (lowercased) form of `text`, computed once at index-build
   * time so the per-keystroke hot path never allocates. Indexers build this
   * via the `field()` helper in `./indexers.ts`.
   */
  foldedText: string;
  /** How much a match in this field contributes to the item's score. */
  weight: FieldWeight;
}

/** A pure in-memory searchable item. */
export interface IndexedItem {
  /**
   * Stable identity, opaque to the engine — the indexer's domain id
   * (e.g. `node:12`, `mesh:3`, `command:cycle-grid-modes`). Consumed by the
   * UI layer to route execution.
   */
  id: string;
  /** Stable category label (e.g. `'node'`, `'command'`, `'issue'`). */
  category: string;
  /** A human label rendered in the result row (name / title / command). */
  label: string;
  /** Optional secondary line rendered under the label (mesh name, path…). */
  subtitle?: string;
  /** Icon hint consumed by the UI layer — an icon name, emoji, or initial. */
  icon?: string;
  /** The weighted fields the engine matches against. */
  fields: IndexedField[];
  /**
   * Optional integer modifier applied on top of the scored match (e.g. an
   * MRU bump). Positive boosts rank the item above equally-scored peers;
   * default 0.
   */
  boost?: number;
}

/** Where a match landed in the original (case-preserved) field text. */
export interface MatchRange {
  /** Start index, inclusive, in the original field text. */
  start: number;
  /** End index, exclusive, in the original field text. */
  end: number;
}

/** How and where a query matched inside one field of an item. */
export interface FieldMatch {
  /** The field's ordinal in the item's `fields` array. */
  fieldIndex: number;
  /** Score contributed by this field (match quality + field weight). */
  score: number;
  /** The exact match ranges in the ORIGINAL case of this field. */
  ranges: MatchRange[];
}

/** The scored result of matching a query against one item. */
export interface FuzzyResult {
  item: IndexedItem;
  /**
   * Aggregate score: the sum of every matching field's contribution plus any
   * item boost. Higher is better. (Review #1425 note: this is a SUM, not a
   * max — all matching fields contribute.)
   */
  score: number;
  /**
   * One entry per field that matched — the UI can highlight a match in the
   * label AND the subtitle simultaneously. Sorted by fieldIndex ascending.
   */
  fieldMatches: FieldMatch[];
  /** The full original text of the best-matching field (for tab-complete). */
  bestFieldText: string;
}

/** How an empty query behaves. */
export type EmptyQueryMode = 'none' | 'all' | 'top';

/**
 * Rank `a` against `b` as a fuzzy-result sort key: strictly-higher score
 * first; scores tied → lexicographically smaller label first (a stable,
 * deterministic tiebreak). Returns a negative number when `a` ranks first.
 */
export function compareResults(a: FuzzyResult, b: FuzzyResult): number {
  if (b.score !== a.score) return b.score - a.score;
  return a.item.label < b.item.label ? -1 : a.item.label > b.item.label ? 1 : 0;
}

const EMPTY_RESULTS: FuzzyResult[] = [];

/**
 * Search an item list for `query` (wayfinder #1371 §3 — "pure, zero-
 * dependency fuzzy scoring in `src/lib/omnibar/fuzzySearch.ts` with match
 * highlighting and category weighting").
 *
 * Matching is character-by-character subsequence matching against each
 * field's pre-folded text: every character of the folded query must appear
 * in order in the field (no gaps are required). The result is a flat score,
 * not a percentage — the caller decides which results to show and how to
 * cut ties.
 *
 * `emptyMode` governs an empty/whitespace query:
 *   - `'none'` — return `[]` (the palette shows its default/recents view).
 *   - `'all'`  — return every item with a zero score, in insertion order.
 *   - `'top'`  — like `'all'`, but only the first `limit` items (the caller
 *                uses this to preview a handful of items with an empty query).
 */
export function searchItems(
  items: readonly IndexedItem[],
  query: string,
  opts?: { emptyMode?: EmptyQueryMode; limit?: number },
): FuzzyResult[] {
  const emptyMode: EmptyQueryMode = opts?.emptyMode ?? 'none';
  const limit = opts?.limit ?? Infinity;

  if (query.trim() === '') {
    if (emptyMode === 'none') return EMPTY_RESULTS;
    // Slice BEFORE mapping so an emptyMode 'top' preview doesn't allocate
    // wrapper objects for items it will immediately discard.
    const scoped = emptyMode === 'top' ? items.slice(0, limit) : items;
    return scoped.map((item) => ({
      item,
      score: 0,
      fieldMatches: [] as FieldMatch[],
      bestFieldText: item.fields[0]?.text ?? '',
    }));
  }

  const foldedQuery = query.toLowerCase();
  const results: FuzzyResult[] = [];
  for (const item of items) {
    const scored = scoreItem(item, foldedQuery);
    if (scored === null) continue;
    results.push(scored);
  }
  results.sort(compareResults);
  return limit === Infinity ? results : results.slice(0, limit);
}

/**
 * Score one item against a query that is ALREADY lowercased (this is the
 * hot path — `searchItems` folds the query once, then calls this per item).
 * Returns `null` when no field matches the query.
 */
function scoreItem(item: IndexedItem, foldedQuery: string): FuzzyResult | null {
  const fieldMatches: FieldMatch[] = [];
  let bestScore = 0;
  let bestFieldText = '';

  for (let fieldIndex = 0; fieldIndex < item.fields.length; fieldIndex++) {
    const field = item.fields[fieldIndex];
    if (field.text === '' || field.foldedText === '') continue;
    const match = scoreField(foldedQuery, field.foldedText, FIELD_WEIGHTS[field.weight]);
    if (match === null) continue;
    fieldMatches.push({ fieldIndex, score: match.score, ranges: match.ranges });
    bestScore += match.score;
    if (bestFieldText === '') bestFieldText = field.text;
  }

  if (fieldMatches.length === 0) return null;
  if (item.boost !== undefined && item.boost !== 0) bestScore += item.boost;
  return { item, score: bestScore, fieldMatches, bestFieldText };
}

/**
 * Score a single field. `foldedQuery` is the lowercased query;
 * `foldedText` is the field's pre-folded text (see `IndexedField.foldedText`);
 * `weight` is the field's additive weight bias.
 *
 * Scoring components (all relative to the field's weight):
 *   - exact-prefix bonus: the folded query is a prefix of the folded field
 *     (e.g. `node` vs `node-sync`) — a strong "the user typed the beginning
 *     of this item" signal. `text === query` (a full exact match) scores
 *     above `text.startsWith(query)`.
 *   - dense-match bonus: the query's characters occupy a contiguous run in
 *     the field with no skipped characters between them (a substring match,
 *     e.g. `sync` in `git-sync`). Near-dense matches are penalised with a
 *     gap term so a scattershot subsequence scores lower than a solid block.
 *   - start bonus: the match begins at the field's first character (word or
 *     whole-field boundary).
 *   - brevity: shorter fields outrank longer ones at equal quality (a 6-char
 *     field matching `node` beats a 30-char field that also contains `node`).
 *
 * The final field contribution is `matchQuality + weight` (additive, NOT
 * multiplicative — see the `FieldWeight` doc).
 *
 * Returns `null` when the query is not a subsequence of the field.
 */
function scoreField(
  foldedQuery: string,
  foldedText: string,
  weight: number,
): { score: number; ranges: MatchRange[] } | null {
  if (foldedQuery.length === 0) {
    return { score: 0, ranges: [] };
  }

  const firstQuery = foldedQuery[0];
  let firstIdx = foldedText.indexOf(firstQuery);
  if (firstIdx === -1) return null;

  // Candidate windows: each occurrence of the query's first character is a
  // potential match start. The best (highest scoring) window wins.
  let best: { score: number; ranges: MatchRange[] } | null = null;
  while (firstIdx !== -1) {
    const scored = scoreAtStart(foldedQuery, foldedText, firstIdx);
    if (scored !== null && (best === null || scored.score > best.score)) {
      best = scored;
    }
    firstIdx = foldedText.indexOf(firstQuery, firstIdx + 1);
  }

  if (best === null) return null;
  best.score += weight;
  return best;
}

/**
 * Try to match the query starting at `start` in `foldedText`. Returns
 * `null` when no subsequence match exists from this start; otherwise the
 * scored match with its original-case ranges.
 *
 * The scan is SINGLE-PASS: the greedy forward walk records the index of each
 * consumed query character in `matched`, and the ranges + gap sum are derived
 * from that array afterwards — no second scan of the field text.
 *
 * Greedy note (review #1425): binding the earliest possible character is
 * optimal for the gap penalty — by exchange, if a later occurrence of the
 * same query character were used, swapping it for the earlier one can only
 * shrink (never grow) the sum of gaps, because every subsequent match index
 * shifts no later. So the "greedy trap" critique does not apply to the
 * gap-sum objective; the oracle test in
 * `tests/unit/omnibar-fuzzy-search.test.ts` pins this against a brute-force
 * best-subsequence oracle.
 */
function scoreAtStart(
  foldedQuery: string,
  foldedText: string,
  start: number,
): { score: number; ranges: MatchRange[] } | null {
  const q = foldedQuery;
  const n = foldedText.length;
  const matched: number[] = new Array(q.length);
  let qi = 0;
  let i = start;
  let prev = -1;

  // Greedy forward scan: consume query characters in order, recording the
  // index of each consumed character.
  while (qi < q.length && i < n) {
    if (foldedText[i] === q[qi]) {
      matched[qi] = i;
      prev = i;
      qi++;
    }
    i++;
  }
  if (qi < q.length) return null; // ran off the end without consuming the query

  // Derive the gap sum from the matched indices.
  let gapSum = 0;
  for (let k = 1; k < q.length; k++) {
    gapSum += matched[k] - matched[k - 1] - 1;
  }

  let base = 0;
  if (start === 0) base += 30; // field-leading match
  if (start + q.length === n) base += 12; // match consumes the whole field
  if (prev - start + 1 === q.length) base += 18; // fully dense — no gaps at all
  else base += Math.max(0, 12 - gapSum); // near-dense, penalised per gap char

  const score = base + Math.max(0, 12 - n); // brevity: shorter fields win ties

  // Build maximal runs of matched indices — the ranges in ORIGINAL-case
  // positions (the fold is length-preserving, so the indices line up).
  const ranges: MatchRange[] = [];
  let runStart = matched[0];
  for (let k = 1; k < q.length; k++) {
    if (matched[k] === matched[k - 1] + 1) continue;
    ranges.push({ start: runStart, end: matched[k - 1] + 1 });
    runStart = matched[k];
  }
  ranges.push({ start: runStart, end: prev + 1 });

  return { score, ranges };
}
