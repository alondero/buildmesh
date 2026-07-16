/**
 * GroupedProviderMenu — harness-grouped Spawn Menu (issue #575 / ADR-0016).
 *
 * The single backend-derived `listProviders()` list is rendered as a
 * harness-grouped, always-expanded flat list: each `group_key` (==
 * `harness_id`) bucket gets a clickable harness header and a flat list
 * of Proxied children underneath. No hover submenus, no click-to-collapse.
 *
 * These tests pin the render shape, the click handler wiring, and the
 * issue #814 WAI-ARIA menu contract (`role="menu"` / `role="menuitem"`,
 * roving tabindex, Escape + arrow-key nav, focus-move-in on mount).
 * The backend `compose_provider_menu` test (in `commands/agent.rs`)
 * pins the data derivation; this file is the pure-render counterpart.
 */
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { GroupedProviderMenu } from '../../src/components/Providers/GroupedProviderMenu';
import type { ProviderInfo } from '../../src/types/generated/ProviderInfo';

const native = (harnessId: string, id?: string): ProviderInfo => ({
  id: id ?? harnessId,
  label: harnessId,
  color: '#000',
  icon: 'X',
  resumable: true,
  harness_id: harnessId,
  provider_id: null,
  is_proxied: false,
  group_key: harnessId,
});

const proxied = (harnessId: string, providerId: string): ProviderInfo => ({
  id: `${harnessId}:${providerId}`,
  label: providerId,
  color: '#000',
  icon: 'X',
  resumable: true,
  harness_id: harnessId,
  provider_id: providerId,
  is_proxied: true,
  group_key: harnessId,
});

afterEach(() => cleanup());

