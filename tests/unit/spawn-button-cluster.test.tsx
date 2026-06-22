/**
 * Tests for the canonical `+ ▾` Spawn Menu cluster (ADR-0016 §2).
 *
 * The cluster is the shared visual for spawning a new agent node from
 * any desktop app entry point — sidebar mesh row, issues-probe row,
 * PRs-probe row. Each parent owns the spawn *action*; the cluster owns
 * the visual + dropdown wiring. These tests pin the cluster's own
 * contract so future refactors don't accidentally let a split-button
 * (or any per-surface variant) creep back into a call site.
 */

import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { SpawnButtonCluster } from '../../src/components/Sidebar/SpawnButtonCluster';
import type { SpawnOption } from '../../src/lib/groups';

// Minimal harness-grouped fixture — two harnesses, one proxied child.
// Mirrors the wire shape `compose_provider_menu` returns so the cluster
// behaves the same as it does against a real `listProviders` response.
const PROVIDERS: SpawnOption[] = [
  { id: 'claude', label: 'Anthropic', color: 'bg-blue-500', icon: 'A', harness_id: 'claude', provider_id: null, is_proxied: false, group_key: 'claude' },
  { id: 'claude:minimax', label: 'Minimax', color: 'bg-indigo-500', icon: 'M', harness_id: 'claude', provider_id: 'minimax', is_proxied: true, group_key: 'claude' },
  { id: 'agy', label: 'Agy', color: 'bg-emerald-500', icon: 'G', harness_id: 'agy', provider_id: null, is_proxied: false, group_key: 'agy' },
];

