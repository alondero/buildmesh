import { describe, it, expect, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { NodeCreationForm } from '../../src/components/Sidebar/NodeCreationForm';
import type { Mesh } from '../../src/stores/meshStore';
import type { ProviderEntry } from '../../src/components/Sidebar/ProviderDropdown';

const MESH: Mesh = {
  id: 7,
  name: 'demo',
  path: '/tmp/demo',
  layout: 'single',
  position: 0,
  created_at: '2026-01-01',
};

const PROVIDERS: ProviderEntry[] = [
  { id: 'anthropic', label: 'Anthropic', color: 'bg-blue-500' },
  { id: 'gemini', label: 'Gemini', color: 'bg-emerald-500' },
];

function setup(overrides: Partial<React.ComponentProps<typeof NodeCreationForm>> = {}) {
  const onToggleDropdown = vi.fn();
  const onSelectProvider = vi.fn();
  const getDefaultProvider = vi.fn().mockResolvedValue('anthropic');
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
  it('adds a node with the default provider when + is clicked', async () => {
    const { onSelectProvider, getDefaultProvider } = setup();

    await userEvent.click(screen.getByTitle('Add agent node'));

    expect(getDefaultProvider).toHaveBeenCalledWith(7);
    await waitFor(() => expect(onSelectProvider).toHaveBeenCalledWith(MESH, 'anthropic'));
  });

  it('toggles the dropdown when the chevron is clicked', async () => {
    const { onToggleDropdown, onSelectProvider } = setup();

    await userEvent.click(screen.getByTitle('Choose provider'));

    expect(onToggleDropdown).toHaveBeenCalledWith(MESH);
    expect(onSelectProvider).not.toHaveBeenCalled();
  });

  it('hides the provider dropdown while closed', () => {
    setup({ isDropdownOpen: false });
    expect(screen.queryByRole('button', { name: 'Gemini' })).toBeNull();
  });

  it('shows the provider dropdown when open', () => {
    setup({ isDropdownOpen: true });
    expect(screen.getByRole('button', { name: 'Anthropic' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Gemini' })).toBeTruthy();
  });

  it('selects a specific provider from the open dropdown', async () => {
    const { onSelectProvider } = setup({ isDropdownOpen: true });

    await userEvent.click(screen.getByRole('button', { name: 'Gemini' }));

    expect(onSelectProvider).toHaveBeenCalledWith(MESH, 'gemini');
  });

  it('reflects the resolved default provider in the + button tooltip on hover', async () => {
    setup();

    await userEvent.hover(screen.getByText('+'));

    await waitFor(() => expect(screen.getByTitle('Add agent node (Anthropic)')).toBeTruthy());
  });
});
