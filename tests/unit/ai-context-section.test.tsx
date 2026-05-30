import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { AiContextSection } from '../../src/components/MeshPropertiesPanel/AiContextSection';
import type { AiContextStatus } from '../../src/lib/tauri';

/** Route invoke() calls by command name so each test controls the backend response. */
function mockBackend(opts: {
  status: AiContextStatus | Error;
  prUrl?: string | Error;
}) {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'detect_ai_context') {
      return opts.status instanceof Error
        ? Promise.reject(opts.status)
        : Promise.resolve(opts.status);
    }
    if (cmd === 'create_ai_context_portability_pr') {
      const v = opts.prUrl ?? 'https://github.com/o/r/pull/1';
      return v instanceof Error ? Promise.reject(v) : Promise.resolve(v);
    }
    return Promise.resolve({});
  });
}

const FULL_CLAUDE: AiContextStatus = {
  claude_md_exists: true,
  agents_md_exists: false,
  skills_dir_exists: true,
  skill_count: 3,
  agents_skills_exists: false,
};

describe('AiContextSection', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it('renders nothing when there is no Claude context', async () => {
    mockBackend({
      status: {
        claude_md_exists: false,
        agents_md_exists: false,
        skills_dir_exists: false,
        skill_count: 0,
        agents_skills_exists: false,
      },
    });
    const { container } = render(
      <AiContextSection meshId={1} meshPath="/repo" isAuthenticated={true} />
    );
    // Give the detect effect a tick to resolve, then assert empty.
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('detect_ai_context', { meshPath: '/repo' }));
    expect(container.firstChild).toBeNull();
  });

  it('offers the button when mirrors are missing', async () => {
    mockBackend({ status: FULL_CLAUDE });
    render(<AiContextSection meshId={1} meshPath="/repo" isAuthenticated={true} />);
    expect(await screen.findByRole('button', { name: /make ai context portable/i })).toBeTruthy();
    expect(screen.getByText(/will create, 3 skills/i)).toBeTruthy();
  });

  it('shows already-portable and no button when both mirrors exist', async () => {
    mockBackend({
      status: { ...FULL_CLAUDE, agents_md_exists: true, agents_skills_exists: true },
    });
    render(<AiContextSection meshId={1} meshPath="/repo" isAuthenticated={true} />);
    expect(await screen.findByText(/already portable/i)).toBeTruthy();
    expect(screen.queryByRole('button', { name: /make ai context portable/i })).toBeNull();
  });

  it('prompts for gh auth instead of the button when unauthenticated', async () => {
    mockBackend({ status: FULL_CLAUDE });
    render(<AiContextSection meshId={1} meshPath="/repo" isAuthenticated={false} />);
    expect(await screen.findByText(/gh auth login/i)).toBeTruthy();
    expect(screen.queryByRole('button', { name: /make ai context portable/i })).toBeNull();
  });

  it('shows the PR URL after a successful create', async () => {
    mockBackend({ status: FULL_CLAUDE, prUrl: 'https://github.com/o/r/pull/42' });
    render(<AiContextSection meshId={7} meshPath="/repo" isAuthenticated={true} />);
    const button = await screen.findByRole('button', { name: /make ai context portable/i });
    fireEvent.click(button);
    const link = await screen.findByText('https://github.com/o/r/pull/42');
    expect(link).toBeTruthy();
    expect(invoke).toHaveBeenCalledWith('create_ai_context_portability_pr', { meshId: 7 });
  });
});
