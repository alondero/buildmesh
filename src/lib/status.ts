import type { FileDiffStatus } from './tauri';
import type { AgentNode } from '../types/generated/AgentNode';
import type { LifecycleChangedPayload } from '../types/generated/LifecycleChangedPayload';
import type { SignalHealth } from '../types/generated/SignalHealth';

/**
 * The circle drawn for a node status by `NodeStatusGlyph`. `dot` is the
 * legacy text glyph (● ○ ✓ etc.) still consumed by the mobile SPA
 * and a couple of the desktop rows that need a coloured character rather
 * than a coloured SVG circle.
 */
export type StatusGlyphShape = 'solid' | 'ring' | 'dashed' | 'slash' | 'half' | 'target' | 'thin' | 'cross';

// `hex` mirrors the resolved value of each entry's Tailwind `color` token
// (see the `--color-*` custom properties in `src/App.css`) as a literal
// hex string. `color`/`bgColor` are the Tailwind classes consumed by desktop
// surfaces that still draw a coloured fill (e.g. the mesh summary row's dot
// in `Sidebar/MeshItem.tsx`). `dot` is the text glyph rendered by the mobile
// SPA (`src/mobile/`) via inline styles; the mobile SPA doesn't run Tailwind.
// One status vocabulary for every spawn/status surface (issue #815).
export const STATUS_CONFIG = {
  // Stage-2 in progress; visually pulses so the user sees liveness.
  pending: {
    color: 'text-text-muted animate-pulse-fast',
    bgColor: 'bg-text-muted animate-pulse-fast',
    dot: '◌',
    glyph: 'dashed',
    label: 'Starting…',
    hex: '#7a8492',
  },

  // Issue #654 — agent launched but the 3s early-exit window hasn't elapsed.
  // Visually mirrors `pending`; conditional promotion to Running fires next.
  spawning: {
    color: 'text-text-muted animate-pulse-fast',
    bgColor: 'bg-text-muted animate-pulse-fast',
    dot: '◌',
    glyph: 'dashed',
    label: 'Starting…',
    hex: '#7a8492',
  },
  running: {
    color: 'status-running',
    bgColor: 'bg-status-running',
    dot: '●',
    glyph: 'solid',
    label: 'Running',
    hex: '#00d4ff',
  },

  idle: {
    color: 'status-idle',
    bgColor: 'bg-status-idle',
    dot: '○',
    glyph: 'ring',
    label: 'Idle',
    // Same cyan as `running` — desktop distinguishes idle/running by the
    // dot glyph (○ vs ●) and label, not color. Intentional; `.status-idle`
    // in App.css maps to `--color-status-idle`, and the equality with
    // `--color-status-running` (the foreground AND -bg variant) is
    // pinned by the "status token layering (#741)" contract tests in
    // tests/unit/theme-tokens*.test.ts. That's the source of truth;
    // update both tokens together when retuning (round-3, #741).
    hex: '#00d4ff',
  },
  awaiting_input: {
    color: 'status-waiting animate-pulse-fast',
    bgColor: 'bg-status-warning animate-pulse-fast',
    dot: '●',
    glyph: 'target',
    label: 'Needs attention',
    hex: '#f59e0b',
  },

  error: {
    color: 'status-error',
    bgColor: 'bg-status-error',
    dot: '✗',
    glyph: 'cross',
    label: 'Error',
    hex: '#ef4444',
  },

  // Issue #1793 — the reaper's terminal state for a circuit-piloted node that
  // stayed running with no session identity and no readable report. Not the
  // same as `error` (nothing was ever observed), so it gets distinct copy on
  // the same red token, with a hollow glyph.
  lost: {
    color: 'status-error',
    bgColor: 'bg-status-error',
    dot: '⊘',
    glyph: 'slash',
    label: 'Lost',
    hex: '#ef4444',
  },
  suspended: {
    color: 'text-violet',
    bgColor: 'bg-accent-violet',
    dot: '⏸',
    glyph: 'half',
    label: 'Suspended',
    hex: '#8b5cf6',
  },
  // Issue #485 — a Circuit-managed node whose wrap-up finished (clean worktree,
  // branch pushed, PR opened). Terminal state; green mirrors the diff
  // "added" accent used elsewhere for success.
  completed: {
    color: 'text-accent-green',
    bgColor: 'bg-accent-green',
    dot: '✓',
    glyph: 'target',
    label: 'PR opened',
    hex: '#22c55e',
  },
  // Issue #1364 — an ordinary turn finished cleanly and the agent is at its
  // prompt, ready for another prompt. The user is NOT needed (unlike
  // awaiting_input) and this is NOT Circuit's PR-opened terminal state
  // (completed). Green ✓ but distinct copy: "Ready", never "PR opened".
  ready: {
    color: 'text-accent-green',
    bgColor: 'bg-accent-green',
    dot: '✓',
    glyph: 'target',
    label: 'Ready',
    // Same green as `completed` — both render `text-accent-green` on
    // desktop, so the mobile hex mirrors that token (#22c55e), not a
    // second green.
    hex: '#22c55e',
  },
  // Issue #788 — an archived node is historical, not actionable work.
  // Muted grey keeps it distinct from live idle/running nodes in the
  // desktop sidebar; the Archive probe tab remains the home for these rows.
  archived: {
    color: 'text-text-muted',
    bgColor: 'bg-text-muted',
    dot: '◌',
    glyph: 'thin',
    label: 'Archived',
    hex: '#7a8492',
  },
} as const;

