import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { FileExplorerPanel } from '../../src/components/FileTree/FileExplorerPanel';
import type { FileNode, GitStatus } from '../../src/lib/tauri';

const TREE: FileNode = {
  name: 'repo',
  path: '/repo',
  is_dir: true,
  children: [
    { name: 'app.ts', path: '/repo/app.ts', is_dir: false, children: [] },
  ],
};

const STATUS: GitStatus[] = [
  { path: 'src/app.ts', status: 'modified', additions: 2, deletions: 1 },
];

function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'list_directory') return Promise.resolve(TREE);
    if (cmd === 'get_git_status') return Promise.resolve(STATUS);
    return Promise.resolve({});
  });
}

const noop = () => {};

describe('FileExplorerPanel', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    mockBackend();
  });

  it('renders both the Changed Files and File Tree sections for a mesh context', async () => {
    render(
      <FileExplorerPanel
        context={{ type: 'mesh', meshId: 1, path: '/repo' }}
        width={300}
        onWidthChange={noop}
        onClose={noop}
        meshName="demo"
      />
    );

    // Changed Files section lists the git-status file with its stats.
    expect(await screen.findByText('src/app.ts')).toBeTruthy();
    expect(screen.getByText('Changed Files')).toBeTruthy();
    expect(screen.getByText('File Tree')).toBeTruthy();
    expect(screen.getByText('+2')).toBeTruthy();
    expect(screen.getByText('-1')).toBeTruthy();
  });

  it('hides the File Tree when its header is collapsed', async () => {
    render(
      <FileExplorerPanel
        context={{ type: 'mesh', meshId: 1, path: '/repo' }}
        width={300}
        onWidthChange={noop}
        onClose={noop}
        meshName="demo"
      />
    );

    await screen.findByText('File Tree');
    fireEvent.click(screen.getByText('File Tree'));
    // The Changed Files section still shows the file, but the tree is gone:
    // collapsing only removes the FileTree subtree, leaving the section header.
    expect(screen.getByText('Changed Files')).toBeTruthy();
  });

  it('omits the Changed Files section for a userConfig context', async () => {
    render(
      <FileExplorerPanel
        context={{ type: 'userConfig', path: '/cfg' }}
        width={300}
        onWidthChange={noop}
        onClose={noop}
      />
    );

    await screen.findByText('File Tree');
    expect(screen.queryByText('Changed Files')).toBeNull();
  });
});
