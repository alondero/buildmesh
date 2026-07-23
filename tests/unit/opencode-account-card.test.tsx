import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, waitFor, act, cleanup } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { OpenCodeAccountCard } from '../../src/components/AppSettings/OpenCodeAccountCard';

// `@tauri-apps/plugin-opener`'s `openUrl` shells out to the OS to open an
// external URL. `vi.hoisted` so the mock factory can capture the spy ref
// before the `vi.mock` call hoists the module replacement. Copied verbatim
// from `tests/unit/safe-link.test.tsx:29-48` (the canonical pattern).
const { openUrlMock } = vi.hoisted(() => ({
  openUrlMock: vi.fn<[], Promise<void>>().mockResolvedValue(undefined),
}));
vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: openUrlMock,
}));

/**
 * Per-command invoke routing. Each callable handler defaults to a benign
 * empty/never-resolves shape — only the test mutates the implementation for
 * the commands it exercises. Tracks call counts so the suite can assert
 * "we called `start_device_flow_console` exactly once, not twice".
 *
 * Reuses the same shape as `tests/unit/authorized-devices-settings.test.tsx:20-78`.
 *
 * Note: this codebase uses `.textContent` + `.toBeTruthy()` for DOM assertions
 * (rather than `@testing-library/jest-dom`'s `toHaveTextContent` /
 * `toHaveValue` — jest-dom isn't installed). Tests read DOM properties
 * directly.
 */
function mockBackend() {
  const calls: Record<string, unknown[]> = {};
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    calls[cmd] = [...(calls[cmd] ?? []), args];
    switch (cmd) {
      case 'start_device_flow_console':
        return Promise.resolve({
          device_code: 'dc_test',
          user_code: 'ABCD-1234',
          verification_uri_complete: 'https://console.opencode.ai/auth/device?code=ABCD-1234',
          interval_secs: 5,
          expires_in_secs: 600,
        });
      case 'poll_opencode_device_token':
        return Promise.resolve({ kind: 'pending' });
      case 'list_opencode_workspaces':
        return Promise.resolve([
          { id: 'wrk_a', name: 'Acme' },
          { id: 'wrk_b', name: 'Beta' },
        ]);
      case 'persist_opencode_tokens':
        return Promise.resolve(undefined);
      case 'revoke_opencode_console':
        return Promise.resolve(undefined);
      default:
        return Promise.resolve({});
    }
  });
  return calls;
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  openUrlMock.mockReset();
  openUrlMock.mockResolvedValue(undefined);
});

// Critical: each test must unmount its OpenCodeAccountCard before the next
// test, otherwise the previous test's `setInterval` polling loop keeps firing
// and re-dispatches into a stale component instance — which collides with
// the new test's expected state and trips the "Cannot read properties of
// undefined" error in the polling effect's `state.deviceCode` read.
// testing-library/react auto-cleanup is gated on env detection; explicit
// `cleanup()` is cheap insurance.
afterEach(() => {
  cleanup();
});

