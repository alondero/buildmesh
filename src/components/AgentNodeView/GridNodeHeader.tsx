import { useMemo, useRef, useState, useEffect, useLayoutEffect } from 'react';
import { createPortal } from 'react-dom';
import { useAgentNodeStore, type AgentNode } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { BuildRunDropdown } from '../BuildRun/BuildRunDropdown';
import { GridRegenerateButton } from './GridRegenerateButton';
import { PrPill } from './PrPill';
import { useGitSummary } from '../../hooks/useGitSummary';
import { useOpenPr } from '../../hooks/useOpenPr';
import { useResizeWidth } from '../../hooks/useResizeWidth';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useProviderList } from '../../hooks/useProviderList';
import { useRegenerateAction } from '../../hooks/useRegenerateAction';
import { useSubmenu, focusWithoutScroll } from '../../hooks/useSubmenu';
import { useAnchoredPosition } from '../../hooks/useAnchoredPosition';
import { getNodeGitPath } from '../../lib/paths';
import { getStatusConfig } from '../../lib/status';
import { canResumeSuspendedNode, hasLostConversation } from '../../lib/suspended';
import { MissingSessionIdBadge } from '../shared/MissingSessionIdBadge';
import type { SpawnOption } from '../../lib/groups';
import { getMeshColor } from '../../lib/meshColors';
import type { AutopilotRunState } from '../../types/generated/AutopilotRunStateKind';
import { ProviderIcon } from '../Providers/ProviderIcon';
import { RegenerateProviderMenu } from '../Providers/RegenerateProviderMenu';
import { InlineEditableText } from '../shared/InlineEditableText';
import { FolderOpenIcon } from '../shared/FolderOpenIcon';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import { openInFileManager } from '../../lib/tauri';
import { isMac } from '../../lib/platform';
import { CircuitsIcon } from '../Probe/probeIcons';
import { AgentReviewButton } from './AgentReviewButton';

interface GridNodeHeaderProps {
  /// Issue #1384 — pass the id only; the header subscribes to
  /// `state.nodesById[nodeId]` directly. Resolving the node here means
  /// the parent doesn't have to ship a fresh object reference on every
  /// fetch, and unrelated attention events on other nodes no longer
  /// re-render this header.
  nodeId: number;
  onBuildRun: (nodeId: number, mode: 'build' | 'run' | 'terminal') => void;
  /// dnd-kit drag listeners/attributes that turn the whole title bar into the
  /// reorder/swap drag handle. Undefined when dragging is disabled (e.g. the
  /// maximized solo view, or in isolation tests).
  dragHandleProps?: Record<string, unknown>;
}

// Responsive tier breakpoints (issue #736). Ordered highest → lowest so the
// first match wins; the `$` last clause keeps the type narrow in JS so
// downstream switches don't need a default branch.
//
// Why these specific values rather than the Tailwind v4 container defaults
// (`xs 320 / sm 384 / md 448 / lg 512 / xl 576`)? The agent-node header sits
// inside a pane set by the GridSplitter / ResizablePanes and can run as narrow
// as ~150 px. A 320 px floor would force every node in a 2-pane layout to start
// in `compact`, so the breakpoints step down to match the actual minimum
// layout (Build ▼ + kebab fits in ~155 px) and stagger in chips once per
// hierarchy level.
export type HeaderTier = 'xl' | 'wide' | 'medium' | 'slim' | 'compact';

export const HEADER_TIER_BREAKPOINTS: Record<Exclude<HeaderTier, 'compact'>, number> = {
  xl: 640,    // diff summary joins the row
  wide: 500,  // PR chip joins the row
  medium: 380, // worktree/root pill joins the row; inline close+max replace kebab
  slim: 280,  // mesh label joins the row; kebab stays
};
// `compact` is the implicit floor (`< HEADER_TIER_BREAKPOINTS.slim`).

// Autopilot pill styling per pipeline state (the typed
// `AutopilotRunState` union — wires in via ts-rs from the Rust enum).
// Violet is the "automation owns this node" accent; the wrap-up / terminal
// states reuse the amber / green / red semantics the rest of the header
// already speaks.
export const AUTOPILOT_PILL_STYLES: Record<AutopilotRunState, { label: string; className: string; title: string }> = {
  implementing: {
    label: 'autopilot',
    className: 'bg-accent-violet/15 text-accent-violet ring-accent-violet/40',
    title: 'Autopilot: agent is implementing the task',
  },
  finishing: {
    label: 'autopilot · wrap-up',
    className: 'bg-accent-amber/15 text-accent-amber ring-accent-amber/40',
    title: 'Autopilot: wrap-up in progress (verify, commit, push, PR)',
  },
  // Issue #993: a loop iteration's deterministic wrap-up passed and the
  // optional `loop_suffix_prompt` is now driving a second PTY turn on the
  // same node. The node is still active (it holds the mesh capacity slot
  // and the iteration row) until that suffix turn yields and the run
  // reaches `completed`. Renders with the same amber treatment as
  // `finishing` so the user can tell this is still in flight, not a wrap-
  // up verification failure.
  suffix_pending: {
    label: 'autopilot · suffix',
    className: 'bg-accent-amber/15 text-accent-amber ring-accent-amber/40',
    title: 'Autopilot: wrap-up verified, running the optional suffix prompt turn before this iteration completes',
  },
  completed: {
    label: 'autopilot · complete',
    className: 'bg-accent-green/10 text-accent-green ring-accent-green/30',
    title: 'Autopilot: wrap-up verified, hands off — the PR is ready for review; the node closes when it merges',
  },
  merged: {
    label: 'autopilot · merged',
    className: 'bg-accent-green/10 text-accent-green ring-accent-green/30',
    title: 'Autopilot: PR merged',
  },
  failed: {
    label: 'autopilot ✗',
    className: 'bg-status-error-bg text-status-error ring-status-error/40',
    title: 'Autopilot: wrap-up failed after 3 attempts — needs a human',
  },
};

