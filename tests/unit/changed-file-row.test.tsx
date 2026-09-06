import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { ChangedFileRow } from '../../src/components/shared/ChangedFileRow';
import type { GitStatus } from '../../src/types/generated/GitStatus';

// Issue #791 — the inner row markup shared by `ChangedFilesSection` and
// `WorktreeCloseDialog`. The component renders only the four spans (status
// letter / path / `+additions` / `-deletions`) inside a Fragment; each caller
// owns the wrapper (button vs. div, hover, highlight, onClick). Tests render
// the row inside a `<ul>` so the four spans end up under a real element and
// `getByText` / container queries work the same way they will inside the
// production wrappers.

const BASE_FILE: GitStatus = {
  path: 'src/app.ts',
  status: 'modified',
  additions: 13,
  deletions: 4,
};

describe('ChangedFileRow', () => {
  it('renders the status letter, path, and +/- counts', () => {
    const { container } = render(
      <ul>
        <li>
          <ChangedFileRow file={BASE_FILE} />
        </li>
      </ul>,
    );

    const letterSpan = container.querySelector('span.font-bold')!;
    expect(letterSpan.textContent).toBe('M');
    expect(screen.getByText('src/app.ts')).toBeTruthy();
    expect(screen.getByText('+13')).toBeTruthy();
    expect(screen.getByText('-4')).toBeTruthy();
  });

  it('uses the meta color for known statuses', () => {
    const { container } = render(
      <ul>
        <li>
          <ChangedFileRow file={{ ...BASE_FILE, status: 'added' }} />
        </li>
      </ul>,
    );

    const letterSpan = container.querySelector('span.font-bold')!;
    expect(letterSpan.textContent).toBe('A');
    expect(letterSpan.className).toContain('text-accent-green');
    expect(letterSpan.getAttribute('title')).toBe('Added');
  });

  it.each([
    { status: 'added', letter: 'A', color: 'text-accent-green', label: 'Added' },
    { status: 'modified', letter: 'M', color: 'text-accent-amber', label: 'Modified' },
    { status: 'deleted', letter: 'D', color: 'text-accent-red', label: 'Deleted' },
    { status: 'renamed', letter: 'R', color: 'text-accent-violet', label: 'Renamed' },
    { status: 'untracked', letter: '?', color: 'text-text-muted', label: 'Untracked' },
  ])('maps "$status" to letter $letter / $color / "$label"', ({ status, letter, color, label }) => {
    const { container } = render(
      <ul>
        <li>
          <ChangedFileRow file={{ ...BASE_FILE, status }} />
        </li>
      </ul>,
    );

    const letterSpan = container.querySelector('span.font-bold')!;
    expect(letterSpan.textContent).toBe(letter);
    expect(letterSpan.className).toContain(color);
    expect(letterSpan.getAttribute('title')).toBe(label);
  });

  it('falls back to the modified meta for unknown statuses', () => {
    // The status vocabulary is closed in Rust, but the generated TS type widens
    // to `string`. A drift in the backend shouldn't render a blank badge —
    // `fileDiffStatusMeta` falls back to the modified row, and that contract
    // must be observable in the shared row.
    const { container } = render(
      <ul>
        <li>
          <ChangedFileRow file={{ ...BASE_FILE, status: 'submodule-modified' }} />
        </li>
      </ul>,
    );

    const letterSpan = container.querySelector('span.font-bold')!;
    expect(letterSpan.textContent).toBe('M');
    expect(letterSpan.className).toContain('text-accent-amber');
    expect(letterSpan.getAttribute('title')).toBe('Modified');
  });

  it('renders zero counts verbatim (no `+0` collapse)', () => {
    render(
      <ul>
        <li>
          <ChangedFileRow
            file={{ path: 'README.md', status: 'added', additions: 0, deletions: 0 }}
          />
        </li>
      </ul>,
    );

    // The caller may be showing a brand-new untracked file or an untouched
    // tracked file; both still want `+0` / `-0` so the row width stays
    // uniform with rows that have non-zero counts.
    expect(screen.getByText('+0')).toBeTruthy();
    expect(screen.getByText('-0')).toBeTruthy();
  });
});