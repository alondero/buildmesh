/**
 * Issue #1536 — context-aware canvas empty state.
 *
 * The same "Add Mesh" splash used to render for three semantically
 * distinct conditions:
 *
 *   1. No meshes exist anywhere.
 *   2. A selected Mesh has zero Agent Nodes.
 *   3. Agent Nodes exist but the active search / provider / status filter
 *      excludes all of them.
 *
 * Each case has a different correct call to action — a single splash that
 * conflates them tells the user the wrong story ("Add a mesh" when they
 * already have one). This component picks one of five branches from a
 * discriminated input and renders a CTA that drives the right store
 * action through the caller-supplied callbacks.
 *
 * The component is intentionally a pure renderer: it reads no store state
 * directly and invokes no global action. Callers (`AgentNodeView`,
 * tests) hand in both the input shape and the four callbacks. That keeps
 * unit tests a single render call with synthetic props and lets the
 * owning UI control which modal mounts (issue #1536 prefers callbacks
 * over the legacy `meshStore.addMesh()` direct call).
 */
import { SHORTCUT_CATALOG, shortcutLabel } from '../../lib/shortcutCatalog';
import type { ViewMode } from '../../stores/uiStore';

/** The shape the empty state needs to pick a branch. Each field is
 *  computed by the owning UI from the live stores — `CanvasEmptyState`
 *  itself subscribes to nothing. The counts/booleans mirror the
 *  issue spec (wayfinder #982 / ticket #986 + issue #1536):
 *  scoped count = nodes the active view-mode scope holds BEFORE the
 *  grid controls narrow it; filtered count = what survived. The
 *  distinction is the whole point of #1536 — the previous splash
 *  collapsed them into one "empty" signal. */
export interface CanvasEmptyStateInput {
  /** Total meshes loaded. Drives the "No meshes yet" branch. */
  meshCount: number;
  /** Total Agent Nodes loaded into the store (across every mesh).
   *  Distinguishes "all meshes empty" from "filters excluded every
   *  node". */
  totalNodeCount: number;
  /** Nodes inside the active scope (mesh/all/filtered) BEFORE the
   *  grid controls narrow it. 0 means the scope is genuinely empty —
   *  the "selected empty mesh" branch keys off this for 'mesh' view. */
  scopedCount: number;
  /** Nodes the active scope actually shows. 0 while `scopedCount` > 0
   *  drives `filters-exclude-all`. */
  filteredCount: number;
  /** The active view mode. */
  viewMode: ViewMode;
  /** The sidebar's selected mesh id, or `null` when none is selected.
   *  Required (not assumed!) for the `selected-empty` branch. The
   *  classifier reads this field explicitly. */
  selectedMeshId: number | null;
  /** Whether ANY non-terminal harness is reachable (issue #822).
   *  Drives the "Setup" routing when the user has no usable agent. */
  harnessReady: boolean;
}

/** Structured classifier result. Each branch carries the data the
 *  renderer needs — the `selectedMeshId` for `selected-empty` is
 *  typed as `number` (non-nullable) so the renderer doesn't need
 *  any `as` cast. Type safety comes from the discriminator, not
 *  from a comment that lies. */
export type CanvasEmptyDecision =
  | { branch: 'no-meshes' }
  | { branch: 'selected-empty'; meshId: number }
  | { branch: 'all-empty' }
  | { branch: 'filters-exclude-all' }
  | { branch: 'pinned-empty' };

/** Callbacks the empty state's CTAs fire. Owners wire these to the
 *  appropriate store action / modal opener:
 *    - onCreateMesh: open the canonical Mesh Create modal
 *    - onOpenSpawnMenu: open the canvas Spawn Menu dialog (meshId passed
 *      when known, e.g. on the "selected empty mesh" branch)
 *    - onClearFilters: reset the grid controls (search / provider / status)
 *    - onOpenSetup: open App Settings → Providers when no harness works
 *    - onViewAll: swap the canvas view mode to 'all' (used by the
 *      Pinned empty state's "View All Nodes" CTA so a new user lands
 *      in the right place instead of bouncing back to Pinned) */
export interface CanvasEmptyStateCallbacks {
  onCreateMesh: () => void;
  onOpenSpawnMenu: (meshId: number | null) => void;
  onClearFilters: () => void;
  onOpenSetup: () => void;
  onViewAll: () => void;
}

