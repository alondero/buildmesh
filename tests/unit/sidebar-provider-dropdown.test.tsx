import { describe, it, expect, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ProviderDropdown, colorClassForProvider, type ProviderEntry } from '../../src/components/Sidebar/ProviderDropdown';

const PROVIDERS: ProviderEntry[] = [
  { id: 'anthropic', label: 'Anthropic', color: 'bg-blue-500' },
  { id: 'agy', label: 'Agy', color: 'bg-emerald-500' },
];

describe('colorClassForProvider', () => {
  it('maps known providers to their badge colour', () => {
    expect(colorClassForProvider('anthropic')).toBe('bg-blue-500');
    expect(colorClassForProvider('agy')).toBe('bg-emerald-500');
  });

  it('falls back to gray for unknown providers', () => {
    expect(colorClassForProvider('mystery')).toBe('bg-gray-500');
  });
});

describe('ProviderDropdown', () => {
  it('renders a button for every provider', () => {
    render(<ProviderDropdown meshId={1} providers={PROVIDERS} onSelect={() => {}} />);
    expect(screen.getByRole('button', { name: 'Anthropic' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Agy' })).toBeTruthy();
  });

  it('tags the container with the mesh id for click-outside detection', () => {
    const { container } = render(<ProviderDropdown meshId={42} providers={PROVIDERS} onSelect={() => {}} />);
    expect(container.querySelector('[data-dropdown-for="42"]')).toBeTruthy();
  });

  it('calls onSelect with the provider id when an option is clicked', async () => {
    const onSelect = vi.fn();
    render(<ProviderDropdown meshId={1} providers={PROVIDERS} onSelect={onSelect} />);

await userEvent.click(screen.getByRole('button', { name: 'Agy' }));

    expect(onSelect).toHaveBeenCalledWith('agy');
  });

  it('stops click propagation so the parent row is not toggled', async () => {
    const onParentClick = vi.fn();
    render(
      <div onClick={onParentClick}>
        <ProviderDropdown meshId={1} providers={PROVIDERS} onSelect={() => {}} />
      </div>,
    );

    await userEvent.click(screen.getByRole('button', { name: 'Anthropic' }));

    expect(onParentClick).not.toHaveBeenCalled();
  });

  it('renders nothing actionable when the provider list is empty', () => {
    render(<ProviderDropdown meshId={1} providers={[]} onSelect={() => {}} />);
    expect(screen.queryAllByRole('button')).toHaveLength(0);
  });
});
