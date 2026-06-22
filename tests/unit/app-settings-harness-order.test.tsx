/**
 * The Settings modal surfaces the spawn-menu harness reorder UI (issue #573)
 * when there are at least two orderable (non-Terminal) harnesses, and hides it
 * otherwise. The drag→persist path is covered by the `reorderIds` and
 * `setHarnessOrder` unit tests; here we pin the modal's wiring of the section.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
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
    // the absence of the section. Issue #574 renamed "Accounts & Usage" to
    // "Providers" — the modal is now organised around a Providers page.
    await screen.findByText('Providers');
    await waitFor(() => expect(screen.queryByText('Spawn menu order')).toBeNull());
  });
});
