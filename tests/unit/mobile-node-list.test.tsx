/**
 * NodeList: archived nodes are hidden, awaiting-input nodes are pinned in
 * the attention section with a readable status label, and an auth failure
 * (revoked token) routes to onAuthFailed instead of the offline overlay.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import NodeList from "../../src/mobile/screens/NodeList";
import type { AgentNode, Mesh, NodeStatus } from "../../src/mobile/api";

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

function mockApi(nodes: AgentNode[], opts?: { status?: number }) {
  const status = opts?.status ?? 200;
  const fn = vi.fn().mockImplementation(async (url: string) => {
    const ok = status >= 200 && status < 300;
    const body = url.includes("/api/meshes")
      ? [mesh]
      : url.includes("/api/nodes")
        ? nodes
        : []; // providers
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
        onOpenSessions={noop}
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
    // Awaiting-input node appears twice: pinned + in its mesh section.
    const attention = screen.getByTestId("attention-section");
    expect(attention.textContent).toContain("node-3");
    expect(attention.textContent).toContain("needs input");
    // Running node renders normally.
    expect(screen.getByTestId("node-1")).toBeTruthy();
  });

  it("routes a 401 to onAuthFailed, not the offline overlay", async () => {
    mockApi([], { status: 401 });
    const onOffline = vi.fn();
    const onAuthFailed = vi.fn();

    render(
      <NodeList
        onOpenNode={noop}
        onOpenSessions={noop}
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
        onOpenSessions={noop}
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
});