describe('GroupedProviderMenu', () => {
  it('renders one harness group per distinct group_key (issue #575)', () => {
    const providers = [
      native('claude'),
      proxied('claude', 'minimax'),
      native('codex'),
      native('terminal'),
    ];
    const { container } = render(
      <GroupedProviderMenu providers={providers} onSelect={() => {}} />,
    );
    // One [data-spawn-group] div per group_key.
    const groups = container.querySelectorAll('[data-spawn-group]');
    const groupKeys = Array.from(groups).map((g) => g.getAttribute('data-spawn-group'));
    expect(groupKeys).toEqual(['claude', 'codex', 'terminal']);
  });

  it('clusters Proxied children under their native harness header', () => {
    const providers = [
      native('claude'),
      proxied('claude', 'minimax'),
      proxied('claude', 'kimi'),
      native('codex'),
    ];
    const { container } = render(
      <GroupedProviderMenu providers={providers} onSelect={() => {}} />,
    );
    const claudeGroup = container.querySelector('[data-spawn-group="claude"]');
    expect(claudeGroup).toBeTruthy();
    // All three Claude-related rows live inside the same group div.
    expect(claudeGroup!.querySelectorAll('button[data-spawn-id]')).toHaveLength(3);
    // And no Proxied child leaks out into the Codex group.
    const codexGroup = container.querySelector('[data-spawn-group="codex"]');
    expect(codexGroup!.querySelectorAll('button[data-spawn-id]')).toHaveLength(1);
  });

  it('marks the harness header button with data-spawn-harness for test targeting', () => {
    const providers = [native('claude'), proxied('claude', 'minimax')];
    render(<GroupedProviderMenu providers={providers} onSelect={() => {}} />);
    // Issue #814 — buttons are now `role="menuitem"` (not default
    // `role="button"`). The accessible name is still derived from the
    // button's text content; the regex matcher keeps the test robust to
    // the trailing "harness" badge on the native row.
    const header = screen.getByRole('menuitem', { name: /claude/i });
    expect(header.getAttribute('data-spawn-harness')).toBe('claude');
    expect(header.getAttribute('data-spawn-id')).toBe('claude');
    const child = screen.getByRole('menuitem', { name: /minimax/i });
    expect(child.getAttribute('data-spawn-harness')).toBe('claude');
    expect(child.getAttribute('data-spawn-id')).toBe('claude:minimax');
  });

  it('invokes onSelect with the row id (native or composite) on click', async () => {
    const onSelect = vi.fn();
    const providers = [native('claude'), proxied('claude', 'minimax')];
    render(<GroupedProviderMenu providers={providers} onSelect={onSelect} />);

    await userEvent.click(screen.getByRole('menuitem', { name: /minimax/i }));
    expect(onSelect).toHaveBeenCalledWith('claude:minimax', false);

    await userEvent.click(screen.getByRole('menuitem', { name: /claude/i }));
    expect(onSelect).toHaveBeenCalledWith('claude', false);
  });

  it('passes the altKey flag through to onSelect when alt-clicked', async () => {
    // `fireEvent.click` is the only way to set the `altKey` modifier
    // on the React synthetic event in jsdom — `userEvent.click`'s
    // `altKey: true` option doesn't propagate to `event.altKey`. The
    // other tests use `userEvent.click` because they don't care about
    // the modifier; the alt-click path is exercised here with
    // `fireEvent` for parity with `sidebar-node-creation-form.test.tsx`.
    const onSelect = vi.fn();
    const providers = [native('claude')];
    render(<GroupedProviderMenu providers={providers} onSelect={onSelect} />);

    fireEvent.click(screen.getByRole('menuitem', { name: /claude/i }), { altKey: true });
    expect(onSelect).toHaveBeenCalledWith('claude', true);
  });

  it('renders nothing when the provider list is empty', () => {
    render(<GroupedProviderMenu providers={[]} onSelect={() => {}} />);
    // Issue #814 — buttons now have `role="menuitem"`, so `getAllByRole`
    // for the default "button" role returns nothing even for an empty
    // list. Pin both the new role and the absence of any item.
    expect(screen.queryAllByRole('menuitem')).toHaveLength(0);
  });

  it('applies a filter before bucketing so non-matching rows are hidden', () => {
    const providers = [native('claude'), proxied('claude', 'minimax'), native('codex')];
    const { container } = render(
      <GroupedProviderMenu
        providers={providers}
        onSelect={() => {}}
        filter={(p) => p.is_proxied}
      />,
    );
    // The filter keeps only `claude:minimax`; the Claude group is the
    // only one left and contains exactly that one row. The native
    // harness header for `claude` is filtered out, so the bucket
    // should NOT render a "harness" badge (the only row in the
    // bucket is a Proxied child).
    expect(container.querySelector('[data-spawn-group="claude"]')).toBeTruthy();
    expect(container.querySelector('[data-spawn-group="codex"]')).toBeNull();
    expect(screen.getAllByRole('menuitem')).toHaveLength(1);
    // The one surviving row has no "harness" badge — it's a Proxied
    // child rendered without a header.
    const buttons = container.querySelectorAll('button[data-spawn-harness]');
    expect(buttons).toHaveLength(1);
    expect(buttons[0].textContent).not.toMatch(/harness/i);
  });

  it('does not mislabel a Proxied child as the harness header when the native row is filtered out', () => {
    // The defensive case from code-review finding B2: ArchivedNodesTab's
    // `resumable` filter could in theory remove the native claude row
    // while keeping a Proxied claude:minimax child. The bucket should
    // render the child WITHOUT the "harness" badge — it would be
    // misleading to label a proxied row as the native harness header.
    const providers = [
      native('claude', 'claude-native'),     // will be filtered out
      proxied('claude', 'minimax'),
      proxied('claude', 'kimi'),
    ];
    const { container } = render(
      <GroupedProviderMenu
        providers={providers}
        onSelect={() => {}}
        filter={(p) => p.is_proxied}
      />,
    );
    const group = container.querySelector('[data-spawn-group="claude"]');
    expect(group).toBeTruthy();
    // No harness header should be rendered.
    expect(group!.querySelector('.text-text-faint')).toBeNull();
    // Both children render as peers.
    expect(screen.getAllByRole('menuitem')).toHaveLength(2);
  });
});

