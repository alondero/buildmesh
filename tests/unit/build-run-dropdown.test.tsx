/**
 * The Build menu on an agent node grew a third item — a raw interactive
 * terminal session started in the worktree directory — to give users a
 * scratch space without leaving the mesh. The terminal item sits below a
 * divider so it doesn't get conflated with the one-shot build/run commands,
 * and the label adapts to the worktree context (the same way Build/Run do)
 * so the user can tell at a glance which directory the shell will land in.
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
 * The dropdown's trigger button and its first menu item both display "Build",
 * which trips up `getByText` queries once the menu is open. Scope queries to
 * the menu container (the wrapper rendered with `data-testid` we add below)
 * so we can target the menu items unambiguously.
 */
function openMenu() {
  // The trigger is the FIRST button containing "Build" — there is no other
  // button until the menu opens, so getAllByText returns [trigger].
  const [trigger] = screen.getAllByText('Build');
  fireEvent.click(trigger);
}

describe('BuildRunDropdown', () => {
  it('renders Build, Run, and Terminal items when the menu is open (worktrees off)', () => {
    const onBuildRun = vi.fn();
    render(<BuildRunDropdown node={NODE} onBuildRun={onBuildRun} />);

    openMenu();

    // After open: trigger (index 0) and three menu items. The menu items
    // are the second occurrence of "Build", the only "Run", and the only
    // "Terminal" — the trigger "Build" stays unique, so queryAllByText on
    // the labels gives us the menu items directly.
    const builds = screen.getAllByText('Build');
    expect(builds).toHaveLength(2);
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
    // The trigger button still says "Build" (it's the always-visible
    // label), but the menu items must use the suffixed form — there
    // should be NO bare "Run"/"Terminal" menu items.
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
    // Pick the menu-item "Build" (the second occurrence, since the
    // trigger is first in the DOM).
    const [, menuBuild] = screen.getAllByText('Build');
    fireEvent.click(menuBuild);
    expect(onBuildRun).toHaveBeenLastCalledWith(NODE.id, 'build');

    openMenu();
    fireEvent.click(screen.getByText('Run'));
    expect(onBuildRun).toHaveBeenLastCalledWith(NODE.id, 'run');
  });
});