export function getHeaderTier(width: number): HeaderTier {
  if (width >= HEADER_TIER_BREAKPOINTS.xl) return 'xl';
  if (width >= HEADER_TIER_BREAKPOINTS.wide) return 'wide';
  if (width >= HEADER_TIER_BREAKPOINTS.medium) return 'medium';
  if (width >= HEADER_TIER_BREAKPOINTS.slim) return 'slim';
  return 'compact';
}

export function GridNodeHeader({ nodeId, onBuildRun, dragHandleProps }: GridNodeHeaderProps) {
  // Issue #1384 — per-id subscription: this header now subscribes directly
  // to `state.nodesById[nodeId]` rather than receiving `node` as a prop. The
  // store's shallow reconciliation keeps the same object reference for
  // unchanged rows, so this selector only re-renders the header when THIS
  // specific node changes. Other nodes' attention/rename events no longer
  // cascade into this header, satisfying the spec's Point 4 / acceptance
  // criterion directly at the header level.
  const node = useAgentNodeStore((state) => state.nodesById[nodeId]);
  // All hooks below MUST run unconditionally (Rules of Hooks). The
  // `node?.id` guard handles the not-yet-loaded case; the JSX section
  // uses `if (!node) return null` once all hooks have run.
  // Issue #376: the chip now opens the unified Probe Panel on the 🔍
  // (Agent Changes) tab for this node, rather than toggling the legacy
  // FileExplorerPanel in the SessionView left pane (deleted in #380; the
  // `AgentChangesTab` review surface is the only one now).
  const openProbeTab = useUIStore(state => state.openProbeTab);
  const probeOpen = useUIStore(state => state.probeOpen);
  const probeTab = useUIStore(state => state.probeTab);
  // Boolean mode selector (wayfinder #982 / #983): Single mode subsumes the
  // old per-node `maximizedNodeId` — in Single mode only the soloed card
  // renders, so "this header is the solo view's" is exactly "the canvas is
  // in Single mode". Boolean (not the raw mode) so headers only re-render
  // when the boolean flips, not on every uiStore change.
  const isSingleMode = useUIStore(state => state.viewMode === 'single');
  const setViewMode = useUIStore(state => state.setViewMode);
  const exitSingleMode = useUIStore(state => state.exitSingleMode);
  const deleteAgentNode = useAgentNodeStore(state => state.deleteAgentNode);
  const isAutopilotNode = useAgentNodeStore(state => state.autopilotStates[nodeId] != null);
  // Pin toggle (wayfinder #982 / #985) — the shared store action is the
  // sole mutation path. Its optimistic patch flips `node.is_pinned` in
  // place, so this header re-renders via the existing `node` prop with no
  // local pin state of its own.
  const toggleNodePinned = useAgentNodeStore(state => state.toggleNodePinned);
  const renameAgentNode = useAgentNodeStore(state => state.renameAgentNode);
  // Resume (user-driven recovery for Suspended nodes) — mirrors the
  // sidebar NodeItem's inline Resume button. The store action reads
  // `cli_session_id` from the row and passes it as the `resume` arg;
  // for adapters that don't honour resume (OpenCode, Terminal) the
  // backend falls through to Fresh. The visibility gate (`canResume`)
  // is the same predicate as the sidebar's `showResume` — autopilot-
  // gate Suspended rows (no `cli_session_id`) get NO affordance here.
  const spawnAgent = useAgentNodeStore((state) => state.spawnAgent);
  // The chip's click focuses the node before opening the probe — the
  // `AgentChangesTab` reads `useProbeContext().activeNodeId` to pick
  // which node's review to render, so without this the user could land
  // on a different terminal's review if a different node was already
  // focused. (The pre-#376 left-pane `FileExplorerPanel` accepted an
  // explicit `nodeId` per click; the new probe context derivation
  // makes "focus" the natural way to express "review THIS node".)
  const setActiveNode = useAgentNodeStore((state) => state.setActiveNode);
  // The cyan chip highlight (post-#376) signals "the probe is showing
  // this node's review right now". `AgentChangesTab` reads `activeNodeId`
  // from this store to pick which node's review to render, so we compare
  // the same value to keep the highlight and the body in sync.
  const isReviewingThisNode = useAgentNodeStore((s) => s.activeNodeId === nodeId);
  const autopilotState = useAgentNodeStore((s) => s.autopilotStates[nodeId]);
  const circuitOwnership = useAgentNodeStore((s) => s.circuitOwnerships[nodeId]);
  const meshesById = useMeshStore(state => state.meshesById);
  // Issue #1502 — the Regenerate picker reads the single shared Spawn
  // Option snapshot (`useProviderList`: one fetch + one
  // `provider-list-changed` subscription app-wide). The header itself
  // owns no provider state.
  const providerList = useProviderList();
  // Shared Regenerate action (issue #778 contract): status gate, confirm
  // state machine, IPC dispatch. Called unconditionally with the
  // possibly-not-yet-loaded node — the hook guards `undefined`
  // internally (Rules of Hooks: no hook may sit below the early
  // return).
  const regen = useRegenerateAction(node, providerList);
  const {
    pendingRegenerate,
    isRegenerateDisabled,
    hasRegenerateTargets,
    pickRegenerateProvider,
    confirmRegenerate,
    cancelRegenerate,
  } = regen;

  // Issue #736 — measure the rendered header width and bucket it into a tier
  // that decides which chips render and whether the close/max buttons live
  // inline or inside a kebab menu. The hook starts at `Infinity` so the
  // initial render shows everything until the ResizeObserver reports; on
  // shrunken panes the worst case is one flash of "wide" before the tier
  // drops on the next frame. Hook placed BEFORE the early-return below
  // to preserve Rules of Hooks ordering across renders.
  const headerRef = useRef<HTMLDivElement>(null);
  const width = useResizeWidth(headerRef);

  // Git summary + open-PR cache hooks. Both must run unconditionally —
  // placing them after the `if (!node) return null` guard would skip
  // them on the re-render where the node was just removed (e.g. after
  // `deleteAgentNode` removes the row from `nodesById`), which would
  // trip React's "rendered fewer hooks than expected" assertion. Pass
  // null when the node isn't loaded so the hooks don't issue IPC for
  // an id that no longer exists.
  const gitPath = node ? getNodeGitPath(node) : null;
  const { summary } = useGitSummary(gitPath);
  const { pr: openPr } = useOpenPr(nodeId, gitPath);
  const meshLabel = useMemo(() => {
    if (!node) return '';
    const m = meshesById.get(node.mesh_id);
    return m ? `[${m.name} #${node.id}]` : `[#${node.id}]`;
  }, [meshesById, node?.mesh_id, node?.id]);

  if (!node) return null;
  const meshColor = getMeshColor(node.mesh_id, meshesById.get(node.mesh_id)?.color);
  const tier = getHeaderTier(width);
  // Convenience booleans for readability — avoids noisy `tier === 'xl' || tier === 'wide'`
  // chains at every chip.
  const showSummary = tier === 'xl';
  const showPr = tier === 'xl' || tier === 'wide';
  const showWorktree = tier === 'xl' || tier === 'wide' || tier === 'medium';
  const showMeshLabel = tier !== 'compact';
  // The worktree pill and the inline close+max buttons appear at the same
  // tiers because both compete for the same horizontal real estate — any
  // future tier shift on one should land on the other. Aliasing here makes
  // that coupling explicit instead of inviting two booleans to drift.
  const showInlineActions = showWorktree;

  const handleClose = async (e: React.MouseEvent) => {
    e.stopPropagation();
    await deleteAgentNode(node.id);
  };

  // Enter Single mode on this node (explicit focus wins — ticket #983), or,
  // when this header IS the solo view, exit back to the grid mode Single was
  // entered from. Replaces the old toggleMaximizedNode.
  const handleToggleSolo = () => {
    if (isSingleMode) {
      exitSingleMode();
      return;
    }
    setActiveNode(node.id);
    setViewMode('single');
  };

  // Pin/Unpin (#985). The store rolls back and surfaces the rejection on
  // `state.error` (→ App toast pipeline), so the local catch only suppresses
  // unhandled-rejection noise. Stop propagation so the click doesn't
  // activate/select the card.
  const handleTogglePin = (e: React.MouseEvent) => {
    e.stopPropagation();
    void toggleNodePinned(node.id).catch(() => {});
  };

  // Resume (user-driven recovery for Suspended nodes). Same rationale
  // as `handleTogglePin` for the silent-catch. The `resume-failed`
  // Tauri event surfaces an App toast on failure
  // (`src/App.tsx:419-424`), so the catch only suppresses unhandled-
  // rejection noise.
  const handleResume = (e: React.MouseEvent) => {
    e.stopPropagation();
    void spawnAgent(node.id, node.provider).catch(() => {});
  };

  // Same predicate as the sidebar NodeItem's `showResume` — the
  // shared `canResumeSuspendedNode` helper keeps both surfaces in
  // lockstep (a Suspended OpenCode node shows the Resume affordance
  // in both places, never just one).
  const canResume = canResumeSuspendedNode(node);

  // Silent on failure — worktree rows can go stale between renders and a
  // toast storm on every click is worse UX than a quiet console line.
  // Same precedent as `WorktreeManagerTab.openInExplorer`.
  const handleOpenInExplorer = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (!gitPath) return;
    try {
      await openInFileManager(gitPath);
    } catch (err) {
      console.error('Failed to open folder in file manager:', err);
    }
  };
  // (Manual `/finish` trigger — issue #484, PRD #480 story 15 — was here
  // until this commit. The wrap-up sequence is an autopilot-only concern;
  // a hand-controlled node has no use for it, so the toolbar button (and
  // its underlying `trigger_finish` IPC) was removed. The autopilot's
  // automatic wrap-up injection lives in `src-tauri/src/autopilot/pipeline.rs`
  // and still emits `autopilot-finishing` for the pill transition.)

  const isPanelNode = probeOpen && probeTab === 'review' && isReviewingThisNode;

  // Issue #668 — advertise the Alt+G / Cmd+G shortcut in the title tooltip
  // alongside the existing double-click affordance, so discoverability
  // doesn't depend on the empty-state splash being on screen.
  const toggleShortcutHint = `${isMac ? '⌘' : 'Alt'}+G`;

  return (
    <div
      {...dragHandleProps}
      onDoubleClick={handleToggleSolo}
      title={isSingleMode
        ? `Double-click or press ${toggleShortcutHint} to restore grid`
        : `Double-click or press ${toggleShortcutHint} to maximize`}
      // Issue #736 — `data-tier` is the test seam: jsdom can't observe the
      // actual visibility of a container-query CSS rule, but the JS state
      // driving the render branches sets `data-tier` on the root, so tests
      // can pin which chip is present at each breakpoint by reading the
      // attribute. Production can also use it as a CSS hook if needed.
      data-tier={tier}
      data-testid="grid-node-header"
      ref={headerRef}
      className={`flex items-center justify-between px-2.5 py-1.5 border-b border-border-default gap-2 ${dragHandleProps ? 'cursor-grab active:cursor-grabbing' : ''}`}
      style={{ backgroundColor: `${meshColor.hex}40` }}
    >
      <div className="flex items-center gap-2 overflow-hidden flex-1 min-w-0">
        {dragHandleProps && (
          <span
            aria-hidden="true"
            title="Drag to reorder, or onto another node to swap"
            className="text-text-muted text-xs leading-none opacity-0 group-hover:opacity-60 transition-opacity select-none"
          >
            ⠿
          </span>
        )}
        <span className={`w-1.5 h-1.5 rounded-full flex-shrink-0 ${getStatusConfig(node.status).bgColor}`} />
        {hasLostConversation(node, isAutopilotNode) && <MissingSessionIdBadge />}
        {/* Issue #1364 §3 — node-level hook-health warning. Layered on top
            of the status, never a status: the node may be running fine while
            its attention hook is broken, and the user must be able to tell
            "no event yet" from "the hook can't reach us". */}
        {node.signal_health === 'unavailable' && (
          <span
            role="img"
            aria-label="Attention signal unavailable"
            title="Attention signal unavailable — the agent's lifecycle hook could not be installed or reached; watch the terminal directly."
            className="text-status-warning text-xs leading-none flex-shrink-0"
          >
            ⚠
          </span>
        )}
        <ProviderIcon providerId={node.provider} className="h-3.5 w-3.5 drop-shadow-sm flex-shrink-0" />
        <span
          onPointerDown={(e) => e.stopPropagation()}
          className="text-sm font-semibold text-text-primary truncate font-sans drop-shadow-sm min-w-0"
        >
          <InlineEditableText
            value={node.name}
            onCommit={(next) => renameAgentNode(node.id, next)}
            className="text-sm font-semibold text-text-primary font-sans drop-shadow-sm"
          />
          {/* Mesh label is the lowest-priority ID-only second string — the
              title must remain legible at every tier, so it's the first
              thing dropped under compaction. Conditional render shifts the
              title horizontally on tier change; a future iteration could
              place it on a second line under the title if the jitter
              becomes user-visible. */}
          {showMeshLabel && (
            <span className="text-xs text-text-secondary font-normal"> {meshLabel}</span>
          )}
        </span>
        {/* Autopilot pill — which nodes automation owns, and where each is
            in the pipeline. Gated one tier looser than the worktree pill
            (visible down to `slim`): "is this node on autopilot?" outranks
            "worktree vs root" when horizontal space runs out. */}
        {showMeshLabel && circuitOwnership && (
          <span
            data-testid="circuit-run-pill"
            title={`${circuitOwnership.circuit_name} · circuit run #${circuitOwnership.run_id}`}
            aria-label={`Circuit run #${circuitOwnership.run_id}`}
            className="text-2xs font-mono px-1.5 py-0.5 rounded-full leading-none font-semibold select-none whitespace-nowrap drop-shadow-sm flex-shrink-0 ring-1 ring-inset bg-accent-violet/15 text-accent-violet ring-accent-violet/40 inline-flex items-center gap-1"
          >
            <CircuitsIcon className="h-3 w-3" />
            <span>#{circuitOwnership.run_id}</span>
          </span>
        )}
        {showMeshLabel && !circuitOwnership && autopilotState && (
          <span
            data-testid="autopilot-pill"
            title={AUTOPILOT_PILL_STYLES[autopilotState].title}
            className={`text-2xs font-mono px-1.5 py-0.5 rounded-full leading-none font-semibold select-none whitespace-nowrap drop-shadow-sm flex-shrink-0 ring-1 ring-inset ${
              AUTOPILOT_PILL_STYLES[autopilotState].className
            }`}
          >
            {AUTOPILOT_PILL_STYLES[autopilotState].label}
          </span>
        )}
        {showWorktree && (
          <span
            title={node.use_worktree
              ? 'Agent runs in a git worktree'
              : 'Agent runs in the repository root'}
            className={`text-2xs font-mono px-1.5 py-0.5 rounded-full leading-none font-medium select-none whitespace-nowrap drop-shadow-sm flex-shrink-0 ${
              node.use_worktree
                ? 'bg-bg-overlay/70 text-text-muted ring-1 ring-inset ring-border-subtle'
                : 'bg-accent-cyan/15 text-accent-cyan ring-1 ring-inset ring-accent-cyan/40 font-semibold'
            }`}
          >
            {node.use_worktree ? 'worktree' : 'root'}
          </span>
        )}
        {showSummary && summary && (
          <span
            onPointerDown={(e) => e.stopPropagation()}
            onClick={(e) => { e.stopPropagation(); setActiveNode(node.id); openProbeTab('review'); }}
            className="text-xs font-mono font-semibold cursor-pointer flex items-center gap-1.5 drop-shadow-sm hover:brightness-125 flex-shrink-0"
            title="Click to see changes"
          >
            {/* Each count carries its own semantic colour so added / modified / deleted
                read at a glance against the mesh-tinted header. Zero counts stay muted
                so the eye lands on the changes that exist. When this node owns the
                agent file-explorer panel the whole chip flips to cyan as a selection
                cue, matching the panel border. */}
            <span className={isPanelNode ? 'text-accent-cyan' : summary.added ? 'text-accent-green' : 'text-text-muted'}>
              +{summary.added}
            </span>
            <span className={isPanelNode ? 'text-accent-cyan' : summary.modified ? 'text-accent-amber' : 'text-text-muted'}>
              ~{summary.modified}
            </span>
            <span className={isPanelNode ? 'text-accent-cyan' : summary.deleted ? 'text-accent-red' : 'text-text-muted'}>
              -{summary.deleted}
            </span>
          </span>
        )}
        {/* Open PR pill — clicking opens a menu with Open on GitHub +
            Merge (squash & delete branch) behind an inline confirm (same
            contract as the Probe Pull Requests tab). Hidden when no PR is
            open (useOpenPr returns null for the common cases: no auth, no
            PR, non-GitHub origin, unborn branch). Tooltip carries the PR
            title; if the PR is a draft, the tooltip is suffixed and merge
            is disabled. */}
        {showPr && openPr && (
          <PrPill nodeId={node.id} gitPath={gitPath} openPr={openPr} />
        )}
      </div>
      <div className="flex items-center gap-1.5 flex-shrink-0" onPointerDown={(e) => e.stopPropagation()}>
        <BuildRunDropdown node={node} onBuildRun={onBuildRun} />
        <AgentReviewButton node={node} />
        {showInlineActions ? (
          <>
            {/* Issue #1502 — Regenerate toolbar affordance (in-place
                kick-start + provider swap). Icon-only 28×28 trigger next
                to Build/Run; the dropdown picker pins `Current (<label>)`
                on top. Collapses into the kebab overflow menu at `slim` /
                `compact` (see `KebabActions` below) — follows the same
                `showInlineActions` boundary as the other header actions
                rather than introducing a second breakpoint. */}
            <GridRegenerateButton
              node={node}
              providerList={providerList}
              isDisabled={isRegenerateDisabled}
              hasTargets={hasRegenerateTargets}
              onPick={pickRegenerateProvider}
            />
            {/* Suspended → Resume affordance (user-driven recovery for
                orphaned nodes). Placed at the start of the inline trio
                so the recovery action is the leftmost / most discoverable
                control when the row IS suspended; the existing trio
                (Reveal / Pin / Maximize / Close) keeps its order otherwise.
                The violet accent mirrors the Suspended status dot — same
                colour cue as the sidebar NodeItem's Resume button. */}
            {canResume && (
              <button
                type="button"
                onClick={handleResume}
                className="w-7 h-7 flex items-center justify-center rounded-md bg-bg-base/60 border border-border-default text-text-primary hover:text-accent-violet hover:bg-accent-violet/15 hover:border-accent-violet/60 transition-colors text-base leading-none"
                title="Resume agent"
                aria-label="Resume agent"
                data-testid="grid-resume-button"
              >
                <span aria-hidden="true">↻</span>
              </button>
            )}
            <button
              type="button"
              onClick={handleOpenInExplorer}
              // Surface matches maximise/close (`bg-bg-base/60` + border)
              // so the trio reads as one control group against the mesh
              // tint. Cyan hover deliberately reuses `PathHeader`'s
              // accent — one verb, one accent, throughout the app.
              // Tooltip includes the resolved path because worktree
              // nodes open into a non-obvious `.claude/worktrees/<name>`
              // subdir, not the mesh root.
              className="w-7 h-7 flex items-center justify-center rounded-md bg-bg-base/60 border border-border-default text-text-primary hover:text-accent-cyan hover:bg-accent-cyan/15 hover:border-accent-cyan/60 transition-colors"
              title={gitPath ? `Open in file explorer (${gitPath})` : 'Open in file explorer'}
              aria-label="Open in file explorer"
            >
              <FolderOpenIcon className="w-3.5 h-3.5" />
            </button>
            <button
              type="button"
              onClick={handleTogglePin}
              // Pin/Unpin toggle (wayfinder #982 / #985) — same 28×28
              // surface and 12px stroke-icon convention as the other header
              // actions, placed beside maximize (before it) so the primary
              // controls keep their order. Filled pin + aria-pressed when
              // pinned; outline pin otherwise. Cyan hover matches the pin
              // accent used by the Pinned view segment.
              className="w-7 h-7 flex items-center justify-center rounded-md bg-bg-base/60 border border-border-default text-text-primary hover:text-accent-cyan hover:bg-accent-cyan/15 hover:border-accent-cyan/60 transition-colors"
              title={node.is_pinned ? 'Unpin node' : 'Pin node'}
              aria-label={node.is_pinned ? 'Unpin node' : 'Pin node'}
              aria-pressed={node.is_pinned}
            >
              <svg width="12" height="12" viewBox="0 0 24 24" fill={node.is_pinned ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M12 17v5" fill="none" />
                <path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z" />
              </svg>
            </button>
            <button
              onClick={(e) => { e.stopPropagation(); handleToggleSolo(); }}
              // Always-visible button surface (was opacity-0 until hover — invisible
              // against the mesh tint). Same `h-7` + `bg-bg-base/60 + border` as
              // BuildRunDropdown so the trio reads as one control group; hover
              // tint flips to cyan, matching the maximise semantic.
              className="w-7 h-7 flex items-center justify-center rounded-md bg-bg-base/60 border border-border-default text-text-primary hover:text-accent-cyan hover:bg-accent-cyan/15 hover:border-accent-cyan/60 transition-colors"
              // Issue #668 — surface the Alt+G / ⌘+G shortcut in the button
              // tooltip so discoverability isn't gated on the header double-click
              // or the empty-state splash.
              title={isSingleMode ? `Restore grid (or ${toggleShortcutHint})` : `Maximize (or ${toggleShortcutHint})`}
              aria-label={isSingleMode ? 'Restore grid layout' : 'Maximize agent node'}
            >
              {isSingleMode ? (
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="M9 9H4m0 0V4m0 5 6-6m5 16v-5m0 0h5m-5 0 6 6M9 15H4m0 0v5m0-5 6 6m5-16V4m0 0h5m-5 0 6 6" />
                </svg>
              ) : (
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="M15 3h6m0 0v6m0-6-7 7M9 21H3m0 0v-6m0 6 7-7" />
                </svg>
              )}
            </button>
            <button
              type="button"
              onClick={handleClose}
              // Same surface treatment as the maximise button so the close icon
              // reads at rest, not just on hover. Hover flips to the error
              // semantic so the destructive intent is unmistakable.
              className="w-7 h-7 flex items-center justify-center rounded-md bg-bg-base/60 border border-border-default text-text-primary hover:text-status-error hover:bg-status-error-bg hover:border-status-error/60 transition-colors text-base leading-none"
              title="Close agent node"
              aria-label="Close agent node"
            >
              ×
            </button>
          </>
        ) : (
          // Kebab menu — replaces the inline trio at `slim` and `compact`
          // widths where all three would crowd the title out. Same three
          // actions (Reveal in Explorer + Maximize/Restore + Close) with
          // identical tooltips/aria-labels so semantics are width-agnostic.
          // Issue #1502 — the Regenerate picker also collapses here (same
          // `showInlineActions` boundary as the inline toolbar button
          // above): the kebab gains a `Regenerate ▸` row whose submenu
          // pins `Current (<label>)` on top for in-place kick-start.
          // NOTE: <KebabActions> still uses the pre-#756 `text-text-muted
          // opacity-0` pattern at narrow widths — the always-visible surface
          // treatment introduced in this PR is intentionally scoped to the
          // inline trio so we don't bloat scope. Tracking the analogous
          // kebab-trigger fix separately (issue TBD).
          <KebabActions
            isSingleMode={isSingleMode}
            isPinned={node.is_pinned}
            toggleShortcutHint={toggleShortcutHint}
            onToggleSolo={(e) => { e.stopPropagation(); handleToggleSolo(); }}
            onTogglePin={handleTogglePin}
            onClose={handleClose}
            onOpenInExplorer={handleOpenInExplorer}
            canResume={canResume}
            onResume={handleResume}
            node={node}
            providerList={providerList}
            isRegenerateDisabled={isRegenerateDisabled}
            hasRegenerateTargets={hasRegenerateTargets}
            onPickRegenerate={pickRegenerateProvider}
          />
        )}
      </div>
      {/* Issue #1502 / #778 — confirmation dialog for running-node
          Regenerate from either the toolbar dropdown or the kebab
          submenu. Mirrors the sidebar `NodeItem` dialog contract. */}
      {pendingRegenerate && (
        <ConfirmDialog
          title="Regenerate this node?"
          message={`Agent is currently working. Regenerate with ${pendingRegenerate.providerLabel}?`}
          confirmLabel="Regenerate"
          onConfirm={confirmRegenerate}
          onCancel={cancelRegenerate}
        />
      )}
    </div>
  );
}

