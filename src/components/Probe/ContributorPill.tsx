/**
 * `<ContributorPill>` — the `@login` chip on the Issues / PRs probe
 * rows.
 *
 * Renders next to the row's metadata chips (issue labels / PR branch
 * ref) and opens the contributor's GitHub profile
 * (`https://github.com/<login>`) via `<SafeLink>` — so the click routes
 * through `openUrl` (Tauri 2 drops `target="_blank"` without a
 * capability we don't grant) and stopPropagation keeps it out of the
 * row's expand-toggle.
 *
 * Returns `null` for an empty login (partial GitHub responses degrade
 * `author` to `""` — see the `GitHubIssue` / `GitHubPullRequest` wire
 * docs), so a row with no known author shows nothing rather than a
 * dead link to `https://github.com/`.
 */

import { SafeLink } from '../shared/SafeLink';

interface ContributorPillProps {
  /** GitHub login of the issue / PR author, without the `@`. */
  login: string;
}

export function ContributorPill({ login }: ContributorPillProps) {
  if (login === '') return null;
  return (
    <SafeLink
      url={`https://github.com/${login}`}
      ariaLabel={`Open ${login}'s GitHub profile`}
      title={`@${login} on GitHub`}
      className="inline-flex items-center gap-1 rounded-md border border-border-subtle bg-bg-card px-1.5 py-px text-2xs text-text-secondary hover:text-accent-cyan hover:border-accent-cyan/40 transition-colors"
    >
      {/* Person glyph — matches the 9px icon idiom the branch chip uses. */}
      <svg
        width="9"
        height="9"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden="true"
        className="shrink-0"
      >
        <path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2" />
        <circle cx="12" cy="7" r="4" />
      </svg>
      <span className="max-w-[120px] truncate">@{login}</span>
    </SafeLink>
  );
}
