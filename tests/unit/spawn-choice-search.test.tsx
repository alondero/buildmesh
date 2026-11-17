import { fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { GroupedProviderMenu } from '../../src/components/Providers/GroupedProviderMenu';
import { __resetHarnessLoginsForTests, publishHarnessLogins } from '../../src/hooks/useHarnessLogins';
import type { SpawnOption } from '../../src/lib/groups';
import type { HarnessLoginStatus } from '../../src/lib/tauri/provider';

function harness(id: string, label: string): SpawnOption {
  return { id, label, harness_id: id, provider_id: null, is_proxied: false, group_key: id, icon: id, color: '', runtime: 'windows' };
}

function recipe(parent: SpawnOption, id: string, name: string, loginId?: string): SpawnOption {
  return {
    ...parent, id, label: name,
    configuration: { id, name, spawn_option_id: parent.id, login_id: loginId, model: null, effort: null, extra_args: null },
  };
}

function login(id: string, label: string, signedIn = true): HarnessLoginStatus {
  return { id, label, harness_id: 'codex', home: '', is_default: id === 'default', signed_in: signedIn, email: null };
}

const claude = harness('claude', 'Claude Code');
const codex = harness('codex', 'Codex');
const terminal = harness('terminal', 'Terminal');
const claudeReview = recipe(claude, 'launch/claude-review', 'Review');
const codexReview = recipe(codex, 'launch/codex-review', 'Review', 'work');
const codexLegacy = { ...recipe(codex, 'launch/legacy', 'Review Legacy'), unavailable_reason: 'Model unavailable' };
const providers = [
  claude, ...Array.from({ length: 10 }, (_, i) => harness(`harness-${i}`, `Harness ${i + 1}`)), codex, terminal,
  claudeReview, codexReview, codexLegacy,
];

function searchBox() {
  return screen.getByRole('combobox', { name: 'Search launch choices' });
}

function activeChoice() {
  const id = searchBox().getAttribute('aria-activedescendant');
  return id ? document.getElementById(id) : null;
}

describe('shared launch choice search', () => {
  beforeEach(__resetHarnessLoginsForTests);

  it('types from a browse row and selects the exact configuration, parent and login', async () => {
    publishHarnessLogins([login('default', 'Default'), login('work', 'Work')]);
    const onSelect = vi.fn();
    const user = userEvent.setup();
    render(<GroupedProviderMenu providers={providers} onSelect={onSelect} />);

    expect(screen.getAllByRole('menuitem')).toHaveLength(13);
    await user.keyboard('ReViEw');
    expect(document.activeElement).toBe(searchBox());
    expect(screen.getAllByRole('option').map(option => option.getAttribute('aria-label'))).toEqual([
      'Review Claude Code', 'Review Legacy Codex · Default', 'Review Codex · Work', 'Review Legacy Codex · Work',
    ]);
    await user.keyboard('{End}');
    expect(activeChoice()).toBe(screen.getByRole('option', { name: 'Review Legacy Codex · Work', description: 'Model unavailable' }));
    await user.keyboard('{Enter}');
    expect(onSelect).not.toHaveBeenCalled();
    await user.keyboard('{Home}{ArrowUp}{ArrowDown}');
    expect(activeChoice()).toBe(screen.getByRole('option', { name: 'Review Claude Code' }));
    await user.keyboard('{ArrowDown}{ArrowDown}');
    expect(activeChoice()).toBe(screen.getByRole('option', { name: 'Review Codex · Work' }));

    await user.keyboard('{Control>}a{/Control} cOdEx   WORK  REVIEW');
    await user.keyboard('{Alt>}{Enter}{/Alt}');
    expect(onSelect).toHaveBeenCalledExactlyOnceWith('codex', true, 'launch/codex-review', 'work');
  });

  it('keeps login availability and the caller’s filter, and offers recovery when nothing matches', async () => {
    publishHarnessLogins([login('default', 'Default'), login('work', 'Work'), login('offline', 'Offline', false)]);
    const onManageConfigurations = vi.fn();
    const onClose = vi.fn();
    const onSelect = vi.fn();
    const user = userEvent.setup();
    render(<GroupedProviderMenu providers={providers} filter={option => option.harness_id === 'codex'}
      onClose={onClose} onSelect={onSelect} onManageConfigurations={onManageConfigurations} />);

    await user.type(searchBox(), 'offline');
    const offline = screen.getByRole('option', { name: /Offline Codex/ });
    expect(offline.getAttribute('aria-disabled')).toBe('true');
    expect(offline.textContent).toContain('Not signed in.');
    await user.keyboard('{Enter}');
    await user.click(offline);
    expect(onSelect).not.toHaveBeenCalled();
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('listbox')).toBeNull();
    expect(onClose).not.toHaveBeenCalled();

    await user.keyboard('claude review');
    expect(screen.getByRole('status').textContent).toContain('No matching launch choices');
    await user.tab();
    await user.keyboard('{Enter}');
    expect(onManageConfigurations).toHaveBeenCalledOnce();
    expect(onClose).toHaveBeenCalledOnce();
  });

  it('does not retarget Enter when an active configuration is deleted', async () => {
    const onSelect = vi.fn();
    const user = userEvent.setup();
    const { rerender } = render(<GroupedProviderMenu providers={[claude, codex, claudeReview, codexReview]} onSelect={onSelect} />);
    await user.keyboard('review');
    expect(activeChoice()).toBe(screen.getByRole('option', { name: 'Review Claude Code' }));
    rerender(<GroupedProviderMenu providers={[claude, codex, codexReview]} onSelect={onSelect} />);
    expect(screen.getAllByRole('option')).toHaveLength(1);
    expect(activeChoice()).toBeNull();
    await user.keyboard('{Enter}');
    expect(onSelect).not.toHaveBeenCalled();
    await user.keyboard('{ArrowDown}{Enter}');
    expect(onSelect).toHaveBeenCalledExactlyOnceWith('codex', false, 'launch/codex-review', undefined);
  });

  it('searches only resumable choices supplied by the adapter and keeps the frozen login', async () => {
    publishHarnessLogins([login('default', 'Default'), login('work', 'Work')]);
    const onSelect = vi.fn();
    const user = userEvent.setup();
    render(<GroupedProviderMenu mode="resume" providers={providers} onSelect={onSelect} />);
    await user.type(searchBox(), 'codex');
    expect(screen.getAllByRole('option')).toHaveLength(1);
    await user.keyboard('{Enter}');
    expect(onSelect).toHaveBeenCalledExactlyOnceWith('codex', false, undefined, undefined);
    fireEvent.change(searchBox(), { target: { value: 'review' } });
    expect(screen.getByRole('status').textContent).toContain('No matching launch choices');
  });

  it('returns focus from a login flyout without closing its surrounding dialog', async () => {
    publishHarnessLogins([login('default', 'Default'), login('work', 'Work')]);
    const onClose = vi.fn();
    const user = userEvent.setup();
    render(<div role="dialog">
      <GroupedProviderMenu providers={[codex]} onSelect={vi.fn()} onClose={onClose} closeOnTab={false} />
    </div>);
    const parent = screen.getByRole('menuitem', { name: 'Codex' });
    await user.keyboard('{ArrowRight}');
    expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'Default' }));
    await user.tab();
    expect(screen.queryByRole('menu', { name: 'Codex logins' })).toBeNull();
    expect(document.activeElement).toBe(parent);
    expect(onClose).not.toHaveBeenCalled();
  });
});