/**
 * Compact kebab menu for the right-side header actions. Single-use
 * component — the agent-node header is the only call site, so we
 * inline it here instead of elevating a generic KebabMenu primitive to
 * `src/components/shared/`. If a second consumer ever wants the same
 * shape, lift it then; pattern-match first.
 *
 * Closes on Escape, outside click, or item activation. Reuses the same
 * WAI-ARIA conventions the sidebar's MeshItem menu applies (issue #735):
 * role=menu / role=menuitem / aria-labelledby on the trigger, focus
 * returns to the trigger on close.
 *
 * Why `fixed` positioning rather than `absolute`? The kebab lives
 * inside `<NodeCard>` whose `overflow-hidden` would clip a popover
 * that escapes the card (e.g. when the menu needs to drop *below*
 * the bottom edge of the row). Fixed coordinates are viewport-scoped
 * and unaffected by ancestor overflow. Anchor to the trigger by
 * snapshotting its `getBoundingClientRect` on the toggle click.
 *
 * Issue #1291 — the menu is portaled to `document.body` for the same
 * reason the sidebar's NodeItem/MeshItem menus were in #1290: the
 * GridNodeHeader row nests inside a flex container that can carry a
 * CSS `filter` (the inactive-row brightness hover state on adjacent
 * rows) and inside the GridSplitter, both of which become containing
 * blocks for `position:fixed`. Portaling keeps `top`/`left` anchored
 * to the viewport so the menu renders where the trigger rect says it
 * should, even when the row above is hovered. The `useAnchoredPosition`
 * math above stays the same — viewport pixels are still the right
 * unit because the menu now lives at body level.
 */
