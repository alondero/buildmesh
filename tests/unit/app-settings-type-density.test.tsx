import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import { openSettingsPane } from '../utils/settings-panes';
import { __resetProviderCachesForTests } from '../../src/lib/tauri';
import type { ProviderInfo } from '../../src/lib/tauri';
import { setTypeDensity } from '../../src/lib/typeDensity';
import { setWindowZoom } from '../../src/lib/webviewZoom';

/**
 * Text density and window zoom controls (Settings → General → Appearance).
 *
 * Renders the real AppSettingsModal and drives the two new controls the way a
 * user does, asserting the observable surfaces: the `data-type-density`
 * attribute the compact ladder keys off, the localStorage value that survives
 * a relaunch, and the native zoom factor the window zoom actually asks the
 * platform for.
 *
 * The stores' own behaviour (clamping, persistence, the CSS contract) is
 * pinned in `type-density.test.ts` and `webview-zoom.test.ts`; this file is
 * the wiring through the settings pane.
 */

const zoom = vi.hoisted(() => ({
  setWebviewZoomFactor: vi.fn(async (_factor: number) => {}),
  isTauriRuntime: vi.fn(() => true),
}));

vi.mock('../../src/lib/tauri/webview', () => ({
  isTauriRuntime: () => zoom.isTauriRuntime(),
  setWebviewZoomFactor: (factor: number) => zoom.setWebviewZoomFactor(factor),
}));

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

const DENSITY_ATTRIBUTE = 'data-type-density';

let user: ReturnType<typeof userEvent.setup>;

/**
 * Tab forward until `target` holds the focus.
 *
 * The modal is a long form, so the interesting property is not "the radio is
 * focusable" but "a keyboard can reach it" — that is what a user who never
 * touches the mouse needs. Bounded so a control that is genuinely unreachable
 * fails the test instead of hanging it.
 */
async function tabTo(target: HTMLElement, maxTabs = 120): Promise<boolean> {
  for (let i = 0; i < maxTabs; i += 1) {
    if (document.activeElement === target) return true;
    await user.tab();
  }
  return document.activeElement === target;
}

beforeEach(async () => {
  user = userEvent.setup();
  vi.mocked(invoke).mockReset();
  __resetProviderCachesForTests();
  mockBackendIpc();
  localStorage.clear();
  document.documentElement.removeAttribute(DENSITY_ATTRIBUTE);
  zoom.setWebviewZoomFactor.mockClear();
  zoom.setWebviewZoomFactor.mockImplementation(async () => {});
  zoom.isTauriRuntime.mockClear();
  zoom.isTauriRuntime.mockImplementation(() => true);
  // Both stores are module singletons. A previous test leaves the in-memory
  // value set, and a radio that is already checked does not fire `change` —
  // so each test has to start from the shipped values, or the control
  // silently does nothing.
  setTypeDensity('comfortable');
  await setWindowZoom(100);
});

afterEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute(DENSITY_ATTRIBUTE);
  setTypeDensity('comfortable');
  // The zoom singleton is reset in the next test's beforeEach, with the
  // resolving mock in place — a test that installs a failing mock must not
  // leave it armed for the teardown.
  zoom.setWebviewZoomFactor.mockReset();
  zoom.setWebviewZoomFactor.mockImplementation(async () => {});
  vi.restoreAllMocks();
});

function renderModal() {
  return render(<AppSettingsModal onClose={() => {}} />);
}

