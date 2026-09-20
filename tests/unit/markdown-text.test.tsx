/**
 * Tests for `<MarkdownText>` — the GFM renderer behind the Probe row
 * bodies' expanded ("toggled") container.
 *
 * The component exists so issue/PR bodies read as markdown instead of raw
 * `**asterisks**` + fenced-source text. Issue bodies are UNTRUSTED content
 * (any repo collaborator can write one), so alongside the rendering
 * features this file pins the safety contract:
 *
 *   1. GFM features render as elements (headings, emphasis, lists, tables,
 *      task lists, fenced code) rather than literal source characters.
 *   2. Links route through `<SafeLink>` — a click calls `openUrl` (the
 *      app's Tauri external-link contract) and nothing navigates inline.
 *   3. Raw HTML is NOT rendered as HTML (react-markdown drops it without
 *      `rehype-raw`); the literal text survives as inert content.
 *   4. `javascript:` URLs never reach an `<a>` (default `urlTransform`).
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { MarkdownText } from '../../src/components/shared/MarkdownText';

const { openUrlMock } = vi.hoisted(() => ({
  openUrlMock: vi.fn<[], Promise<void>>().mockResolvedValue(undefined),
}));
vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: openUrlMock,
}));

describe('MarkdownText', () => {
  beforeEach(() => {
    openUrlMock.mockReset();
    openUrlMock.mockResolvedValue(undefined);
  });

  // ---- GFM rendering ----------------------------------------------------

  it('renders emphasis as elements, not literal asterisks', () => {
    const { container } = render(
      <MarkdownText source="The **widget** wobbles and *sometimes* jiggles." />,
    );

    expect(container.querySelector('strong')?.textContent).toBe('widget');
    expect(container.querySelector('em')?.textContent).toBe('sometimes');
    expect(container.textContent).not.toContain('**');
  });

  it('renders ATX headings as heading elements', () => {
    const { container } = render(<MarkdownText source="## Steps to reproduce" />);

    const heading = container.querySelector('h2');
    expect(heading).toBeTruthy();
    expect(heading!.textContent).toBe('Steps to reproduce');
  });

  it('renders bullet lists as list elements', () => {
    const { container } = render(
      <MarkdownText source={'- one\n- two\n- three'} />,
    );

    expect(container.querySelector('ul')).toBeTruthy();
    expect(container.querySelectorAll('li')).toHaveLength(3);
  });

  it('renders fenced code blocks as pre/code', () => {
    const { container } = render(
      <MarkdownText source={'```ts\nconst x = 1;\n```'} />,
    );

    expect(container.querySelector('pre')).toBeTruthy();
    // The fence keeps its trailing newline — assert the code content, not
    // the whitespace.
    expect(container.querySelector('pre code')?.textContent?.replace(/\n$/, '')).toBe('const x = 1;');
  });

  it('renders inline code as a code element', () => {
    const { container } = render(
      <MarkdownText source={'run `npm test` first'} />,
    );

    expect(container.querySelector('code')?.textContent).toBe('npm test');
  });

  it('renders GFM tables as table elements', () => {
    const { container } = render(
      <MarkdownText source={'| a | b |\n| - | - |\n| 1 | 2 |'} />,
    );

    expect(container.querySelector('table')).toBeTruthy();
    expect(container.querySelectorAll('th')).toHaveLength(2);
    expect(container.querySelectorAll('td')).toHaveLength(2);
  });

  it('renders GFM task lists as disabled checkboxes', () => {
    const { container } = render(
      <MarkdownText source={'- [x] done\n- [ ] todo'} />,
    );

    const checkbox = container.querySelector('input[type="checkbox"]');
    expect(checkbox).toBeTruthy();
    // Disabled so the rendered preview is never interactive — the check
    // state belongs to GitHub, not this viewer.
    expect((checkbox as HTMLInputElement).disabled).toBe(true);
  });

  it('renders blockquotes as blockquote elements', () => {
    const { container } = render(
      <MarkdownText source={'> quoted wisdom'} />,
    );

    expect(container.querySelector('blockquote')?.textContent).toContain('quoted wisdom');
  });

  // ---- Link routing (SafeLink contract) ---------------------------------

  it('renders markdown links as anchors with the href preserved', () => {
    render(
      <MarkdownText source={'[buildmesh](https://github.com/alondero/buildmesh)'} />,
    );

    const link = screen.getByRole('link', { name: 'buildmesh' });
    expect(link.getAttribute('href')).toBe('https://github.com/alondero/buildmesh');
  });

  it('routes link clicks through openUrl (SafeLink contract)', async () => {
    render(
      <MarkdownText source={'[buildmesh](https://github.com/alondero/buildmesh)'} />,
    );

    fireEvent.click(screen.getByRole('link', { name: 'buildmesh' }));
    // Clicks delegate to the OS browser — the Tauri WebView drops
    // target="_blank", so direct navigation is never the path.
    await vi.waitFor(() => {
      expect(openUrlMock).toHaveBeenCalledWith('https://github.com/alondero/buildmesh');
    });
  });

  it('does not call openUrl for a plain-text body (no links)', () => {
    render(<MarkdownText source="just some words" />);

    fireEvent.click(screen.getByText('just some words'));
    expect(openUrlMock).not.toHaveBeenCalled();
  });

  // ---- Safety: raw HTML + dangerous URLs --------------------------------

  it('does not render raw HTML as HTML', () => {
    // The classic issue-body injection attempt. Without rehype-raw,
    // react-markdown drops the tag entirely — the literal source text may
    // survive as inert text, but no <img> element may reach the DOM.
    const { container } = render(
      <MarkdownText source={'<img src=x onerror="alert(1)">hello'} />,
    );

    expect(container.querySelector('img')).toBeNull();
    expect(container.querySelector('script')).toBeNull();
    expect(container.textContent).toContain('hello');
  });

  it('never emits an anchor for a javascript: URL', () => {
    const { container } = render(
      <MarkdownText source={'[click me](javascript:alert(1))'} />,
    );

    // react-markdown's default urlTransform strips the scheme, so the
    // SafeLink fallback renders the label as inert text — never a live
    // javascript: href.
    const anchor = container.querySelector('a[href^="javascript:"]');
    expect(anchor).toBeNull();
  });

  it('renders the empty string as nothing', () => {
    const { container } = render(<MarkdownText source="" />);

    expect(container.textContent).toBe('');
  });
});
