/**
 * Tests for `<SafeLink>` — issue #463.
 *
 * Pins the contracts that were duplicated across `GitIssuesTab`,
 * `GitPullRequestsTab`, and the Issues tab's blocked-by indicator:
 *
 *   1. Empty / missing URL: renders a plain `<span>` (no `<a>`).
 *      The Tauri 2 WebView self-navigates on bare `<a href="">`, so
 *      the span fallback is the safer default.
 *   2. Non-empty URL: renders an `<a>` with `href`, `target="_blank"`,
 *      `rel="noopener noreferrer"`, and a click handler that routes
 *      through `openUrl` (because Tauri 2 drops `target="_blank"`
 *      without the `core:webview:allow-create-webview-window`
 *      capability, which we don't grant — see
 *      `buildmesh-empty-url-frontend-guard`).
 *   3. Click on the link: `e.preventDefault()` (no DOM navigation),
 *      `e.stopPropagation()` (the row's expand toggle on the parent
 *      doesn't fire), and `openUrl(url)` is called exactly once with
 *      the link's URL.
 *   4. Right-click / keyboard activation: the `<a href>` is preserved
 *      for the browser-native affordances (right-click → "Open in
 *      browser", ⌘-click, screen readers, etc.) — `openUrl` is only
 *      invoked via the left-click onClick route.
 *   5. Empty URL + click: nothing navigates and `openUrl` is not
 *      called. Pins the empty-URL fallback against a future regression
 *      that re-introduces a bare `<a href="">`.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { SafeLink } from '../../src/components/shared/SafeLink';

// `@tauri-apps/plugin-opener`'s `openUrl` shells out to the OS to open
// an external URL. `vi.hoisted` so the mock factory can capture the
// spy ref before the `vi.mock` call hoists the module replacement.
const { openUrlMock } = vi.hoisted(() => ({
  openUrlMock: vi.fn<[], Promise<void>>().mockResolvedValue(undefined),
}));
vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: openUrlMock,
}));

describe('SafeLink (#463)', () => {
  beforeEach(() => {
    openUrlMock.mockReset();
    openUrlMock.mockResolvedValue(undefined);
  });

  // ---- Empty-URL fallback -----------------------------------------------

  it('renders plain text (no <a>) when url is empty', () => {
    render(<SafeLink url="" className="text-sm">Title</SafeLink>);
    // No <a> in the DOM at all — the empty-URL case must not render a
    // bare <a href=""> (Tauri 2 WebView self-navigation hazard).
    expect(screen.queryByRole('link')).toBeNull();
    // The children survive so callers that rely on the title text
    // (e.g. ProbeRow's issue title) still see something useful — the
    // span is inert, not absent.
    expect(screen.getByText('Title').tagName).toBe('SPAN');
  });

  it('does NOT forward title or aria-label to the empty-URL span', () => {
    // Regression guard: a non-clickable element must not advertise
    // itself as a link. Both `title` (hover tooltip — "this opens
    // GitHub") and `aria-label` (screen readers announce as a link
    // control) are link semantics that would mislead users into
    // clicking inert text. The pre-refactor code rendered a plain
    // `<span>` with neither attribute; SafeLink preserves that.
    render(
      <SafeLink
        url=""
        className="text-sm"
        title="Open on GitHub"
        ariaLabel="Open on GitHub"
      >
        Title
      </SafeLink>,
    );
    const span = screen.getByText('Title');
    expect(span.tagName).toBe('SPAN');
    expect(span.getAttribute('title')).toBeNull();
    expect(span.getAttribute('aria-label')).toBeNull();
  });

  it('does NOT style the empty-URL fallback as a clickable link', () => {
    // User-reported bug — "View on GitHub" appeared as cyan-with-hover
    // text in the Issues / PRs probe headers even when the underlying
    // URL didn't resolve. The fallback <span> used to inherit the
    // caller's link-styling classes verbatim, so the empty case
    // looked exactly like the working <a> (cyan text + hover brighten)
    // right next to working controls — clicking it did nothing
    // because the span has no `href` / `onClick`. The fix strips the
    // link-styling tokens (`text-accent-cyan`, `hover:text-accent-cyan/*`,
    // `transition-colors`, `cursor-pointer`) via `stripLinkStyling`
    // and replaces them with `cursor-default` so the span reads as
    // inert text. Layout tokens (text-2xs, font-medium, ml-1, etc.)
    // pass through so the row doesn't reflow. Pin the regex set so
    // adding a new link-styling token at a call site (without also
    // extending the strip) is caught here as a regression.
    render(
      <SafeLink
        url=""
        className="text-2xs font-medium text-accent-cyan hover:text-accent-cyan/80 transition-colors"
      >
        View on GitHub ↗
      </SafeLink>,
    );
    const fallback = screen.getByText('View on GitHub ↗');
    expect(fallback.tagName).toBe('SPAN');
    // Link-styling tokens are gone — the span no longer LOOKS clickable.
    expect(fallback.className).not.toMatch(/text-accent-cyan/);
    expect(fallback.className).not.toMatch(/cursor-pointer/);
    expect(fallback.className).not.toMatch(/hover:text-accent-cyan/);
    expect(fallback.className).not.toMatch(/transition-colors/);
    // And the inert cue is applied — cursor stays default so the
    // user doesn't get a pointer hand over text that does nothing.
    expect(fallback.className).toMatch(/cursor-default/);
    // Layout tokens survive — the row must NOT reflow when the URL
    // resolves from null to the real string.
    expect(fallback.className).toMatch(/text-2xs/);
    expect(fallback.className).toMatch(/font-medium/);
  });

  it('still renders an <a> when url is a non-empty placeholder string', () => {
    // The empty-URL guard is specifically `url === ''` — anything
    // non-empty (even a placeholder like "#" or "javascript:void(0)")
    // renders as a real link. The contract is narrow: callers are
    // responsible for not passing nonsense URLs. Pin the boundary so
    // a future widening (e.g. "treat '#' as empty") is intentional,
    // not accidental.
    render(<SafeLink url="#">Placeholder</SafeLink>);
    expect(screen.getByRole('link', { name: 'Placeholder' })).toBeTruthy();
  });

  // ---- Non-empty URL: the <a> contract ----------------------------------

  it('renders an anchor with href/target/rel attrs when url is set', () => {
    render(
      <SafeLink
        url="https://github.com/acme/demo/issues/101"
        className="text-sm"
      >
        Fix the wobble
      </SafeLink>,
    );
    const link = screen.getByRole('link', { name: 'Fix the wobble' });
    expect(link.tagName).toBe('A');
    expect(link.getAttribute('href')).toBe(
      'https://github.com/acme/demo/issues/101',
    );
    expect(link.getAttribute('target')).toBe('_blank');
    // The rel attribute must contain BOTH noopener and noreferrer
    // — they're separate tokens so a future regression that drops
    // either (e.g. leaves just "noopener") is caught here.
    const rel = link.getAttribute('rel') ?? '';
    expect(rel).toContain('noopener');
    expect(rel).toContain('noreferrer');
  });

  it('passes through className and title for styling + hover tooltip', () => {
    render(
      <SafeLink
        url="https://github.com/acme/demo/pull/201"
        className="text-sm text-text-primary hover:underline ml-1 truncate"
        title="Open on GitHub"
      >
        Add widget
      </SafeLink>,
    );
    const link = screen.getByRole('link', { name: 'Add widget' });
    expect(link.className).toContain('text-sm');
    expect(link.className).toContain('truncate');
    expect(link.getAttribute('title')).toBe('Open on GitHub');
  });

  it('wires ariaLabel for icon-only variants (e.g. the ↗ icon)', () => {
    // The icon variant of the link has no text children — only an icon
    // glyph. Screen readers need the aria-label to announce what the
    // link does. Mirrors the issues tab's `Open issue on GitHub` and
    // the PR tab's `Open pull request on GitHub` labels.
    render(
      <SafeLink
        url="https://github.com/acme/demo/issues/101"
        ariaLabel="Open issue on GitHub"
        className="text-text-muted"
      >
        ↗
      </SafeLink>,
    );
    const link = screen.getByLabelText('Open issue on GitHub');
    expect(link.tagName).toBe('A');
    expect(link.textContent).toBe('↗');
  });

  // ---- Click routing (the actual bug fix) -------------------------------

  it('calls openUrl with the link URL on left-click', async () => {
    render(
      <SafeLink url="https://github.com/acme/demo/issues/101">
        Fix the wobble
      </SafeLink>,
    );
    const link = screen.getByRole('link', { name: 'Fix the wobble' });
    await userEvent.click(link);

    expect(openUrlMock).toHaveBeenCalledTimes(1);
    expect(openUrlMock).toHaveBeenCalledWith(
      'https://github.com/acme/demo/issues/101',
    );
  });

  it('does not call openUrl when an empty-URL fallback is clicked', async () => {
    // The span has no onClick — clicking it must not invoke openUrl.
    // Pins the empty-URL guard end-to-end (DOM + click handler).
    render(<SafeLink url="">Title text</SafeLink>);
    const span = screen.getByText('Title text');
    await userEvent.click(span);

    expect(openUrlMock).not.toHaveBeenCalled();
  });

  it('stops click propagation so an outer row handler does not fire', () => {
    // The link lives inside a row whose onClick toggles expand. A
    // click on the link must NOT bubble to the row — clicking the
    // link navigates to GitHub, NOT toggles expand. We assert via a
    // spy on the parent click handler.
    const rowClickSpy = vi.fn();
    render(
      <div onClick={rowClickSpy}>
        <SafeLink url="https://github.com/acme/demo/issues/101">
          Fix the wobble
        </SafeLink>
      </div>,
    );
    const link = screen.getByRole('link', { name: 'Fix the wobble' });
    // Use fireEvent so we get the React synthetic event but can also
    // inspect propagation behaviour cleanly. userEvent also works but
    // fireEvent is more direct for "does this handler run" assertions.
    fireEvent.click(link);

    // openUrl routed (the link's onClick ran).
    expect(openUrlMock).toHaveBeenCalledTimes(1);
    // Parent's onClick did NOT fire — propagation was stopped.
    expect(rowClickSpy).not.toHaveBeenCalled();
  });

  it('prevents default so the WebView does not navigate on top of openUrl', () => {
    // jsdom doesn't navigate, but we can assert via `defaultPrevented`
    // that the onClick called preventDefault. Without it the link's
    // default action (DOM navigation to href) would compete with
    // openUrl — and in Tauri 2 the default action is silently dropped
    // anyway, but a future port to a real browser would regress.
    render(
      <SafeLink url="https://github.com/acme/demo/issues/101">
        Fix the wobble
      </SafeLink>,
    );
    const link = screen.getByRole('link', { name: 'Fix the wobble' });
    const event = new MouseEvent('click', { bubbles: true, cancelable: true });
    link.dispatchEvent(event);

    expect(event.defaultPrevented).toBe(true);
    expect(openUrlMock).toHaveBeenCalledTimes(1);
  });

  // ---- Multiple links coexist -------------------------------------------

  it('each link routes to its own URL when several are rendered together', () => {
    render(
      <div>
        <SafeLink url="https://github.com/acme/demo/issues/101">
          First
        </SafeLink>
        <SafeLink url="https://github.com/acme/demo/issues/102">
          Second
        </SafeLink>
      </div>,
    );
    fireEvent.click(screen.getByRole('link', { name: 'First' }));
    fireEvent.click(screen.getByRole('link', { name: 'Second' }));

    expect(openUrlMock).toHaveBeenCalledTimes(2);
    expect(openUrlMock).toHaveBeenNthCalledWith(
      1,
      'https://github.com/acme/demo/issues/101',
    );
    expect(openUrlMock).toHaveBeenNthCalledWith(
      2,
      'https://github.com/acme/demo/issues/102',
    );
  });
});