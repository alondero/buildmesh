/**
 * NodeList: archived nodes are hidden, awaiting-input nodes are pinned in
 * the attention section with a readable status label, and an auth failure
 * (revoked token) routes to onAuthFailed instead of the offline overlay.
 * Issue #815 — the spawn picker consumes the live `listProviders()` rows
 * (First-class / Proxied providers, harness ordering) rather than a
 * hardcoded fallback, so newly-configured harnesses reach mobile.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
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
    // The badge's color/icon come from the live ProviderInfo for the node's
    // provider id (the same lookup that drives the row label). A regression
    // that re-introduces a hard-coded map — `PROVIDER_CHIP_COLORS`,
    // `providerIcon`/`providerColor`, the deleted `FALLBACK_PROVIDERS`, or a
    // stale cache — would either fail the color match or land on the
    // fallback `'?' / '#555'`. The fallback case is also pinned below
    // (unknown provider id → deterministic grey).
    mockApi(
      [
        // Known provider id — must hit the live row, so the chip is the
        // distinctive `#abcdef` color and the "Z" glyph.
        { ...makeNode(1, "running"), provider: "liveprovider" },
        // Unknown provider id (e.g. a since-removed harness profile) —
        // the chip must render the deterministic fallback `'?' / '#555'`
        // rather than crashing or showing a stale brand color.
        { ...makeNode(2, "running"), provider: "ghost-provider" },
      ],
      {
        providers: [
          {
            id: "liveprovider",
            label: "Live Provider",
            color: "#abcdef",
            icon: "Z",
            resumable: false,
            harness_id: "liveprovider",
            provider_id: null,
            is_proxied: false,
            group_key: "liveprovider",
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
    // the badge only picks up `meta.color`/`meta.icon` after the fetch
    // resolves, so polling just `node-1` would race the badge's first
    // render (and trip the `'?' / '#555'` fallback path accidentally).
    await waitFor(() => {
      expect(screen.getByTestId("node-1").textContent).toContain("Live Provider");
    });

    // Live row: chip background is the live hex (jsdom normalises hex → rgb())
    // and the glyph is the live single-char icon.
    const liveAvatar = screen.getByTestId("node-1").querySelector(
      '[data-testid="node-avatar"]',
    ) as HTMLElement;
    expect(liveAvatar).toBeTruthy();
    expect(liveAvatar.style.background).toBe(hexToRgbString("#abcdef"));
    expect(liveAvatar.textContent).toBe("Z");

    // Unknown row: chip is the deterministic fallback. Pulling from the same
    // avatar selector (rather than `firstElementChild.textContent`) keeps
    // the assertion robust to a future wrapper element.
    const ghostAvatar = screen.getByTestId("node-2").querySelector(
      '[data-testid="node-avatar"]',
    ) as HTMLElement;
    expect(ghostAvatar).toBeTruthy();
    expect(ghostAvatar.style.background).toBe(hexToRgbString("#555"));
    expect(ghostAvatar.textContent).toBe("?");
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