interface CanvasEmptyStateProps {
  input: CanvasEmptyStateInput;
  callbacks: CanvasEmptyStateCallbacks;
}

/** Pick the right branch from the input. Centralised so the test surface
 *  can assert the discriminator directly without rendering — and so a
 *  future caller adding a branch only edits one switch instead of
 *  re-nesting an inline ternary in the render.
 *
 * Decision order matters (issue #1536, senior-review round 4):
 *   1. "No meshes" wins first — regardless of view mode. A
 *      brand-new user with zero meshes in Pinned mode would
 *      otherwise see "No pinned nodes. Pin agents from any mesh"
 *      even though they have no meshes to pin into. The
 *      application-level "no meshes" CTA (New mesh) is the only
 *      branch that addresses every "no meshes" state regardless of
 *      how the user got there.
 *   2. Pinned wins next — its dedicated CTA swaps view mode, which
 *      is the right escape when the user has meshes but nothing
 *      pinned.
 *   3. "Selected mesh is empty" — view mode is mesh AND that mesh
 *      has zero nodes AND the user explicitly picked it
 *      (selectedMeshId is set). Distinct from "all meshes empty"
 *      because the user has a place to spawn into.
 *      `scopeNodesForMode` (viewModes.ts) falls back to the active
 *      node's mesh, then to the first mesh — so the mesh-with-no-
 *      selection case is unreachable in production. The classifier
 *      routes only the explicit-selection case to `selected-empty`;
 *      a mesh-view-with-fallback-scope renders through to the
 *      `filters-exclude-all` branch via the existing checks below.
 *   4. "All meshes empty" — meshes exist but zero nodes globally.
 *      The CTA still offers a spawn action because at least one
 *      mesh is wired up; it just has no agents yet.
 *   5. "Filters exclude all" — there ARE nodes, just none that
 *      match the active controls. The way out is `onClearFilters`,
 *      NOT adding meshes or spawning (issue #1609 mirrors this for
 *      the dedicated Filtered view; #1536 generalises it).
 */
export function classifyCanvasEmpty(input: CanvasEmptyStateInput): CanvasEmptyDecision {
  if (input.meshCount === 0) return { branch: 'no-meshes' };
  if (input.viewMode === 'pinned') return { branch: 'pinned-empty' };
  if (input.viewMode === 'mesh' && input.scopedCount === 0 && input.selectedMeshId !== null) {
    return { branch: 'selected-empty', meshId: input.selectedMeshId };
  }
  if (input.totalNodeCount === 0) return { branch: 'all-empty' };
  if (input.filteredCount === 0) return { branch: 'filters-exclude-all' };
  // Defensive — caller should not render the empty state when both
  // counts are positive. Treat as "no-matches" so the user always sees
  // a clear next step rather than the silent splash.
  return { branch: 'filters-exclude-all' };
}

/** The shared shell — centered, max-w-sm, heading + body + accent-cyan
 *  CTA. Mirrors the existing splash + Pinned/Filtered empty-state
 *  geometry so a branch swap is a same-position transition for the
 *  user. Each branch supplies its own heading / body / CTA — the shell
 *  just lays them out. */
function EmptyShell({
  icon,
  heading,
  body,
  cta,
}: {
  icon: React.ReactNode;
  heading: string;
  body: string;
  cta: React.ReactNode;
}) {
  return (
    <div className="flex-1 flex items-center justify-center text-text-muted">
      <div className="text-center max-w-sm" data-testid="canvas-empty-state">
        {icon}
        <p className="text-xl mb-2 text-text-primary font-sans font-semibold">{heading}</p>
        <p className="text-sm text-text-secondary mb-6 font-sans">{body}</p>
        {cta}
      </div>
    </div>
  );
}

const folderIcon = (
  <svg
    className="mx-auto mb-4 w-8 h-8 text-text-muted"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.75"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden
  >
    <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />
  </svg>
);

const agentIcon = (
  <svg
    className="mx-auto mb-4 w-8 h-8 text-text-muted"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.75"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden
  >
    <path d="M12 17v5" />
    <path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z" />
  </svg>
);

const filterIcon = (
  <svg
    className="mx-auto mb-4 w-8 h-8 text-text-muted"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.75"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden
  >
    <polygon points="22 3 2 3 10 12.46 10 19 14 21 14 12.46 22 3" />
  </svg>
);

