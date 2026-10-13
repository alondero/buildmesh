/**
 * Mobile session-cookie recovery (issue #1577).
 *
 * A phone can lose the HttpOnly `bm_session` cookie while its device is still
 * paired. The paired device token in `localStorage` is the only credential
 * JavaScript can re-present, so a 401 must buy exactly one coalesced
 * `POST /api/session` refresh and replay the original request — instead of
 * clearing the credential and bouncing the user back to Connect.
 *
 * These tests drive the real `api.ts` transport and a real mobile screen, so
 * they cover the seam (401 → refresh → replay → data) rather than restating the
 * implementation: the refresh count, the replay count, the credential state and
 * the rendered screen are all asserted against observable output.
 */
import { useCallback, useState, type ReactNode } from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import ChangesScreen from "../../src/mobile/screens/ChangesScreen";
import {
  ApiError,
  clearStoredToken,
  eventsWsUrl,
  isAuthError,
  listNodes,
  readStoredToken,
  type AgentNode,
} from "../../src/mobile/api";

const DEVICE_TOKEN = "device-token-abc123";

const node: AgentNode = {
  id: 5,
  mesh_id: 1,
  name: "node",
  path: "/tmp/worktree",
  branch: "main",
  provider: "anthropic",
  status: "running",
  cli_session_id: null,
  created_at: "2026-06-11T00:00:00Z",
};

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

/** A 204 must carry no body — `new Response(body, { status: 204 })` throws. */
function noContent(): Response {
  return new Response(null, { status: 204 });
}

function unauthorized(): Response {
  return json(401, { error: "session expired" });
}

/**
 * Models the server's cookie semantics across the whole test: only a
 * successful `/api/session` (204) plants a cookie, and every other route
 * 401s while the jar is empty. That is exactly the "stored token, no cookie"
 * state issue #1577 describes, and it is why a retry only succeeds *after* the
 * refresh has run — so each route override is handed the jar's state and must
 * honour it itself.
 */
