import { describe, it, expect, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ProviderDropdown } from '../../src/components/Sidebar/ProviderDropdown';
import { colorClassForProvider, type SpawnOption } from '../../src/lib/groups';

// `SafeLink` (rendered by the issue #822 empty state) routes its click
// through `openUrl` — Tauri 2 drops `target="_blank"` without the
// webview-window capability. Stub it so the empty-state link is inert
// under jsdom.
const { openUrlMock } = vi.hoisted(() => ({
  openUrlMock: vi.fn<[], Promise<void>>().mockResolvedValue(undefined),
}));
vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: openUrlMock,
}));

// Issue #575 / ADR-0016 — Spawn Options carry the full wire shape. Test
// fixtures here stand in for the backend's grouped, harness-ordered list;
// a fixture row that's the only one in its bucket is a native harness
// header (the only clickable row in a one-harness group).
const PROVIDERS: SpawnOption[] = [
  { id: 'claude', label: 'Anthropic', color: 'bg-blue-500', icon: 'A', harness_id: 'claude', provider_id: null, is_proxied: false, group_key: 'claude' },
  { id: 'agy', label: 'Agy', color: 'bg-emerald-500', icon: 'G', harness_id: 'agy', provider_id: null, is_proxied: false, group_key: 'agy' },
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
    // The harness header carries a "harness" badge label, so the
    // accessible name is "<label> harness" — the regex matcher keeps
    // the test robust to that suffix.
    expect(screen.getByRole('button', { name: /Anthropic/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: /Agy/ })).toBeTruthy();
  });

  it('tags the container with the mesh id for click-outside detection', () => {
    const { container } = render(<ProviderDropdown meshId={42} providers={PROVIDERS} onSelect={() => {}} />);
    expect(container.querySelector('[data-dropdown-for="42"]')).toBeTruthy();
  });

  it('calls onSelect with the provider id when an option is clicked', async () => {
    const onSelect = vi.fn();
    render(<ProviderDropdown meshId={1} providers={PROVIDERS} onSelect={onSelect} />);

    await userEvent.click(screen.getByRole('button', { name: /Agy/ }));

    expect(onSelect).toHaveBeenCalledWith('agy', false);
  });

  it('stops click propagation so the parent row is not toggled', async () => {
    const onParentClick = vi.fn();
    render(
      <div onClick={onParentClick}>
        <ProviderDropdown meshId={1} providers={PROVIDERS} onSelect={() => {}} />
      </div>,
    );

    await userEvent.click(screen.getByRole('button', { name: /Anthropic/ }));

    expect(onParentClick).not.toHaveBeenCalled();
  });

  it('renders nothing actionable when the provider list is empty', () => {
    render(<ProviderDropdown meshId={1} providers={[]} onSelect={() => {}} />);
    expect(screen.queryAllByRole('button')).toHaveLength(0);
  });

  describe('first-run empty state (issue #822)', () => {
    // A fresh machine with no agent CLI detected and no keyed provider gets
    // a Terminal-only menu. The empty-state panel explains what to install;
    // Terminal stays clickable below it.
    const TERMINAL_ONLY: SpawnOption[] = [
      { id: 'terminal', label: 'Terminal', color: 'bg-gray-500', icon: 'T', harness_id: 'terminal', provider_id: null, is_proxied: false, group_key: 'terminal' },
    ];

    it('shows the "No agent CLIs found" panel when only Terminal is offered', () => {
      render(<ProviderDropdown meshId={1} providers={TERMINAL_ONLY} onSelect={() => {}} />);
      expect(screen.getByTestId('spawn-menu-empty-state')).toBeTruthy();
      expect(screen.getByText('No agent CLIs found')).toBeTruthy();
      // Terminal is still spawnable below the hint.
      expect(screen.getByRole('button', { name: /Terminal/ })).toBeTruthy();
    });

    it('links to the README prerequisites via openUrl (Tauri 2 routing)', async () => {
      render(<ProviderDropdown meshId={1} providers={TERMINAL_ONLY} onSelect={() => {}} />);
      await userEvent.click(screen.getByRole('link', { name: /View setup instructions/ }));
      expect(openUrlMock).toHaveBeenCalledWith('https://github.com/alondero/buildmesh#prerequisites');
    });

    it('hides the panel once a real agent harness is present', () => {
      render(<ProviderDropdown meshId={1} providers={PROVIDERS} onSelect={() => {}} />);
      expect(screen.queryByTestId('spawn-menu-empty-state')).toBeNull();
    });

    it('hides the panel when a keyed Proxied provider is present even without a native harness', () => {
      const proxiedOnly: SpawnOption[] = [
        { id: 'claude:minimax', label: 'MiniMax', color: 'bg-indigo-500', icon: 'M', harness_id: 'claude', provider_id: 'minimax', is_proxied: true, group_key: 'claude' },
        { id: 'terminal', label: 'Terminal', color: 'bg-gray-500', icon: 'T', harness_id: 'terminal', provider_id: null, is_proxied: false, group_key: 'terminal' },
      ];
      render(<ProviderDropdown meshId={1} providers={proxiedOnly} onSelect={() => {}} />);
      expect(screen.queryByTestId('spawn-menu-empty-state')).toBeNull();
    });
  });

  it('renders a harness-grouped menu (issue #575) with no "Legacy" header', () => {
    // The legacy enum rows and their "Legacy" header were retired in
    // #538. Issue #575 reframes the list as a harness-grouped Spawn
    // Menu: each harness gets a clickable header (`<button
    // data-spawn-harness=...>`), and Proxied children render indented
    // inside the same group.
    const profiles: SpawnOption[] = [
      { id: 'claude', label: 'Claude Code', color: 'bg-blue-500', icon: 'A', harness_id: 'claude', provider_id: null, is_proxied: false, group_key: 'claude' },
      { id: 'claude:minimax', label: 'MiniMax', color: 'bg-indigo-500', icon: 'M', harness_id: 'claude', provider_id: 'minimax', is_proxied: true, group_key: 'claude' },
      { id: 'codex', label: 'Codex', color: 'bg-gray-500', icon: 'C', harness_id: 'codex', provider_id: null, is_proxied: false, group_key: 'codex' },
      { id: 'terminal', label: 'Terminal', color: 'bg-gray-500', icon: 'T', harness_id: 'terminal', provider_id: null, is_proxied: false, group_key: 'terminal' },
    ];
    render(<ProviderDropdown meshId={1} providers={profiles} onSelect={() => {}} />);
    expect(screen.queryByText('Legacy')).toBeNull();
    // 4 clickable rows: 3 native headers + 1 proxied child.
    expect(screen.getAllByRole('button')).toHaveLength(4);
    expect(screen.getByRole('button', { name: /Claude Code/ })).toBeTruthy();
    // The harness headers carry the `data-spawn-harness` attribute so a
    // future test (or e2e) can target them directly.
    expect(screen.getByRole('button', { name: /Claude Code/ }).getAttribute('data-spawn-harness')).toBe('claude');
    // The Proxied child renders the label, not a "harness" badge.
    expect(screen.getByRole('button', { name: /^MiniMax$/ })).toBeTruthy();
  });
});