interface KebabActionsProps {
  isSingleMode: boolean;
  isPinned: boolean;
  toggleShortcutHint: string;
  onToggleSolo: (e: React.MouseEvent) => void;
  onTogglePin: (e: React.MouseEvent) => void;
  onClose: (e: React.MouseEvent) => void;
  onOpenInExplorer: (e: React.MouseEvent) => void;
  // Resume (user-driven recovery for Suspended nodes) — added when the
  // fifth kebab item shipped. Rendered as a menu row that only renders
  // when `canResume` is true; passed through here so the kebab can
  // render it without duplicating the visibility predicate. Without
  // this gate the kebab would always show five items regardless of
  // status, breaking the `toHaveLength(4)` assertions in the
  // responsive-tier test for non-Suspended nodes.
  canResume: boolean;
  onResume: (e: React.MouseEvent) => void;
  // Issue #1502 — Regenerate picker (collapses here at `slim`/`compact`).
  // The parent owns the provider list + gating so the inline toolbar
  // button and this kebab submenu stay in lockstep; `onPickRegenerate`
  // is the parent's confirm-gated activation (running → dialog, else
  // immediate IPC + toast). `node` is a `Pick` of the generated
  // `AgentNode` wire type (never a hand-declared shape) — only the
  // fields this menu reads.
  node: Pick<AgentNode, 'id' | 'provider' | 'status'>;
  providerList: SpawnOption[];
  isRegenerateDisabled: boolean;
  hasRegenerateTargets: boolean;
  onPickRegenerate: (providerId: string, providerLabel: string) => void;
}