describe('GroupedProviderMenu — WAI-ARIA menu semantics (issue #814)', () => {
  const PROVIDERS = [native('claude'), proxied('claude', 'minimax'), native('codex')];

  it('declares role="menu" on the root container with an accessible name', () => {
    render(<GroupedProviderMenu providers={PROVIDERS} onSelect={() => {}} />);
    const menu = screen.getByRole('menu', { name: /select a provider/i });
    expect(menu).toBeTruthy();
  });

  it('declares role="menuitem" on every interactive row', () => {
    render(<GroupedProviderMenu providers={PROVIDERS} onSelect={() => {}} />);
    // 3 rows: claude (native), claude:minimax (proxied), codex (native).
    expect(screen.getAllByRole('menuitem')).toHaveLength(3);
  });

  it('uses a roving tabindex so only the first menuitem is in the natural Tab order on mount', () => {
    render(<GroupedProviderMenu providers={PROVIDERS} onSelect={() => {}} />);
    const items = screen.getAllByRole('menuitem');
    // First row is `tabIndex=0` (the auto-focused item); the rest are
    // `tabIndex=-1` so Tab leaves the menu and Arrow keys cycle within it.
    expect(items[0].tabIndex).toBe(0);
    expect(items[1].tabIndex).toBe(-1);
    expect(items[2].tabIndex).toBe(-1);
  });

  it('auto-focuses the first menuitem on mount so keyboard nav has a starting point', () => {
    render(<GroupedProviderMenu providers={PROVIDERS} onSelect={() => {}} />);
    // The first menuitem receives document focus synchronously after
    // the menu commits (via useLayoutEffect). Reading document.activeElement
    // is the right shape for jsdom (the menuitem has been focused).
    const firstItem = screen.getAllByRole('menuitem')[0];
    expect(document.activeElement).toBe(firstItem);
  });

  it('moves focus to the next menuitem on ArrowDown with wrap-around', () => {
    render(<GroupedProviderMenu providers={PROVIDERS} onSelect={() => {}} />);
    const items = screen.getAllByRole('menuitem');
    fireEvent.keyDown(document.activeElement ?? document.body, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(items[1]);
    fireEvent.keyDown(document.activeElement ?? document.body, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(items[2]);
    // Wrap-around: ArrowDown from the last item loops to the first.
    fireEvent.keyDown(document.activeElement ?? document.body, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(items[0]);
  });

  it('moves focus to the previous menuitem on ArrowUp with wrap-around', () => {
    render(<GroupedProviderMenu providers={PROVIDERS} onSelect={() => {}} />);
    const items = screen.getAllByRole('menuitem');
    // From the first (auto-focused) item, ArrowUp wraps to the last.
    fireEvent.keyDown(document.activeElement ?? document.body, { key: 'ArrowUp' });
    expect(document.activeElement).toBe(items[2]);
    fireEvent.keyDown(document.activeElement ?? document.body, { key: 'ArrowUp' });
    expect(document.activeElement).toBe(items[1]);
    fireEvent.keyDown(document.activeElement ?? document.body, { key: 'ArrowUp' });
    expect(document.activeElement).toBe(items[0]);
  });

  it('Home and End jump focus to the first and last menuitems', () => {
    render(<GroupedProviderMenu providers={PROVIDERS} onSelect={() => {}} />);
    const items = screen.getAllByRole('menuitem');
    fireEvent.keyDown(document.activeElement ?? document.body, { key: 'End' });
    expect(document.activeElement).toBe(items[2]);
    fireEvent.keyDown(document.activeElement ?? document.body, { key: 'Home' });
    expect(document.activeElement).toBe(items[0]);
  });

  it('invokes onClose on Escape (the parent flips its isOpen to false)', () => {
    const onClose = vi.fn();
    render(
      <GroupedProviderMenu providers={PROVIDERS} onSelect={() => {}} onClose={onClose} />,
    );
    // Escape only fires while focus is inside the menu — the auto-focus
    // on mount guarantees that condition.
    fireEvent.keyDown(document.activeElement ?? document.body, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('ignores arrow / Escape keys when focus is outside the menu', () => {
    const onClose = vi.fn();
    render(
      <div>
        <button data-testid="outside">outside</button>
        <GroupedProviderMenu providers={PROVIDERS} onSelect={() => {}} onClose={onClose} />
      </div>,
    );
    // Move focus to the outside button — the menu's keyboard handler
    // gates on `menuRef.current.contains(document.activeElement)` and
    // should bail out.
    const outside = screen.getByTestId('outside');
    outside.focus();
    fireEvent.keyDown(outside, { key: 'Escape' });
    expect(onClose).not.toHaveBeenCalled();
  });

  it('updates the roving tabindex as focus moves so only the active item is in the Tab order', () => {
    render(<GroupedProviderMenu providers={PROVIDERS} onSelect={() => {}} />);
    const items = screen.getAllByRole('menuitem');
    fireEvent.keyDown(document.activeElement ?? document.body, { key: 'ArrowDown' });
    // After ArrowDown, index 1 is the active item; index 0 + 2 should
    // fall back to tabIndex=-1.
    expect(items[0].tabIndex).toBe(-1);
    expect(items[1].tabIndex).toBe(0);
    expect(items[2].tabIndex).toBe(-1);
  });
});