describe('SpawnButtonCluster (#575 / ADR-0016)', () => {
  it('renders a + and a ▾ as siblings (no split-button text)', () => {
    render(
      <SpawnButtonCluster
        providers={PROVIDERS}
        meshId={1}
        isOpen={false}
        onToggleDropdown={() => {}}
        onSpawnDefault={() => {}}
        onSelectProvider={() => {}}
      />,
    );

    // The cluster's two affordances are testid-tagged so every caller
    // (sidebar, issues, PRs) can reach them by role without depending on
    // the rendered text. Pin both — if either disappears, a caller has
    // regressed to a split-button or a single-affordance layout.
    expect(screen.getByTestId('spawn-default')).toBeTruthy();
    expect(screen.getByTestId('spawn-dropdown-toggle')).toBeTruthy();

    // Regression guard: there should be NO "Spawn" text label anywhere
    // (the cluster uses a bare `+` icon and the `+` rewrites to
    // "Spawning..." only when this cluster's spawn is in flight).
    expect(screen.queryByText('Spawn')).toBeNull();
  });

  it('calls onSpawnDefault when the + is clicked (without altKey)', async () => {
    const onSpawnDefault = vi.fn();
    render(
      <SpawnButtonCluster
        providers={PROVIDERS}
        meshId={1}
        isOpen={false}
        onToggleDropdown={() => {}}
        onSpawnDefault={onSpawnDefault}
        onSelectProvider={() => {}}
      />,
    );

    await userEvent.click(screen.getByTestId('spawn-default'));

    expect(onSpawnDefault).toHaveBeenCalledTimes(1);
    expect(onSpawnDefault).toHaveBeenCalledWith(false);
  });

  it('forwards altKey on the + click so the sidebar can spawn in mesh root', async () => {
    // Sidebar-specific behaviour: alt-click overrides `use_worktree` to
    // spawn the new node in the mesh root, bypassing the per-mesh default.
    // The cluster just carries altKey through; the parent decides what to
    // do with it (Issues/PRs parents ignore it).
    //
    // Implementation note: userEvent.click(el, { altKey: true }) doesn't
    // propagate altKey through to React's SyntheticEvent in jsdom (the
    // native MouseEvent carries it, but the synthetic mapping drops it).
    // fireEvent.click with event init bypasses userEvent and feeds the
    // properties directly into React's test renderer.
    const onSpawnDefault = vi.fn();
    render(
      <SpawnButtonCluster
        providers={PROVIDERS}
        meshId={1}
        isOpen={false}
        onToggleDropdown={() => {}}
        onSpawnDefault={onSpawnDefault}
        onSelectProvider={() => {}}
      />,
    );

    fireEvent.click(screen.getByTestId('spawn-default'), { altKey: true });

    expect(onSpawnDefault).toHaveBeenCalledWith(true);
  });

  it('toggles the dropdown open via onToggleDropdown when ▾ is clicked', async () => {
    const onToggleDropdown = vi.fn();
    render(
      <SpawnButtonCluster
        providers={PROVIDERS}
        meshId={1}
        isOpen={false}
        onToggleDropdown={onToggleDropdown}
        onSpawnDefault={() => {}}
        onSelectProvider={() => {}}
      />,
    );

    await userEvent.click(screen.getByTestId('spawn-dropdown-toggle'));

    expect(onToggleDropdown).toHaveBeenCalledTimes(1);
  });

  it('flips the + label to "Spawning..." and disables both buttons when isSpawning', () => {
    render(
      <SpawnButtonCluster
        providers={PROVIDERS}
        meshId={1}
        isOpen={false}
        onToggleDropdown={() => {}}
        onSpawnDefault={() => {}}
        onSelectProvider={() => {}}
        isSpawning
      />,
    );

    const plus = screen.getByTestId('spawn-default') as HTMLButtonElement;
    const toggle = screen.getByTestId('spawn-dropdown-toggle') as HTMLButtonElement;
    expect(plus.textContent).toBe('Spawning...');
    expect(plus.disabled).toBe(true);
    expect(toggle.disabled).toBe(true);
  });

  it('disables both buttons when `disabled` is true (any spawn in flight across the surface)', () => {
    render(
      <SpawnButtonCluster
        providers={PROVIDERS}
        meshId={1}
        isOpen={false}
        onToggleDropdown={() => {}}
        onSpawnDefault={() => {}}
        onSelectProvider={() => {}}
        disabled
      />,
    );

    expect((screen.getByTestId('spawn-default') as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByTestId('spawn-dropdown-toggle') as HTMLButtonElement).disabled).toBe(true);
    // The label stays `+` when only `disabled` is set — `isSpawning` is the
    // sole trigger for the "Spawning..." rewrite. The parent flips both
    // flags when this row is the one in flight.
    expect((screen.getByTestId('spawn-default') as HTMLButtonElement).textContent).toBe('+');
  });

  it('does not render the dropdown when isSpawning is true even if isOpen is true', () => {
    // Defensive: the cluster should never show the dropdown while a spawn
    // is in flight (a stale dropdown after the user clicked + would be
    // confusing — the row already has the spawn in motion).
    render(
      <SpawnButtonCluster
        providers={PROVIDERS}
        meshId={1}
        isOpen={true}
        onToggleDropdown={() => {}}
        onSpawnDefault={() => {}}
        onSelectProvider={() => {}}
        isSpawning
      />,
    );

    expect(screen.queryByRole('button', { name: /Anthropic/ })).toBeNull();
    expect(screen.queryByRole('button', { name: /Agy/ })).toBeNull();
  });

  it('shows the default provider name in the + tooltip when getDefaultProvider resolves', async () => {
    // Hover/focus triggers the default-provider fetch; once it resolves,
    // the tooltip rewrites from the generic "Add agent node" to the
    // specific "Add agent node (Anthropic)". The fixture's default
    // provider is the `claude` row (label = "Anthropic").
    const getDefaultProvider = vi.fn().mockResolvedValue('claude');
    render(
      <SpawnButtonCluster
        providers={PROVIDERS}
        meshId={1}
        isOpen={false}
        onToggleDropdown={() => {}}
        onSpawnDefault={() => {}}
        onSelectProvider={() => {}}
        getDefaultProvider={getDefaultProvider}
      />,
    );

    const plus = screen.getByTestId('spawn-default');
    plus.focus();

    // Wait for the async default-provider fetch to resolve + the title
    // to rewrite. `findByTitle` retries until the assertion passes (or
    // times out); avoids a brittle `waitFor(() => expect(...))` shape.
    const titleEl = await screen.findByTitle('Add agent node (Anthropic)');
    expect(titleEl).toBeTruthy();
    expect(titleEl).toBe(plus);

    expect(getDefaultProvider).toHaveBeenCalledTimes(1);
  });

  it('falls back to the generic "Add agent node" tooltip when getDefaultProvider is omitted', () => {
    // Optional prop — surfaces that don't track a per-mesh default
    // (or don't care to surface it) just skip the prop and get the
    // generic tooltip.
    render(
      <SpawnButtonCluster
        providers={PROVIDERS}
        meshId={1}
        isOpen={false}
        onToggleDropdown={() => {}}
        onSpawnDefault={() => {}}
        onSelectProvider={() => {}}
      />,
    );

    expect(screen.getByTitle('Add agent node')).toBeTruthy();
  });
});
