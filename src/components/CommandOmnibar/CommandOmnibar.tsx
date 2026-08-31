/**
 * <CommandOmnibar> — the Universal Command Omnibar palette (wayfinder #1371,
 * task #1411).
 *
 * A WAI-ARIA 1.2 combobox overlay: a text input (role="combobox") driving a
 * floating result list (role="listbox" / role="option") fed by the #1410
 * search engine, with full keyboard interaction — ArrowUp/Down with wrap
 * around, Enter to execute the active option, Escape or a backdrop click to
 * dismiss, and Tab to drill into the active result's domain (apply its
 * prefix filter) or complete its primary text once the query is already
 * scoped.
 *
 * Mount/unmount discipline (same as <Modal>): the palette renders only while
 * `omnibarOpen` is true, so arming the window-level Escape listener is the
 * mount itself. Opening and closing never touches the terminal grid — the
 * palette is a sibling overlay, and <TerminalManager>'s xterm instances are
 * keyed on node id, so nothing is disposed or remounted underneath it. Focus
 * is captured on mount and restored on unmount, so closing the palette
 * drops the user straight back into the terminal (or input) they came from.
 */
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from 'react';
import { useUIStore, type OmnibarMode } from '../../stores/uiStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import {
  APP_COMMANDS,
  CATEGORY_PREFIX,
  PREFIX_FILTERS,
  buildOmnibarIndex,
  searchOmnibar,
  type Category,
  type IndexedItem,
  type OmnibarIndex,
} from '../../lib/omnibar';
import type { FuzzyResult } from '../../lib/omnibar';
import type { SpawnOption } from '../../lib/groups';
import {
  executeOmnibarItem,
  loadSpawnOptions,
  type OmnibarActionContext,
} from './omnibarActions';

/** Cap on rendered results — the engine ranks, the palette pages by query. */
const RESULT_LIMIT = 30;

const LISTBOX_ID = 'command-omnibar-listbox';
const optionId = (index: number) => `command-omnibar-option-${index}`;

const isKnownPrefix = (query: string): boolean =>
  PREFIX_FILTERS.some((f) => f.prefix === query[0]);

export function CommandOmnibar() {
  const omnibarOpen = useUIStore((s) => s.omnibarOpen);
  const omnibarMode = useUIStore((s) => s.omnibarMode);
  const closeOmnibar = useUIStore((s) => s.closeOmnibar);

  // Nothing renders (and no listeners arm) while the palette is closed —
  // the mount IS the open.
  if (!omnibarOpen) return null;
  return <OmnibarPalette mode={omnibarMode} onClose={closeOmnibar} />;
}

