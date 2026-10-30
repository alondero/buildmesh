import { act } from 'react';
import { render, screen, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { describe, expect, it, vi } from 'vitest';
import { useProviderList } from '../../src/hooks/useProviderList';
import { PROVIDER_LIST_CHANGED_EVENT } from '../../src/lib/streams/providerListStream';
import { HARNESS_CAPABILITIES } from '../../src/types/generated/HarnessCapabilitiesTable';
import type { ProviderInfo } from '../../src/types/generated/ProviderInfo';

function option(id: 'terminal' | 'opencode2'): ProviderInfo {
  return {
    id, label: id === 'opencode2' ? 'OpenCode 2' : 'Terminal', color: '#f59e0b', icon: 'O',
    harness_id: id, group_key: id, provider_id: null, is_proxied: false,
    resumable: id === 'opencode2', capabilities: HARNESS_CAPABILITIES[id], configurations: [],
  };
}

function SpawnOptions() {
  return <ul>{useProviderList().map(option => <li key={option.id}>{option.label}</li>)}</ul>;
}

describe('deferred harness discovery', () => {
  it('refreshes the cached initial menu when the backend probe completes', async () => {
    let available = [option('terminal')];
    vi.mocked(invoke).mockImplementation(async command => command === 'list_providers' ? available : undefined);
    render(<SpawnOptions />);
    await screen.findByText('Terminal');
    expect(screen.queryByText('OpenCode 2')).toBeNull();
    available = [option('opencode2'), option('terminal')];
    await act(async () => { await emit(PROVIDER_LIST_CHANGED_EVENT); });
    await screen.findByText('OpenCode 2');
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === 'list_providers')).toHaveLength(2);
  });

  it('does not let a pre-probe response replace the newly detected harness', async () => {
    let resolveInitial!: (rows: ProviderInfo[]) => void;
    const initial = new Promise<ProviderInfo[]>(resolve => { resolveInitial = resolve; });
    let reads = 0;
    vi.mocked(invoke).mockImplementation(async command => {
      if (command !== 'list_providers') return undefined;
      return ++reads === 1 ? initial : [option('opencode2'), option('terminal')];
    });
    render(<SpawnOptions />);
    await waitFor(() => expect(reads).toBe(1));
    await act(async () => { await emit(PROVIDER_LIST_CHANGED_EVENT); });
    await screen.findByText('OpenCode 2');
    await act(async () => { resolveInitial([option('terminal')]); });
    expect(screen.getByText('OpenCode 2')).toBeTruthy();
    expect(reads).toBe(2);
  });
});