const KEBAB_MIN_WIDTH = 160;

function KebabActions({ isSingleMode, isPinned, toggleShortcutHint, onToggleSolo, onTogglePin, onClose, onOpenInExplorer, canResume, onResume, node, providerList, isRegenerateDisabled, hasRegenerateTargets, onPickRegenerate }: KebabActionsProps) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const menuItemRefs = useRef<(HTMLButtonElement | null)[]>([]);
  // Issue #1502 — Regenerate picker submenu via the shared `useSubmenu`
  // hook (same hook drives the sidebar `NodeItem` picker): hover/click
  // opens, ArrowRight opens-and-focuses, ArrowLeft closes, ArrowDown/Up
  // wraps. No local submenu state, refs, or modulo loops.
  const regenSubmenu = useSubmenu({
    disabled: isRegenerateDisabled,
    itemCount: (providerList ?? []).length,
  });
  const regenDisabled = isRegenerateDisabled || !hasRegenerateTargets;
  // Stable id linking the trigger to the menu for the WAI-ARIA
  // disclosure pattern (aria-controls). Each header instance owns one
  // kebab, so a module-scoped counter is enough.
  const menuIdRef = useRef(`grid-node-kebab-menu-${Math.random().toString(36).slice(2, 9)}`);
  const menuId = menuIdRef.current;
  // Reveal-in-explorer joined the existing maximize/close pair (#736);
  // Pin/Unpin joined in wayfinder #982 (#985). The manual Finish item
  // (#484) was removed — wrap-up is an autopilot-only concern. The
  // Resume item joined for user-driven recovery of Suspended nodes —
  // rendered conditionally on `canResume` so the menu's row count
  // matches the parent header's visibility gate. Issue #1502 — the
  // Regenerate row joins as the FIRST item (mirrors the sidebar context
  // menu order for discoverability): 5 items when no recovery is
  // available, 6 when the node is Suspended with a stored
  // `cli_session_id`. Arrow navigation wraps at the live count so
  // ArrowDown at the bottom doesn't focus a phantom slot whose ref was
  // never assigned.
  const itemCount = (canResume ? 6 : 5);
  const closeAndReturnFocus = () => {
    const trigger = triggerRef.current;
    regenSubmenu.closeSubmenu();
    setOpen(false);
    requestAnimationFrame(() => trigger?.focus());
  };

  const handleToggle = (e: React.MouseEvent) => {
    e.stopPropagation();
    regenSubmenu.closeSubmenu();
    setOpen((o) => !o);
  };

  const handleRegenPick = (providerId: string, providerLabel: string) => {
    regenSubmenu.closeSubmenu();
    closeAndReturnFocus();
    onPickRegenerate(providerId, providerLabel);
  };

  // Issue #814 — converged on the shared `useClickOutside` hook
  // (#492) for the outside-mousedown close path. The hook scopes by
  // `[data-dropdown-for="<menuId>"]` and `menuId` is per-instance
  // (one kebab per agent node), so two open kebabs on different
  // nodes don't interfere. Place `data-dropdown-for={menuId}` on the
  // menu root AND the regenerate submenu so clicks inside either
  // subtree count as "inside" (mirrors `NodeItem`'s parent+submenu
  // scoping, issue #814).
  useClickOutside<string>(open ? menuId : null, () => {
    regenSubmenu.closeSubmenu();
    setOpen(false);
  });

  // Outside click + Escape close. Same `document`-vs-`window` jsdom
  // caveat as `MeshItem` (issue #735): tests dispatch on `document`,
  // not `window`, because in jsdom the two are independent targets.
  // Submenu arrows delegate to the shared `useSubmenu` hook (same as
  // `NodeItem`); only the parent-menu traversal below is kebab-local.
  // The listener attaches once per open: submenu state lives in the
  // hook behind stable callbacks, so hovering the picker never churns
  // this subscription.
  useEffect(() => {
    if (!open) return;
    // Issue #1502 — skip disabled rows (the Regenerate trigger is
    // disabled when the mesh offers no providers). Focusing a disabled
    // button is a no-op, so a naive modulo walk would stall on the
    // disabled slot and break Arrow-wrap.
    const focusSibling = (currentIdx: number, dir: 1 | -1) => {
      for (let step = 1; step <= itemCount; step++) {
        const next = (currentIdx + dir * step + itemCount) % itemCount;
        const el = menuItemRefs.current[next];
        if (el && !el.hasAttribute('disabled')) {
          el.focus();
          return;
        }
      }
    };
    const onKeyDown = (e: KeyboardEvent) => {
      const menu = menuRef.current;
      const active = document.activeElement;
      const inMenu = menu && active instanceof Node && menu.contains(active);
      const inSubmenu = regenSubmenu.submenuContainsFocus();
      if (!inMenu && !inSubmenu) return;
      if (e.key === 'Escape') {
        e.preventDefault();
        closeAndReturnFocus();
        return;
      }
      if (e.key === 'Tab') {
        // Non-modal popover (matches MeshItem, #735): Tab leaves the
        // menu and closes it; the browser moves focus to the next
        // tabbable element naturally.
        regenSubmenu.closeSubmenu();
        setOpen(false);
        return;
      }
      if (e.key === 'ArrowRight' && inMenu && !inSubmenu) {
        if (active?.getAttribute('aria-haspopup') !== 'menu') return;
        e.preventDefault();
        regenSubmenu.openSubmenuViaKeyboard();
        return;
      }
      if (
        e.key === 'ArrowLeft' &&
        (inSubmenu || (inMenu && regenSubmenu.isSubmenuOpen()))
      ) {
        e.preventDefault();
        regenSubmenu.closeSubmenu();
        const trigger = menu?.querySelector<HTMLButtonElement>('button[aria-haspopup="menu"]');
        if (trigger) focusWithoutScroll(trigger);
        return;
      }
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        if (inSubmenu) {
          regenSubmenu.stepSubmenuFocus(1);
        } else {
          const items = menuItemRefs.current;
          const activeIdx = items.findIndex((el) => el === document.activeElement);
          if (activeIdx >= 0) focusSibling(activeIdx, 1);
        }
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        if (inSubmenu) {
          regenSubmenu.stepSubmenuFocus(-1);
        } else {
          const items = menuItemRefs.current;
          const activeIdx = items.findIndex((el) => el === document.activeElement);
          if (activeIdx >= 0) focusSibling(activeIdx, -1);
        }
        return;
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
    };
    // `itemCount` is constant while open (`canResume` can't flip
    // mid-menu); everything submenu-shaped comes from the hook's stable
    // callbacks — hence no `regenSubmenuOpen` dep and no listener churn
    // on hover.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, itemCount]);

  // Fixed-menu anchoring, viewport clamping, and scroll tracking are shared
  // with the PR pill so both title-bar menus follow the same positioning rules.
  useAnchoredPosition(triggerRef, menuRef, open, { align: 'end' });

  // Autofocus the first ENABLED menuitem on open (WAI-ARIA menu contract).
  // Issue #1502 — the first row is now Regenerate, which is disabled when
  // the mesh offers no providers (the common case in tests, where
  // `listProviders` isn't mocked and resolves to `[]`). Focusing a
  // disabled button is a no-op in browsers/jsdom, which would leave focus
  // outside the menu and break the Escape-to-close contract (the key
  // handler gates on focus-inside-menu). Skip disabled rows so Escape
  // always has a focused item to gate on.
  // Issue #1291 — `preventScroll: true` keeps Chromium from auto-scrolling
  // a flex/scroll ancestor to bring the focused item into view. The kebab
  // lives inside the GridNodeHeader row inside the splitter; without the
  // flag, opening the menu would visibly nudge the grid. Local-only —
  // `useSubmenu`'s `focusWithoutScroll` and the inline `useAriaMenu` both
  // have their own scroll behaviour (e.g. ProviderDropdown relies on the
  // scroll-into-view side effect), so we DON'T route this through them.
  useLayoutEffect(() => {
    if (!open) return;
    const firstEnabled = menuItemRefs.current.find((el) => el && !el.hasAttribute('disabled'));
    firstEnabled?.focus({ preventScroll: true });
  }, [open]);

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        onClick={handleToggle}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        aria-label="Agent node actions"
        title="Agent node actions"
        className="w-5 h-5 flex items-center justify-center rounded-md text-text-muted hover:text-text-primary hover:bg-bg-base transition-[color,background-color] opacity-0 group-hover:opacity-100 focus-visible:opacity-100"
      >
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <circle cx="12" cy="5" r="1.2" fill="currentColor" />
          <circle cx="12" cy="12" r="1.2" fill="currentColor" />
          <circle cx="12" cy="19" r="1.2" fill="currentColor" />
        </svg>
      </button>
      {open && createPortal(
        <div
          ref={menuRef}
          id={menuId}
          // Issue #814 — scoped attribute for `useClickOutside`. The
          // menu's per-instance `menuId` ensures sibling kebabs (one per
          // agent node in the grid) don't share the selector.
          data-dropdown-for={menuId}
          role="menu"
          aria-label="Agent node actions"
          className="fixed bg-bg-overlay border border-border-default rounded-md shadow-md animate-scale-in origin-top-right z-[100] py-1"
          style={{ top: 0, left: 0, minWidth: KEBAB_MIN_WIDTH }}
        >
          {/* Issue #1502 — Regenerate row (first, mirrors the sidebar
              context-menu order). Hover or ArrowRight/click opens the
              provider picker submenu pinned with `Current (<label>)` on
              top for in-place kick-start. The submenu opens to the LEFT
              (`right-full`) because the kebab itself hugs the header's
              right edge — opening to the right would overflow the
              viewport. Same `data-dropdown-for` scoping as the parent so
              `useClickOutside` treats both as "inside". */}
          <div
            role="presentation"
            className="relative"
            onMouseEnter={() => {
              if (!regenDisabled) regenSubmenu.setSubmenuOpen(true);
            }}
            onMouseLeave={() => regenSubmenu.closeSubmenu()}
          >
            <button
              ref={(el) => { menuItemRefs.current[0] = el; }}
              role="menuitem"
              aria-haspopup="menu"
              aria-expanded={regenSubmenu.submenuOpen}
              disabled={regenDisabled}
              onClick={() => {
                if (regenDisabled) return;
                regenSubmenu.setSubmenuOpen(true);
              }}
              title={
                isRegenerateDisabled
                  ? 'Regenerate unavailable while node is in this state'
                  : !hasRegenerateTargets
                    ? 'No providers are available on this mesh'
                    : 'Pick a Model Provider for this node (including current to kick-start)'
              }
              data-testid="grid-regenerate-trigger"
              className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:bg-transparent"
            >
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8" />
                <path d="M21 3v5h-5" />
                <path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16" />
                <path d="M3 21v-5h5" />
              </svg>
              Regenerate
              <span aria-hidden="true" className="ml-auto">▸</span>
            </button>
            {regenSubmenu.submenuOpen && (
              <div
                ref={regenSubmenu.submenuRef}
                role="menu"
                aria-label="Pick target provider"
                data-testid="grid-regenerate-submenu"
                data-dropdown-for={menuId}
                className="absolute right-full top-0 mr-1 min-w-[200px] bg-bg-overlay border border-border-default rounded-md shadow-md py-1 z-[101]"
                onMouseDown={(e) => e.stopPropagation()}
              >
                <RegenerateProviderMenu
                  providers={providerList}
                  currentProviderId={node.provider}
                  onPick={handleRegenPick}
                  submenuTestId="grid-regenerate-submenu"
                />
              </div>
            )}
          </div>
          <button
            ref={(el) => { menuItemRefs.current[1] = el; }}
            role="menuitem"
            onClick={(e) => { closeAndReturnFocus(); onOpenInExplorer(e); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <FolderOpenIcon className="w-3 h-3" />
            Open in file explorer
          </button>
          <button
            ref={(el) => { menuItemRefs.current[2] = el; }}
            role="menuitem"
            aria-pressed={isPinned}
            onClick={(e) => { closeAndReturnFocus(); onTogglePin(e); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <svg width="12" height="12" viewBox="0 0 24 24" fill={isPinned ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <path d="M12 17v5" fill="none" />
              <path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z" />
            </svg>
            {isPinned ? 'Unpin node' : 'Pin node'}
          </button>
          <button
            ref={(el) => { menuItemRefs.current[3] = el; }}
            role="menuitem"
            onClick={(e) => { closeAndReturnFocus(); onToggleSolo(e); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              {isSingleMode ? (
                <path d="M9 9H4m0 0V4m0 5 6-6m5 16v-5m0 0h5m-5 0 6 6M9 15H4m0 0v5m0-5 6 6m5-16V4m0 0h5m-5 0 6 6" />
              ) : (
                <path d="M15 3h6m0 0v6m0-6-7 7M9 21H3m0 0v-6m0 6 7-7" />
              )}
            </svg>
            {isSingleMode ? `Restore grid (${toggleShortcutHint})` : `Maximize (${toggleShortcutHint})`}
          </button>
          <button
            ref={(el) => { menuItemRefs.current[4] = el; }}
            role="menuitem"
            onClick={(e) => { closeAndReturnFocus(); onClose(e); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <span className="text-text-muted" aria-hidden="true">×</span>
            Close agent node
          </button>
          {canResume && (
            <button
              ref={(el) => { menuItemRefs.current[5] = el; }}
              role="menuitem"
              onClick={(e) => { closeAndReturnFocus(); onResume(e); }}
              data-testid="grid-resume-button"
              className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
            >
              <span className="text-accent-violet" aria-hidden="true">↻</span>
              Resume agent
            </button>
          )}
        </div>,
        document.body,
      )}
    </>
  );
}
