/**
 * FileTree keyboard a11y — WAI-ARIA tree contract (issue #728).
 *
 * The tree is rendered as a flat list of `[role="treeitem"]` rows with
 * roving tabIndex and a delegated keydown handler on the container
 * (`role="tree"`). These tests pin the contract at the component level:
 *
 *   - role + aria-label on the container, role + aria-level on every row,
 *     aria-expanded on folders only (not leaves).
 *   - One row at a time is in the tab order (`tabIndex={0}`); the rest are
 *     `tabIndex={-1}`.
 *   - ArrowDown / ArrowUp move the active row down / up.
 *   - ArrowRight expands a collapsed folder, or descends to the first child
 *     of an already-expanded folder.
 *   - ArrowLeft collapses an expanded folder, or ascends to the parent of
 *     a leaf / collapsed folder.
 *   - Home / End jump to the first / last visible treeitem.
 *   - Enter (and Space) activate the row's primary action: toggle expand
 *     on folders, fire `onUnchangedFileSelect` on files.
 *
 * Out of scope (covered elsewhere):
 *   - Git-status reconciliation: tests/unit/file-tree-git-status.test.tsx
 *   - Typeahead (`a-z` jumps to next row whose name starts with that key):
 *     not in the WAI-ARIA Authoring Practices "Required" list, deferred.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { FileTree } from '../../src/components/FileTree/FileTree';
import type { FileNode } from '../../src/lib/tauri';

// Tree shape — three top-level rows, two folders with children, one file.
// `git_status` returns `[]` so every file is "unchanged" — Enter must fire
// `onUnchangedFileSelect` (not `onChangedFileSelect`).
const TREE: FileNode = {
  name: 'repo',
  path: '/repo',
  is_dir: true,
  children: [
    {
      name: 'src',
      path: '/repo/src',
      is_dir: true,
      children: [
        { name: 'app.ts', path: '/repo/src/app.ts', is_dir: false, children: [] },
        { name: 'index.ts', path: '/repo/src/index.ts', is_dir: false, children: [] },
      ],
    },
    {
      name: 'docs',
      path: '/repo/docs',
      is_dir: true,
      children: [
        { name: 'README.md', path: '/repo/docs/README.md', is_dir: false, children: [] },
      ],
    },
    { name: 'LICENSE', path: '/repo/LICENSE', is_dir: false, children: [] },
  ],
};

function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'list_directory') return Promise.resolve(TREE);
    if (cmd === 'get_git_status') return Promise.resolve([]);
    if (cmd === 'diff_file_against_head') return Promise.resolve({ files: [] });
    return Promise.resolve({});
  });
}

const noop = () => {};

/** Read a string attribute via the underlying DOM node. */
function attr(el: Element, name: string): string | null {
  return el.getAttribute(name);
}

/** Get every `[role="treeitem"]` in the document (re-queries each call so
 *  post-keypress re-renders are visible). */
function treeItems(): HTMLElement[] {
  return Array.from(screen.getAllByRole('treeitem'));
}