export function getStatusConfig(status: string | undefined | null) {
  if (!status) return STATUS_CONFIG.idle;
  return STATUS_CONFIG[status as keyof typeof STATUS_CONFIG] || STATUS_CONFIG.idle;
}

/**
 * Store fields an `agent-lifecycle` event may write.
 *
 * `process_running` is the post-spawn early-exit promotion. The list refetch
 * that follows `node-spawn-completed` can still observe `spawning`, and this
 * event is the snapshot that was stored with the `running` write. Clients
 * adopt that status and snapshot. They do not copy its signal health onto
 * the node: the snapshot reports the row's current health, and writing that
 * back would turn an unknown column into the unverified tooltip.
 *
 * `cleared_session_id` (issue #2137) drops the identity from the client copy.
 * `resolveSpawnAgentIntent` turns any non-empty `cli_session_id` into a
 * `--resume <id>` request, so a stale copy would keep asking the backend to
 * resume an id it has just discarded. The backend answers that with a fresh
 * launch rather than an error, but the client would still be unable to resume
 * its real session until the next refetch.
 */
export function lifecycleNodePatch(
  payload: LifecycleChangedPayload,
): Partial<Pick<AgentNode, 'status' | 'lifecycle' | 'signal_health' | 'cli_session_id'>> {
  const identity = payload.cleared_session_id ? { cli_session_id: null } : {};
  if (payload.kind === 'process_running') {
    return { ...identity, status: payload.status, lifecycle: payload };
  }
  if (payload.kind === 'signal_unavailable') {
    return { ...identity, signal_health: payload.signal_health };
  }
  return {
    ...identity,
    status: payload.status,
    lifecycle: payload,
    ...(payload.signal_health ? { signal_health: payload.signal_health } : {}),
  };
}

/** One vocabulary for live events and reconnect snapshots on both clients. */
export function getNodeStatusConfig(node: Pick<AgentNode, 'status' | 'lifecycle' | 'signal_health'>) {
  const config = getStatusConfig(node.status);
  const observation = node.lifecycle;
  // Absent unless there is something to add: a node with no signal-health note
  // keeps its exact previous title, so the clause never becomes stray punctuation.
  const suffix = signalHealthNote(node.signal_health);
  const note = suffix ? `. ${suffix}` : '';
  if (!observation || observation.status !== node.status) return { ...config, title: `${config.label}${note}` };
  const labels: Partial<Record<typeof observation.kind, string>> = {
    background_running: 'Waiting for background work',
    question_requested: 'Needs an answer',
    permission_requested: 'Needs permission',
  };
  const label = labels[observation.kind] ?? config.label;
  return { ...config, label, title: `${label}. Last observed ${observation.timestamp}${observation.provider_event ? ` (${observation.provider_event})` : ''}${note}` };
}

/**
 * Whether a node's status reporting has a problem the user can act on.
 *
 * `unverified` is deliberately excluded: it records that hooks are installed but
 * this process has not delivered an event yet, which is the normal state of a
 * healthy session between turns. Painting it as a warning put an unactionable
 * amber glyph on every node. It stays available in the status tooltip instead.
 */
export function isSignalHealthProblem(health: SignalHealth | null | undefined): health is 'degraded' | 'unavailable' {
  return health === 'degraded' || health === 'unavailable';
}

/** Tooltip clause describing signal health, or undefined when there is nothing to say. */
export function signalHealthNote(health: SignalHealth | null | undefined): string | undefined {
  if (health === 'degraded') return 'A status signal arrived but could not be interpreted; check the terminal for current activity.';
  if (health === 'unavailable') return 'No status signal is reaching Buildmesh; watch the terminal directly.';
  if (health === 'unverified') return 'Status reporting is not confirmed yet; the terminal is the source of truth.';
  return undefined;
}

export function nodeInputContext(node: Pick<AgentNode, 'status' | 'lifecycle'>): string | undefined {
  if (node.status !== 'awaiting_input' || node.lifecycle?.status !== node.status) return undefined;
  return node.lifecycle.semantic_turn?.description ?? node.lifecycle.message ?? undefined;
}

// ---------------------------------------------------------------------------
// Input request semantics (issue #1966)
// ---------------------------------------------------------------------------
//
// An `awaiting_input` node is blocked on *something*, and the normalized
// lifecycle kind says what. Only a `permission_requested` observation is
// evidence for a yes/no decision: sending `y`/`n` at a question is a guess
// about a harness prompt Buildmesh never read, and a rejected guess reads to
// the user as "the agent refused my answer". An unobserved or unclassifiable
// request is the same case with even less evidence, so it gets the same
// treatment.

