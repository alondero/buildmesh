/**
 * Connect screen: token → cookie login (issue #500).
 *
 * The token is exchanged for an HttpOnly bm_session cookie via POST
 * /api/session (Authorization: Bearer) — never a ?token= URL. A bad token
 * reports inline; a successful login stores the token and calls onConnected.
 */
import React from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import Connect from "../../src/mobile/screens/Connect";

// A 2xx login returns the persistent device token in the body (issue #502); the
// client stores THAT, not the token it presented.
function mockFetchStatus(status: number, body: unknown = { token: "device-tok" }) {
  const fn = vi.fn().mockResolvedValue({
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
  });
  vi.stubGlobal("fetch", fn);
  return fn;
}

describe("Connect", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("shows an inline error on an invalid token and does not store it", async () => {
    const fetchMock = mockFetchStatus(401);
    const onConnected = vi.fn();
    render(<Connect onConnected={onConnected} />);

    await userEvent.type(screen.getByTestId("token-input"), "deadbeef");
    await userEvent.click(screen.getByTestId("connect-submit"));

    await waitFor(() => {
      expect(screen.getByTestId("connect-error").textContent).toMatch(
        /invalid token/i,
      );
    });
    // Login posts the token in the Authorization header to /api/session —
    // never as a ?token= query param (issue #500).
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/session",
      expect.objectContaining({
        method: "POST",
        credentials: "include",
        headers: { Authorization: "Bearer deadbeef" },
      }),
    );
    expect(localStorage.getItem("buildmesh_token")).toBeNull();
    expect(onConnected).not.toHaveBeenCalled();
  });

  it("shows a reachability error when the desktop app is down", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("network")));
    render(<Connect onConnected={vi.fn()} />);

    await userEvent.type(screen.getByTestId("token-input"), "deadbeef");
    await userEvent.click(screen.getByTestId("connect-submit"));

    await waitFor(() => {
      expect(screen.getByTestId("connect-error").textContent).toMatch(
        /can't reach the desktop app/i,
      );
    });
  });

  it("stores the device token from the server and connects after login", async () => {
    mockFetchStatus(200, { token: "device-tok" });
    const onConnected = vi.fn();
    render(<Connect onConnected={onConnected} />);

    await userEvent.type(screen.getByTestId("token-input"), "cafef00d");
    await userEvent.click(screen.getByTestId("connect-submit"));

    await waitFor(() => {
      // The server-issued device token is persisted, not the pasted token.
      expect(localStorage.getItem("buildmesh_token")).toBe("device-tok");
    });
    expect(onConnected).toHaveBeenCalled();
  });

  it("treats a 200 with no token in the body as a failed login (never stores the pasted token)", async () => {
    // Guards the #502 regression: a body without a token must NOT downgrade to
    // persisting the presented (possibly root) token.
    mockFetchStatus(200, {});
    const onConnected = vi.fn();
    render(<Connect onConnected={onConnected} />);

    await userEvent.type(screen.getByTestId("token-input"), "root-paste");
    await userEvent.click(screen.getByTestId("connect-submit"));

    await waitFor(() => {
      expect(screen.getByTestId("connect-error").textContent).toMatch(/invalid token/i);
    });
    expect(localStorage.getItem("buildmesh_token")).toBeNull();
    expect(onConnected).not.toHaveBeenCalled();
  });

  it("persists the device token the server returns, not the pasted token", async () => {
    // Issue #502: pairing returns a per-device token; the phone stores that
    // (revocable on its own) in place of the root token it pasted.
    const fn = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({ token: "dev-tok-123" }),
    });
    vi.stubGlobal("fetch", fn);
    const onConnected = vi.fn();
    render(<Connect onConnected={onConnected} />);

    await userEvent.type(screen.getByTestId("token-input"), "root-paste");
    await userEvent.click(screen.getByTestId("connect-submit"));

    await waitFor(() => {
      expect(localStorage.getItem("buildmesh_token")).toBe("dev-tok-123");
    });
    expect(onConnected).toHaveBeenCalled();
  });

  it("requires a token before submitting", async () => {
    const fetchMock = mockFetchStatus(200);
    render(<Connect onConnected={vi.fn()} />);

    await userEvent.click(screen.getByTestId("connect-submit"));

    expect(screen.getByTestId("connect-error").textContent).toMatch(
      /enter a token/i,
    );
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("renders the notice explaining why the user landed here", () => {
    mockFetchStatus(200);
    render(<Connect onConnected={vi.fn()} notice="Connection expired" />);
    expect(screen.getByTestId("connect-notice").textContent).toBe(
      "Connection expired",
    );
  });

  it("POSTs /api/session exactly once when mounted under StrictMode with ?token=", async () => {
    // Issue #1260: React StrictMode double-invokes the mount effect in dev,
    // so an unguarded `connectWith(urlToken)` runs twice and mints two rows in
    // pair_device_session_inner. The guard ref `consumedTokenRef` makes the
    // side-effect idempotent across the simulated remount. The existing
    // replaceState URL strip ALSO masks the second invocation in jsdom and
    // most browsers today; the guard is defense-in-depth against the case
    // where the URL survives (older React, browser quirks, future changes).
    const fetchMock = mockFetchStatus(200, { token: "device-tok" });
    const onConnected = vi.fn();
    window.history.replaceState(null, "", "/?token=abc123");

    render(
      <React.StrictMode>
        <Connect onConnected={onConnected} />
      </React.StrictMode>,
    );

    await waitFor(() => {
      expect(onConnected).toHaveBeenCalledTimes(1);
    });
    // Exactly one POST — the StrictMode double-mount must not double-fire.
    expect(
      fetchMock.mock.calls.filter(
        ([url]: [unknown]) => url === "/api/session",
      ),
    ).toHaveLength(1);
    // Token was stripped from the address bar on first effect.
    expect(window.location.search).not.toContain("token=");
  });
});
