/**
 * GroupedProviderMenu — harness-grouped Spawn Menu (issue #575 / ADR-0016).
 *
 * The single backend-derived `listProviders()` list is rendered as a
 * harness-grouped, always-expanded flat list: each `group_key` (==
 * `harness_id`) bucket gets a clickable harness header and a flat list
 * of Proxied children underneath. No hover submenus, no click-to-collapse.
 *
 * These tests pin the render shape and the click handler wiring. The
 * backend `compose_provider_menu` test (in `commands/agent.rs`) pins
 * the data derivation; this file is the pure-render counterpart.
 */
import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
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
    const header = screen.getByRole('button', { name: /claude/ });
    expect(header.getAttribute('data-spawn-harness')).toBe('claude');
    expect(header.getAttribute('data-spawn-id')).toBe('claude');
    const child = screen.getByRole('button', { name: /minimax/ });
    expect(child.getAttribute('data-spawn-harness')).toBe('claude');
    expect(child.getAttribute('data-spawn-id')).toBe('claude:minimax');
  });

  it('invokes onSelect with the row id (native or composite) on click', async () => {
    const onSelect = vi.fn();
    const providers = [native('claude'), proxied('claude', 'minimax')];
    render(<GroupedProviderMenu providers={providers} onSelect={onSelect} />);

    await userEvent.click(screen.getByRole('button', { name: /minimax/ }));
    expect(onSelect).toHaveBeenCalledWith('claude:minimax', false);

    await userEvent.click(screen.getByRole('button', { name: /claude/ }));
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

    fireEvent.click(screen.getByRole('button', { name: /claude/ }), { altKey: true });
    expect(onSelect).toHaveBeenCalledWith('claude', true);
  });

  it('renders nothing when the provider list is empty', () => {
    render(<GroupedProviderMenu providers={[]} onSelect={() => {}} />);
    expect(screen.queryAllByRole('button')).toHaveLength(0);
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
    expect(screen.getAllByRole('button')).toHaveLength(1);
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
    expect(screen.getAllByRole('button')).toHaveLength(2);
  });
});
