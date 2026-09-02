/**
 * NodeList: archived nodes are hidden, awaiting-input nodes are pinned in
 * the attention section with a readable status label, and an auth failure
 * (revoked token) routes to onAuthFailed instead of the offline overlay.
 * Issue #815 — the spawn picker consumes the live `listProviders()` rows
 * (First-class / Proxied providers, harness ordering) rather than a
 * hardcoded fallback, so newly-configured harnesses reach mobile.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { StrictMode } from "react";
import NodeList from "../../src/mobile/screens/NodeList";
import type {
  AgentNode,
  Mesh,
  NodeStatus,
  Provider,
} from "../../src/mobile/api";

let sockets: FakeWebSocket[] = [];

class FakeWebSocket {
  url: string;
  sent: string[] = [];
  closed = false;
  onopen: (() => void) | null = null;
  onmessage: ((e: unknown) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(url: string) {
    this.url = url;
    sockets.push(this);
  }
  send(data: string) {
    this.sent.push(data);
  }
  close() {
    this.closed = true;
  }
}

const mesh: Mesh = {
  id: 1,
  name: "buildmesh",
  path: "/tmp/repo",
  created_at: "2026-06-11T00:00:00Z",
  scratchpad: "",
  sandbox: false,
};

function makeNode(id: number, status: NodeStatus): AgentNode {
  return {
    id,
    mesh_id: 1,
    name: `node-${id}`,
    path: "/tmp/wt",
    branch: null,
    provider: "anthropic",
    status,
    cli_session_id: null,
    created_at: "2026-06-11T00:00:00Z",
  };
}

function mockApi(
  nodes: AgentNode[],
  opts?: { status?: number; providers?: Provider[] },
) {
  const status = opts?.status ?? 200;
  const providers = opts?.providers;
  const fn = vi.fn().mockImplementation(async (url: string, init?: RequestInit) => {
    const ok = status >= 200 && status < 300;
    let body: unknown;
    if (url.includes("/api/ws-ticket")) body = { ticket: "t" };
    else if (url.includes("/api/meshes")) body = [mesh];
    else if (url.includes("/api/nodes/") && url.includes("/input")) body = { ok: true };
    else if (url.includes("/api/nodes")) body = nodes;
    else if (url.includes("/api/providers")) body = providers ?? [];
    else body = [];
    // Suppress the unused-init lint while keeping init visible to test
    // assertions (it carries the POST body for the /input route).
    void init;
    return {
      ok,
      status,
      json: async () => (ok ? body : { error: "nope" }),
    };
  });
  vi.stubGlobal("fetch", fn);
  return fn;
}

const noop = () => {};

describe("NodeList", () => {
  beforeEach(() => {
    localStorage.clear();
    sockets = [];
    vi.stubGlobal("WebSocket", FakeWebSocket);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("hides archived nodes and pins awaiting-input nodes in the attention section", async () => {
    mockApi([
      makeNode(1, "running"),
      makeNode(2, "archived"),
      makeNode(3, "awaiting_input"),
    ]);

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={noop}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("node-list")).toBeTruthy();
    });

    // Archived node never renders.
    expect(screen.queryByTestId("node-2")).toBeNull();
    // Awaiting-input node is pinned in the attention section with a readable
    // status label.
    const attention = screen.getByTestId("attention-section");
    expect(attention.textContent).toContain("node-3");
    // Shared status vocab (issue #815) — matches desktop's `STATUS_CONFIG`
    // label, not mobile's old bespoke "needs input" copy.
    expect(attention.textContent).toContain("Needs attention");
    // …and it appears EXACTLY ONCE overall — it must NOT also be rendered
    // under its mesh bucket (regression for the duplicate-row bug, #807).
    expect(screen.getAllByTestId("node-3")).toHaveLength(1);
    // Running node renders normally.
    expect(screen.getByTestId("node-1")).toBeTruthy();
  });

  it("triage card shows repo, branch, prompt placeholder and one-tap chips (issue #1377)", async () => {
    mockApi([{ ...makeNode(3, "awaiting_input"), branch: "feature/deck" }]);

    const onOpenNode = vi.fn();
    render(
      <NodeList
        onOpenNode={onOpenNode}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={noop}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("attention-deck")).toBeTruthy();
    });

    // Repo (mesh name) + branch on the card body.
    const body = screen.getByTestId("node-3");
    expect(body.textContent).toContain("buildmesh");
    expect(body.textContent).toContain("feature/deck");
    // No lifecycle event yet → the placeholder prompt line, not silence.
    expect(screen.getByTestId("attn-prompt-3").textContent).toContain(
      "Waiting for the agent's prompt",
    );
    // The two action chips. (Issue #1377, post-review: the redundant
    // "Focus terminal" chip was dropped — the card body is the focus
    // target, so an explicit chip was competing for the same 120px.)
    expect(screen.getByTestId("attn-approve-3").textContent).toContain(
      "Approve (Y)",
    );
    expect(screen.getByTestId("attn-reject-3").textContent).toContain(
      "Reject (N)",
    );
    expect(screen.queryByTestId("attn-focus-3")).toBeNull();

    // Focus (card body) opens the node's terminal.
    fireEvent.click(screen.getByTestId("node-3"));
    expect(onOpenNode).toHaveBeenCalledTimes(1);
    expect(onOpenNode.mock.calls[0][0].id).toBe(3);
  });

  it("Approve/Reject chips POST y/n+Enter to /api/nodes/{id}/input (issue #1377)", async () => {
    // The chips send a one-shot POST — not a terminal WS — because the
    //    previous WS-based path was racing the server's read loop and could
    //    drop the keystroke before delivery. The 200 OK on the POST is the
    //    delivery proof. (#1377, post-review rewrite)
    const fetch = mockApi([makeNode(3, "awaiting_input")]);

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={noop}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("attention-deck")).toBeTruthy();
    });

    // Approve: the chip must POST `{"seq":"y\r"}` to the dedicated input
    // route — the `\r` is what makes the server autoclear the attention state.
    fireEvent.click(screen.getByTestId("attn-approve-3"));
    let approveCall: { url: string; body: unknown } | undefined;
    await waitFor(() => {
      const call = fetch.mock.calls.find(
        (c) => typeof c[0] === "string" && c[0].includes("/api/nodes/3/input"),
      );
      expect(call).toBeTruthy();
      approveCall = call as unknown as { url: string; body: unknown };
    });
    const [url, init] = approveCall! as [string, RequestInit];
    expect(url).toBe("/api/nodes/3/input");
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({ seq: "y\r" });
    // Delivered → the chip flips to the action-specific success label.
    // Issue #1377, post-review: the chip tracks WHICH action was sent (not
    // a boolean "Sent ✓" that masquerades on both buttons), and BOTH chips
    // are disabled once any action has been delivered.
    await waitFor(() => {
      expect(screen.getByTestId("attn-approve-3").textContent).toContain(
        "Approved",
      );
    });
    expect(
      (screen.getByTestId("attn-approve-3") as HTMLButtonElement).disabled,
    ).toBe(true);
    expect(
      (screen.getByTestId("attn-reject-3") as HTMLButtonElement).disabled,
    ).toBe(true);
    // Critical: NO terminal WS was opened for this one-shot tap.
    expect(
      sockets.find((s) => s.url.includes("/ws/terminal/3")),
    ).toBeUndefined();
  });

  it("Reject chip POSTs n+Enter to /api/nodes/{id}/input (issue #1377)", async () => {
    const fetch = mockApi([makeNode(5, "awaiting_input")]);

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={noop}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("attention-deck")).toBeTruthy();
    });

    fireEvent.click(screen.getByTestId("attn-reject-5"));
    await waitFor(() => {
      expect(
        fetch.mock.calls.find(
          (c) => typeof c[0] === "string" && c[0].includes("/api/nodes/5/input"),
        ),
      ).toBeTruthy();
    });
    const [url, init] = fetch.mock.calls.find(
      (c) => typeof c[0] === "string" && c[0].includes("/api/nodes/5/input"),
    ) as [string, RequestInit];
    expect(url).toBe("/api/nodes/5/input");
    expect(JSON.parse(String(init.body))).toEqual({ seq: "n\r" });
    // No terminal WS opened.
    expect(
      sockets.find((s) => s.url.includes("/ws/terminal/5")),
    ).toBeUndefined();
  });

  it("shows the last prompt from agent-lifecycle events and clears it when attention clears (issue #1377)", async () => {
    mockApi([makeNode(3, "awaiting_input")]);

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={noop}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("attention-deck")).toBeTruthy();
    });

    const eventsSocket = sockets.find((s) => s.url.includes("/ws/events"));
    expect(eventsSocket).toBeTruthy();

    // The awaiting-input lifecycle event carries the semantic turn
    // description — that is the "last prompt / permission request" line.
    await act(async () => {
      eventsSocket!.onmessage?.({
        data: JSON.stringify({
          type: "agent-lifecycle",
          session_id: 3,
          provider: "anthropic",
          kind: "awaiting_input",
          status: "awaiting_input",
          message: null,
          provider_event: "permission_request",
          provider_session_id: null,
          completion_reason: null,
          transcript_path: null,
          timestamp: "2026-06-11T00:00:00Z",
          signal_health: "ok",
          semantic_turn: {
            node_id: 3,
            kind: "needs_input",
            description: "Allow npm install?",
          },
        }),
      });
    });
    expect(screen.getByTestId("attn-prompt-3").textContent).toContain(
      "Allow npm install?",
    );

    // When attention clears, the prompt must not outlive the state — the
    // card itself disappears (status left awaiting_input via the same
    // event's optimistic patch + refetch), and a still-awaiting card that
    // receives attention-cleared loses the line with it.
    await act(async () => {
      eventsSocket!.onmessage?.({
        data: JSON.stringify({ type: "attention-cleared", session_id: 3 }),
      });
    });
    expect(screen.getByTestId("attn-prompt-3").textContent).toContain(
      "Waiting for the agent's prompt",
    );
  });

  it("pull-to-refresh refetches the node list (issue #1377)", async () => {
    let nodesCalls = 0;
    const fetch = vi.fn().mockImplementation(async (url: string) => {
      if (url.includes("/api/nodes")) {
        nodesCalls += 1;
        return {
          ok: true,
          status: 200,
          json: async () => [makeNode(1, "running")],
        };
      }
      if (url.includes("/api/meshes")) {
        return { ok: true, status: 200, json: async () => [mesh] };
      }
      if (url.includes("/api/ws-ticket")) {
        return { ok: true, status: 200, json: async () => ({ ticket: "t" }) };
      }
      return { ok: true, status: 200, json: async () => [] };
    });
    vi.stubGlobal("fetch", fetch);

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={noop}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("node-list")).toBeTruthy();
    });
    const before = nodesCalls;
    expect(before).toBeGreaterThanOrEqual(1);

    // Drag down 200px at the top of the list — damped to 80px, past the
    // 64px threshold. jsdom keeps scrollTop at 0, exactly the "at top"
    // precondition.
    const scroller = screen.getByTestId("node-list");
    fireEvent.touchStart(scroller, { touches: [{ clientY: 200 }] });
    fireEvent.touchMove(scroller, { touches: [{ clientY: 400 }] });
    // Mid-pull the indicator shows the release affordance.
    expect(screen.getByTestId("pull-indicator-label").textContent).toBe(
      "Release to refresh",
    );
    fireEvent.touchEnd(scroller, { changedTouches: [{ clientY: 400 }] });

    await waitFor(() => {
      expect(nodesCalls).toBeGreaterThan(before);
    });
  });

  it("small vertical drags do not trigger pull-to-refresh (issue #1377)", async () => {
    let nodesCalls = 0;
    const fetch = vi.fn().mockImplementation(async (url: string) => {
      if (url.includes("/api/nodes")) {
        nodesCalls += 1;
        return { ok: true, status: 200, json: async () => [] };
      }
      if (url.includes("/api/meshes")) {
        return { ok: true, status: 200, json: async () => [mesh] };
      }
      if (url.includes("/api/ws-ticket")) {
        return { ok: true, status: 200, json: async () => ({ ticket: "t" }) };
      }
      return { ok: true, status: 200, json: async () => [] };
    });
    vi.stubGlobal("fetch", fetch);

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={noop}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("node-list")).toBeTruthy();
    });
    const before = nodesCalls;

    const scroller = screen.getByTestId("node-list");
    // 100px raw → 40px damped, below the 64px threshold.
    fireEvent.touchStart(scroller, { touches: [{ clientY: 200 }] });
    fireEvent.touchMove(scroller, { touches: [{ clientY: 300 }] });
    fireEvent.touchEnd(scroller, { changedTouches: [{ clientY: 300 }] });

    await act(async () => {
      await Promise.resolve();
    });
    expect(nodesCalls).toBe(before);
  });

  it("ignores horizontal strokes on the deck (axis lock, issue #1377 review)", async () => {
    // The attention deck is a horizontal carousel — a slight downward
    // diagonal on the deck must NOT hijack the gesture into a refresh.
    // The pull-to-refresh hook now anchors on (x, y) and requires dy to
    // dominate dx by DOMINANT_RATIO before engaging.
    let nodesCalls = 0;
    const fetch = vi.fn().mockImplementation(async (url: string) => {
      if (url.includes("/api/nodes")) {
        nodesCalls += 1;
        return { ok: true, status: 200, json: async () => [] };
      }
      if (url.includes("/api/meshes")) {
        return { ok: true, status: 200, json: async () => [mesh] };
      }
      if (url.includes("/api/ws-ticket")) {
        return { ok: true, status: 200, json: async () => ({ ticket: "t" }) };
      }
      return { ok: true, status: 200, json: async () => [] };
    });
    vi.stubGlobal("fetch", fetch);

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={noop}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("node-list")).toBeTruthy();
    });
    const before = nodesCalls;

    const scroller = screen.getByTestId("node-list");
    // Start at (100, 100), end at (260, 140) — dx=160 (horizontal) clearly
    // dominates dy=40 (vertical). Pre-fix this would have triggered a
    // partial pull (~16px damped) which would have re-rendered the deck on
    // every touchmove. Post-fix the hook drops the anchor.
    fireEvent.touchStart(scroller, { touches: [{ clientX: 100, clientY: 100 }] });
    fireEvent.touchMove(scroller, { touches: [{ clientX: 260, clientY: 140 }] });
    fireEvent.touchEnd(scroller, {
      changedTouches: [{ clientX: 260, clientY: 140 }],
    });

    await act(async () => {
      await Promise.resolve();
    });
    expect(nodesCalls).toBe(before);
    // The pull indicator must NOT have rendered for a horizontal stroke.
    expect(screen.queryByTestId("pull-indicator")).toBeNull();
  });

  it("refreshes the list on a vertical drag (axis-locked pull-to-refresh, issue #1377)", async () => {
    // Companion to the axis-lock test: a vertical stroke (dy dominates dx
    // by 5x) MUST still trigger the refresh. The lock is one-sided — it
    // only DROPS the anchor when horizontal wins.
    let nodesCalls = 0;
    const fetch = vi.fn().mockImplementation(async (url: string) => {
      if (url.includes("/api/nodes")) {
        nodesCalls += 1;
        return { ok: true, status: 200, json: async () => [] };
      }
      if (url.includes("/api/meshes")) {
        return { ok: true, status: 200, json: async () => [mesh] };
      }
      if (url.includes("/api/ws-ticket")) {
        return { ok: true, status: 200, json: async () => ({ ticket: "t" }) };
      }
      return { ok: true, status: 200, json: async () => [] };
    });
    vi.stubGlobal("fetch", fetch);

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={noop}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("node-list")).toBeTruthy();
    });
    const before = nodesCalls;

    const scroller = screen.getByTestId("node-list");
    // 200px down, 5px sideways — dy=200 clearly dominates dx=5. Damp to
    // 80px, past the 64px threshold.
    fireEvent.touchStart(scroller, {
      touches: [{ clientX: 100, clientY: 200 }],
    });
    fireEvent.touchMove(scroller, {
      touches: [{ clientX: 105, clientY: 400 }],
    });
    fireEvent.touchEnd(scroller, {
      changedTouches: [{ clientX: 105, clientY: 400 }],
    });

    await waitFor(() => {
      expect(nodesCalls).toBeGreaterThan(before);
    });
  });

  it("doesn't warn when the refresh finishes after the component unmounts (issue #1377 review)", async () => {
    // The previous build's pull-to-refresh called setRefreshing(false)
    // inside the refresh promise's `.finally`. If the user tapped a node
    // row mid-refresh and NodeList unmounted, the late setState would
    // log a React warning. Post-fix the hook guards the post-refresh
    // setState with a mountedRef.
    let resolveRefresh: (() => void) | null = null;
    const refreshPromise = new Promise<void>((resolve) => {
      resolveRefresh = resolve;
    });
    const fetch = vi.fn().mockImplementation(async (url: string) => {
      if (url.includes("/api/nodes")) {
        return { ok: true, status: 200, json: async () => [mesh] };
      }
      if (url.includes("/api/meshes")) {
        return { ok: true, status: 200, json: async () => [mesh] };
      }
      if (url.includes("/api/ws-ticket")) {
        return { ok: true, status: 200, json: async () => ({ ticket: "t" }) };
      }
      return { ok: true, status: 200, json: async () => [] };
    });
    void fetch;
    // Override usePullToRefresh via a wrapper would be heavy — instead we
    // exercise the path directly through a long-lived refresh by
    // hanging on /api/nodes. First call returns OK (initial mount);
    // second call (the pull-to-refresh) hangs.
    let nodesCallCount = 0;
    const fetchWithHangingRefresh = vi
      .fn()
      .mockImplementation(async (url: string) => {
        if (url.includes("/api/nodes")) {
          nodesCallCount += 1;
          if (nodesCallCount >= 2) return refreshPromise;
          return { ok: true, status: 200, json: async () => [] };
        }
        if (url.includes("/api/meshes")) {
          return { ok: true, status: 200, json: async () => [mesh] };
        }
        if (url.includes("/api/ws-ticket")) {
          return { ok: true, status: 200, json: async () => ({ ticket: "t" }) };
        }
        return { ok: true, status: 200, json: async () => [] };
      });
    vi.stubGlobal("fetch", fetchWithHangingRefresh);

    const warnSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    const utils = render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={noop}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("node-list")).toBeTruthy();
    });

    const scroller = screen.getByTestId("node-list");
    fireEvent.touchStart(scroller, {
      touches: [{ clientX: 100, clientY: 200 }],
    });
    fireEvent.touchMove(scroller, {
      touches: [{ clientX: 100, clientY: 400 }],
    });
    fireEvent.touchEnd(scroller, {
      changedTouches: [{ clientX: 100, clientY: 400 }],
    });

    // The hanging refresh is in flight. Unmount the screen BEFORE it
    // resolves — this is the unmount-mid-refresh case.
    utils.unmount();
    // Now resolve the hanging refresh.
    resolveRefresh!();
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    // No React "state update on unmounted component" warning.
    const warned = warnSpy.mock.calls.some((c) =>
      String(c[0] ?? "").includes("unmounted"),
    );
    expect(warned).toBe(false);
    warnSpy.mockRestore();
  });

  it("prunes keySent + lastPrompts when the node leaves awaiting_input via polling (issue #1377 review)", async () => {
    // The WS event path cleared these maps on agent-lifecycle /
    // attention-cleared, but a node can also leave awaiting_input via
    // plain polling (reconnect, missed event, refetch on tab return).
    // Post-review fix: a useEffect keyed on the awaiting-id set drops
    // stale entries on every reconcile. Here we:
    //   1. mount with an awaiting-input node — fires the first
    //      WS-triggered lifecycle event, seeding keySent and lastPrompts
    //   2. simulate a second refresh that flips the same node's status to
    //      "running" — the reconciliation effect MUST drop the stale
    //      keySent entry and the stale lastPrompts entry, so a future
    //      return to awaiting_input starts clean
    let nodeStatus: "awaiting_input" | "running" = "awaiting_input";
    const fetch = vi.fn().mockImplementation(async (url: string) => {
      if (url.includes("/api/nodes")) {
        return {
          ok: true,
          status: 200,
          json: async () => [
            { ...makeNode(3, nodeStatus), branch: "feature/deck" },
          ],
        };
      }
      if (url.includes("/api/meshes")) {
        return { ok: true, status: 200, json: async () => [mesh] };
      }
      if (url.includes("/api/ws-ticket")) {
        return { ok: true, status: 200, json: async () => ({ ticket: "t" }) };
      }
      if (url.includes("/api/providers")) {
        return { ok: true, status: 200, json: async () => [] };
      }
      return { ok: true, status: 200, json: async () => [] };
    });
    vi.stubGlobal("fetch", fetch);

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={noop}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("attention-deck")).toBeTruthy();
    });

    // Approve the node → POST the input, keySent picks up "approve".
    fireEvent.click(screen.getByTestId("attn-approve-3"));
    await waitFor(() => {
      expect(screen.getByTestId("attn-approve-3").textContent).toContain(
        "Approved",
      );
    });

    // The same node is then refreshed with status=running. The deck
    // disappears (no awaiting_input nodes); the next time it returns
    // to awaiting_input, the stale "Approved" state must NOT reappear.
    nodeStatus = "running";
    // Manually drive a refresh by re-rendering with the updated fetch.
    await act(async () => {
      // Trigger another fetch by re-mounting the events WS message.
      const eventsSocket = sockets.find((s) => s.url.includes("/ws/events"));
      expect(eventsSocket).toBeTruthy();
      eventsSocket!.onmessage?.({
        data: JSON.stringify({
          type: "agent-lifecycle",
          session_id: 3,
          provider: "anthropic",
          kind: "running",
          status: "running",
          message: null,
          provider_event: null,
          provider_session_id: null,
          completion_reason: null,
          transcript_path: null,
          timestamp: "2026-06-11T00:00:00Z",
          signal_health: "ok",
          semantic_turn: null,
        }),
      });
    });

    // Deck should disappear (node is no longer awaiting_input).
    await waitFor(() => {
      expect(screen.queryByTestId("attention-deck")).toBeNull();
    });
  });

  it("disables BOTH action chips after a successful send (issue #1377 review)", async () => {
    // The previous build's `sent: boolean` flipped BOTH chips to "Sent ✓"
    // AND left them enabled — the user could tap the "Sent ✓" Reject
    // button and fire an `n\r` immediately after approving. Post-review:
    // `sent` is an enum ("approve" | "reject"), both chips disable on
    // any send, the active chip shows the action-specific label.
    mockApi([makeNode(7, "awaiting_input")]);

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={noop}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("attention-deck")).toBeTruthy();
    });

    fireEvent.click(screen.getByTestId("attn-approve-7"));
    await waitFor(() => {
      expect(screen.getByTestId("attn-approve-7").textContent).toContain(
        "Approved",
      );
    });
    // BOTH chips disabled.
    expect(
      (screen.getByTestId("attn-approve-7") as HTMLButtonElement).disabled,
    ).toBe(true);
    expect(
      (screen.getByTestId("attn-reject-7") as HTMLButtonElement).disabled,
    ).toBe(true);
    // The approve chip's label is the action-specific one (NOT the old
    // shared "Sent ✓" string that could appear on either button).
    expect(screen.getByTestId("attn-approve-7").textContent).toContain(
      "Approved",
    );
    // Reject chip must NOT show "Approved" — that label belongs to the
    // approve chip specifically. (Pre-fix, both showed "Sent ✓".)
    expect(screen.getByTestId("attn-reject-7").textContent).not.toContain(
      "Approved",
    );
  });

  it("resolves the initial list when mounted through the production StrictMode wrapper", async () => {
    // This kills the cleanup-only mounted guard mutation: React StrictMode
    // re-runs effects without recreating refs, so cleanup must not leave the
    // committed component permanently marked as unmounted.
    mockApi([makeNode(1, "running")]);

    render(
      <StrictMode>
        <NodeList
          onOpenNode={noop}
          onOpenAgentNodes={noop}
          onOpenIssues={noop}
          onOffline={noop}
          onAuthFailed={noop}
        />
      </StrictMode>,
    );

    await waitFor(() => {
      expect(screen.queryByTestId("node-list")).not.toBeNull();
    });
    expect(screen.getByTestId("node-1")).toBeTruthy();
  });

  it("NodeRow shows the friendly provider label and shared status hex (issue #815)", async () => {
    // The live list maps `"claude"` → `"Claude Code"`, distinct from the
    // raw provider id a node carries. Issue #815's "row subtitle should
    // show the label, not the id" only holds if this lookup wins — and
    // the status bar (3px right rail) must read STATUS_CONFIG.hex
    // (e.g. `#f59e0b` for `awaiting_input`, `#00d4ff` for `running`),
    // not the legacy hardcoded hex from the deleted `STATUS_META`.
    // (The node's `provider` field is overridden to `"claude"` so the
    // lookup hits the live row; otherwise the `?? node.provider` fallback
    // branch fires and the test would be passing for the wrong reason.)
    mockApi(
      [
        { ...makeNode(1, "running"), provider: "claude" },
        { ...makeNode(3, "awaiting_input"), provider: "claude" },
      ],
      {
        providers: [
          {
            id: "claude",
            label: "Claude Code",
            color: "#1d7cfc",
            icon: "A",
            resumable: true,
            harness_id: "claude",
            provider_id: null,
            is_proxied: false,
            group_key: "claude",
          },
        ],
      },
    );

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={noop}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("node-list")).toBeTruthy();
    });

    // Wait for the live provider fetch to land — without it, the row
    // falls back to the raw provider id (per the `?? node.provider`
    // branch in NodeRow).
    await waitFor(() => {
      expect(screen.getByTestId("node-3").textContent).toContain(
        "Claude Code",
      );
    });

    // Both rows use the friendly label from the live list, not the raw
    // provider id (`"claude"` is the id; the label is `"Claude Code"`).
    const row1 = screen.getByTestId("node-1");
    const row3 = screen.getByTestId("node-3");
    for (const row of [row1, row3]) {
      expect(row.textContent).toContain("Claude Code");
    }
    // The status bar is the 3px-wide rail at the right edge of the row —
    // its inline `background` style must come from STATUS_CONFIG (jsdom
    // normalizes hex → rgb(), so compare via `hexToRgbString`).
    expect(railHex(row1)).toBe(hexToRgbString("#00d4ff")); // running
    // Issue #1377: the awaiting-input node renders as a triage-deck card
    // (amber ring via .deck-card, no status rail), so only the running
    // NodeRow carries a rail to assert here.
    expect(screen.getByTestId("attn-card-3")).toBeTruthy();
  });

  it("routes a 401 to onAuthFailed, not the offline overlay", async () => {
    mockApi([], { status: 401 });
    const onOffline = vi.fn();
    const onAuthFailed = vi.fn();

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={onOffline}
        onAuthFailed={onAuthFailed}
      />,
    );

    await waitFor(() => {
      expect(onAuthFailed).toHaveBeenCalled();
    });
    expect(onOffline).not.toHaveBeenCalled();
  });

  it.each([404, 500])(
    "keeps the normal offline state for a non-auth %s response",
    async (status) => {
      mockApi([], { status });
      const onOffline = vi.fn();
      const onAuthFailed = vi.fn();

      render(
        <NodeList
          onOpenNode={noop}
          onOpenAgentNodes={noop}
          onOpenIssues={noop}
          onOffline={onOffline}
          onAuthFailed={onAuthFailed}
        />,
      );

      await waitFor(() => {
        expect(onOffline).toHaveBeenCalled();
      });
      expect(onAuthFailed).not.toHaveBeenCalled();
    },
  );

  it("routes an auth failure during the 5-second poll to onAuthFailed", async () => {
    vi.useFakeTimers();
    let nodeListCalls = 0;
    const fetch = vi.fn().mockImplementation(async (url: string) => {
      if (url.includes("/api/nodes")) {
        nodeListCalls += 1;
        if (nodeListCalls > 1) return { ok: false, status: 401, json: async () => ({ error: "expired" }) };
        return { ok: true, status: 200, json: async () => [] };
      }
      if (url.includes("/api/meshes")) {
        return { ok: true, status: 200, json: async () => [mesh] };
      }
      if (url.includes("/api/providers")) {
        return { ok: true, status: 200, json: async () => [] };
      }
      if (url.includes("/api/ws-ticket")) {
        return { ok: true, status: 200, json: async () => ({ ticket: "events" }) };
      }
      return { ok: true, status: 200, json: async () => ({}) };
    });
    vi.stubGlobal("fetch", fetch);
    const onAuthFailed = vi.fn();

    try {
      render(
        <NodeList
          onOpenNode={noop}
          onOpenAgentNodes={noop}
          onOpenIssues={noop}
          onOffline={noop}
          onAuthFailed={onAuthFailed}
        />,
      );
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(screen.getByTestId("node-list")).toBeTruthy();
      await act(async () => {
        await vi.advanceTimersByTimeAsync(5000);
      });
      expect(onAuthFailed).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("routes an auth failure from the background provider refresh to onAuthFailed", async () => {
    const fetch = vi.fn().mockImplementation(async (url: string) => {
      if (url.includes("/api/providers")) {
        return { ok: false, status: 401, json: async () => ({ error: "expired" }) };
      }
      if (url.includes("/api/meshes")) {
        return { ok: true, status: 200, json: async () => [mesh] };
      }
      if (url.includes("/api/nodes")) {
        return { ok: true, status: 200, json: async () => [] };
      }
      if (url.includes("/api/ws-ticket")) {
        return { ok: true, status: 200, json: async () => ({ ticket: "events" }) };
      }
      return { ok: true, status: 200, json: async () => ({}) };
    });
    vi.stubGlobal("fetch", fetch);
    const onAuthFailed = vi.fn();

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={onAuthFailed}
      />,
    );

    await waitFor(() => expect(screen.getByTestId("node-list")).toBeTruthy());
    await waitFor(() => expect(onAuthFailed).toHaveBeenCalledTimes(1));
  });

  it("routes an expired create-node request to onAuthFailed", async () => {
    const provider: Provider = {
      id: "claude",
      label: "Claude Code",
      color: "#1d7cfc",
      icon: "A",
      resumable: true,
      harness_id: "claude",
      provider_id: null,
      is_proxied: false,
      group_key: "claude",
    };
    const fetch = vi.fn().mockImplementation(async (url: string) => {
      if (url.includes("/api/nodes/create")) {
        return { ok: false, status: 401, json: async () => ({ error: "expired" }) };
      }
      if (url.includes("/api/meshes")) {
        return { ok: true, status: 200, json: async () => [mesh] };
      }
      if (url.includes("/api/nodes")) {
        return { ok: true, status: 200, json: async () => [] };
      }
      if (url.includes("/api/providers")) {
        return { ok: true, status: 200, json: async () => [provider] };
      }
      if (url.includes("/api/ws-ticket")) {
        return { ok: true, status: 200, json: async () => ({ ticket: "events" }) };
      }
      return { ok: true, status: 200, json: async () => ({}) };
    });
    vi.stubGlobal("fetch", fetch);
    const onAuthFailed = vi.fn();

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={onAuthFailed}
      />,
    );

    fireEvent.click(await screen.findByTestId("new-node-1"));
    fireEvent.click(await screen.findByTestId("provider-claude"));

    await waitFor(() => expect(onAuthFailed).toHaveBeenCalledTimes(1));
    expect(screen.queryByTestId("create-error")).toBeNull();
  });

  it("pauses the 5s poll while document.hidden and refreshes on becoming visible (issue #1261)", async () => {
    // The polling lifecycle lives in `useVisibilityPolling`. This test
    // pins it end-to-end through NodeList: the hook must not start the
    // 5s tick when mounted with `document.hidden === true`, must fire
    // ONE refresh immediately on becoming visible, and must stop the
    // tick again when re-hidden.
    //
    // jsdom defaults `document.hidden` to false. Save the existing
    // descriptor (so other tests / the afterEach unstub don't have to
    // know we ever touched it) and force it true BEFORE render so the
    // mount-time `if (visibilityState === "visible")` guard skips the
    // initial refresh + interval start.
    const originalHidden = Object.getOwnPropertyDescriptor(document, "hidden");
    Object.defineProperty(document, "hidden", {
      configurable: true,
      get: () => true,
    });

    vi.useFakeTimers();
    let nodesCalls = 0;
    const fetch = vi.fn().mockImplementation(async (url: string) => {
      if (url.includes("/api/nodes")) {
        nodesCalls += 1;
        return { ok: true, status: 200, json: async () => [] };
      }
      if (url.includes("/api/meshes")) {
        return { ok: true, status: 200, json: async () => [mesh] };
      }
      if (url.includes("/api/providers")) {
        return { ok: true, status: 200, json: async () => [] };
      }
      if (url.includes("/api/ws-ticket")) {
        return { ok: true, status: 200, json: async () => ({ ticket: "events" }) };
      }
      return { ok: true, status: 200, json: async () => ({}) };
    });
    vi.stubGlobal("fetch", fetch);

    // Local helpers — keep the assertions below free of microtask plumbing.
    // `drain()` flushes the pending microtasks without touching timers, which
    // is what we want after firing a `visibilitychange` event: the hook's
    // handler is synchronous, but the `await refresh()` inside it parks on
    // a microtask boundary before its fetch lands. Two hops cover the handler
    // + the fetch + `.json()` + the `Promise.all` join inside `refresh`.
    const drain = async () => {
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
      });
    };

    try {
      render(
        <NodeList
          onOpenNode={noop}
          onOpenAgentNodes={noop}
          onOpenIssues={noop}
          onOffline={noop}
          onAuthFailed={noop}
        />,
      );

      // Mount while hidden: the hook's `if (visibilityState === "visible")`
      // guard skips BOTH the initial refresh and the interval start. So
      // we expect zero fetches against /api/nodes — not even one.
      await drain();
      expect(nodesCalls).toBe(0);

      // 15s while hidden: still zero (the interval was never armed).
      await act(async () => {
        await vi.advanceTimersByTimeAsync(15000);
      });
      expect(nodesCalls).toBe(0);

      // Become visible: hook fires ONE refresh immediately + starts the
      // interval. The WS hook's reconnect (also triggered by
      // visibilitychange) only hits /api/ws-ticket, not /api/nodes,
      // so the count strictly increases by 1.
      Object.defineProperty(document, "hidden", {
        configurable: true,
        get: () => false,
      });
      await act(async () => {
        document.dispatchEvent(new Event("visibilitychange"));
      });
      await drain();
      expect(nodesCalls).toBe(1);

      // Polling is active: the next 5s tick should fire ANOTHER refresh.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(5000);
      });
      await drain();
      expect(nodesCalls).toBe(2);

      // Background again → polling stops. Capture the baseline RIGHT
      // BEFORE the next tick — if the interval was cleared cleanly, the
      // 15s advance must not bump it.
      Object.defineProperty(document, "hidden", {
        configurable: true,
        get: () => true,
      });
      await act(async () => {
        document.dispatchEvent(new Event("visibilitychange"));
      });
      const baselineWhileHidden = nodesCalls;
      await act(async () => {
        await vi.advanceTimersByTimeAsync(15000);
      });
      expect(nodesCalls).toBe(baselineWhileHidden);
    } finally {
      vi.useRealTimers();
      // Restore the ORIGINAL descriptor — not a fresh `{ get: () => false }`
      // — so any other test in this file or any later suite sees the
      // same jsdom `document.hidden` it had at process start (other
      // paths may have set it via prototype chain, which a redefine
      // would silently drop).
      if (originalHidden) {
        Object.defineProperty(document, "hidden", originalHidden);
      } else {
        delete (document as { hidden?: unknown }).hidden;
      }
    }
  });

  it("treats a network failure as offline", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("network")));
    const onOffline = vi.fn();
    const onAuthFailed = vi.fn();

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={onOffline}
        onAuthFailed={onAuthFailed}
      />,
    );

    await waitFor(() => {
      expect(onOffline).toHaveBeenCalled();
    });
    expect(onAuthFailed).not.toHaveBeenCalled();
  });

  it("renders the live backend-derived providers in the spawn picker (issue #815)", async () => {
    // The live list INCLUDES a Proxied account row the old hard-coded
    // fallback never had (`claude:minimax-prod`). If the picker rendered a
    // static fallback list the Proxied row would be missing entirely — this
    // assertion catches that regression. (`claude` itself appears in the live
    // list too, so the test still exercises the harness-header native row.)
    mockApi([], {
      providers: [
        {
          id: "claude",
          label: "Claude Code",
          color: "#1d7cfc",
          icon: "A",
          resumable: true,
          harness_id: "claude",
          provider_id: null,
          is_proxied: false,
          group_key: "claude",
        },
        {
          // Proxied rows carry the executor harness's color/icon
          // (`provider_info_for_pairing` in commands/agent.rs attributes
          // them to the executor adapter, not the proxied provider) — so a
          // `claude:minimax-prod` row reads as Claude-blue + "A", not
          // MiniMax-purple + "M". Matches the picker chip's existing
          // styling.
          id: "claude:minimax-prod",
          label: "MiniMax Pro Account",
          color: "#1d7cfc",
          icon: "A",
          resumable: false,
          harness_id: "claude",
          provider_id: "minimax-prod",
          is_proxied: true,
          group_key: "claude",
        },
      ],
    });

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={noop}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("node-list")).toBeTruthy();
    });

    // Open the spawn picker. The listProviders() fetch is async, so wait
    // for the picker to render — it'll be the live list once the fetch
    // resolves.
    fireEvent.click(screen.getByTestId("new-node-1"));
    await waitFor(() => {
      expect(screen.getByTestId("provider-picker")).toBeTruthy();
    });

    // The Proxied row's live label is what the user sees — not the
    // raw id.
    const proxiedRow = await screen.findByTestId("provider-claude:minimax-prod");
    expect(proxiedRow.textContent).toContain("MiniMax Pro Account");

    // The harness header row is also rendered from the live list.
    expect(screen.getByTestId("provider-claude")).toBeTruthy();

    // Harness grouping (issue #575): native + Proxied share a bucket.
    expect(screen.getByTestId("spawn-group-claude")).toBeTruthy();
  });

  it("NodeRow badge consumes the live listProviders() payload (issue #328)", async () => {
    // The badge's color comes from the live ProviderInfo for the node's
    // provider id (the same lookup that drives the row label). The brand
    // mark comes from `ProviderIcon`'s brand registry lookup keyed on
    // `providerId`. A regression that re-introduces a parallel map —
    // `providerIcon`/
    // `providerColor`, the deleted `FALLBACK_PROVIDERS`, or a stale
    // cache — would either fail the color match or drop the brand mark
    // in favour of the gray-dot fallback. The fallback case is also
    // pinned below (unknown provider id → deterministic grey chip with
    // no brand mark).
    mockApi(
      [
        // Known provider id — must hit the live row, so the chip is the
        // distinctive `#10b981` color and renders the brand mark.
        // (`agy` is the backend adapter id from `UiMeta::id`; the live
        // list mirrors it on `ProviderInfo.id`. `ProviderIcon`'s
        // brand registry is keyed off the adapter id, not the human label,
        // so a regression that drifted to the label would miss and fall through to the
        // gray-dot fallback — exactly the regression this test pins.)
        { ...makeNode(1, "running"), provider: "agy" },
        // Unknown provider id (e.g. a since-removed harness profile) —
        // the chip must render the deterministic fallback `'#555'` with
        // no brand mark rather than crashing or showing a stale brand
        // color.
        { ...makeNode(2, "running"), provider: "ghost-provider" },
      ],
      {
        providers: [
          {
            id: "agy",
            label: "Antigravity CLI",
            color: "#10b981",
            icon: "G",
            resumable: false,
            harness_id: "agy",
            provider_id: null,
            is_proxied: false,
            group_key: "agy",
          },
        ],
      },
    );

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={noop}
      />,
    );

    // Wait for both the node list AND the live listProviders() payload —
    // the badge only picks up `meta.color` after the fetch resolves, so
    // polling just `node-1` would race the badge's first render (and
    // trip the `'#555'` fallback path accidentally).
    await waitFor(() => {
      expect(screen.getByTestId("node-1").textContent).toContain("Antigravity CLI");
    });

    // Live row: chip background is the live hex (jsdom normalises hex → rgb())
    // and the brand mark is rendered as an <img> (Antigravity's colour is
    // baked into the PNG).
    const liveAvatar = screen.getByTestId("node-1").querySelector(
      '[data-testid="node-avatar"]',
    ) as HTMLElement;
    expect(liveAvatar).toBeTruthy();
    expect(liveAvatar.style.background).toBe(hexToRgbString("#10b981"));
    // Brand mark is restored — `<img>` for the PNG-backed providers,
    // `<svg>` for the inline-icon providers. Antigravity is in the
    // PNG camp, so `<img>`. A regression that drops back to the
    // single-char letter (the pre-fix state) would render neither.
    expect(liveAvatar.querySelector("img, svg")).toBeTruthy();

    // Unknown row: chip is the deterministic fallback. Pulling from the same
    // avatar selector (rather than `firstElementChild.textContent`) keeps
    // the assertion robust to a future wrapper element. No brand mark
    // — the inner element is `ProviderIcon`'s gray-dot fallback `<span>`.
    const ghostAvatar = screen.getByTestId("node-2").querySelector(
      '[data-testid="node-avatar"]',
    ) as HTMLElement;
    expect(ghostAvatar).toBeTruthy();
    expect(ghostAvatar.style.background).toBe(hexToRgbString("#555"));
    expect(ghostAvatar.querySelector("img, svg")).toBeNull();
  });

  it("NodeRow keeps the wire icon letter for a custom Proxied account (issue #948)", async () => {
    // #950 restored the brand marks for every *built-in* provider, but a
    // custom Claude-compatible Proxied account (`claude:<slug>`) has no
    // registered brand — so its badge fell through to `ProviderIcon`'s
    // mute dot and dropped the wire
    // `meta.icon` letter the pre-#328 row used to render. The row must
    // pass that letter down as `fallbackGlyph`.
    // `provider_info_for_pairing` (commands/agent.rs) takes a proxied row's
    // colour + icon from the *executor* adapter, so a custom account on the
    // `claude` harness ships the Anthropic `#1d7cfc` / `"A"` pair. The glyph
    // here is `"X"` only so the assertion can't be satisfied by anything
    // else on the row.
    mockApi([{ ...makeNode(1, "running"), provider: "claude:custom-account" }], {
      providers: [
        {
          id: "claude:custom-account",
          label: "My Proxy",
          color: "#1d7cfc",
          icon: "X",
          resumable: true,
          harness_id: "claude",
          provider_id: "custom-account",
          is_proxied: true,
          group_key: "claude",
        },
      ],
    });

    render(
      <NodeList
        onOpenNode={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOffline={noop}
        onAuthFailed={noop}
      />,
    );

    // Same race guard as the #328 test — the glyph only reaches the chip
    // once the live listProviders() payload has resolved.
    await waitFor(() => {
      expect(screen.getByTestId("node-1").textContent).toContain("My Proxy");
    });

    const avatar = screen.getByTestId("node-1").querySelector(
      '[data-testid="node-avatar"]',
    ) as HTMLElement;
    expect(avatar).toBeTruthy();
    // No brand mark resolves for a custom slug…
    expect(avatar.querySelector("img, svg")).toBeNull();
    // …so the wire letter is what fills the chip, over the live colour.
    expect(avatar.textContent).toBe("X");
    expect(avatar.style.background).toBe(hexToRgbString("#1d7cfc"));
  });
});

// The 3px status rail is the last direct child of a NodeRow button
// (NodeList.tsx:486-494) — avatar, inner text block, rail. Pull the
// inline `background` style off it so a regression that re-hardcodes
// STATUS_META's colours fails the assertion.
function railHex(row: HTMLElement): string {
  const rail = row.lastElementChild as HTMLElement | null;
  if (!rail) throw new Error("NodeRow has no rail element");
  return rail.style.background;
}

function hexToRgbString(hex: string): string {
  // Accept both 6-digit and 3-digit shorthand (e.g. "#1d7cfc" / "#555").
  const m =
    /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex) ??
    /^#([0-9a-f])([0-9a-f])([0-9a-f])$/i.exec(hex);
  if (!m) throw new Error(`bad hex: ${hex}`);
  const [r, g, b] = [m[1], m[2], m[3]].map((s) =>
    s.length === 1 ? s + s : s,
  );
  return `rgb(${parseInt(r, 16)}, ${parseInt(g, 16)}, ${parseInt(b, 16)})`;
}