function OmnibarPalette({ mode, onClose }: { mode: OmnibarMode; onClose: () => void }) {
  const inputRef = useRef<HTMLInputElement>(null);
  const previouslyFocusedRef = useRef<HTMLElement | null>(null);

  // `omnibarMode` seeds the search box at mount AND re-seeds when the mode
  // changes while the palette is open (the editors' quick-open convention):
  // pressing the other chord (e.g. ⌘/Ctrl+P while the files palette is up)
  // re-seeds to that mode per the `openOmnibar` contract in uiStore. Mode
  // switching inside the palette happens via the prefix characters, not the
  // store.
  const [query, setQuery] = useState(() => (mode === 'commands' ? '>' : ''));
  const [activeIndex, setActiveIndex] = useState(0);
  const [spawnOptions, setSpawnOptions] = useState<SpawnOption[]>([]);
  useEffect(() => {
    setQuery(mode === 'commands' ? '>' : '');
    setActiveIndex(0);
  }, [mode]);

  const agentNodes = useAgentNodeStore((s) => s.agentNodes);
  const meshesById = useMeshStore((s) => s.meshesById);
  const meshes = useMemo(
    () => [...meshesById.values()].sort((a, b) => a.position - b.position),
    [meshesById],
  );

  // GitHub issues/PRs have no cache store — they're fetched per-tab in the
  // Probe — so those two domains index empty until a cache exists (the '#'
  // prefix then simply shows no results). Nodes, meshes, commands and the
  // spawn menu cover the remaining domains live.
  const index: OmnibarIndex = useMemo(
    () =>
      buildOmnibarIndex({
        nodes: agentNodes,
        meshes,
        commands: [...APP_COMMANDS],
        spawnOptions,
        issues: [],
        pullRequests: [],
      }),
    [agentNodes, meshes, spawnOptions],
  );

  const results = useMemo(
    () => searchOmnibar(index, query, { limit: RESULT_LIMIT }),
    [index, query],
  );

  // Focus: capture the element the user came from, move into the search box,
  // restore on unmount (the palette's version of the <Modal> contract). This
  // is also what drops the user back into a focused xterm terminal without
  // touching TerminalManager — the terminal was never unmounted.
  useEffect(() => {
    previouslyFocusedRef.current = document.activeElement as HTMLElement | null;
    inputRef.current?.focus();
    return () => {
      previouslyFocusedRef.current?.focus?.();
    };
  }, []);

  // Refresh the spawn menu on every open (one IPC per open — see
  // `loadSpawnOptions`; no module cache, so providers added in Settings
  // appear without a restart). Guarded against unmount: state writes after
  // the palette closed are dropped.
  useEffect(() => {
    let cancelled = false;
    loadSpawnOptions().then((options) => {
      if (!cancelled && options.length > 0) setSpawnOptions(options);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  // Escape dismisses from anywhere (backdrop clicks move focus to body, so
  // the input's own keydown can't be the only Escape path).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      e.preventDefault();
      onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  // Keep the active index valid as the result set shrinks/grows.
  const clampedActive = results.length === 0 ? 0 : Math.min(activeIndex, results.length - 1);

  useEffect(() => {
    if (results.length === 0) return;
    document
      .getElementById(optionId(clampedActive))
      ?.scrollIntoView({ block: 'nearest' });
  }, [clampedActive, results.length]);

  const executeItem = (item: IndexedItem) => {
    const ctx: OmnibarActionContext = {
      meshes,
      spawnOptions,
      setViewMode: useUIStore.getState().setViewMode,
      openProbeTab: useUIStore.getState().openProbeTab,
    };
    executeOmnibarItem(item.id, ctx);
    onClose();
  };

  const handleKeyDown = (e: ReactKeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (results.length > 0) {
        // Navigate from the CLAMPED index: if the result set shrank since
        // the last move (spawn options loaded, store updates), the raw
        // activeIndex may exceed the list — wrapping from it would jump to
        // the wrong row (issue #1411 review).
        setActiveIndex((clampedActive + 1) % results.length);
      }
      return;
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (results.length > 0) {
        setActiveIndex((clampedActive - 1 + results.length) % results.length);
      }
      return;
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      const result = results[clampedActive];
      if (result) executeItem(result.item);
      return;
    }
    if (e.key === 'Tab' && !e.shiftKey) {
      // Forward Tab drills in; Shift+Tab falls through untouched so the
      // browser's reverse focus navigation keeps working. Tab drills into
      // the active result's domain: with an unscoped query and a
      // prefixable active result, apply that domain's prefix filter (e.g.
      // `settings` + a command hit → `>settings`). Once the query is
      // already scoped (or the domain has no prefix, e.g. meshes), Tab
      // completes the query to the active result's primary field — the
      // standard palette tab-complete gesture. preventDefault only fires
      // when there IS a completion action, so an empty result set never
      // traps Tab.
      const result = results[clampedActive];
      if (!result) return;
      e.preventDefault();
      const category = result.item.category as Category;
      const prefix = CATEGORY_PREFIX[category];
      if (prefix !== undefined && !isKnownPrefix(query)) {
        setQuery(prefix + query);
      } else {
        const activePrefix = query.startsWith(prefix ?? '') ? (prefix ?? '') : '';
        setQuery(activePrefix + (result.item.fields[0]?.text ?? ''));
      }
      return;
    }
  };

  const activeId = results.length > 0 ? optionId(clampedActive) : undefined;

  return (
    // Backdrop. The panel stops propagation, so clicks that reach this
    // handler are real dismissals (same discriminator as <Modal>).
    <div
      className="fixed inset-0 z-50 flex items-center justify-center"
      onClick={onClose}
      data-testid="command-omnibar-backdrop"
    >
      <div className="absolute inset-0 bg-bg-base/70 backdrop-blur-sm animate-fade-in" />
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Command Omnibar"
        className="relative w-full max-w-xl bg-bg-overlay border border-border-default rounded-lg shadow-md animate-scale-in outline-none overflow-hidden"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center gap-2 px-4 py-3 border-b border-border-subtle">
          <input
            ref={inputRef}
            role="combobox"
            aria-expanded={results.length > 0}
            aria-controls={results.length > 0 ? LISTBOX_ID : undefined}
            aria-activedescendant={activeId}
            aria-haspopup="listbox"
            aria-autocomplete="list"
            aria-label="Search commands, nodes, meshes and more"
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setActiveIndex(0);
            }}
            onKeyDown={handleKeyDown}
            placeholder="Type to search…  > commands  @ nodes  / spawn  # issues"
            spellCheck={false}
            className="flex-1 bg-transparent outline-none text-base text-text-primary placeholder:text-text-muted"
          />
        </div>

        {results.length > 0 ? (
          <ul
            id={LISTBOX_ID}
            role="listbox"
            aria-label="Omnibar results"
            className="max-h-[50vh] overflow-y-auto py-1"
          >
            {results.map((result, i) => (
              <ResultRow
                key={result.item.id}
                result={result}
                active={i === clampedActive}
                index={i}
                // mousedown preventDefault keeps the input focused — the
                // combobox owns focus for the palette's whole lifetime.
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => executeItem(result.item)}
              />
            ))}
          </ul>
        ) : (
          <div className="px-4 py-6 text-sm text-text-muted" data-testid="command-omnibar-empty">
            No matching results
          </div>
        )}

        <div className="flex items-center gap-3 px-4 py-2 border-t border-border-subtle text-2xs text-text-muted">
          {/* One badge per description, not per prefix — `/` and `+` both
              scope spawning (issue #1410 §2) and would render two
              identical badges. */}
          {PREFIX_FILTERS.filter(
            (f, i, all) => all.findIndex((o) => o.description === f.description) === i,
          ).map((f) => (
            <span key={f.description} className="flex items-center gap-1">
              {PREFIX_FILTERS.filter((o) => o.description === f.description).map((o) => (
                <kbd
                  key={o.prefix}
                  className="px-1 rounded-md bg-bg-card border border-border-default font-mono"
                >
                  {o.prefix}
                </kbd>
              ))}
              {f.description}
            </span>
          ))}
        </div>
      </div>
    </div>
  );
}

