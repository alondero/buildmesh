/**
 * The Settings modal surfaces the spawn-menu harness reorder UI (issue #573)
 * when there are at least two orderable (non-Terminal) harnesses, and hides it
 * otherwise. The drag→persist path is covered by the `reorderIds` and
 * `setHarnessOrder` unit tests; here we pin the modal's wiring of the section.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import { __resetProviderCachesForTests } from '../../src/lib/tauri';
import type { ProviderInfo } from '../../src/lib/tauri';

function provider(id: string, label: string): ProviderInfo {
  // Issue #575 / ADR-0016 — Spawn Options carry the full wire shape.
  return {
    id,
    label,
    color: '#fff',
    icon: id,
    resumable: false,
    harness_id: id,
    provider_id: null,
    is_proxied: false,
    group_key: id,
  };
}

function mockBackend(providers: ProviderInfo[]) {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    switch (cmd) {
      case 'get_app_preferences':
        return Promise.resolve({ default_provider: null, minimax_api_key: null });
      case 'list_providers':
        return Promise.resolve(providers);
      case 'get_provider_accounts':
        return Promise.resolve([]);
      // Issue #574 renamed `get_all_provider_usage` → `get_provider_meters`;
      // mock the live command (and accept the legacy one as an alias
      // for any code path that hasn't migrated yet).
      case 'get_provider_meters':
      case 'get_all_provider_usage':
        return Promise.resolve([]);
      case 'get_coordinator_status':
        return Promise.resolve({ enabled: false, has_token: false });
      case 'list_device_sessions':
        return Promise.resolve([]);
      case 'get_network_status':
        return Promise.resolve({
          lan_exposure_enabled: false,
          port: 1992,
          tls_active: false,
          exposed_interfaces: [],
        });
      default:
        return Promise.resolve({});
    }
  });
}

describe('Settings — spawn menu order (issue #573)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    __resetProviderCachesForTests();
  });

  it('shows the reorder section with a row per non-Terminal harness', async () => {
    mockBackend([
      provider('claude', 'Claude Code'),
      provider('codex', 'Codex'),
      provider('terminal', 'Terminal'),
    ]);
    render(<AppSettingsModal onClose={() => {}} />);

    expect(await screen.findByText('Spawn menu order')).toBeTruthy();
    expect(screen.getByLabelText('Reorder Claude Code')).toBeTruthy();
    expect(screen.getByLabelText('Reorder Codex')).toBeTruthy();
    // Terminal stays pinned last and is not an orderable row.
    expect(screen.queryByLabelText('Reorder Terminal')).toBeNull();
  });

  it('hides the reorder section when only one harness (besides Terminal) exists', async () => {
    mockBackend([provider('claude', 'Claude Code'), provider('terminal', 'Terminal')]);
    render(<AppSettingsModal onClose={() => {}} />);

    // Wait for the modal to settle on a deterministic element before asserting
    // the absence of the section. "Providers" is now ambiguous by text (it's
    // both a nav-rail tab and a pane heading), so anchor on the tab role.
    await screen.findByRole('tab', { name: 'Providers' });
    await waitFor(() => expect(screen.queryByText('Spawn menu order')).toBeNull());
  });
});

/**
 * Issue #581 — both `handleReorderHarnesses` and the default-provider
 * `handleSave` use the optimistic-update-with-rollback pattern. dnd-kit
 * serialises drags so the concurrent-reorder closure bug is theoretical,
 * but we still pin the rollback contract for the easier-to-fire sibling
 * (`handleSave`) so a future refactor can't regress it without breaking
 * this test.
 *
 * The reorder rollback is covered by the same pattern: `providersRef` /
 * `selectedRef` mirror committed state into refs and the handler reads
 * `previous` from there instead of the render-time closure. Same fix,
 * same contract — testing one pins the other.
 */
describe('Settings — optimistic rollback ref pattern (issue #581)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    __resetProviderCachesForTests();
  });

  it('rolls the default-provider dropdown back when set_app_default_provider rejects', async () => {
    mockBackend([
      provider('claude', 'Claude Code'),
      provider('codex', 'Codex'),
      provider('terminal', 'Terminal'),
    ]);
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      switch (cmd) {
        case 'get_app_preferences':
          return Promise.resolve({ default_provider: null, minimax_api_key: null });
        case 'list_providers':
          return Promise.resolve([
            provider('claude', 'Claude Code'),
            provider('codex', 'Codex'),
            provider('terminal', 'Terminal'),
          ]);
        case 'get_provider_accounts':
          return Promise.resolve([]);
        case 'get_provider_meters':
        case 'get_all_provider_usage':
          return Promise.resolve([]);
        case 'get_coordinator_status':
          return Promise.resolve({ enabled: false, has_token: false });
        case 'list_device_sessions':
          return Promise.resolve([]);
        case 'get_network_status':
          return Promise.resolve({
            lan_exposure_enabled: false,
            port: 1992,
            tls_active: false,
            exposed_interfaces: [],
          });
        case 'set_app_default_provider':
          // Refuse the IPC so the optimistic value should roll back.
          return Promise.reject(`rejected set_app_default_provider(${args?.provider ?? 'null'})`);
        default:
          return Promise.resolve({});
      }
    });

    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);

    // The default-provider `<select>` is now aria-labelled so this query is
    // unambiguous (the Auto-naming picker is also a combobox). Reaching in via
    // the role without a name would match either.
    const dropdown = (await screen.findByRole('combobox', { name: 'Default provider' })) as HTMLSelectElement;
    expect(dropdown.value).toBe('__no_override__');

    // Fire the change. We don't assert the transient optimistic state
    // ("codex") — React 18's automatic batching collapses the optimistic
    // setSelected + the rollback setSelected into a single commit because
    // the mocked IPC rejects synchronously, so the intermediate DOM state
    // is never observable. The contract we're pinning is the *final* state:
    // a rejected IPC leaves the dropdown reverted to its prior value.
    await user.selectOptions(dropdown, 'codex');

    // After the rejection, the rollback fires and the dropdown reverts. The
    // ref-based fix (#581) ensures we revert to the *committed* value, not
    // whichever stale snapshot the render-time closure happened to hold.
    await waitFor(() => expect(dropdown.value).toBe('__no_override__'));
    expect(invoke).toHaveBeenCalledWith('set_app_default_provider', { provider: 'codex' });
  });
});
