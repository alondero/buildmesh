import { describe, it, expect } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { Diff, FileDiffCard } from '../../src/components/Diff/Diff';
import type { FileDiff } from '../../src/lib/tauri';

function file(partial: Partial<FileDiff> & { path: string }): FileDiff {
  return {
    hunks: [],
    status: 'modified',
    old_path: null,
    additions: 0,
    deletions: 0,
    binary: false,
    ...partial,
  };
}

const textHunk = (content: string) => ({
  old_start: 1,
  old_lines: 0,
  new_start: 1,
  new_lines: 1,
  old_highlighted: '',
  new_highlighted: '',
  lines: [
    { line_type: 'add' as const, content, old_num: null, new_num: 1 },
  ],
  lines_highlighted: [`<span>${content}</span>`],
});

describe('Diff component', () => {
  it('shows the correct status letter per file (not always "M")', () => {
    render(
      <Diff
        files={[
          file({ path: 'a.ts', status: 'added' }),
          file({ path: 'b.ts', status: 'deleted' }),
          file({ path: 'c.ts', status: 'renamed', old_path: 'old.ts' }),
        ]}
      />
    );
    expect(screen.getByTitle('Added').textContent).toBe('A');
    expect(screen.getByTitle('Deleted').textContent).toBe('D');
    expect(screen.getByTitle('Renamed').textContent).toBe('R');
  });

  it('renders the rename source → target', () => {
    render(
      <FileDiffCard
        file={file({ path: 'new/name.ts', status: 'renamed', old_path: 'old/name.ts' })}
      />
    );
    expect(screen.getByText(/old\/name\.ts/)).toBeTruthy();
  });

  it('shows a placeholder for binary files instead of bytes', () => {
    render(<FileDiffCard file={file({ path: 'logo.png', status: 'added', binary: true })} />);
    expect(screen.getByText('Binary file not shown')).toBeTruthy();
  });

  it('collapses and expands a file body', () => {
    render(<FileDiffCard file={file({ path: 'x.ts', hunks: [textHunk('hello')] })} />);
    expect(screen.getByText('hello')).toBeTruthy();
    // The toggle is the card header button.
    fireEvent.click(screen.getByTitle('Modified'));
    expect(screen.queryByText('hello')).toBeNull();
  });

  it('renders syntax-highlighted line HTML when provided', () => {
    const { container } = render(
      <FileDiffCard file={file({ path: 'x.ts', hunks: [textHunk('const y = 1')] })} />
    );
    // The highlighted span from lines_highlighted is injected as HTML into the
    // code column (`.whitespace-pre`).
    expect(
      container.querySelector('.whitespace-pre span')?.textContent
    ).toContain('const y = 1');
  });
});
