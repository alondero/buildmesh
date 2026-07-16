import { describe, it, expect, vi } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { NodeCreationForm } from '../../src/components/Sidebar/NodeCreationForm';
import type { Mesh } from '../../src/stores/meshStore';
import type { SpawnOption } from '../../src/lib/groups';

const MESH: Mesh = {
  id: 7,
  name: 'demo',
  path: '/tmp/demo',
  layout: 'single',
  position: 0,
  created_at: '2026-01-01',
  scratchpad: '',
  sandbox: false,
};

const PROVIDERS: SpawnOption[] = [
  // Issue #575 / ADR-0016 — Spawn Options carry the full wire shape.
  { id: 'claude', label: 'Anthropic', color: 'bg-blue-500', icon: 'A', harness_id: 'claude', provider_id: null, is_proxied: false, group_key: 'claude' },
  { id: 'agy', label: 'Agy', color: 'bg-emerald-500', icon: 'G', harness_id: 'agy', provider_id: null, is_proxied: false, group_key: 'agy' },
];

function setup(overrides: Partial<React.ComponentProps<typeof NodeCreationForm>> = {}) {
  const onToggleDropdown = vi.fn();
  const onSelectProvider = vi.fn();
  // Issue #575 — the default-provider id is now the *composite* Spawn
  // Option id (or a bare harness id for native). The first fixture
  // row carries the bare `claude` harness id with label "Anthropic"
  // (post-#538 the Claude Code profile IS the Anthropic subscription),
  // so 'claude' is the right default here.
  const getDefaultProvider = vi.fn().mockResolvedValue('claude');
  render(
    <NodeCreationForm
      mesh={MESH}
      isDropdownOpen={false}
      providers={PROVIDERS}
      onToggleDropdown={onToggleDropdown}
      onSelectProvider={onSelectProvider}
      getDefaultProvider={getDefaultProvider}
      {...overrides}
    />,
  );
  return { onToggleDropdown, onSelectProvider, getDefaultProvider };
}

describe('NodeCreationForm', () => {
  it('adds a node with the default provider when + is clicked, deferring use_worktree to the mesh default', async () => {
    // Regression: a normal click must NOT force use_worktree=true, because
    // meshes with `use_worktree = false` would otherwise get a worktree node
    // and the wrong "worktree" pill / "Build from worktree" label. Passing
    // undefined lets the backend fall back to `meshes.use_worktree`.
    const { onSelectProvider, getDefaultProvider } = setup();

    await userEvent.click(screen.getByTitle('Add agent node'));

    expect(getDefaultProvider).toHaveBeenCalledWith(7);
    // Issue #575 — the default is now the bare harness id `claude` (not
    // the legacy `anthropic` enum value, which is the same executor but
    // a separate wire id).
    await waitFor(() => expect(onSelectProvider).toHaveBeenCalledWith(MESH, 'claude', undefined));
  });

  it('adds a node in the mesh root when + is alt-clicked', async () => {
    const { onSelectProvider, getDefaultProvider } = setup();

    fireEvent.click(screen.getByTitle('Add agent node'), { altKey: true });

    expect(getDefaultProvider).toHaveBeenCalledWith(7);
    await waitFor(() => expect(onSelectProvider).toHaveBeenCalledWith(MESH, 'claude', false));
  });

  it('toggles the dropdown when the chevron is clicked', async () => {
    const { onToggleDropdown, onSelectProvider } = setup();

    await userEvent.click(screen.getByTitle('Choose provider'));

    expect(onToggleDropdown).toHaveBeenCalledWith(MESH);
    expect(onSelectProvider).not.toHaveBeenCalled();
  });

  it('hides the provider dropdown while closed', () => {
    setup({ isDropdownOpen: false });
    expect(screen.queryByRole('button', { name: /Agy/ })).toBeNull();
  });

  it('shows the provider dropdown when open', () => {
    setup({ isDropdownOpen: true });
    // Issue #575 — the harness header carries a "harness" badge, so
    // the accessible name is "<label> harness" rather than just the
    // label. Regex matchers keep the test robust to that suffix.
    expect(screen.getByRole('menuitem', { name: /Anthropic/ })).toBeTruthy();
    expect(screen.getByRole('menuitem', { name: /Agy/ })).toBeTruthy();
  });

  it('selects a specific provider from the open dropdown, deferring use_worktree to the mesh default', async () => {
    const { onSelectProvider } = setup({ isDropdownOpen: true });

    await userEvent.click(screen.getByRole('menuitem', { name: /Agy/ }));

    expect(onSelectProvider).toHaveBeenCalledWith(MESH, 'agy', undefined);
  });

  it('selects a specific provider in the mesh root from the open dropdown when alt-clicked', async () => {
    const { onSelectProvider } = setup({ isDropdownOpen: true });

    fireEvent.click(screen.getByRole('menuitem', { name: /Agy/ }), { altKey: true });

    expect(onSelectProvider).toHaveBeenCalledWith(MESH, 'agy', false);
  });

  it('reflects the resolved default provider in the + button tooltip on hover', async () => {
    setup();

    await userEvent.hover(screen.getByText('+'));

    await waitFor(() => expect(screen.getByTitle('Add agent node (Anthropic)')).toBeTruthy());
  });
});
