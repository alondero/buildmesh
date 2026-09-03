/**
 * probeIcons — the Probe dock's SVG icon set.
 *
 * The activity bar and dock header used to carry per-tab emoji glyphs
 * (📁 🔍 ⏱ 🌳 ⚙️ 🐙 🔀 🕒 📝). Emoji render with platform-dependent
 * artwork (and several fall back to monochrome glyphs on Windows), which
 * clashed with the app's Lucide-style stroke iconography and design
 * tokens. This module replaces them with one consistent SVG family:
 *
 *   - 24×24 viewBox, `fill="none"`, `stroke="currentColor"` — the icon
 *     inherits colour from the surrounding text class (muted at rest,
 *     accent-cyan when active), so light/dark themes need no per-icon
 *     work.
 *   - 1.75 stroke width + round caps/joins — the same idiom as the
 *     inline icons in `GitPullRequestsTab` and `FolderOpenIcon`.
 *   - Every icon takes a `className` for sizing (`w-4 h-4` in the dock
 *     header, `w-[18px] h-[18px]` in the rail).
 *
 * Also home to the two probe-shell empty-state glyphs (`CompassIcon`,
 * `SearchIcon`) so `Spinner.tsx`'s `EmptyState` can stay emoji-free on
 * the probe's own surfaces.
 */

import type { ComponentType } from 'react';
import type { ProbeTab } from '../../stores/uiStore';

export type ProbeIcon = ComponentType<{ className?: string }>;

interface IconProps {
  className?: string;
}

/** Shared SVG wrapper — pins the stroke idiom so each glyph is just its
 *  path data. */
function Svg({ className, children }: IconProps & { children: React.ReactNode }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.75"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      {children}
    </svg>
  );
}

/** Lucide `folder` — Project Files. */
export function FilesIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />
    </Svg>
  );
}

/** Lucide `git-compare-arrows` — Agent Changes (diff review). */
export function ReviewIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <circle cx="5" cy="6" r="3" />
      <path d="M12 6h5a2 2 0 0 1 2 2v7" />
      <path d="m15 9-3-3 3-3" />
      <circle cx="19" cy="18" r="3" />
      <path d="M12 18H7a2 2 0 0 1-2-2V9" />
      <path d="m9 15 3 3-3 3" />
    </Svg>
  );
}

/** Lucide `gauge` — Usage meters. */
export function UsageIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <path d="m12 14 4-4" />
      <path d="M3.34 19a10 10 0 1 1 17.32 0" />
    </Svg>
  );
}

/** Lucide `git-fork` — Worktree Manager (branches + linked worktrees). */
export function WorktreesIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <circle cx="12" cy="18" r="3" />
      <circle cx="6" cy="6" r="3" />
      <circle cx="18" cy="6" r="3" />
      <path d="M18 9v2c0 .6-.4 1-1 1H7c-.6 0-1-.4-1-1V9" />
      <path d="M12 12v3" />
    </Svg>
  );
}

/** Lucide `settings` — Mesh Properties. */
export function PropertiesIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
      <circle cx="12" cy="12" r="3" />
    </Svg>
  );
}

/** Lucide `circle-dot` — Git Issues (GitHub's issue glyph). */
export function IssuesIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <circle cx="12" cy="12" r="10" />
      <circle cx="12" cy="12" r="1" />
    </Svg>
  );
}

/** Lucide `git-pull-request` — Pull Requests. */
export function PullsIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <circle cx="18" cy="18" r="3" />
      <circle cx="6" cy="6" r="3" />
      <path d="M13 6h3a2 2 0 0 1 2 2v7" />
      <line x1="6" x2="6" y1="9" y2="21" />
    </Svg>
  );
}

/** Lucide `archive` — Archive (discovered/archived agent nodes). */
export function ArchiveIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <rect width="20" height="5" x="2" y="3" rx="1" />
      <path d="M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8" />
      <path d="M10 12h4" />
    </Svg>
  );
}

/** Lucide `square-pen` — Scratch Pad. */
export function NotesIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <path d="M12 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7" />
      <path d="M18.375 2.625a1 1 0 0 1 3 3l-9.013 9.014a2 2 0 0 1-.853.505l-2.873.84a.5.5 0 0 1-.62-.62l.84-2.873a2 2 0 0 1 .506-.852z" />
    </Svg>
  );
}

/** Lucide `compass` — "No project selected" shell empty state. */
export function CompassIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <circle cx="12" cy="12" r="10" />
      <polygon points="16.24 7.76 14.12 14.12 7.76 16.24 9.88 9.88 16.24 7.76" />
    </Svg>
  );
}

/** Lucide `repeat` — Autopilot (wayfinder #990 ticket #994). Reads as the
 *  loop the destination doc names for this tab; also advertises the
 *  generic Autopilot surface that the mode toggle inside the tab
 *  switches between issue-driven and looping. */
export function AutopilotIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <path d="m17 2 4 4-4 4" />
      <path d="M3 11v-1a4 4 0 0 1 4-4h14" />
      <path d="m7 22-4-4 4-4" />
      <path d="M21 13v1a4 4 0 0 1-4 4H3" />
    </Svg>
  );
}

/** Lucide `workflow` — Autopilot Circuits (spec #1205). Reads as the
 *  trigger-action graph the circuit blueprint serialises. */
export function CircuitsIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <rect width="8" height="8" x="3" y="3" rx="2" />
      <path d="M7 11v4a2 2 0 0 0 2 2h4" />
      <rect width="8" height="8" x="13" y="13" rx="2" />
    </Svg>
  );
}

/** Lucide `search` — "No active agent node" shell empty state. */
export function SearchIcon({ className }: IconProps) {
  return (
    <Svg className={className}>
      <circle cx="11" cy="11" r="8" />
      <path d="m21 21-4.3-4.3" />
    </Svg>
  );
}

/**
 * One icon per inspector destination. The map lives here (not in
 * `ProbePanel`) so other surfaces — e.g. the command palette's tool
 * discovery start screen — can render the same destination glyphs without
 * importing the full inspector body and its tab implementations.
 */
export const PROBE_TAB_ICONS: Record<ProbeTab, ProbeIcon> = {
  files: FilesIcon,
  review: ReviewIcon,
  usage: UsageIcon,
  worktrees: WorktreesIcon,
  properties: PropertiesIcon,
  autopilot: AutopilotIcon,
  circuits: CircuitsIcon,
  issues: IssuesIcon,
  pulls: PullsIcon,
  sessions: ArchiveIcon,
  scratchpad: NotesIcon,
};