/**
 * One option row. Renders the label with the fuzzy match ranges highlighted
 * when the match landed in the primary field (whose text equals the label
 * for every domain except GitHub, where the label is `#N title` but the
 * primary field is just the title).
 */
function ResultRow({
  result,
  active,
  index,
  onMouseDown,
  onClick,
}: {
  result: FuzzyResult;
  active: boolean;
  index: number;
  onMouseDown: (e: ReactMouseEvent) => void;
  onClick: () => void;
}) {
  const { item } = result;
  const primaryField = item.fields[0];
  // The label usually IS the primary field's text (nodes, meshes, commands,
  // spawn), so the match ranges apply 1:1. GitHub rows differ: the label is
  // `#N <title>` while the primary field is just the title — the ranges
  // still apply, shifted by the `#N ` prefix the label adds (issue #1411
  // review: don't disable highlighting for the whole GitHub domain).
  let ranges: { start: number; end: number }[] | undefined;
  if (primaryField !== undefined) {
    const raw = result.fieldMatches.find((m) => m.fieldIndex === 0)?.ranges;
    if (raw !== undefined) {
      if (primaryField.text === item.label) {
        ranges = raw;
      } else if (item.label.endsWith(primaryField.text)) {
        const offset = item.label.length - primaryField.text.length;
        ranges = raw.map((r) => ({ start: r.start + offset, end: r.end + offset }));
      }
    }
  }

  return (
    <li
      id={optionId(index)}
      role="option"
      aria-selected={active}
      onMouseDown={onMouseDown}
      onClick={onClick}
      data-testid="command-omnibar-option"
      className={`flex items-center gap-3 px-4 py-2 cursor-pointer ${
        active ? 'bg-bg-card' : ''
      }`}
    >
      <div className="flex-1 min-w-0">
        <div className="text-sm text-text-primary truncate">
          {ranges && ranges.length > 0 ? (
            <Highlighted text={item.label} ranges={ranges} />
          ) : (
            item.label
          )}
        </div>
        {item.subtitle && (
          <div className="text-2xs text-text-muted truncate">{item.subtitle}</div>
        )}
      </div>
      <span className="text-2xs text-text-muted shrink-0">{item.category}</span>
    </li>
  );
}

function Highlighted({
  text,
  ranges,
}: {
  text: string;
  ranges: { start: number; end: number }[];
}) {
  const segments: ReactNode[] = [];
  let cursor = 0;
  for (const range of ranges) {
    if (range.start > cursor) {
      segments.push(text.slice(cursor, range.start));
    }
    segments.push(
      <mark key={`${range.start}-${range.end}`} className="bg-transparent text-accent-cyan font-semibold">
        {text.slice(range.start, range.end)}
      </mark>,
    );
    cursor = Math.max(cursor, range.end);
  }
  if (cursor < text.length) {
    segments.push(text.slice(cursor));
  }
  return <>{segments}</>;
}
