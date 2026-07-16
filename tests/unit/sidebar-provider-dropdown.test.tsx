import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent, act } from '@testing-library/react';
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
  it('renders a menuitem for every provider (issue #814)', () => {
    render(<ProviderDropdown meshId={1} providers={PROVIDERS} onSelect={() => {}} />);
    // Issue #814 — WAI-ARIA menu semantics: each provider row is
    // `role="menuitem"` (was a bare `<button>`). Tests query by the new
    // role so the assertion matches the rendered accessibility tree.
    expect(screen.getByRole('menuitem', { name: /Anthropic/ })).toBeTruthy();
    expect(screen.getByRole('menuitem', { name: /Agy/ })).toBeTruthy();
  });

  it('tags the container with the mesh id for click-outside detection', () => {
    const { container } = render(<ProviderDropdown meshId={42} providers={PROVIDERS} onSelect={() => {}} />);
    expect(container.querySelector('[data-dropdown-for="42"]')).toBeTruthy();
  });

  it('calls onSelect with the provider id when an option is clicked', async () => {
    const onSelect = vi.fn();
    render(<ProviderDropdown meshId={1} providers={PROVIDERS} onSelect={onSelect} />);

    await userEvent.click(screen.getByRole('menuitem', { name: /Agy/ }));

    expect(onSelect).toHaveBeenCalledWith('agy', false);
  });

  it('stops click propagation so the parent row is not toggled', async () => {
    const onParentClick = vi.fn();
    render(
      <div onClick={onParentClick}>
        <ProviderDropdown meshId={1} providers={PROVIDERS} onSelect={() => {}} />
      </div>,
    );

    await userEvent.click(screen.getByRole('menuitem', { name: /Anthropic/ }));

    expect(onParentClick).not.toHaveBeenCalled();
  });

  it('renders nothing actionable when the provider list is empty', () => {
    render(<ProviderDropdown meshId={1} providers={[]} onSelect={() => {}} />);
    expect(screen.queryAllByRole('menuitem')).toHaveLength(0);
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
      expect(screen.getByRole('menuitem', { name: /Terminal/ })).toBeTruthy();
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
    // Issue #814 — 4 menuitems: 3 native headers + 1 proxied child.
    expect(screen.getAllByRole('menuitem')).toHaveLength(4);
    expect(screen.getByRole('menuitem', { name: /Claude Code/ })).toBeTruthy();
    // The harness headers carry the `data-spawn-harness` attribute so a
    // future test (or e2e) can target them directly.
    expect(screen.getByRole('menuitem', { name: /Claude Code/ }).getAttribute('data-spawn-harness')).toBe('claude');
    // The Proxied child renders the label, not a "harness" badge.
    expect(screen.getByRole('menuitem', { name: /^MiniMax$/ })).toBeTruthy();
  });

  describe('Escape forwards through to GroupedProviderMenu (issue #814)', () => {
    // The keyboard handler lives inside `GroupedProviderMenu` (it owns
    // focus + roving tabindex); `ProviderDropdown` just forwards
    // `onClose` through. We pin the forwarding here so a future
    // refactor cannot accidentally drop the prop.
    it('forwards onClose to GroupedProviderMenu (Escape closes via the menu)', () => {
      const onClose = vi.fn();
      render(<ProviderDropdown meshId={1} providers={PROVIDERS} onSelect={() => {}} onClose={onClose} />);
      // The first menuitem is auto-focused on mount. Escape fires
      // `onClose` via the keyboard handler in `GroupedProviderMenu`.
      const firstItem = screen.getByRole('menuitem', { name: /Anthropic/ });
      firstItem.focus();
      fireEvent.keyDown(firstItem, { key: 'Escape' });
      expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('does not throw when onClose is omitted and Escape is pressed', () => {
      render(<ProviderDropdown meshId={1} providers={PROVIDERS} onSelect={() => {}} />);
      const firstItem = screen.getByRole('menuitem', { name: /Anthropic/ });
      firstItem.focus();
      expect(() => fireEvent.keyDown(firstItem, { key: 'Escape' })).not.toThrow();
    });
  });

  describe('viewport clamping (issue #814)', () => {
    let rectSpy: ReturnType<typeof vi.spyOn> | undefined;

    afterEach(() => {
      rectSpy?.mockRestore();
      rectSpy = undefined;
    });

    it('applies a negative translateY when the menu would overflow the bottom of the viewport', () => {
      // Stub `HTMLElement.prototype.getBoundingClientRect` (NOT the
      // individual menu element) so the layout effect sees the
      // overflow rect on its initial mount. `top` must be > MARGIN
      // (4px) or `maxShift` clamps to 0 and the effect bails out —
      // the realistic case is a menu positioned mid-screen whose
      // bottom extends past the viewport.
      rectSpy = vi
        .spyOn(HTMLElement.prototype, 'getBoundingClientRect')
        .mockReturnValue({
          top: 400,
          bottom: window.innerHeight + 200,   // 200 px past viewport bottom
          left: 0,
          right: 200,
          width: 200,
          height: 200,
          x: 0,
          y: 400,
          toJSON: () => ({}),
        } as DOMRect);

      render(<ProviderDropdown meshId={1} providers={PROVIDERS} onSelect={() => {}} />);
      const menu = document.querySelector('[data-dropdown-for="1"]') as HTMLElement;
      expect(menu.style.transform).toMatch(/translateY\(-/);
    });

    it('does not apply translateY when the menu fits in the viewport', () => {
      // Rect fits comfortably inside the viewport.
      rectSpy = vi
        .spyOn(HTMLElement.prototype, 'getBoundingClientRect')
        .mockReturnValue({
          top: 100,
          bottom: 200,
          left: 0,
          right: 200,
          width: 200,
          height: 100,
          x: 0,
          y: 100,
          toJSON: () => ({}),
        } as DOMRect);

      render(<ProviderDropdown meshId={1} providers={PROVIDERS} onSelect={() => {}} />);
      const menu = document.querySelector('[data-dropdown-for="1"]') as HTMLElement;
      // No overflow → no transform applied.
      expect(menu.style.transform).toBe('');
    });
  });
});
