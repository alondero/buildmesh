import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import { openSettingsPane } from '../utils/settings-panes';
import { terminalManager } from '../../src/components/Terminal/Terminal';
import { buildRunTerminalManager } from '../../src/components/Terminal/BuildRunTerminalRegistry';
import { __resetProviderCachesForTests } from '../../src/lib/tauri';
import type { ProviderInfo } from '../../src/lib/tauri';

/**
 * End-to-end theme toggle (issue #734).
 *
 * Renders the real AppSettingsModal, clicks the Light radio in the
 * General pane, and asserts the three observable surfaces flip in
 * lockstep: <html data-theme>, both xterm registries' terminal
 * palettes, and localStorage. This is the acceptance-criterion smoke
 * — "Tests: assert [data-theme='light'] is set/cleared; render a key
 * component and snapshot class resolution."
 *
 * Per-command IPC mocks below mirror the pattern in
 * app-settings-harness-order.test.tsx: the global vitest.setup.ts mock
 * returns `{}` for everything, but AppSettingsModal calls
 * `providers.map(...)` (line 1025) — an empty object would throw at
 * first render. We override the commands the modal actually fires on
 * mount.
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
          autopilot_pool_size: null,
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

// JSDOM doesn't ship matchMedia; some focus utilities query it. Polyfill
// before vi.spyOn would have a function to spy on.
if (typeof window.matchMedia !== 'function') {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
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

// JSDOM doesn't implement ResizeObserver; the TerminalRegistry creates
// one inside attachToDOM when the modal-mount tests open a terminal.
// Same polyfill pattern as terminal-registry.test.ts — no-op methods
// are enough because the theme tests don't observe resize events.
globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof ResizeObserver;

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  __resetProviderCachesForTests();
  mockBackendIpc();
  localStorage.clear();
  document.documentElement.removeAttribute('data-theme');
  // Both registries ship module-level singletons. Reset the underlying
  // theme state by re-flipping to dark so each test starts clean.
  terminalManager.applyTheme('dark');
  buildRunTerminalManager.applyTheme('dark');
});

afterEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute('data-theme');
  vi.restoreAllMocks();
});

function renderModal() {
  return render(<AppSettingsModal onClose={() => {}} />);
}

describe('AppSettingsModal — Appearance theme picker (#734)', () => {
  it('renders both Light and Dark radios in the General pane', async () => {
    renderModal();
    // The Appearance picker is inside the General pane (which is the
    // default activeTab). The radios carry data-testid so tests don't
    // have to scope by label.
    expect(await screen.findByTestId('theme-radio-light')).toBeTruthy();
    expect(screen.getByTestId('theme-radio-dark')).toBeTruthy();
  });

  it('clicking the Light radio sets data-theme="light" and writes localStorage', async () => {
    renderModal();
    // Wait for the modal to mount its radios (the load effect races
    // with the first paint; openSettingsPane is the canonical wait).
    await openSettingsPane(/^General$/);
    const lightRadio = await screen.findByTestId('theme-radio-light');
    fireEvent.click(lightRadio);
    await waitFor(() => {
      expect(document.documentElement.getAttribute('data-theme')).toBe('light');
    });
    expect(localStorage.getItem('buildmesh.theme')).toBe('light');
  });

  it('clicking the Dark radio clears data-theme and removes localStorage', async () => {
    localStorage.setItem('buildmesh.theme', 'light');
    renderModal();
    await openSettingsPane(/^General$/);
    // First flip to light (so the test isn't trivially passing from a
    // pre-set state), then back to dark.
    fireEvent.click(await screen.findByTestId('theme-radio-light'));
    await waitFor(() => {
      expect(document.documentElement.getAttribute('data-theme')).toBe('light');
    });
    fireEvent.click(screen.getByTestId('theme-radio-dark'));
    await waitFor(() => {
      expect(document.documentElement.hasAttribute('data-theme')).toBe(false);
    });
    expect(localStorage.getItem('buildmesh.theme')).toBeNull();
  });

  it('a freshly opened terminal picks up the active theme from the picker', async () => {
    // Open the modal, flip to light, then mount a terminal on each
    // registry. The new terminal's term.options.theme must already be
    // the light palette (createTerminalOptions reads currentTheme()
    // synchronously, so this isn't a pub/sub round-trip).
    renderModal();
    await openSettingsPane(/^General$/);
    fireEvent.click(await screen.findByTestId('theme-radio-light'));
    await waitFor(() => {
      expect(document.documentElement.getAttribute('data-theme')).toBe('light');
    });

    const agentContainer = document.createElement('div');
    const buildRunContainer = document.createElement('div');
    const agentInst = await terminalManager.attach(9001, agentContainer);
    const buildRunInst = await buildRunTerminalManager.attach(
      9002,
      'build',
      false,
      buildRunContainer,
    );
    expect(agentInst).not.toBeNull();
    expect(buildRunInst).not.toBeNull();
    expect(
      (agentInst as { term: { options: { theme: object } } }).term.options.theme,
    ).toMatchObject({ background: '#fafafa' });
    expect(
      (buildRunInst as { term: { options: { theme: object } } }).term.options.theme,
    ).toMatchObject({ background: '#fafafa' });

    terminalManager.dispose(9001);
    buildRunTerminalManager.dispose(9002, 'build', false);
  });

  it('flipping back to dark after light re-applies the dark palette to live terminals', async () => {
    // Open a terminal under dark, flip the modal to light, assert
    // the live terminal's theme updated via the ThemeManager pub/sub,
    // then flip back to dark and re-assert.
    const container = document.createElement('div');
    const inst = await terminalManager.attach(9003, container);
    expect(inst).not.toBeNull();
    const themeOf = () =>
      (inst as { term: { options: { theme: object } } }).term.options.theme;

    renderModal();
    await openSettingsPane(/^General$/);
    fireEvent.click(await screen.findByTestId('theme-radio-light'));
    await waitFor(() => {
      expect(themeOf()).toMatchObject({ background: '#fafafa' });
    });
    fireEvent.click(screen.getByTestId('theme-radio-dark'));
    await waitFor(() => {
      expect(themeOf()).toMatchObject({ background: '#09090f' });
    });

    terminalManager.dispose(9003);
  });

  it('the dirty-tracker is NOT engaged for theme flips (no discard banner)', async () => {
    // A theme flip is synchronous + local; a "Discard unsaved changes?"
    // prompt would be more confusing than helpful. Pinning the absence
    // here so a future regression that wires theme through siteDirtyChange
    // fails the gate.
    renderModal();
    await openSettingsPane(/^General$/);
    fireEvent.click(await screen.findByTestId('theme-radio-light'));
    await waitFor(() => {
      expect(document.documentElement.getAttribute('data-theme')).toBe('light');
    });
    // The Modal's dirty-state surface is the "Discard unsaved changes?"
    // banner — assert it's not present.
    expect(screen.queryByText(/Discard unsaved changes/i)).toBeNull();
  });
});