describe('FileTree keyboard a11y — WAI-ARIA tree contract', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    mockBackend();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('renders role="tree" with an aria-label summarising the repo path', async () => {
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={false}
        onUnchangedFileSelect={noop}
        selectedFile={null}
        onFileSelect={noop}
      />
    );
    const tree = await screen.findByRole('tree');
    expect(attr(tree, 'aria-label')).toBe('Files in /repo');
  });

  it('renders each row as role="treeitem" with aria-level and aria-expanded on folders', async () => {
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={false}
        onUnchangedFileSelect={noop}
        selectedFile={null}
        onFileSelect={noop}
      />
    );
    await screen.findByRole('tree');
    const items = treeItems();
    // Three top-level rows before any folder is expanded.
    expect(items).toHaveLength(3);
    expect(attr(items[0], 'aria-level')).toBe('1');
    expect(attr(items[1], 'aria-level')).toBe('1');
    expect(attr(items[2], 'aria-level')).toBe('1');
    // Folders advertise expand state.
    expect(attr(items[0], 'aria-expanded')).toBe('false');
    expect(attr(items[1], 'aria-expanded')).toBe('false');
    // Files do NOT carry aria-expanded — invalid on leaf treeitems.
    expect(items[2].hasAttribute('aria-expanded')).toBe(false);
  });

  it('uses roving tabIndex: exactly one row has tabIndex=0', async () => {
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={false}
        onUnchangedFileSelect={noop}
        selectedFile={null}
        onFileSelect={noop}
      />
    );
    await screen.findByRole('tree');
    const items = treeItems();
    const tabbable = items.filter((el) => el.tabIndex === 0);
    const skipped = items.filter((el) => el.tabIndex === -1);
    expect(tabbable).toHaveLength(1);
    expect(tabbable[0]).toBe(items[0]);
    expect(skipped).toHaveLength(items.length - 1);
  });

  it('ArrowDown moves focus to the next treeitem and updates roving tabIndex', async () => {
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={false}
        onUnchangedFileSelect={noop}
        selectedFile={null}
        onFileSelect={noop}
      />
    );
    const items = await screen.findAllByRole('treeitem');
    items[0].focus();
    fireEvent.keyDown(items[0], { key: 'ArrowDown' });
    const after = treeItems();
    expect(document.activeElement).toBe(after[1]);
    expect(after[1].tabIndex).toBe(0);
    expect(after[0].tabIndex).toBe(-1);
  });

  it('ArrowUp moves focus to the previous treeitem', async () => {
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={false}
        onUnchangedFileSelect={noop}
        selectedFile={null}
        onFileSelect={noop}
      />
    );
    const items = await screen.findAllByRole('treeitem');
    items[1].focus();
    fireEvent.keyDown(items[1], { key: 'ArrowUp' });
    const after = treeItems();
    expect(document.activeElement).toBe(after[0]);
  });

  it('ArrowDown at the last visible row does not move focus', async () => {
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={false}
        onUnchangedFileSelect={noop}
        selectedFile={null}
        onFileSelect={noop}
      />
    );
    const items = await screen.findAllByRole('treeitem');
    const last = items[items.length - 1];
    last.focus();
    fireEvent.keyDown(last, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(last);
  });

  it('ArrowRight expands a collapsed folder', async () => {
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={false}
        onUnchangedFileSelect={noop}
        selectedFile={null}
        onFileSelect={noop}
      />
    );
    const items = await screen.findAllByRole('treeitem');
    expect(attr(items[0], 'aria-expanded')).toBe('false');
    items[0].focus();
    fireEvent.keyDown(items[0], { key: 'ArrowRight' });
    const after = treeItems();
    expect(attr(after[0], 'aria-expanded')).toBe('true');
    // 3 → 5: src + its 2 children + docs + LICENSE
    expect(after).toHaveLength(5);
  });

  it('ArrowRight on an expanded folder descends to its first child', async () => {
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={false}
        onUnchangedFileSelect={noop}
        selectedFile={null}
        onFileSelect={noop}
      />
    );
    const items = await screen.findAllByRole('treeitem');
    items[0].focus();
    fireEvent.keyDown(items[0], { key: 'ArrowRight' }); // expand src
    const expanded = treeItems();
    expect(attr(expanded[0], 'aria-expanded')).toBe('true');
    expanded[0].focus();
    fireEvent.keyDown(expanded[0], { key: 'ArrowRight' }); // descend
    const after = treeItems();
    expect(document.activeElement).toBe(after[1]); // app.ts
  });

  it('ArrowLeft collapses an expanded folder', async () => {
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={false}
        onUnchangedFileSelect={noop}
        selectedFile={null}
        onFileSelect={noop}
      />
    );
    const items = await screen.findAllByRole('treeitem');
    items[0].focus();
    fireEvent.keyDown(items[0], { key: 'ArrowRight' }); // expand
    expect(attr(treeItems()[0], 'aria-expanded')).toBe('true');
    fireEvent.keyDown(treeItems()[0], { key: 'ArrowLeft' }); // collapse
    const collapsed = treeItems();
    expect(attr(collapsed[0], 'aria-expanded')).toBe('false');
    expect(collapsed).toHaveLength(3);
  });

  it('ArrowLeft on a leaf ascends to its parent folder', async () => {
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={false}
        onUnchangedFileSelect={noop}
        selectedFile={null}
        onFileSelect={noop}
      />
    );
    const items = await screen.findAllByRole('treeitem');
    items[0].focus();
    fireEvent.keyDown(items[0], { key: 'ArrowRight' }); // expand src
    const expanded = treeItems();
    expanded[1].focus(); // focus on app.ts (child)
    expect(expanded[1].hasAttribute('aria-expanded')).toBe(false);
    fireEvent.keyDown(expanded[1], { key: 'ArrowLeft' });
    const after = treeItems();
    expect(document.activeElement).toBe(after[0]); // back to src
  });

  it('Home and End jump to the first and last visible treeitem', async () => {
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={false}
        onUnchangedFileSelect={noop}
        selectedFile={null}
        onFileSelect={noop}
      />
    );
    const items = await screen.findAllByRole('treeitem');
    items[0].focus();
    fireEvent.keyDown(items[0], { key: 'End' });
    let after = treeItems();
    expect(document.activeElement).toBe(after[after.length - 1]);
    fireEvent.keyDown(document.activeElement as HTMLElement, { key: 'Home' });
    after = treeItems();
    expect(document.activeElement).toBe(after[0]);
  });

  it('Enter on a file row fires onUnchangedFileSelect with the file path', async () => {
    const onUnchangedFileSelect = vi.fn();
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={false}
        onUnchangedFileSelect={onUnchangedFileSelect}
        selectedFile={null}
        onFileSelect={noop}
      />
    );
    const items = await screen.findAllByRole('treeitem');
    items[2].focus(); // LICENSE is the third top-level row.
    fireEvent.keyDown(items[2], { key: 'Enter' });
    expect(onUnchangedFileSelect).toHaveBeenCalledTimes(1);
    expect(onUnchangedFileSelect).toHaveBeenCalledWith('/repo/LICENSE');
  });

  it('Enter on a folder row toggles expand/collapse', async () => {
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={false}
        onUnchangedFileSelect={noop}
        selectedFile={null}
        onFileSelect={noop}
      />
    );
    const items = await screen.findAllByRole('treeitem');
    items[0].focus();
    expect(attr(items[0], 'aria-expanded')).toBe('false');
    fireEvent.keyDown(items[0], { key: 'Enter' });
    expect(attr(treeItems()[0], 'aria-expanded')).toBe('true');
    fireEvent.keyDown(treeItems()[0], { key: 'Enter' });
    expect(attr(treeItems()[0], 'aria-expanded')).toBe('false');
  });

  it('Space on a file row activates it (same as Enter)', async () => {
    const onUnchangedFileSelect = vi.fn();
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={false}
        onUnchangedFileSelect={onUnchangedFileSelect}
        selectedFile={null}
        onFileSelect={noop}
      />
    );
    const items = await screen.findAllByRole('treeitem');
    items[2].focus();
    fireEvent.keyDown(items[2], { key: ' ' });
    expect(onUnchangedFileSelect).toHaveBeenCalledWith('/repo/LICENSE');
  });

  it('clamps the active index when a folder collapse hides the focused descendant', async () => {
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={false}
        onUnchangedFileSelect={noop}
        selectedFile={null}
        onFileSelect={noop}
      />
    );
    const items = await screen.findAllByRole('treeitem');
    items[0].focus();
    fireEvent.keyDown(items[0], { key: 'ArrowRight' }); // expand src
    // Walk deep into the tree, then collapse the ancestor — the roving
    // tabindex must NOT get stuck on an index past visible.length.
    let after = treeItems();
    after[2].focus(); // focus index.ts (third row, child of src)
    fireEvent.keyDown(after[2], { key: 'ArrowLeft' }); // ascend to app.ts
    after = treeItems();
    fireEvent.keyDown(after[1], { key: 'ArrowLeft' }); // ascend to src
    after = treeItems();
    fireEvent.keyDown(after[0], { key: 'ArrowLeft' }); // collapse src
    const collapsed = treeItems();
    // Back to 3 top-level rows; the previously-focused index (deep inside
    // src) was clamped to the parent folder's index, which is now 0.
    expect(collapsed).toHaveLength(3);
    expect(collapsed.map((el) => el.tabIndex)).toEqual([0, -1, -1]);
  });
});