/** "No meshes yet" — the on-board view. Brief explanation of what a
 *  mesh is, then the canonical Mesh Create CTA. The shortcut rows
 *  underneath (the first-launch "things you can do" list) stay so a
 *  brand-new user sees the catalogue even before they add anything. */
function NoMeshesBranch({ onCreateMesh }: { onCreateMesh: () => void }) {
  return (
    <EmptyShell
      icon={folderIcon}
      heading="Buildmesh"
      body="Orchestrate AI agents across your meshes. Add a mesh pointing at a Git repository, then spawn agents to work in parallel."
      cta={
        <>
          <button
            type="button"
            data-testid="canvas-empty-create-mesh"
            onClick={onCreateMesh}
            className="inline-flex items-center gap-2 px-4 py-2 rounded-md bg-accent-cyan/10 text-accent-cyan font-sans font-medium text-sm hover:bg-accent-cyan/20 transition-colors border border-accent-cyan/20"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />
            </svg>
            New mesh
          </button>
          <div className="mt-8 text-xs text-text-muted font-mono space-y-1">
            {SHORTCUT_CATALOG.filter((entry) => entry.splash).map((entry) => (
              <p key={entry.action}>
                <kbd className="px-1 py-0.5 rounded-md bg-bg-card border border-border-default">
                  {shortcutLabel(entry)}
                </kbd>
                {' '}{entry.description}
              </p>
            ))}
          </div>
        </>
      }
    />
  );
}

const addAgentIcon = (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <line x1="12" y1="5" x2="12" y2="19" />
    <line x1="5" y1="12" x2="19" y2="12" />
  </svg>
);

const clearFiltersIcon = (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <polygon points="22 3 2 3 10 12.46 10 19 14 21 14 12.46 22 3" />
    <line x1="22" y1="3" x2="2" y2="3" />
  </svg>
);

/** "Selected Mesh is empty" — the right mesh exists, it just has no
 *  agents yet. Primary action opens the Spawn Menu for that mesh;
 *  if no harness is usable (fresh install, no keyed provider), the
 *  CTA routes to Setup instead so the user fixes the underlying
 *  problem rather than clicking a menu that has nothing to offer. */
function SelectedEmptyBranch({
  harnessReady,
  callbacks,
  selectedMeshId,
}: {
  harnessReady: boolean;
  callbacks: CanvasEmptyStateCallbacks;
  selectedMeshId: number;
}) {
  return (
    <EmptyShell
      icon={agentIcon}
      heading="No agents in this mesh"
      body="Spawn your first agent to start working in this repository. The Spawn Menu picks the harness to launch — native CLI if installed, or a keyed provider."
      cta={
        harnessReady ? (
          <button
            type="button"
            data-testid="canvas-empty-spawn-agent"
            onClick={() => callbacks.onOpenSpawnMenu(selectedMeshId)}
            className="inline-flex items-center gap-2 px-4 py-2 rounded-md bg-accent-cyan/10 text-accent-cyan font-sans font-medium text-sm hover:bg-accent-cyan/20 transition-colors border border-accent-cyan/20"
          >
            {addAgentIcon}
            Spawn agent
          </button>
        ) : (
          <button
            type="button"
            data-testid="canvas-empty-open-setup"
            onClick={callbacks.onOpenSetup}
            className="inline-flex items-center gap-2 px-4 py-2 rounded-md bg-accent-cyan/10 text-accent-cyan font-sans font-medium text-sm hover:bg-accent-cyan/20 transition-colors border border-accent-cyan/20"
          >
            Open Settings
          </button>
        )
      }
    />
  );
}

/** "All meshes empty" — at least one mesh exists, but none have an
 *  agent. Mirrors the selected-empty branch but the Spawn Menu opens
 *  with `meshId: null` (caller picks a sensible default — typically the
 *  most recently selected mesh, falling back to the first one). Same
 *  harness-readiness routing to Setup. */