function cookieServer(
  overrides: {
    session?: (call: number) => Response | Promise<Response>;
    routes?: Record<string, (ctx: { cookie: boolean }) => Response>;
  } = {},
) {
  let cookie = false;
  const calls: { path: string; init: RequestInit | undefined }[] = [];
  const sessionCalls: (RequestInit | undefined)[] = [];
  const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
    calls.push({ path: String(path), init });
    if (String(path) === "/api/session") {
      sessionCalls.push(init);
      const resp = (await overrides.session?.(sessionCalls.length)) ?? noContent();
      if (resp.status === 204) cookie = true;
      return resp;
    }
    const override = overrides.routes?.[String(path)];
    if (override) return override({ cookie });
    return cookie ? json(200, []) : unauthorized();
  });
  return {
    fetchMock,
    calls,
    sessionCalls,
    /** How many times each path was requested, retries included. */
    count: (path: string) => calls.filter((c) => c.path === path).length,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

/**
 * Mirrors App.handleAuthFailed's observable contract: the screen's own recovery
 * callback is what clears the credential and shows Connect — the transport
 * never does it behind the screen's back.
 *
 * The callback is memoised because `ChangesScreen`'s effect lists
 * `onAuthFailed` as a dependency: a fresh closure per render would restart the
 * effect on every successful load and refetch forever.
 */
function AuthRecoveryHarness({ children }: { children: (onAuthFailed: () => void) => ReactNode }) {
  const [connected, setConnected] = useState(true);
  const onAuthFailed = useCallback(() => {
    clearStoredToken();
    setConnected(false);
  }, []);
  if (!connected) return <div data-testid="connect-screen">Connect</div>;
  return <>{children(onAuthFailed)}</>;
}

describe("mobile session-cookie recovery (issue #1577)", () => {
  beforeEach(() => {
    localStorage.clear();
    localStorage.setItem("buildmesh_token", DEVICE_TOKEN);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
    localStorage.clear();
  });

  it("keeps the screen mounted and loads its data after recovering the cookie", async () => {
    const server = cookieServer({
      routes: {
        "/api/agents/5/git/status": ({ cookie }) =>
          !cookie
            ? unauthorized()
            : json(200, [
                {
                  path: "src/a.ts",
                  status: "modified",
                  additions: 1,
                  deletions: 0,
                  binary: false,
                  old_path: null,
                  old_num: null,
                  new_num: 3,
                  hunks: [],
                },
              ]),
        "/api/agents/5/git/summary": ({ cookie }) =>
          cookie ? json(200, { added: 1, modified: 1, deleted: 0 }) : unauthorized(),
        "/api/agents/5/git/branch": ({ cookie }) =>
          cookie ? json(200, { branch: "main" }) : unauthorized(),
        "/api/gh/auth": ({ cookie }) =>
          cookie ? json(200, { ok: true }) : unauthorized(),
      },
    });
    vi.stubGlobal("fetch", server.fetchMock);

    render(
      <AuthRecoveryHarness>
        {(onAuthFailed) => (
          <ChangesScreen
            node={node}
            onBack={() => {}}
            onOpenDiff={() => {}}
            onOpenPr={() => {}}
            onAuthFailed={onAuthFailed}
          />
        )}
      </AuthRecoveryHarness>,
    );

    // The recovered data is what proves the replay worked — the screen staying
    // mounted only proves recovery did not navigate away.
    expect(await screen.findByTestId("changes-file-src/a.ts")).toBeTruthy();
    expect(screen.queryByTestId("connect-screen")).toBeNull();
    expect(screen.getByTestId("changes-screen")).toBeTruthy();
    // The credential survives a successful recovery — that is what makes a
    // *second* cookie loss recoverable too.
    expect(readStoredToken()).toBe(DEVICE_TOKEN);
    // No duplicate session: the coalesced refresh ran once even though the
    // screen fired four requests that all 401'd.
    expect(server.sessionCalls).toHaveLength(1);
  });

  it("shares one refresh across concurrent 401s and replays each request once", async () => {
    const gate = deferred<Response>();
    const server = cookieServer({
      session: () => gate.promise,
      routes: {
        "/api/nodes": ({ cookie }) =>
          cookie ? json(200, [{ id: 1, name: "n" }]) : unauthorized(),
      },
    });
    vi.stubGlobal("fetch", server.fetchMock);

    const first = listNodes();
    const second = listNodes();

    try {
      // Both callers are parked on the shared refresh, which is held open here
      // so the overlap is guaranteed rather than hoped for.
      await waitFor(() => expect(server.sessionCalls).toHaveLength(1));
    } finally {
      gate.resolve(noContent());
    }

    await expect(first).resolves.toEqual([{ id: 1, name: "n" }]);
    await expect(second).resolves.toEqual([{ id: 1, name: "n" }]);
    expect(server.count("/api/nodes")).toBe(4); // 2 attempts + 2 replays
    expect(server.sessionCalls).toHaveLength(1);
  });

  it("clears the credential and shows Connect when the refresh is rejected", async () => {
    const server = cookieServer({ session: () => unauthorized() });
    vi.stubGlobal("fetch", server.fetchMock);

    render(
      <AuthRecoveryHarness>
        {(onAuthFailed) => (
          <ChangesScreen
            node={node}
            onBack={() => {}}
            onOpenDiff={() => {}}
            onOpenPr={() => {}}
            onAuthFailed={onAuthFailed}
          />
        )}
      </AuthRecoveryHarness>,
    );

    expect(await screen.findByTestId("connect-screen")).toBeTruthy();
    expect(readStoredToken()).toBeNull();
    // Rejected, so the replay never happened — a rejected credential must not
    // buy a second attempt.
    expect(server.count("/api/agents/5/git/status")).toBe(1);
  });

  it("replays at most once when the refreshed cookie is still unauthorized", async () => {
    // A cookie that survives a refresh but still 401s means the device itself
    // was revoked. Retrying again would loop, so the transport gives up and
    // reports the auth failure.
    const server = cookieServer({
      session: () => noContent(),
      routes: { "/api/nodes": () => unauthorized() },
    });
    vi.stubGlobal("fetch", server.fetchMock);

    await expect(listNodes()).rejects.toThrowError(
      expect.objectContaining({ status: 401 }) as unknown as Error,
    );
    expect(server.count("/api/nodes")).toBe(2); // attempt + one replay
    expect(server.count("/api/session")).toBe(1);
  });

  it("recovers the cookie before the WebSocket ticket mint, which bypasses the shared transport", async () => {
    // The mint carries the 429 response's headers, so it cannot route through
    // `apiFetch` — it is the one request that must rejoin the recovery seam by
    // hand, and it is exactly the request a reconnecting terminal makes first.
    const server = cookieServer({
      routes: {
        "/api/ws-ticket": ({ cookie }) =>
          cookie ? json(200, { ticket: "t-1" }) : unauthorized(),
      },
    });
    vi.stubGlobal("fetch", server.fetchMock);

    const url = await eventsWsUrl();

    expect(url).toContain("ticket=t-1");
    expect(server.count("/api/session")).toBe(1);
    expect(server.count("/api/ws-ticket")).toBe(2); // attempt + one replay
  });

  it("surfaces an auth error without a refresh when there is no saved token", async () => {
    localStorage.clear();
    const server = cookieServer();
    vi.stubGlobal("fetch", server.fetchMock);

    const failure = listNodes().catch((e) => e);

    expect(await failure).toBeInstanceOf(ApiError);
    expect(isAuthError(await failure)).toBe(true);
    // Nothing to re-present, so no refresh is attempted at all.
    expect(server.count("/api/session")).toBe(0);
    expect(server.count("/api/nodes")).toBe(1);
  });

  it("does not refresh on a 403, which no session can grant", async () => {
    const server = cookieServer({
      routes: {
        "/api/nodes": () => json(403, { error: "scope" }),
      },
    });
    vi.stubGlobal("fetch", server.fetchMock);

    const failure = await listNodes().catch((e) => e);

    expect(isAuthError(failure)).toBe(true);
    expect(server.count("/api/session")).toBe(0);
    expect(server.count("/api/nodes")).toBe(1);
  });

  it("keeps the device token out of URLs and the console", async () => {
    const logged: unknown[][] = [];
    for (const level of ["log", "info", "warn", "error", "debug"] as const) {
      vi.spyOn(console, level).mockImplementation((...args: unknown[]) => {
        logged.push(args);
      });
    }
    const server = cookieServer({
      session: () => noContent(),
      routes: { "/api/nodes": ({ cookie }) => (cookie ? json(200, []) : unauthorized()) },
    });
    vi.stubGlobal("fetch", server.fetchMock);

    await listNodes();

    // The token travels on exactly one header — the refresh — and nowhere else.
    expect(server.sessionCalls).toHaveLength(1);
    expect(server.sessionCalls[0]?.headers).toEqual({
      Authorization: `Bearer ${DEVICE_TOKEN}`,
    });
    for (const call of server.calls) {
      expect(String(call.path)).not.toContain(DEVICE_TOKEN);
      const headers = JSON.stringify(call.init?.headers ?? {});
      if (call.path !== "/api/session") expect(headers).not.toContain(DEVICE_TOKEN);
    }
    for (const args of logged) expect(JSON.stringify(args)).not.toContain(DEVICE_TOKEN);
  });
});