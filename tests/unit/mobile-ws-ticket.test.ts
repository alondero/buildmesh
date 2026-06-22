/**
 * Issue #551: the WS ticket is bound at mint time to the surface/node the client
 * will open, so a leaked ticket can only unlock that one target. These tests pin
 * the mobile side of that contract: `mintWsTicket` POSTs the target as the
 * request body, and the two URL builders mint with the *right* target (the
 * node's `terminal`, or the node-less `events` push).
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  mintWsTicket,
  terminalWsUrl,
  eventsWsUrl,
} from "../../src/mobile/api";

function okTicket(): Response {
  return new Response(JSON.stringify({ ticket: "abc123" }), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

describe("WS ticket target binding (issue #551)", () => {
  let fetchMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    fetchMock = vi.fn().mockResolvedValue(okTicket());
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("posts the bound target as the request body", async () => {
    const ticket = await mintWsTicket({ surface: "terminal", node_id: 7 });
    expect(ticket).toBe("abc123");

    const [path, init] = fetchMock.mock.calls[0];
    expect(path).toBe("/api/ws-ticket");
    expect(init.method).toBe("POST");
    // The cookie is the credential — never the URL (issue #500).
    expect(init.credentials).toBe("include");
    expect(JSON.parse(init.body as string)).toEqual({
      surface: "terminal",
      node_id: 7,
    });
  });

  it("terminalWsUrl mints a ticket bound to that node's terminal", async () => {
    const url = await terminalWsUrl(42);

    const body = JSON.parse(fetchMock.mock.calls[0][1].body as string);
    expect(body).toEqual({ surface: "terminal", node_id: 42 });
    // The minted ticket rides the upgrade URL; the long-lived token never does.
    expect(url).toContain("/ws/terminal/42?ticket=abc123");
  });

  it("eventsWsUrl mints a node-less ticket bound to the events surface", async () => {
    const url = await eventsWsUrl();

    const body = JSON.parse(fetchMock.mock.calls[0][1].body as string);
    expect(body).toEqual({ surface: "events", node_id: null });
    expect(url).toContain("/ws/events?ticket=abc123");
  });
});