function AllEmptyBranch({
  harnessReady,
  callbacks,
}: {
  harnessReady: boolean;
  callbacks: CanvasEmptyStateCallbacks;
}) {
  return (
    <EmptyShell
      icon={agentIcon}
      heading="No agents yet"
      body="Pick a mesh from the sidebar and spawn your first agent. Each mesh maps to a Git repository; agents work in parallel branches and you watch the live terminals here."
      cta={
        harnessReady ? (
          <button
            type="button"
            data-testid="canvas-empty-spawn-agent"
            onClick={() => callbacks.onOpenSpawnMenu(null)}
            className="inline-flex items-center gap-2 px-4 py-2 rounded-md bg-accent-cyan/10 text-accent-cyan font-sans font-medium text-sm hover:bg-accent-cyan/20 transition-colors border border-accent-cyan/20"
          >
            {addAgentIcon}
            Spawn agent
          </button>
        ) : (
          <button
            type="button"
            data-testid="canvas-empty-open-setup"
            onClick={callbacks.onOpenSetup}
            className="inline-flex items-center gap-2 px-4 py-2 rounded-md bg-accent-cyan/10 text-accent-cyan font-sans font-medium text-sm hover:bg-accent-cyan/20 transition-colors border border-accent-cyan/20"
          >
            Open Settings
          </button>
        )
      }
    />
  );
}

/** "Filters exclude all" — there are nodes, just none that match the
 *  active search / provider / status. The way out is relaxing the
 *  controls, NOT adding meshes or spawning. Mirrors the Filtered
 *  empty state (#1609) but is reachable from any non-Filtered mode
 *  where an unscoped match would have been visible: pre-#1536 a stale
 *  search hid nodes silently behind the user's back. */
function FiltersExcludeAllBranch({ onClearFilters }: { onClearFilters: () => void }) {
  return (
    <EmptyShell
      icon={filterIcon}
      heading="No nodes match"
      body="No agent matches the active search or filters. Clear them to see every node again."
      cta={
        <button
          type="button"
          data-testid="canvas-empty-clear-filters"
          onClick={onClearFilters}
          className="inline-flex items-center gap-2 px-4 py-2 rounded-md bg-accent-cyan/10 text-accent-cyan font-sans font-medium text-sm hover:bg-accent-cyan/20 transition-colors border border-accent-cyan/20"
        >
          {clearFiltersIcon}
          Clear search & filters
        </button>
      }
    />
  );
}

/** "Pinned Grid is empty" — preserves the dedicated Pinned empty
 *  state (#982 / #986). The CTA swaps view mode to 'all' (the same
 *  switch the original PinnedEmptyState fires) so a new user lands
 *  in the right place instead of bouncing back to Pinned. */
function PinnedEmptyBranch({ onViewAll }: { onViewAll: () => void }) {
  return (
    <EmptyShell
      icon={agentIcon}
      heading="No pinned nodes"
      body="Pin agents from any mesh to keep them in reach here. Use the pin button in a node's header, or right-click a node in the sidebar."
      cta={
        <button
          type="button"
          data-testid="canvas-empty-view-all"
          onClick={onViewAll}
          className="inline-flex items-center gap-2 px-4 py-2 rounded-md bg-accent-cyan/10 text-accent-cyan font-sans font-medium text-sm hover:bg-accent-cyan/20 transition-colors border border-accent-cyan/20"
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <rect width="7" height="9" x="3" y="3" rx="1" />
            <rect width="7" height="5" x="14" y="3" rx="1" />
            <rect width="7" height="9" x="14" y="12" rx="1" />
            <rect width="7" height="5" x="3" y="16" rx="1" />
          </svg>
          View All Nodes
        </button>
      }
    />
  );
}

export function CanvasEmptyState({
  input,
  callbacks,
}: CanvasEmptyStateProps) {
  const decision = classifyCanvasEmpty(input);
  switch (decision.branch) {
    case 'no-meshes':
      return <NoMeshesBranch onCreateMesh={callbacks.onCreateMesh} />;
    case 'selected-empty':
      // The classifier's discriminated union returns `meshId: number`
      // (non-nullable) on this branch — TypeScript narrows the type
      // automatically, no `as` cast required.
      return (
        <SelectedEmptyBranch
          harnessReady={input.harnessReady}
          callbacks={callbacks}
          selectedMeshId={decision.meshId}
        />
      );
    case 'all-empty':
      return (
        <AllEmptyBranch
          harnessReady={input.harnessReady}
          callbacks={callbacks}
        />
      );
    case 'filters-exclude-all':
      return <FiltersExcludeAllBranch onClearFilters={callbacks.onClearFilters} />;
    case 'pinned-empty':
      return <PinnedEmptyBranch onViewAll={callbacks.onViewAll} />;
  }
}
