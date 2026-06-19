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
 * `<span>` fallback styling
 * -------------------------
 * The span uses the same `className` as the `<a>`, so callers can pass
 * a single class string and both renders look identical when the URL
 * is missing — important for layout stability (the title row doesn't
 * reflow when the URL arrives late).
 */

import { type ReactNode } from 'react';
import { openUrl } from '@tauri-apps/plugin-opener';

export interface SafeLinkProps {
  /**
   * The external URL. The empty string (`''`) falls back to a `<span>`
   * (no link). Anything non-empty renders an `<a>` — callers are
   * responsible for not passing nonsense values like `'#'` or
   * `'javascript:void(0)'`.
   */
  url: string;
  /** Link body. Text for the title variant, an icon glyph for the ↗ variant. */
  children: ReactNode;
  /**
   * Tailwind classes. Applied to both the `<a>` (URL set) and the
   * fallback `<span>` (empty URL) so layout is identical between the
   * two cases.
   */
  className?: string;
  /** Accessible name for icon-only variants. Strongly recommended when children is not text. */
  ariaLabel?: string;
  /** Hover tooltip (also picked up by some screen readers). */
  title?: string;
}

export function SafeLink({ url, children, className, ariaLabel, title }: SafeLinkProps) {
  if (url === '') {
    // Empty-URL guard — see file header. The `<span>` carries the same
    // className as the `<a>` so callers can rely on identical layout
    // whether or not the URL is set. No `href`, no click handler — a
    // future regression that adds `onClick={openUrl(url)}` here would
    // be caught by the "does not call openUrl when empty-URL fallback
    // is clicked" test.
    //
    // `title` and `aria-label` are deliberately OMITTED in this branch.
    // Both are link semantics: `title` produces a "this is a link to X"
    // hover tooltip that would mislead users into clicking non-clickable
    // text; `aria-label` makes screen readers announce the element as
    // a control that opens GitHub when it doesn't. The pre-refactor
    // code rendered a plain `<span>` with neither attribute — preserved
    // here so an empty-URL title looks like inert text, not a dead link.
    return <span className={className}>{children}</span>;
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