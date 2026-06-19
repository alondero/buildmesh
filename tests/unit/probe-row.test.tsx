/**
 * Tests for `<ProbeRow>` — issue #463.
 *
 * The row body was duplicated 80%+ between `GitIssuesTab` and
 * `GitPullRequestsTab` after PRs #459 and #461 added click-to-expand
 * + title hyperlinks to both tabs. `<ProbeRow>` is the shared
 * abstraction; this file pins the invariants that were duplicated.
 *
 * The five contracts that MUST hold (sourced from PRs #459/#461 +
 * memory `buildmesh-empty-url-frontend-guard`):
 *
 *   1. Click-to-expand: clicking the body text toggles the row's
 *      expanded state. The chevron rotates 90° when expanded.
 *   2. Title renders as a link (delegated to `<SafeLink>` — noopener /
 *      noreferrer / target="_blank" / openUrl routing). Right-click +
 *      keyboard activation still work because the `<a href>` is
 *      preserved.
 *   3. Clicking the link does NOT toggle expand — the link's
 *      `e.stopPropagation()` keeps the row handler out of the way.
 *   4. Empty URL: title renders as plain text, no `<a>`, no ↗ icon.
 *   5. Empty body: the clamped/expanded body region is not rendered
 *      at all (no empty `<p class="line-clamp-2"></p>`).
 *
 * Plus the slot contract (rightSlot / belowSlot) so the two tabs can
 * keep their tab-specific actions (split spawn + blocked-by for
 * Issues; merge + spawn + view changes + error rows for PRs) without
 * ProbeRow knowing about them.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ProbeRow } from '../../src/components/Probe/ProbeRow';

const { openUrlMock } = vi.hoisted(() => ({
  openUrlMock: vi.fn<[], Promise<void>>().mockResolvedValue(undefined),
}));
vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: openUrlMock,
}));

const noop = () => {};

describe('ProbeRow (#463)', () => {
  beforeEach(() => {
    openUrlMock.mockReset();
    openUrlMock.mockResolvedValue(undefined);
  });

  // ---- Title link delegation (delegates to SafeLink) -------------------

  it('renders the title as an anchor with the issue URL', () => {
    render(
      <ProbeRow
        dataAttr="issue"
        rowKey={101}
        number={101}
        title="Fix the wobble"
        url="https://github.com/acme/demo/issues/101"
        iconAriaLabel="Open issue on GitHub"
        isExpanded={false}
        onToggle={noop}
        body="The widget wobbles under load."
      />,
    );

    const link = screen.getByRole('link', { name: 'Fix the wobble' });
    expect(link.getAttribute('href')).toBe(
      'https://github.com/acme/demo/issues/101',
    );
    expect(link.getAttribute('target')).toBe('_blank');
    const rel = link.getAttribute('rel') ?? '';
    expect(rel).toContain('noopener');
    expect(rel).toContain('noreferrer');
  });

  it('renders the number prefix as a styled span', () => {
    // `#NN` is the cyan font-mono affordance. Pin it so a future
    // refactor that drops the number doesn't silently lose it.
    render(
      <ProbeRow
        dataAttr="issue"
        rowKey={101}
        number={101}
        title="Fix the wobble"
        url=""
        iconAriaLabel="Open issue on GitHub"
        isExpanded={false}
        onToggle={noop}
      />,
    );

    const numSpan = screen.getByText('#101');
    expect(numSpan.tagName).toBe('SPAN');
    expect(numSpan.className).toContain('text-accent-cyan');
    expect(numSpan.className).toContain('font-mono');
  });

  it('renders the ↗ icon with the tab-specific aria label', () => {
    // The icon is the discoverability hint next to the title — same
    // target, same routing, but with a tab-specific accessible name
    // ("Open issue on GitHub" vs "Open pull request on GitHub").
    render(
      <ProbeRow
        dataAttr="pr"
        rowKey={201}
        number={201}
        title="Add widget"
        url="https://github.com/acme/demo/pull/201"
        iconAriaLabel="Open pull request on GitHub"
        isExpanded={false}
        onToggle={noop}
      />,
    );

    const iconLink = screen.getByLabelText('Open pull request on GitHub');
    expect(iconLink.tagName).toBe('A');
    expect(iconLink.textContent).toBe('↗');
  });

  // ---- Empty-URL fallback (delegates to SafeLink) ----------------------

  it('renders the title as plain text (no <a>, no ↗) when url is empty', () => {
    render(
      <ProbeRow
        dataAttr="issue"
        rowKey={200}
        number={200}
        title="Mystery issue"
        url=""
        iconAriaLabel="Open issue on GitHub"
        isExpanded={false}
        onToggle={noop}
      />,
    );

    const title = screen.getByText('Mystery issue');
    expect(title.closest('a')).toBeNull();
    // The ↗ icon is hidden too — both anchors are gated on url.
    expect(screen.queryByLabelText('Open issue on GitHub')).toBeNull();
  });

  // ---- Expand toggle (the load-bearing interaction) --------------------

  it('calls onToggle when the body region is clicked', () => {
    const onToggle = vi.fn();
    render(
      <ProbeRow
        dataAttr="issue"
        rowKey={101}
        number={101}
        title="Fix the wobble"
        url="https://github.com/acme/demo/issues/101"
        iconAriaLabel="Open issue on GitHub"
        isExpanded={false}
        onToggle={onToggle}
        body="The widget wobbles under load."
      />,
    );

    const body = screen.getByText('The widget wobbles under load.');
    fireEvent.click(body);
    expect(onToggle).toHaveBeenCalledTimes(1);
  });

  it('does NOT call onToggle when the title link is clicked', () => {
    // The link's stopPropagation keeps the row handler out of the way.
    // Without it, a left-click on the title would both navigate to
    // GitHub AND toggle the row's expand state — both fire on click.
    const onToggle = vi.fn();
    render(
      <ProbeRow
        dataAttr="issue"
        rowKey={101}
        number={101}
        title="Fix the wobble"
        url="https://github.com/acme/demo/issues/101"
        iconAriaLabel="Open issue on GitHub"
        isExpanded={false}
        onToggle={onToggle}
        body="The widget wobbles under load."
      />,
    );

    const link = screen.getByRole('link', { name: 'Fix the wobble' });
    fireEvent.click(link);

    expect(onToggle).not.toHaveBeenCalled();
    // The link did its routing job.
    expect(openUrlMock).toHaveBeenCalledTimes(1);
  });

  it('does NOT call onToggle when the ↗ icon is clicked', () => {
    // Same contract as the title link — the icon is just a smaller
    // affordance for the same target.
    const onToggle = vi.fn();
    render(
      <ProbeRow
        dataAttr="pr"
        rowKey={201}
        number={201}
        title="Add widget"
        url="https://github.com/acme/demo/pull/201"
        iconAriaLabel="Open pull request on GitHub"
        isExpanded={false}
        onToggle={onToggle}
      />,
    );

    const iconLink = screen.getByLabelText('Open pull request on GitHub');
    fireEvent.click(iconLink);

    expect(onToggle).not.toHaveBeenCalled();
    expect(openUrlMock).toHaveBeenCalledTimes(1);
  });

  // ---- Chevron rotation + body state ----------------------------------

  it('does not rotate the chevron when collapsed', () => {
    const { container } = render(
      <ProbeRow
        dataAttr="issue"
        rowKey={101}
        number={101}
        title="Fix the wobble"
        url=""
        iconAriaLabel="Open issue on GitHub"
        isExpanded={false}
        onToggle={noop}
        body="The widget wobbles under load."
      />,
    );

    // The chevron is the only span with `transition-transform` — that's
    // how the original code styled it, and we keep the same selector
    // signature so any future CSS regression surfaces here.
    const chevron = container.querySelector('span.transition-transform')!;
    expect(chevron.className).not.toContain('rotate-90');
    expect(chevron.textContent).toBe('▸');
  });

  it('rotates the chevron 90° when expanded', () => {
    const { container } = render(
      <ProbeRow
        dataAttr="issue"
        rowKey={101}
        number={101}
        title="Fix the wobble"
        url=""
        iconAriaLabel="Open issue on GitHub"
        isExpanded={true}
        onToggle={noop}
        body="The widget wobbles under load."
      />,
    );

    const chevron = container.querySelector('span.transition-transform')!;
    expect(chevron.className).toContain('rotate-90');
  });

  it('renders the body as a clamped paragraph when collapsed', () => {
    const { container } = render(
      <ProbeRow
        dataAttr="issue"
        rowKey={101}
        number={101}
        title="Fix the wobble"
        url=""
        iconAriaLabel="Open issue on GitHub"
        isExpanded={false}
        onToggle={noop}
        body="The widget wobbles under load."
      />,
    );

    const clampedBody = container.querySelector('p.line-clamp-2');
    expect(clampedBody).toBeTruthy();
    expect(clampedBody!.textContent).toBe('The widget wobbles under load.');
    // No scrollable container when collapsed.
    expect(container.querySelector('div.max-h-48')).toBeNull();
  });

  it('renders the body as a scrollable container with the data-* attribute when expanded', () => {
    // The `data-issue-body-expanded` / `data-pr-body-expanded` attribute
    // is used by existing tests + downstream CSS hooks. Pin both the
    // prefix (driven by `dataAttr`) and the data attribute name.
    const { container } = render(
      <ProbeRow
        dataAttr="pr"
        rowKey={201}
        number={201}
        title="Add widget"
        url=""
        iconAriaLabel="Open pull request on GitHub"
        isExpanded={true}
        onToggle={noop}
        body="Adds the widget."
      />,
    );

    const expandedBody = container.querySelector('[data-pr-body-expanded]');
    expect(expandedBody).toBeTruthy();
    expect(expandedBody!.className).toContain('max-h-48');
    expect(expandedBody!.textContent).toBe('Adds the widget.');
    // The clamped paragraph is gone.
    expect(container.querySelector('p.line-clamp-2')).toBeNull();
  });

  // ---- Empty body -----------------------------------------------------

  it('does not render any body region when body is empty', () => {
    // No `<p>` or expanded container — the row is just the title line.
    // Pin against a future regression that renders an empty
    // `<p class="line-clamp-2"></p>`.
    const { container } = render(
      <ProbeRow
        dataAttr="issue"
        rowKey={102}
        number={102}
        title="Add a /v2 endpoint"
        url=""
        iconAriaLabel="Open issue on GitHub"
        isExpanded={false}
        onToggle={noop}
        body={null}
      />,
    );

    expect(container.querySelector('p.line-clamp-2')).toBeNull();
    expect(container.querySelector('div.max-h-48')).toBeNull();
  });

  it('does not render any body region when body is the empty string', () => {
    // Same as null — Rust serde default = "" for empty optional strings.
    const { container } = render(
      <ProbeRow
        dataAttr="issue"
        rowKey={103}
        number={103}
        title="No body"
        url=""
        iconAriaLabel="Open issue on GitHub"
        isExpanded={true}
        onToggle={noop}
        body=""
      />,
    );

    expect(container.querySelector('p.line-clamp-2')).toBeNull();
    expect(container.querySelector('div.max-h-48')).toBeNull();
  });

  // ---- Data attribute -------------------------------------------------

  it('emits the data-attribute prefix as ${dataAttr}-row={rowKey}', () => {
    // The row is identified in tests by `[data-X-row]` — pin the format
    // exactly so a future typo (e.g. `data-row-key`) breaks the test
    // loud rather than silently breaking the tests.
    const { container } = render(
      <ProbeRow
        dataAttr="issue"
        rowKey={101}
        number={101}
        title="Fix the wobble"
        url=""
        iconAriaLabel="Open issue on GitHub"
        isExpanded={false}
        onToggle={noop}
      />,
    );
    const row = container.querySelector('[data-issue-row]')!;
    expect(row).toBeTruthy();
    expect(row.getAttribute('data-issue-row')).toBe('101');
  });

  it('uses the supplied dataAttr for the body-expanded selector too', () => {
    // Symmetric contract — `data-pr-body-expanded` for PRs,
    // `data-issue-body-expanded` for issues. Existing tests query
    // these selectors verbatim.
    const { container: prContainer } = render(
      <ProbeRow
        dataAttr="pr"
        rowKey={201}
        number={201}
        title="Add widget"
        url=""
        iconAriaLabel="Open pull request on GitHub"
        isExpanded={true}
        onToggle={noop}
        body="Adds the widget."
      />,
    );
    expect(prContainer.querySelector('[data-pr-body-expanded]')).toBeTruthy();

    const { container: issueContainer } = render(
      <ProbeRow
        dataAttr="issue"
        rowKey={101}
        number={101}
        title="Fix the wobble"
        url=""
        iconAriaLabel="Open issue on GitHub"
        isExpanded={true}
        onToggle={noop}
        body="The widget wobbles under load."
      />,
    );
    expect(issueContainer.querySelector('[data-issue-body-expanded]')).toBeTruthy();
  });

  // ---- Slots -----------------------------------------------------------

  it('renders the rightSlot as a sibling of the clickable column', () => {
    // The slot is opaque — the rightSlot content is a sibling of the
    // left clickable column, not inside it. Clicking the slot's
    // button does NOT trigger the row's onToggle. Pin via a known
    // selector inside the rightSlot.
    const onToggle = vi.fn();
    render(
      <ProbeRow
        dataAttr="issue"
        rowKey={101}
        number={101}
        title="Fix the wobble"
        url=""
        iconAriaLabel="Open issue on GitHub"
        isExpanded={false}
        onToggle={onToggle}
        rightSlot={<button>Spawn</button>}
      />,
    );

    const button = screen.getByRole('button', { name: 'Spawn' });
    // The button is NOT inside the clickable column. Find the row, then
    // find the clickable column, then assert the button is outside.
    const row = button.closest('[data-issue-row]')!;
    const clickableColumn = row.querySelector('.cursor-pointer')!;
    expect(clickableColumn.contains(button)).toBe(false);
    expect(row.contains(button)).toBe(true);
  });

  it('renders the belowSlot as a child of the row, below the title row', () => {
    // Used by the PR tab for inline merge-error / spawn-error rows.
    const { container } = render(
      <ProbeRow
        dataAttr="pr"
        rowKey={201}
        number={201}
        title="Add widget"
        url=""
        iconAriaLabel="Open pull request on GitHub"
        isExpanded={false}
        onToggle={noop}
        belowSlot={<p data-pr-error>Merge failed: conflicts</p>}
      />,
    );

    const error = container.querySelector('[data-pr-error]')!;
    expect(error).toBeTruthy();
    // Below the title row, not inside it — the title row is the inner
    // `flex items-start` div.
    const row = container.querySelector('[data-pr-row]')!;
    const titleRow = row.firstElementChild!;
    expect(titleRow.contains(error)).toBe(false);
    expect(row.contains(error)).toBe(true);
  });
});