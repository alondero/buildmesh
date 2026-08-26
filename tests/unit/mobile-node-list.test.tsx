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

class FakeWebSocket {
  onopen: (() => void) | null = null;
  onmessage: ((e: unknown) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  close() {}
  send() {}
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
  const fn = vi.fn().mockImplementation(async (url: string) => {
    const ok = status >= 200 && status < 300;
    let body: unknown;
    if (url.includes("/api/meshes")) body = [mesh];
    else if (url.includes("/api/nodes")) body = nodes;
    else if (url.includes("/api/providers")) body = providers ?? [];
    else body = [];
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
    expect(railHex(row3)).toBe(hexToRgbString("#f59e0b")); // awaiting_input
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
    // Backgrounded tab poll would burn battery + server churn for state
    // the user can't see — mirror the WS hook's resume-on-foreground
    // shape (useWsEvents / issue #806): on `visibilitychange` to
    // `hidden`, stop the interval; on the way back, fire ONE refresh
    // immediately and resume polling.
    //
    // jsdom defaults `document.hidden` to false. Force it true BEFORE
    // render so the mount-time `if (!document.hidden) start()` skips
    // the interval entirely.
    //
    // NOTE: under `vi.useFakeTimers()`, `waitFor` polls on a faked
    // `setTimeout` so it can never wake up. Match the existing
    // 5-second-poll test's shape — drain microtasks with `await
    // Promise.resolve()` chains inside `act()` rather than `waitFor`.
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

      // Initial mount: ONE refresh (the unconditional mount fetch), no
      // polling started because `document.hidden === true`.
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(nodesCalls).toBe(1);

      // 15s while hidden → still 1. The interval was never armed.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(15000);
      });
      expect(nodesCalls).toBe(1);

      // Become visible → visibilitychange handler refreshes once
      // immediately, then starts the interval.
      Object.defineProperty(document, "hidden", {
        configurable: true,
        get: () => false,
      });
      await act(async () => {
        document.dispatchEvent(new Event("visibilitychange"));
        // Drain microtasks for refresh() + the WS hook's reconnect
        // fetch that visibilitychange also fires.
        await Promise.resolve();
        await Promise.resolve();
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(nodesCalls).toBe(2);

      // Now polling is active: the next 5s tick should fire ANOTHER
      // refresh.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(5000);
      });
      expect(nodesCalls).toBe(3);

      // Background again → polling stops. No further fetch on the tick.
      Object.defineProperty(document, "hidden", {
        configurable: true,
        get: () => true,
      });
      await act(async () => {
        document.dispatchEvent(new Event("visibilitychange"));
        await Promise.resolve();
      });
      // (Use the count captured RIGHT BEFORE the next tick — if the
      // interval was cleared cleanly, 15s shouldn't bump it.)
      const baselineWhileHidden = nodesCalls;
      await act(async () => {
        await vi.advanceTimersByTimeAsync(15000);
      });
      expect(nodesCalls).toBe(baselineWhileHidden);
    } finally {
      vi.useRealTimers();
      // jsdom exposes document.hidden as a getter — restore the default
      // so subsequent tests in the file (which don't expect a hidden
      // document) aren't surprised.
      Object.defineProperty(document, "hidden", {
        configurable: true,
        get: () => false,
      });
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