/** How a node's pending input request should be answered. */
export type InputRequestMode =
  /** The harness asked for a tool-approval decision. */
  | 'permission'
  /** The harness asked a question, with or without an answer list. */
  | 'question'
  /** The harness yielded for input without saying what it needs. */
  | 'unknown';

export interface NodeInputRequest {
  /**
   * Identity of *this* request, not of the node. A node that asks a second
   * question while still `awaiting_input` gets a new key, so action state
   * recorded against the previous request cannot disable the new one. Falls
   * back to a constant for unobserved requests — those are also un-actionable,
   * so nothing to carry over either way.
   */
  key: string;
  mode: InputRequestMode;
  /** What the harness asked, when the observation carried a description. */
  message?: string;
  /**
   * Answers the harness enumerated, in harness order. Empty unless the
   * observation carried a request schema, so callers can never render a
   * choice list they inferred from prose.
   */
  choices: string[];
}

/**
 * Describe the input a node is blocked on, or `undefined` when it is not
 * blocked on user input at all.
 */
export function nodeInputRequest(
  node: Pick<AgentNode, 'status' | 'lifecycle'>,
): NodeInputRequest | undefined {
  if (node.status !== 'awaiting_input') return undefined;
  // A snapshot only describes the current status revision; the DB read drops
  // it otherwise, and a stale kind must not decide today's reply controls.
  const observed = node.lifecycle?.status === node.status ? node.lifecycle : undefined;
  const choices = observed?.request?.choices ?? [];
  const mode: InputRequestMode =
    observed?.kind === 'permission_requested'
      ? 'permission'
      : observed?.kind === 'question_requested'
        ? 'question'
        : 'unknown';
  return {
    key: observed ? `${observed.kind}@${observed.timestamp}` : 'unobserved',
    mode,
    message: observed?.semantic_turn?.description ?? observed?.message ?? undefined,
    // Only a question can carry a choice list. A permission decision that
    // somehow arrives with one is a yes/no prompt, not a menu.
    choices: mode === 'question' ? choices : [],
  };
}

// ---------------------------------------------------------------------------
// File-diff status meta (issue #725). The badge "A/M/D/R/? + coloured letter"
// shown on a file card / tree row was duplicated three ways before this
// consolidation: `<Diff>` owned the table, `ChangedFilesSection` held two
// parallel maps (statusColors / statusPrefix), and `PrDiffView` had its own
// minimal copy. The shared review surface, the file tree, and the PR list
// all read from this single source of truth.
// ---------------------------------------------------------------------------

export interface FileDiffStatusMeta {
  letter: string;
  label: string;
  /** Tailwind text colour token for the badge. */
  color: string;
  /** Literal hex mirror of `color` (the `--color-accent-*` /
      `--color-text-muted` tokens in src/App.css) for surfaces that render
      inline styles instead of Tailwind — the mobile diff badge reads these
      so both platforms share the one vocabulary. */
  hex: string;
  /** Translucent chip fill for the same badge: the canonical 15% accent
      wash (see DIFF_LINE_BG in components/Diff/Diff.tsx), or a solid
      surface for the hue-less untracked state. */
  hexBg: string;
}

// Mirrors `FileDiffStatus` in `lib/tauri.ts` (which is the hand-typed
// closed-set alias; the generated `FileDiff.status` is the wider `string`
// — see ADR-0009). Unknown statuses fall back to the `modified` row so a
// drifted vocabulary doesn't render blank badges.
const FILE_DIFF_STATUS_META: Record<FileDiffStatus, FileDiffStatusMeta> = {
  added: { letter: 'A', label: 'Added', color: 'text-accent-green', hex: '#22c55e', hexBg: 'rgba(34, 197, 94, 0.15)' },
  modified: { letter: 'M', label: 'Modified', color: 'text-accent-amber', hex: '#f59e0b', hexBg: 'rgba(245, 158, 11, 0.15)' },
  deleted: { letter: 'D', label: 'Deleted', color: 'text-accent-red', hex: '#ef4444', hexBg: 'rgba(239, 68, 68, 0.15)' },
  renamed: { letter: 'R', label: 'Renamed', color: 'text-accent-violet', hex: '#8b5cf6', hexBg: 'rgba(139, 92, 246, 0.15)' },
  untracked: { letter: '?', label: 'Untracked', color: 'text-text-muted', hex: '#7a8492', hexBg: '#1b1b23' },
};

export function fileDiffStatusMeta(status: string): FileDiffStatusMeta {
  const raw = (status || '').trim().toLowerCase();
  const key: FileDiffStatus =
    raw === 'a' || raw === 'added'
      ? 'added'
      : raw === 'd' || raw === 'deleted'
      ? 'deleted'
      : raw === 'r' || raw === 'renamed'
      ? 'renamed'
      : raw === '?' || raw === 'u' || raw === 'untracked'
      ? 'untracked'
      : raw === 'm' || raw === 'modified'
      ? 'modified'
      : (raw as FileDiffStatus);
  return (
    FILE_DIFF_STATUS_META[key] ?? FILE_DIFF_STATUS_META.modified
  );
}

export function needsAgentAttention(status: AgentNode['status']): boolean {
  return ['error', 'lost', 'awaiting_input', 'suspended'].includes(status);
}
