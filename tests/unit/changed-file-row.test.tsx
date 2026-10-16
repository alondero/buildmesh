/**
 * ChangedFileRow (#2406)
 *
 * The one changed-file row body shared by `ChangedFilesSection` (wrapped in a
 * clickable `<button>`) and `WorktreeCloseDialog` (wrapped in a plain
 * `<div>`). The call sites own the wrapper; this component owns the status
 * letter, the path, and the `+additions / -deletions` counts. It returns a
 * fragment, so the first test below is what keeps it wrapper-free: an element
 * here would change the rendered output of both lists.
 */

import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { ChangedFileRow } from '../../src/components/FileTree/ChangedFileRow';
import { fileDiffStatusMeta } from '../../src/lib/status';
import type { GitStatus } from '../../src/lib/tauri';

function renderRow(file: GitStatus) {
  return render(<ChangedFileRow file={file} meta={fileDiffStatusMeta(file.status)} />);
}

describe('ChangedFileRow (#2406)', () => {
  it('renders no wrapper element of its own', () => {
    const { container } = renderRow({
      path: 'src/app.ts',
      status: 'modified',
      additions: 13,
      deletions: 4,
    });

    // Four sibling spans, straight into whatever element the call site used —
    // the first child is the letter itself, not a wrapper this component owns.
    expect(container.firstElementChild?.tagName).toBe('SPAN');
    expect(container.childElementCount).toBe(4);
  });

  it('renders the path and the +additions / -deletions counts', () => {
    renderRow({ path: 'src/app.ts', status: 'modified', additions: 13, deletions: 4 });

    expect(screen.getByText('src/app.ts')).toBeTruthy();
    expect(screen.getByText('+13')).toBeTruthy();
    expect(screen.getByText('-4')).toBeTruthy();
  });

  it('takes the letter and colour from the fileDiffStatusMeta passed in', () => {
    renderRow({ path: 'README.md', status: 'added', additions: 7, deletions: 0 });

    const letter = screen.getByText('A');
    expect(letter.className).toContain('text-accent-green');
    // The tooltip is how the one-letter vocabulary stays readable.
    expect(letter.getAttribute('title')).toBe('Added');
  });

  it('reads an untracked file as "?" rather than a modified file', () => {
    renderRow({ path: 'notes.md', status: 'untracked', additions: 1, deletions: 0 });

    const letter = screen.getByText('?');
    expect(letter.className).toContain('text-text-muted');
    expect(letter.getAttribute('title')).toBe('Untracked');
  });
});
