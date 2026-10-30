import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import { openSettingsPane } from '../utils/settings-panes';
import { __resetProviderCachesForTests } from '../../src/lib/tauri';
import type { ProviderInfo } from '../../src/lib/tauri';
import {
  TEXT_SCALE_DEFAULT,
  TEXT_SCALE_MAX,
  TEXT_SCALE_MIN,
  setTextScale,
} from '../../src/lib/textScale';

/**
 * Interface text size control (Settings → General → Appearance).
 *
 * Renders the real AppSettingsModal and drives the slider the way a user does,
 * asserting the three observable surfaces: the `--ui-text-scale` custom
 * property the whole type scale multiplies by, the localStorage value that
 * survives a relaunch, and the percentage readout beside the slider. The store's
 * own behaviour (clamping, persistence, the CSS contract) is pinned separately
 * in `text-scale.test.ts`; this file is the wiring through the settings pane.
 *
 * Per-command IPC mocks mirror app-settings-theme-toggle.test.tsx: the global
 * vitest.setup.ts mock returns `{}` for everything, but AppSettingsModal calls
 * `providers.map(...)` — an empty object would throw at first render.
 */

function provider(id: string, label: string): ProviderInfo {
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
    capabilities: {
      harness_id: id,
      supports_resume: false,
      auto_resume_on_startup: false,
      requires_attention_hook: false,
      produces_readable_transcript: false,
      supports_model_override: false,
      supports_effort_override: false,
      supports_prefill: false,
      is_plain_terminal: id === 'terminal',
      effort_control: { kind: 'none' },
      available_on: ['windows'],
    },
  };
}

function mockBackendIpc() {
  const allProviders = [
    provider('anthropic', 'Anthropic'),
    provider('codex', 'Codex'),
    provider('terminal', 'Terminal'),
  ];
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    switch (cmd) {
      case 'get_app_preferences':
        return Promise.resolve({
          default_provider: null,
          minimax_api_key: null,
          google_cloud_project: null,
          naming_provider: null,
          circuit_agent_pool_size: null,
          harness_order: [],
          provider_pairings: [],
          harness_profiles: [],
          provider_accounts: [],
        });
      case 'list_providers':
        return Promise.resolve(allProviders);
      case 'get_provider_accounts':
        return Promise.resolve([]);
      case 'get_provider_pairings':
        return Promise.resolve([]);
      case 'compatible_providers_for_harness':
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
      default:
        return Promise.resolve({});
    }
  });
}

// JSDOM doesn't ship matchMedia; some focus utilities query it.
if (typeof window.matchMedia !== 'function') {
  (window as any).matchMedia = (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: vi.fn(),
    removeListener: vi.fn(),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    dispatchEvent: vi.fn(),
  });
}

globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof ResizeObserver;

const CSS_VAR = '--ui-text-scale';

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  __resetProviderCachesForTests();
  mockBackendIpc();
  localStorage.clear();
  // The store is a module singleton: put it back on the shipped size so each
  // test starts from what a fresh install renders.
  setTextScale(TEXT_SCALE_DEFAULT);
  document.documentElement.style.removeProperty(CSS_VAR);
});

afterEach(() => {
  localStorage.clear();
  // No `setTextScale` here: it notifies subscribers, and React Testing
  // Library's own cleanup has not unmounted the pane yet at this point. The
  // next test's beforeEach resets the singleton with nothing mounted.
  document.documentElement.style.removeProperty(CSS_VAR);
  vi.restoreAllMocks();
});

function renderModal() {
  return render(<AppSettingsModal onClose={() => {}} />);
}

describe('AppSettingsModal — interface text size', () => {
  it('renders a slider in the General pane, starting at the shipped size', async () => {
    renderModal();
    await openSettingsPane(/^General$/);
    const slider = (await screen.findByTestId('interface-text-scale-slider')) as HTMLInputElement;

    expect(slider.type).toBe('range');
    expect(slider.min).toBe(String(TEXT_SCALE_MIN * 100));
    expect(slider.max).toBe(String(TEXT_SCALE_MAX * 100));
    expect(slider.value).toBe(String(TEXT_SCALE_DEFAULT * 100));
    expect(slider.getAttribute('aria-valuetext')).toBe('100%');
    expect(screen.getByTestId('interface-text-scale-value').textContent).toBe('100%');
  });

  it('raising the slider rescales the app and persists the choice', async () => {
    renderModal();
    await openSettingsPane(/^General$/);
    const slider = await screen.findByTestId('interface-text-scale-slider');

    fireEvent.change(slider, { target: { value: '125' } });

    await waitFor(() => {
      // The whole type scale multiplies by this one property.
      expect(document.documentElement.style.getPropertyValue(CSS_VAR)).toBe('1.25');
    });
    expect(localStorage.getItem('buildmesh.text-scale')).toBe('125');
    expect(screen.getByTestId('interface-text-scale-value').textContent).toBe('125%');
    expect(slider.getAttribute('aria-valuetext')).toBe('125%');
  });

  it('Reset returns to the shipped size and clears the stored choice', async () => {
    renderModal();
    await openSettingsPane(/^General$/);

    const reset = (await screen.findByTestId('interface-text-scale-reset')) as HTMLButtonElement;
    // Nothing to reset at the default: the control says so rather than
    // looking like a live action.
    expect(reset.disabled).toBe(true);

    fireEvent.change(screen.getByTestId('interface-text-scale-slider'), {
      target: { value: '140' },
    });
    await waitFor(() => expect(reset.disabled).toBe(false));

    fireEvent.click(reset);
    await waitFor(() => {
      expect(document.documentElement.style.getPropertyValue(CSS_VAR)).toBe('');
    });
    expect(localStorage.getItem('buildmesh.text-scale')).toBeNull();
    expect(screen.getByTestId('interface-text-scale-value').textContent).toBe('100%');
    expect(reset.disabled).toBe(true);
  });

  it('shows a persisted size on open, so the control agrees with the app', async () => {
    setTextScale(1.3);
    renderModal();
    await openSettingsPane(/^General$/);
    const slider = (await screen.findByTestId('interface-text-scale-slider')) as HTMLInputElement;
    expect(slider.value).toBe('130');
    expect(screen.getByTestId('interface-text-scale-value').textContent).toBe('130%');
  });

  it('is not a dirty draft — no discard prompt when Settings closes', async () => {
    // Like the theme picker: the write is synchronous localStorage plus a
    // custom property, so there is no half-saved state for the modal to
    // protect with a confirmation.
    renderModal();
    await openSettingsPane(/^General$/);
    fireEvent.change(await screen.findByTestId('interface-text-scale-slider'), {
      target: { value: '120' },
    });
    await waitFor(() => {
      expect(document.documentElement.style.getPropertyValue(CSS_VAR)).toBe('1.2');
    });
    expect(screen.queryByText(/Discard unsaved changes/i)).toBeNull();
  });
});
