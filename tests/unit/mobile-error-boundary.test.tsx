/**
 * Mobile ErrorBoundary — regression test for the black-screen defect
 * identified in the mobile HTTPS investigation (issue: no existing boundary
 * wraps the mobile root, so any render-time throw unmounts to a silent black
 * page with zero diagnostic surface).
 *
 * Mirrors `tests/unit/error-boundary.test.tsx` (desktop boundary) but the
 * mobile boundary CANNOT use `@tauri-apps/api` (mobile SPA is pure fetch,
 * no Tauri runtime), so it POSTs to `/__debug/log` directly — the same
 * endpoint `assets::serve_spa_shell` already targets with the error-catcher
 * shim (`src-tauri/src/http/assets.rs:39-58`). Single transport, one log
 * sink, no Tauri dependency on the phone.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import { MobileErrorBoundary } from '../../src/mobile/ErrorBoundary';

function Bomb({ message }: { message: string }): never {
  throw new Error(message);
}

describe('MobileErrorBoundary', () => {
  let fetchMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    fetchMock = vi.fn().mockResolvedValue({ ok: true, status: 204 });
    vi.stubGlobal('fetch', fetchMock);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('renders children when no error', () => {
    render(
      <MobileErrorBoundary>
        <div>healthy</div>
      </MobileErrorBoundary>,
    );
    expect(screen.getByText('healthy')).toBeTruthy();
  });

  it('renders a fallback card with reload button when a child throws', () => {
    // Silence React's expected console.error from the caught throw.
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});

    render(
      <MobileErrorBoundary>
        <Bomb message="kaboom" />
      </MobileErrorBoundary>,
    );

    expect(screen.getByText(/Buildmesh hit a render error/i)).toBeTruthy();
    expect(screen.getByText(/kaboom/)).toBeTruthy();
    expect(screen.getByRole('button', { name: /reload/i })).toBeTruthy();

    spy.mockRestore();
  });

  it('POSTs the error to /__debug/log with kind=boundary + component stack', async () => {
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});

    render(
      <MobileErrorBoundary>
        <Bomb message="render-crash" />
      </MobileErrorBoundary>,
    );

    // componentDidCatch fires synchronously, but we yield once so the
    // fetch call has a chance to land.
    await Promise.resolve();

    const call = fetchMock.mock.calls.find(
      (c) => typeof c[0] === 'string' && c[0].endsWith('/__debug/log'),
    );
    expect(call, 'expected fetch POST to /__debug/log').toBeDefined();

    const init = call![1] as RequestInit;
    expect(init.method).toBe('POST');
    expect(JSON.stringify(init.headers)).toContain('application/json');

    const body = JSON.parse(init.body as string);
    expect(body.kind).toBe('boundary');
    expect(body.msg).toContain('render-crash');
    expect(body.msg).toContain('Component stack');
    // Sanity: name + stack ride along so the desktop log can correlate.
    expect(body.stack).toContain('render-crash');

    spy.mockRestore();
  });

  it('does not throw when the /__debug/log POST fails (best-effort)', async () => {
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
    fetchMock.mockRejectedValueOnce(new TypeError('network'));

    // Must not crash the boundary itself on a failed report.
    expect(() =>
      render(
        <MobileErrorBoundary>
          <Bomb message="report-fail" />
        </MobileErrorBoundary>,
      ),
    ).not.toThrow();

    // Fallback still renders even when the diagnostic POST fails.
    expect(screen.getByText(/Buildmesh hit a render error/i)).toBeTruthy();

    spy.mockRestore();
  });

  // Regression: issue #1239. The mobile SPA mounts on a URL of the form
  // `/?token=<ROOT_TOKEN>` and `Connect.tsx` strips the query before
  // mounting the rest of the app. If a render throw fires BEFORE that
  // strip (e.g. during initial mount of a child), `location.href` would
  // have included `?token=` verbatim — and the boundary's POST body
  // would have shipped the root token to the desktop log. This test
  // pins the fix: the boundary must report only the pathname, never the
  // search string.
  it('does not leak ?token= (or any query string) into /__debug/log', async () => {
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});

    // Model the pre-mount URL: pathname present, search carries the
    // root token. The boundary must observe only the pathname.
    const originalLocation = window.location;
    Object.defineProperty(window, 'location', {
      value: {
        pathname: '/connect',
        search: '?token=ROOT_TOKEN_SHOULD_NOT_LEAK',
        href: '/connect?token=ROOT_TOKEN_SHOULD_NOT_LEAK',
      },
      configurable: true,
      writable: true,
    });

    try {
      render(
        <MobileErrorBoundary>
          <Bomb message="root-token-leak-test" />
        </MobileErrorBoundary>,
      );
      await Promise.resolve();

      const call = fetchMock.mock.calls.find(
        (c) => typeof c[0] === 'string' && c[0].endsWith('/__debug/log'),
      );
      expect(call, 'expected fetch POST to /__debug/log').toBeDefined();

      const body = JSON.parse((call![1] as RequestInit).body as string);
      // The pathname alone is the only field that should reach the log.
      expect(body.src).toBe('/connect');
      // The token / query string must NOT appear anywhere in the payload —
      // not in `src`, not in `stack`, not in `msg`. A single substring
      // check on the JSON-stringified body is the simplest regression
      // guard for "did any field leak the secret?"
      expect(JSON.stringify(body)).not.toContain('ROOT_TOKEN_SHOULD_NOT_LEAK');
      expect(JSON.stringify(body)).not.toContain('token=');
    } finally {
      Object.defineProperty(window, 'location', {
        value: originalLocation,
        configurable: true,
        writable: true,
      });
      spy.mockRestore();
    }
  });
});