/**
 * The Build menu on an agent node grew a third item — a raw interactive
 * terminal session started in the worktree directory — to give users a
 * scratch space without leaving the mesh. The terminal item sits below a
 * divider so it doesn't get conflated with the one-shot build/run commands,
 * and the label adapts to the worktree context (the same way Build/Run do)
 * so the user can tell at a glance which directory the shell will land in.
 *
 * The trigger button is now an icon-only wrench + chevron (matching the
 * close + expand buttons in GridNodeHeader for a balanced trio). Title-bar
 * space shrinks ~34 px and the menu items keep their original labels.
 */
import { describe, it, expect, vi } from 'vitest';
import { render, fireEvent, screen } from '@testing-library/react';
import { type AgentNode } from '../../src/stores/agentNodeStore';
import { BuildRunDropdown } from '../../src/components/BuildRun/BuildRunDropdown';

const NODE: AgentNode = {
  id: 7,
  mesh_id: 1,
  name: 'agent-7',
  path: '/repo',
  branch: 'main',
  env: 'wsl',
  provider: 'anthropic',
  status: 'running',
  use_worktree: false,
  created_at: new Date(0).toISOString(),
};

/**
 * The trigger is icon-only and identified by `aria-label` rather than its
 * old "Build" text label. Using the accessible name (instead of any visible
 * label) keeps this helper robust against future label/icon tweaks as long
 * as the aria-label contract holds.
 */
function openMenu() {
  fireEvent.click(screen.getByLabelText('Open build menu'));
}

describe('BuildRunDropdown', () => {
  it('hides the "Build" word from the trigger and uses an accessible name', () => {
    const onBuildRun = vi.fn();
    render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);

    // No "Build" text on the trigger — it should not exist before the menu
    // opens. After openMenu() below, the only "Build" matches are the menu
    // item, never the trigger.
    expect(screen.queryByText('Build')).toBeNull();
    expect(screen.getByLabelText('Open build menu')).toBeTruthy();

    openMenu();
    // Now there IS one "Build" — the menu item, not the trigger.
    expect(screen.getAllByText('Build')).toHaveLength(1);
  });

  it('trigger matches the close + maximise trio surface (h-7 + bg + border)', () => {
    // The trio lives on a single row in GridNodeHeader. The close + expand
    // asserts live in grid-node-header.test.tsx (where BuildRunDropdown is
    // mocked to null for unrelated git-summary-chip tests); the Build-side
    // counterpart has to live here where BuildRunDropdown is the real DOM.
    // Note: the trio shares HEIGHT (h-7), not width — Build is content-width
    // (wrench + chevron), while close + maximise are fixed square w-7 h-7.
    const onBuildRun = vi.fn();
    render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);

    const cls = screen.getByLabelText('Open build menu').className;
    expect(cls).toMatch(/\bh-7\b/);
    expect(cls).toContain('bg-bg-base/60');
    expect(cls).toContain('border-border-default');
  });

  it('renders Build, Run, and Terminal items when the menu is open (worktrees off)', () => {
    const onBuildRun = vi.fn();
    render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);

    openMenu();

    expect(screen.getByText('Build')).toBeTruthy();
    expect(screen.getByText('Run')).toBeTruthy();
    expect(screen.getByText('Terminal')).toBeTruthy();
  });

  it('renders the worktree-suffixed labels when use_worktree is true', () => {
    const onBuildRun = vi.fn();
    render(
      <BuildRunDropdown
        node={{ ...NODE, use_worktree: true }}
        onBuildRun={onBuildRun}
      />,
    );

    openMenu();

    expect(screen.getByText('Build from worktree')).toBeTruthy();
    expect(screen.getByText('Run from worktree')).toBeTruthy();
    expect(screen.getByText('Terminal in worktree')).toBeTruthy();
    // The menu items must use the suffixed form — there should be NO
    // bare "Run"/"Terminal" menu items.
    expect(screen.queryByText('Run')).toBeNull();
    expect(screen.queryByText('Terminal')).toBeNull();
  });

  it('invokes onBuildRun with terminal mode when the terminal item is clicked', () => {
    const onBuildRun = vi.fn();
    render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);

    openMenu();
    fireEvent.click(screen.getByText('Terminal'));

    expect(onBuildRun).toHaveBeenCalledWith(NODE.id, 'terminal');
  });

  it('still invokes onBuildRun with build and run modes (regression)', () => {
    const onBuildRun = vi.fn();
    render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);

    openMenu();
    fireEvent.click(screen.getByText('Build'));
    expect(onBuildRun).toHaveBeenLastCalledWith(NODE.id, 'build');

    openMenu();
    fireEvent.click(screen.getByText('Run'));
    expect(onBuildRun).toHaveBeenLastCalledWith(NODE.id, 'run');
  });
});
