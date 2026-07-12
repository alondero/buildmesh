/**
 * Lucide folder-open (24×24, stroke-based). Lifted from `PathHeader`
 * because three call sites now render the same glyph: `PathHeader`
 * (Probe Panel), `WorktreeManagerTab` (per-worktree rows), and
 * `GridNodeHeader` (agent node title bar).
 *
 * Single-purpose presentational primitive — no click handling, no
 * aria-label inheritance, no Tailwind state classes. Each parent
 * composes the icon inside its own button so semantics stay
 * width/context-agnostic.
 */
export function FolderOpenIcon({ className }: { className?: string }) {
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
      <path d="M6 14l1.45-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.55 6A2 2 0 0 1 18.39 19H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.93a2 2 0 0 1 1.66.9l.82 1.2a2 2 0 0 0 1.66.9H18a2 2 0 0 1 2 2v2" />
    </svg>
  );
}
