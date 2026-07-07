/**
 * `<SafeLink>` — issue #463.
 *
 * External link with two contracts that the duplicated tab code did by
 * hand, and that bit us in production:
 *
 *   1. **Empty-URL fallback.** When `url` is the empty string, render
 *      the children as plain `<span>` text — never as a bare
 *      `<a href="">`. The Tauri 2 WebView self-navigates on bare empty
 *      hrefs (the WebView is a window, not a browser, and an empty
 *      link effectively means "go to the current URL"), so a partial
 *      GitHub response that left `html_url` blank would have caused
 *      the dock to navigate away mid-click. See
 *      `buildmesh-empty-url-frontend-guard`.
 *
 *   2. **Tauri 2 routing.** `target="_blank"` is silently dropped
 *      without the `core:webview:allow-create-webview-window`
 *      capability (which we don't grant in
 *      `src-tauri/capabilities/default.json`). The link's `onClick`
 *      calls `e.preventDefault()` to suppress the dead default action
 *      and `openUrl(url)` from `@tauri-apps/plugin-opener` to delegate
 *      to the OS, which knows how to open an external browser. The
 *      `<a href>` is preserved unchanged so right-click → "Open in
 *      browser", ⌘-click, and screen readers still work — `openUrl`
 *      is the *click* route, not the *only* route.
 *
 * Stop-propagation contract
 * -------------------------
 * The link always calls `e.stopPropagation()` in its onClick handler.
 * Every current call site sits inside a clickable container (the row's
 * expand-toggle on a parent `<div>`); without stopPropagation a left
 * click on the link would bubble up and flip the row's expand state on
 * top of navigating to GitHub. The cost for an un-embedded link (e.g.
 * a future footer that wants SafeLink without a row above it) is a
 * harmless no-op — stopPropagation on an event with no other listener
 * is free.
 *
 * Empty-URL span is INERT, not styled like a link
 * ------------------------------------------------
 * The fallback `<span>` keeps the caller's `children` (so e.g.
 * `ProbeRow` still shows the issue title as plain text when an issue
 * has no `url`) and the layout-relevant Tailwind tokens (text size,
 * font weight, padding, truncation, min-w-0/flex-1), but **strips**
 * the link-styling tokens (`text-accent-cyan`, `hover:underline`,
 * `hover:text-accent-cyan/*`, `transition-colors`, `cursor-pointer`)
 * and replaces them with `cursor-default` so the span reads as inert
 * text. Without this, every empty-URL fallback rendered an exact copy
 * of the working link's cyan-with-hover styling and clicking it did
 * nothing (the user-reported "links aren't links" regression on the
 * GitIssuesTab / GitPullRequestsTab probe headers). The class-name
 * strip is a deliberately narrow regex, not a CSS parser — adding
 * new link-styling tokens is opt-in via the regex below, so a future
 * visual regression shows up as either a missed strip (looks clickable)
 * or an over-strip (something layout-only gets removed and the row
 * reflows). The "does NOT style the empty-URL fallback as a clickable
 * link" test pins the narrow set above.
 *
 * `title` and `aria-label` are still deliberately OMITTED in the
 * fallback branch. Both are link semantics: `title` produces a "this
 * is a link to X" hover tooltip that would mislead users into
 * clicking non-clickable text; `aria-label` makes screen readers
 * announce the element as a control that opens GitHub when it
 * doesn't.
 */

import { type ReactNode } from 'react';
import { openUrl } from '@tauri-apps/plugin-opener';

export interface SafeLinkProps {
  /**
   * The external URL. The empty string (`''`) falls back to a
   * text-only `<span>` (children preserved, link-styling stripped —
   * see the "Empty-URL span is INERT" note in the file header). Any
   * non-empty value renders an `<a>` — callers are responsible for
   * not passing nonsense values like `'#'` or `'javascript:void(0)'`.
   */
  url: string;
  /** Link body. Text for the title variant, an icon glyph for the ↗ variant. */
  children: ReactNode;
  /**
   * Tailwind classes. Applied to the `<a>` as-is. In the empty-URL
   * `<span>` fallback, layout tokens are kept and link-styling tokens
   * (`text-accent-*`, `hover:text-accent-*`, `hover:underline`,
   * `transition-colors`, `cursor-pointer`) are dropped — see file
   * header.
   */
  className?: string;
  /** Accessible name for icon-only variants. Strongly recommended when children is not text. */
  ariaLabel?: string;
  /** Hover tooltip (also picked up by some screen readers). */
  title?: string;
}

/**
 * Drop the visual markers that signal "this is a link" from a
 * caller's `className`. Layout tokens (text-sm, ml-1, truncate,
 * min-w-0, flex-1, font-medium, etc.) pass through unchanged so the
 * span doesn't reflow when the URL resolves.
 *
 * Keep this regex in sync with the "does NOT style the empty-URL
 * fallback as a clickable link" test in `safe-link.test.tsx` — a new
 * link-styling token added at a call site that's NOT in the regex
 * will silently make the fallback look clickable again (the user-
 * reported "links aren't links" regression we're fixing here).
 */
function stripLinkStyling(className: string | undefined): string {
  if (!className) return '';
  return className
    .split(/\s+/)
    .filter((token) => !/^(text-accent-|hover:text-accent-|hover:underline|transition-colors|cursor-pointer)/.test(token))
    .join(' ');
}

export function SafeLink({ url, children, className, ariaLabel, title }: SafeLinkProps) {
  if (url === '') {
    // File header documents why the className is stripped; the
    // children are preserved so ProbeRow's empty-url titles still
    // read as plain text instead of vanishing entirely.
    return (
      <span className={`${stripLinkStyling(className)} cursor-default`.trim()}>
        {children}
      </span>
    );
  }

  return (
    <a
      href={url}
      target="_blank"
      rel="noopener noreferrer"
      aria-label={ariaLabel}
      title={title}
      className={className}
      onClick={(e) => {
        // `preventDefault` stops the dead `<a href>` default action
        // (Tauri 2's WebView drops `target="_blank"` without an explicit
        // capability, so the default would either silently fail or —
        // on a future port to a real browser — actually navigate,
        // competing with `openUrl`). `stopPropagation` keeps the link
        // click out of the row's expand-toggle handler on the parent
        // div — see file header for the "always stopPropagation"
        // rationale.
        e.preventDefault();
        e.stopPropagation();
        openUrl(url).catch(console.error);
      }}
    >
      {children}
    </a>
  );
}