describe('AppSettingsModal — text density', () => {
  it('offers comfortable and compact, with comfortable selected by default', async () => {
    renderModal();
    await openSettingsPane(/^General$/);

    const comfortable = await screen.findByTestId('type-density-radio-comfortable');
    const compact = screen.getByTestId('type-density-radio-compact');
    expect((comfortable as HTMLInputElement).checked).toBe(true);
    expect((compact as HTMLInputElement).checked).toBe(false);
  });

  it('choosing compact switches the app onto the compact ladder and persists it', async () => {
    renderModal();
    await openSettingsPane(/^General$/);

    fireEvent.click(await screen.findByTestId('type-density-radio-compact'));

    await waitFor(() => {
      expect(document.documentElement.getAttribute(DENSITY_ATTRIBUTE)).toBe('compact');
    });
    expect(localStorage.getItem('buildmesh.type-density')).toBe('compact');
    expect((screen.getByTestId('type-density-radio-compact') as HTMLInputElement).checked).toBe(true);
  });

  it('going back to comfortable clears the attribute and the stored choice', async () => {
    renderModal();
    await openSettingsPane(/^General$/);

    fireEvent.click(await screen.findByTestId('type-density-radio-compact'));
    await waitFor(() => {
      expect(document.documentElement.getAttribute(DENSITY_ATTRIBUTE)).toBe('compact');
    });

    fireEvent.click(screen.getByTestId('type-density-radio-comfortable'));
    await waitFor(() => {
      expect(document.documentElement.getAttribute(DENSITY_ATTRIBUTE)).toBeNull();
    });
    expect(localStorage.getItem('buildmesh.type-density')).toBeNull();
  });

  it('is not a dirty draft — no discard prompt when Settings closes', async () => {
    renderModal();
    await openSettingsPane(/^General$/);
    fireEvent.click(await screen.findByTestId('type-density-radio-compact'));
    await waitFor(() => {
      expect(document.documentElement.getAttribute(DENSITY_ATTRIBUTE)).toBe('compact');
    });
    expect(screen.queryByText(/Discard unsaved changes/i)).toBeNull();
  });
});

describe('AppSettingsModal — window zoom', () => {
  it('offers 100% to 200%', async () => {
    renderModal();
    await openSettingsPane(/^General$/);

    expect(await screen.findByTestId('window-zoom-radio-100')).toBeTruthy();
    expect(screen.getByTestId('window-zoom-radio-150')).toBeTruthy();
    expect(screen.getByTestId('window-zoom-radio-200')).toBeTruthy();
    expect((screen.getByTestId('window-zoom-radio-100') as HTMLInputElement).checked).toBe(true);
  });

  it('choosing 200% asks the platform for a zoom factor of 2', async () => {
    renderModal();
    await openSettingsPane(/^General$/);

    fireEvent.click(await screen.findByTestId('window-zoom-radio-200'));

    await waitFor(() => {
      expect(zoom.setWebviewZoomFactor).toHaveBeenCalledWith(2);
    });
    expect(localStorage.getItem('buildmesh.window-zoom')).toBe('200');
  });

  it('tells the user when the platform refuses the zoom', async () => {
    zoom.setWebviewZoomFactor.mockImplementation(async () => {
      throw new Error('not supported on this platform');
    });
    renderModal();
    await openSettingsPane(/^General$/);

    fireEvent.click(await screen.findByTestId('window-zoom-radio-200'));

    const alert = await screen.findByTestId('window-zoom-error');
    expect(alert.textContent).toMatch(/not supported/i);
    // The window is still at 100%, so the control must not keep claiming
    // 200% and nothing may be stored that the window is not at.
    await waitFor(() => {
      expect((screen.getByTestId('window-zoom-radio-100') as HTMLInputElement).checked).toBe(true);
    });
    expect((screen.getByTestId('window-zoom-radio-200') as HTMLInputElement).checked).toBe(false);
    expect(localStorage.getItem('buildmesh.window-zoom')).toBeNull();
  });

  it('the density and zoom radios are reachable with the keyboard', async () => {
    renderModal();
    await openSettingsPane(/^General$/);

    const comfortable = await screen.findByTestId('type-density-radio-comfortable');
    const zoom100 = screen.getByTestId('window-zoom-radio-100');

    // Both are real radio inputs inside a named fieldset, so a keyboard
    // reaches them and a screen reader announces the group.
    expect(comfortable.closest('fieldset')?.getAttribute('aria-label')).toBe('Text density');
    expect(zoom100.closest('fieldset')?.getAttribute('aria-label')).toBe('Window zoom');

    expect(await tabTo(comfortable)).toBe(true);
    expect(document.activeElement).toBe(comfortable);
    expect(await tabTo(zoom100)).toBe(true);
    expect(document.activeElement).toBe(zoom100);
  });

  it('the keyboard can choose a density', async () => {
    renderModal();
    await openSettingsPane(/^General$/);

    const comfortable = await screen.findByTestId('type-density-radio-comfortable');
    expect(await tabTo(comfortable)).toBe(true);

    await user.keyboard(' ');

    await waitFor(() => {
      expect(document.documentElement.getAttribute(DENSITY_ATTRIBUTE)).toBeNull();
    });
    expect((screen.getByTestId('type-density-radio-comfortable') as HTMLInputElement).checked).toBe(true);
  });
});
