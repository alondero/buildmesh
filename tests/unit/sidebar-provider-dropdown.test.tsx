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

  it('maps the plain terminal provider to the gray badge colour', () => {
    // The new "Terminal" provider is intentionally a neutral grey: a
    // shell is not branded. Pin the colour here so the explicit map
    // entry (vs the unknown-provider fallback) stays in place.
    expect(colorClassForProvider('terminal')).toBe('bg-gray-500');
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

    expect(onSelect).toHaveBeenCalledWith('agy', false);
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

  it('groups legacy providers under a "Legacy" header below the dynamic ones (#536)', () => {
    const mixed: ProviderEntry[] = [
      { id: 'claude', label: 'Claude Code', color: 'bg-blue-500', legacy: false },
      { id: 'anthropic', label: 'Anthropic', color: 'bg-blue-500', legacy: true },
      { id: 'codex', label: 'Codex', color: 'bg-gray-500', legacy: true },
    ];
    const { container } = render(<ProviderDropdown meshId={1} providers={mixed} onSelect={() => {}} />);

    // The header is present once.
    expect(screen.getByText('Legacy')).toBeTruthy();

    // The dynamic profile renders before the Legacy header; the legacy ones after.
    const text = container.textContent ?? '';
    expect(text.indexOf('Claude Code')).toBeLessThan(text.indexOf('Legacy'));
    expect(text.indexOf('Legacy')).toBeLessThan(text.indexOf('Anthropic'));
  });

  it('shows no "Legacy" header when every entry is dynamic', () => {
    const dynamicOnly: ProviderEntry[] = [
      { id: 'claude', label: 'Claude Code', color: 'bg-blue-500', legacy: false },
      { id: 'terminal', label: 'Terminal', color: 'bg-gray-500', legacy: false },
    ];
    render(<ProviderDropdown meshId={1} providers={dynamicOnly} onSelect={() => {}} />);
    expect(screen.queryByText('Legacy')).toBeNull();
    expect(screen.getAllByRole('button')).toHaveLength(2);
  });

  it('keeps both same-id rows actionable across the dynamic/legacy split', async () => {
    // A detected "terminal" profile and the legacy "terminal" enum share an id;
    // namespaced React keys must let both render as separate buttons.
    const onSelect = vi.fn();
    const sameId: ProviderEntry[] = [
      { id: 'terminal', label: 'Terminal (profile)', color: 'bg-gray-500', legacy: false },
      { id: 'terminal', label: 'Terminal (legacy)', color: 'bg-gray-500', legacy: true },
    ];
    render(<ProviderDropdown meshId={1} providers={sameId} onSelect={onSelect} />);
    expect(screen.getByRole('button', { name: 'Terminal (profile)' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Terminal (legacy)' }));
    expect(onSelect).toHaveBeenCalledWith('terminal', false);
  });
});