describe('OpenCodeAccountCard (issue #969)', () => {
  it('renders the signedOut branch with a Sign-in button on mount', () => {
    const calls = mockBackend();
    render(<OpenCodeAccountCard />);
    expect(screen.getByRole('button', { name: /sign in with opencode console/i })).toBeTruthy();
    expect(calls['start_device_flow_console']).toBeUndefined();
    expect(openUrlMock).not.toHaveBeenCalled();
  });

  it('Sign-in click invokes start_device_flow_console and opens the verification URL', async () => {
    const calls = mockBackend();
    const user = userEvent.setup();
    render(<OpenCodeAccountCard />);

    await user.click(screen.getByTestId('opencode-sign-in'));

    await waitFor(() => {
      const code = screen.getByTestId('opencode-user-code');
      expect(code.textContent).toBe('ABCD-1234');
    });

    expect(calls['start_device_flow_console']).toHaveLength(1);
    expect(openUrlMock).toHaveBeenCalledWith(
      'https://console.opencode.ai/auth/device?code=ABCD-1234',
    );

    // Issue #1010 wire-contract pin: the first poll must carry the
    // ORIGINAL window length under the renamed key, NOT a per-tick
    // countdown. The Rust gate treats this arg as the immutable
    // lifetime — passing a countdown here made a 600s code expire at
    // ~300s. The reducer test pins the field; this assertion pins the
    // IPC arg key + value the component sends.
    await waitFor(() => {
      expect(calls['poll_opencode_device_token']).toBeDefined();
      expect(calls['poll_opencode_device_token']!.length).toBeGreaterThanOrEqual(1);
    });
    const firstPollArgs = calls['poll_opencode_device_token']![0] as Record<string, unknown>;
    expect(firstPollArgs.originalExpiresInSecs).toBe(600);
    expect(firstPollArgs.deviceCode).toBe('dc_test');
    expect(firstPollArgs.startedAtMs).toEqual(expect.any(Number));
  });

  it('a successful poll transitions to signedIn after persist + workspaces resolve', async () => {
    const calls = mockBackend();
    // Override poll to return a Success token on the first call.
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      calls[cmd] = [...(calls[cmd] ?? []), args];
      switch (cmd) {
        case 'start_device_flow_console':
          return Promise.resolve({
            device_code: 'dc_poll',
            user_code: 'POLL-9999',
            verification_uri_complete: 'https://console.opencode.ai/auth/device?code=POLL-9999',
            interval_secs: 5,
            expires_in_secs: 600,
          });
        case 'poll_opencode_device_token':
          return Promise.resolve({
            kind: 'success',
            token: {
              access_token: 'at_poll',
              refresh_token: 'rt_poll',
              expires_in_secs: 600,
            },
          });
        case 'list_opencode_workspaces':
          return Promise.resolve([{ id: 'wrk_a', name: 'Acme' }]);
        case 'persist_opencode_tokens':
          return Promise.resolve(undefined);
        case 'revoke_opencode_console':
          return Promise.resolve(undefined);
        default:
          return Promise.resolve({});
      }
    });

    const user = userEvent.setup();
    render(<OpenCodeAccountCard />);

    await user.click(screen.getByTestId('opencode-sign-in'));

    await waitFor(() => {
      const name = screen.getByTestId('opencode-workspace-name');
      expect(name.textContent).toBe('Acme');
    });

    expect(calls['persist_opencode_tokens']).toHaveLength(1);
    expect(calls['list_opencode_workspaces']).toHaveLength(1);
    expect(screen.getByTestId('opencode-sign-out')).toBeTruthy();
  });

  it('passes the freshly-polled access_token into list_opencode_workspaces on first sign-in', async () => {
    // Regression pin: the first-time sign-in flow has NO credential in
    // Windows Credential Manager yet (the token was just polled, not yet
    // persisted). The IPC must use the polled access_token instead of
    // reading the missing credential — otherwise the workspace list comes
    // back empty, the persisted workspace_id is None, and the live
    // `_server billing.get` probe refuses to dispatch. This is the
    // spec-class bug the opencode-account-card.test.tsx flow test once
    // missed (the manual UI shell covered the success path but the
    // backend was passing None to persist).
    const calls = mockBackend();
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      calls[cmd] = [...(calls[cmd] ?? []), args];
      switch (cmd) {
        case 'start_device_flow_console':
          return Promise.resolve({
            device_code: 'dc_tokenpass',
            user_code: 'PASS-1234',
            verification_uri_complete: 'https://console.opencode.ai/auth/device?code=PASS-1234',
            interval_secs: 5,
            expires_in_secs: 600,
          });
        case 'poll_opencode_device_token':
          return Promise.resolve({
            kind: 'success',
            token: {
              access_token: 'at_first_sign_in',
              refresh_token: 'rt_first_sign_in',
              expires_in_secs: 600,
            },
          });
        case 'list_opencode_workspaces':
          return Promise.resolve([{ id: 'wrk_first', name: 'First Workspace' }]);
        case 'persist_opencode_tokens':
          return Promise.resolve(undefined);
        default:
          return Promise.resolve({});
      }
    });
    const user = userEvent.setup();
    render(<OpenCodeAccountCard />);

    await user.click(screen.getByTestId('opencode-sign-in'));
    await waitFor(() => {
      expect(screen.getByTestId('opencode-workspace-name')).toBeTruthy();
    });

    // The list call MUST carry the access_token; the persist call MUST
    // carry the workspace_id sourced from the list response.
    const listArgs = calls['list_opencode_workspaces']![0] as Record<string, unknown>;
    expect(listArgs.accessToken).toBe('at_first_sign_in');
    const persistArgs = calls['persist_opencode_tokens']![0] as Record<string, unknown>;
    expect(persistArgs.workspaceId).toBe('wrk_first');
  });

  it('Sign Out two-step flow calls revoke_opencode_console and returns to signedOut', async () => {
    const calls = mockBackend();
    // Override ONLY the poll call. Re-routing the impl removes the
    // mockBackend()'s built-in tracking, so this wrapper re-pushes to the
    // same `calls` record to keep the assertions at the end of the test
    // honest about which commands fired.
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      calls[cmd] = [...(calls[cmd] ?? []), args];
      if (cmd === 'start_device_flow_console') {
        return Promise.resolve({
          device_code: 'dc_out',
          user_code: 'OUT-7777',
          verification_uri_complete: 'https://console.opencode.ai/auth/device?code=OUT-7777',
          interval_secs: 5,
          expires_in_secs: 600,
        });
      }
      if (cmd === 'poll_opencode_device_token') {
        return Promise.resolve({
          kind: 'success',
          token: {
            access_token: 'at_out',
            refresh_token: 'rt_out',
            expires_in_secs: 600,
          },
        });
      }
      if (cmd === 'list_opencode_workspaces') {
        return Promise.resolve([{ id: 'wrk_a', name: 'Acme' }]);
      }
      return Promise.resolve(undefined);
    });
    const user = userEvent.setup();
    render(<OpenCodeAccountCard />);

    await user.click(screen.getByTestId('opencode-sign-in'));
    await waitFor(() => {
      const name = screen.getByTestId('opencode-workspace-name');
      expect(name.textContent).toBe('Acme');
    });

    await user.click(screen.getByTestId('opencode-sign-out'));
    await user.click(screen.getByTestId('opencode-sign-out'));

    await waitFor(() => {
      expect(screen.queryByTestId('opencode-sign-in')).toBeTruthy();
    });
    expect(calls['revoke_opencode_console']).toHaveLength(1);
  });

  it('Sign Out failure rolls back to signedIn with the previous workspace preserved', async () => {
    mockBackend();
    let pollOnce = true;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'start_device_flow_console') {
        return Promise.resolve({
          device_code: 'dc_rollback',
          user_code: 'ROLL-2222',
          verification_uri_complete:
            'https://console.opencode.ai/auth/device?code=ROLL-2222',
          interval_secs: 5,
          expires_in_secs: 600,
        });
      }
      if (cmd === 'poll_opencode_device_token' && pollOnce) {
        pollOnce = false;
        return Promise.resolve({
          kind: 'success',
          token: {
            access_token: 'at_rollback',
            refresh_token: 'rt_rollback',
            expires_in_secs: 600,
          },
        });
      }
      if (cmd === 'list_opencode_workspaces') {
        return Promise.resolve([
          { id: 'wrk_a', name: 'Acme' },
          { id: 'wrk_b', name: 'Beta' },
        ]);
      }
      if (cmd === 'revoke_opencode_console') {
        return Promise.reject(
          new Error('Credential store temporarily unavailable'),
        );
      }
      return Promise.resolve(undefined);
    });

    const user = userEvent.setup();
    render(<OpenCodeAccountCard />);

    await user.click(screen.getByTestId('opencode-sign-in'));
    await waitFor(() => {
      const name = screen.getByTestId('opencode-workspace-name');
      expect(name.textContent).toBe('Acme');
    });

    // Two-step Sign Out.
    await user.click(screen.getByTestId('opencode-sign-out'));
    await user.click(screen.getByTestId('opencode-sign-out'));

    await waitFor(() => {
      // After rollback we're still signedIn.
      expect(screen.getByTestId('opencode-workspace-name')).toBeTruthy();
    });
    expect(screen.queryByTestId('opencode-sign-in')).toBeNull();
  });

  it('start_device_flow_console rejection renders the error branch with a retry button', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'start_device_flow_console') {
        return Promise.reject(new Error('network unreachable'));
      }
      return Promise.resolve({});
    });
    const user = userEvent.setup();
    render(<OpenCodeAccountCard />);

    await user.click(screen.getByTestId('opencode-sign-in'));

    await waitFor(() => {
      const err = screen.getByTestId('opencode-error');
      expect(err.textContent ?? '').toMatch(/network unreachable/i);
    });
    expect(screen.getByTestId('opencode-retry')).toBeTruthy();
    expect(openUrlMock).not.toHaveBeenCalled();
  });

  it('awaitingActivation view displays the verification URL as text so the user can copy/paste it as a fallback', async () => {
    // Regression pin for "the OpenCode verification thingy doesn't open a
    // browser window, nor does clicking the link either that is shown"
    // (issue tracked at the top of this fix). The card MUST surface the
    // verification URL as plain text in the awaitingActivation branch so:
    //   - if `openUrl()` fails (capability drift, OS default-browser
    //     mis-config), the user can still copy the URL and paste it into a
    //     browser manually;
    //   - the URL is visible at a glance instead of being inferred from
    //     the "console.opencode.ai" string in the signedOut prompt;
    //   - screen readers announce the destination alongside the user code.
    mockBackend();
    const user = userEvent.setup();
    render(<OpenCodeAccountCard />);

    await user.click(screen.getByTestId('opencode-sign-in'));

    const url = await screen.findByTestId('opencode-verification-url');
    expect(url.textContent).toBe(
      'https://console.opencode.ai/auth/device?code=ABCD-1234',
    );
  });

  it('awaitingActivation view renders the verification URL as a clickable anchor (not just a button)', async () => {
    // The canonical Tauri-2-safe external-link pattern (see SafeLink.tsx
    // file header) is `<a href={url} onClick={(e) => { e.preventDefault();
    // openUrl(url); }}>`. The dual route lets right-click → "Open in
    // browser", ⌘-click, and assistive tech all work even when the JS
    // open fails. A bare `<button onClick={openUrl}>` would lose every
    // one of those fallbacks — which is exactly the regression that
    // surfaced in production: clicking the button did nothing AND there
    // was no visible URL to fall back to.
    mockBackend();
    const user = userEvent.setup();
    render(<OpenCodeAccountCard />);

    await user.click(screen.getByTestId('opencode-sign-in'));

    const link = await screen.findByTestId('opencode-verification-link');
    expect(link.tagName).toBe('A');
    expect(link.getAttribute('href')).toBe(
      'https://console.opencode.ai/auth/device?code=ABCD-1234',
    );
    expect(link.getAttribute('target')).toBe('_blank');
    expect(link.getAttribute('rel')).toBe('noopener noreferrer');

    // Pin the click contract: clicking the anchor must route through
    // `openUrl` (the documented SafeLink behaviour). Pre-fix this was a
    // bare `<button onClick>` — same call shape but the user had no
    // right-click / ⌘-click fallback if the JS open failed.
    openUrlMock.mockClear();
    await user.click(link);
    expect(openUrlMock).toHaveBeenCalledWith(
      'https://console.opencode.ai/auth/device?code=ABCD-1234',
    );
  });

  it('workspace picker dropdown appears when >1 workspaces and reacts to selection', async () => {
    let pollOnce = true;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'start_device_flow_console') {
        return Promise.resolve({
          device_code: 'dc_picker',
          user_code: 'PICK-1111',
          verification_uri_complete: 'https://console.opencode.ai/auth/device?code=PICK-1111',
          interval_secs: 5,
          expires_in_secs: 600,
        });
      }
      if (cmd === 'poll_opencode_device_token' && pollOnce) {
        pollOnce = false;
        return Promise.resolve({
          kind: 'success',
          token: {
            access_token: 'at_picker',
            refresh_token: 'rt_picker',
            expires_in_secs: 600,
          },
        });
      }
      if (cmd === 'list_opencode_workspaces') {
        return Promise.resolve([
          { id: 'wrk_a', name: 'Acme' },
          { id: 'wrk_b', name: 'Beta' },
        ]);
      }
      return Promise.resolve(undefined);
    });

    const user = userEvent.setup();
    render(<OpenCodeAccountCard />);

    await user.click(screen.getByTestId('opencode-sign-in'));

    const picker = await screen.findByTestId<HTMLSelectElement>(
      'opencode-workspace-picker',
    );
    expect(picker.value).toBe('wrk_a');

    // Switch to Beta — this dispatches WORKSPACE_CHOSEN which is purely
    // visual state (no IPC), so we can assert the value flips.
    await act(async () => {
      await user.selectOptions(picker, 'wrk_b');
    });
    await waitFor(() => {
      const name = screen.getByTestId('opencode-workspace-name');
      expect(name.textContent).toBe('Beta');
    });
  });
});
