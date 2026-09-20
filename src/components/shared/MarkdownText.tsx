/**
 * `<MarkdownText>` — GitHub-Flavored-Markdown renderer for untrusted issue /
 * PR bodies (Probe dock).
 *
 * The Issues and PRs probe rows hand their bodies to `<ProbeRow>`, and the
 * expanded ("toggled") body container renders them through this component so
 * a body written as markdown reads as markdown (headings, lists, code,
 * links) instead of showing raw `**asterisks**` and ```fences```. The
 * collapsed 2-line preview deliberately stays plain text — see ProbeRow.
 *
 * Safety contract
 * ---------------
 * Issue/PR bodies are untrusted content (any repo collaborator can write
 * one), so the component is safe by construction rather than by filtering:
 *
 *   - react-markdown parses to an AST and renders React elements — there is
 *     no `dangerouslySetInnerHTML` anywhere on this path, so script/event-
 *     handler injection cannot happen by construction.
 *   - Raw HTML in the source (`<img onerror=…>`, `<script>`) is NOT rendered
 *     as HTML — react-markdown drops it (no `rehype-raw` is configured) and
 *     the literal text survives into the output, pinned by a test.
 *   - Link/URL hrefs go through react-markdown's default `urlTransform`,
 *     which only permits `http`, `https`, `mailto`, `xmpp`, `irc`… and
 *     strips everything else — `javascript:` URLs never reach an `<a>`.
 *
 * Link routing
 * ------------
 * The `a` element is overridden to render `<SafeLink>`, so markdown links
 * inherit the app-wide external-link contracts (issue #463): clicks route
 * through `openUrl` (the Tauri 2 WebView drops `target="_blank"` without a
 * capability we don't grant), and the handler calls `stopPropagation` — the
 * expanded body lives inside the row's click-to-toggle column, so without
 * that a link click would also flip the row's expand state.
 *
 * Typography
 * ----------
 * Sized for the probe dock's `text-2xs` body (see
 * `docs/development/probe-ui-checklist.md` §2 — 240px is the width to design
 * for): headings stay at body size with weight/colour doing the work,
 * margins are 4px-scale, code is `font-mono` on a pill, fenced blocks scroll
 * horizontally instead of widening the dock, and tables shrink to fit.
 * Everything is expressed as Tailwind child-variants on the wrapper so the
 * app's token utilities (`text-text-secondary`, `border-border-subtle`, …)
 * flip with the theme, matching how `CircuitsProbeTab` scopes child styles.
 */

import { memo, type AnchorHTMLAttributes } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { SafeLink } from './SafeLink';

export interface MarkdownTextProps {
  /** Raw markdown source (exactly what the GitHub API `body` field carries). */
  source: string;
  /** Extra classes for the wrapper div (layout/spacing only). */
  className?: string;
}

export const MarkdownText = memo(function MarkdownText({ source, className }: MarkdownTextProps) {
  return (
    <div
      className={
        // Block rhythm: compact margins between blocks, no leading/trailing
        // gap against the container's own padding.
        'space-y-1 [&>*:first-child]:mt-0 [&>*:last-child]:mb-0 ' +
        // Headings — same font size as body (probe dock density), weight +
        // primary colour carry the hierarchy. GFM `#`-6 levels all map here.
        '[&_h1]:text-2xs [&_h1]:font-semibold [&_h1]:text-text-primary [&_h1]:leading-snug ' +
        '[&_h2]:text-2xs [&_h2]:font-semibold [&_h2]:text-text-primary [&_h2]:leading-snug ' +
        '[&_h3]:text-2xs [&_h3]:font-semibold [&_h3]:text-text-primary [&_h3]:leading-snug ' +
        '[&_h4]:text-2xs [&_h4]:font-semibold [&_h4]:text-text-secondary [&_h4]:leading-snug ' +
        '[&_h5]:text-2xs [&_h5]:font-semibold [&_h5]:text-text-secondary [&_h5]:leading-snug ' +
        '[&_h6]:text-2xs [&_h6]:font-medium [&_h6]:text-text-muted [&_h6]:leading-snug ' +
        // Lists — disc/decimal markers inside the container's padding; nested
        // lists indent naturally via the same pl-4 rule.
        '[&_ul]:list-disc [&_ul]:pl-4 [&_ol]:list-decimal [&_ol]:pl-4 [&_li]:my-0.5 ' +
        // Task lists (remark-gfm) — the checkbox input is disabled by
        // react-markdown; give it the accent so checked/unchecked reads.
        '[&_li>input[type=checkbox]]:accent-accent-cyan [&_li>input[type=checkbox]]:mr-1 ' +
        // Inline code pill; fenced blocks (`pre > code`) strip the pill and
        // inherit the pre's own treatment below.
        '[&_code]:font-mono [&_code]:bg-bg-card [&_code]:border [&_code]:border-border-subtle ' +
        '[&_code]:rounded-sm [&_code]:px-1 [&_code]:py-px [&_pre>code]:bg-transparent ' +
        '[&_pre>code]:border-0 [&_pre>code]:p-0 ' +
        // Fenced code blocks — horizontal scroll is contained to the block so
        // a long line can't widen the dock (checklist §1: overflow-x must be
        // deliberate), and long unspaced tokens still wrap inside it.
        '[&_pre]:overflow-x-auto [&_pre]:bg-bg-card [&_pre]:border [&_pre]:border-border-subtle ' +
        '[&_pre]:rounded-sm [&_pre]:p-1.5 [&_pre]:break-words [&_pre]:whitespace-pre-wrap ' +
        // Blockquote — the classic left-bar treatment.
        '[&_blockquote]:border-l-2 [&_blockquote]:border-border-strong [&_blockquote]:pl-2 ' +
        '[&_blockquote]:text-text-muted ' +
        // Tables (remark-gfm) — shrink to the container, hairline row rules,
        // top-aligned so multi-line cells read.
        '[&_table]:w-full [&_table]:border-collapse [&_th]:border-b [&_th]:border-border-default ' +
        '[&_th]:px-1 [&_th]:py-0.5 [&_th]:text-left [&_th]:font-semibold [&_th]:text-text-secondary ' +
        '[&_td]:border-b [&_td]:border-border-subtle [&_td]:px-1 [&_td]:py-0.5 [&_td]:align-top ' +
        // HR + images.
        '[&_hr]:border-border-default [&_img]:max-w-full ' +
        (className ?? '')
      }
    >
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          // Route markdown links through SafeLink (openUrl + stopPropagation;
          // empty-href edge case renders the inert span). Class mimics the
          // app's cyan link affordance on the small body text.
          // Props are annotated explicitly (not left to contextual
          // inference) so `noImplicitAny` stays satisfied here even when
          // the `components` prop's contextual type is unavailable.
          a: ({ children, href }: AnchorHTMLAttributes<HTMLAnchorElement>) => (
            <SafeLink
              url={href ?? ''}
              className="text-accent-cyan hover:underline break-all"
            >
              {children}
            </SafeLink>
          ),
        }}
      >
        {source}
      </ReactMarkdown>
    </div>
  );
});
