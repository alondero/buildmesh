import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import CaptureIdea from "../../src/mobile/screens/CaptureIdea";
import NodeOverview from "../../src/mobile/screens/NodeOverview";
import NodeList from "../../src/mobile/screens/NodeList";
import type { AgentNode, Mesh, Provider } from "../../src/mobile/api";

const mesh = { id: 1, name: "Buildmesh" } as Mesh;
const provider = {
  id: "anthropic",
  label: "Claude Code",
  capabilities: { supports_prefill: true },
} as Provider;
const node = {
  id: 7,
  mesh_id: 1,
  name: "Fix search",
  provider: "anthropic",
  status: "idle",
} as AgentNode;
const callbacks = () => ({ onStarted: vi.fn(), onAuthFailed: vi.fn() });
const response = (data: unknown, status = 200) =>
  new Response(JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json" },
  });

beforeEach(() => {
  localStorage.clear();
  vi.stubGlobal(
    "WebSocket",
    class {
      close() {}
    },
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
});

describe("mobile idea capture", () => {
  it("keeps a draft across remounts and sends the selected mesh and initial prompt", async () => {
    const cb = callbacks();
    const fetcher = vi.fn().mockResolvedValue(response(node));
    vi.stubGlobal("fetch", fetcher);
    const first = render(
      <CaptureIdea meshes={[mesh]} providers={[provider]} {...cb} />,
    );
    expect(screen.getByRole("heading", { name: "New idea" })).toBeTruthy();
    expect(screen.queryByText("What should we build?")).toBeNull();
    fireEvent.change(screen.getByLabelText("Your idea"), {
      target: { value: "Fix search\nKeep keyboard support." },
    });
    first.unmount();
    render(<CaptureIdea meshes={[mesh]} providers={[provider]} {...cb} />);
    expect(
      (screen.getByLabelText("Your idea") as HTMLTextAreaElement).value,
    ).toBe("Fix search\nKeep keyboard support.");
    fireEvent.click(screen.getByText("Start working on this"));
    await waitFor(() => expect(cb.onStarted).toHaveBeenCalledWith(node));
    expect(fetcher.mock.calls[0][0]).toBe("/api/nodes/create");
    expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({
      rows: 24,
      cols: 80,
      mesh_id: 1,
      provider: "anthropic",
      prompt: "Fix search\nKeep keyboard support.",
    });
    expect(localStorage.getItem("buildmesh_mobile_idea")).toBeNull();
  });

  it("retains text and prevents duplicate launches while pending, then exposes a failure", async () => {
    let resolve!: (value: Response) => void;
    const fetcher = vi.fn(
      () =>
        new Promise<Response>((r) => {
          resolve = r;
        }),
    );
    vi.stubGlobal("fetch", fetcher);
    render(
      <CaptureIdea meshes={[mesh]} providers={[provider]} {...callbacks()} />,
    );
    fireEvent.change(screen.getByLabelText("Your idea"), {
      target: { value: "A useful idea" },
    });
    fireEvent.click(screen.getByText("Start working on this"));
    fireEvent.click(screen.getByText("Starting agent…"));
    expect(fetcher).toHaveBeenCalledTimes(1);
    await act(async () => resolve(response({ error: "Launch failed" }, 503)));
    expect(await screen.findByRole("alert")).toHaveProperty(
      "textContent",
      expect.stringContaining("Launch failed"),
    );
    expect(localStorage.getItem("buildmesh_mobile_idea")).toBe("A useful idea");
  });

  it("excludes agents without prompt support and keeps the draft through auth expiry", async () => {
    const cb = callbacks();
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(response({ error: "expired" }, 401)),
    );
    render(
      <CaptureIdea
        meshes={[mesh]}
        providers={[
          provider,
          {
            ...provider,
            id: "terminal",
            label: "Terminal",
            capabilities: { supports_prefill: false },
          } as Provider,
        ]}
        {...cb}
      />,
    );
    expect(screen.queryByRole("option", { name: "Terminal" })).toBeNull();
    fireEvent.change(screen.getByLabelText("Your idea"), {
      target: { value: "Persist this idea" },
    });
    fireEvent.click(screen.getByText("Start working on this"));
    await waitFor(() => expect(cb.onAuthFailed).toHaveBeenCalledOnce());
    expect(localStorage.getItem("buildmesh_mobile_idea")).toBe(
      "Persist this idea",
    );
    expect(cb.onStarted).not.toHaveBeenCalled();
  });
});

describe("mobile work details", () => {
  it("sends a reply without opening the terminal and enforces the encoded body limit", async () => {
    const fetcher = vi.fn((url: string) =>
      Promise.resolve(response(url === "/api/nodes" ? [node] : { ok: true })),
    );
    vi.stubGlobal("fetch", fetcher);
    const onTerminal = vi.fn();
    render(
      <NodeOverview
        node={node}
        visitId={1}
        onBack={vi.fn()}
        onTerminal={onTerminal}
        onChanges={vi.fn()}
        onAuthFailed={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByLabelText("Reply or give direction"), {
      target: { value: "Continue with the fix" },
    });
    fireEvent.click(screen.getByText("Send reply"));
    expect(
      await screen.findByText("Reply delivered to the terminal."),
    ).toBeTruthy();
    const inputCall = fetcher.mock.calls.find((call) =>
      call[0].endsWith("/input"),
    );
    expect(inputCall?.[0]).toBe("/api/nodes/7/input");
    expect(onTerminal).not.toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText("Reply or give direction"), {
      target: { value: "🙂".repeat(260) },
    });
    expect(screen.getByText("Send reply")).toHaveProperty("disabled", true);
    expect(screen.getByRole("alert").textContent).toContain("too long");
  });

  it("disables replies when a refreshed agent has disappeared", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(() => Promise.resolve(response([]))),
    );
    render(
      <NodeOverview
        node={node}
        visitId={1}
        onBack={vi.fn()}
        onTerminal={vi.fn()}
        onChanges={vi.fn()}
        onAuthFailed={vi.fn()}
      />,
    );
    expect(await screen.findByRole("alert")).toHaveProperty(
      "textContent",
      expect.stringContaining("no longer available"),
    );
    expect(screen.getByText("Send reply")).toHaveProperty("disabled", true);
  });

  it("clears a persisted reply if delivery finishes after details unmount", async () => {
    let resolveInput!: (value: Response) => void;
    const inputResponse = new Promise<Response>((resolve) => {
      resolveInput = resolve;
    });
    const fetcher = vi.fn((url: string) =>
      url.endsWith("/input")
        ? inputResponse
        : Promise.resolve(response([node])),
    );
    vi.stubGlobal("fetch", fetcher);
    const onDraftChange = vi.fn();
    const onReplySendingChange = vi.fn();
    const onReplyNoticeChange = vi.fn();
    const view = render(
      <NodeOverview
        node={node}
        visitId={1}
        draft="Check the deployment logs"
        replySending={false}
        replyNotice=""
        onDraftChange={onDraftChange}
        onReplySendingChange={onReplySendingChange}
        onReplyNoticeChange={onReplyNoticeChange}
        onBack={vi.fn()}
        onTerminal={vi.fn()}
        onChanges={vi.fn()}
        onAuthFailed={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByText("Send reply"));
    await waitFor(() =>
      expect(onReplySendingChange).toHaveBeenCalledWith(true, node.id, 1),
    );
    view.unmount();

    await act(async () => resolveInput(response({ ok: true })));

    expect(onDraftChange).toHaveBeenLastCalledWith("", node.id, 1);
    expect(onReplyNoticeChange).toHaveBeenLastCalledWith(
      "Reply delivered to the terminal.",
      node.id,
      1,
    );
    expect(onReplySendingChange).toHaveBeenLastCalledWith(false, node.id, 1);
  });
});

it("surfaces errors and degraded signals before normal work, and filters work by mesh", async () => {
  vi.stubGlobal(
    "WebSocket",
    class {
      close() {}
    },
  );
  const nodes = [
    { ...node, status: "awaiting_input" },
    { ...node, id: 8, mesh_id: 2, name: "Deploy Preview", status: "error" },
    { ...node, id: 9, name: "Missing signals", signal_health: "unavailable" },
  ];
  vi.stubGlobal(
    "fetch",
    vi.fn((url: string) =>
      Promise.resolve(
        response(
          url === "/api/nodes"
            ? nodes
            : url === "/api/meshes"
              ? [mesh, { ...mesh, id: 2, name: "Website" }]
              : url === "/api/providers"
                ? [provider]
                : { ticket: "test" },
        ),
      ),
    ),
  );
  render(
    <NodeList
      onOpenNode={vi.fn()}
      onOpenAgentNodes={vi.fn()}
      onOpenIssues={vi.fn()}
      onOffline={vi.fn()}
      onAuthFailed={vi.fn()}
    />,
  );
  await screen.findByTestId("node-list");
  expect(screen.getByRole("heading", { name: "Overview" })).toBeTruthy();
  expect(
    screen.queryByText("See what needs you. Keep things moving."),
  ).toBeNull();
  expect(screen.getByTestId("node-8").textContent).toContain("Mesh: Website");
  expect(screen.getByTestId("attn-card-7").textContent).toContain(
    "Mesh: Buildmesh",
  );
  expect(
    screen.getByRole("region", { name: "Problems" }).textContent,
  ).toContain("Deploy Preview");
  expect(
    screen.getByRole("region", { name: "Problems" }).textContent,
  ).toContain("Missing signals");
  fireEvent.click(screen.getByRole("button", { name: "Work", exact: true }));
  fireEvent.change(screen.getByLabelText("Search work"), {
    target: { value: "nothing matches" },
  });
  expect(screen.queryByTestId("node-7")).toBeNull();
  fireEvent.change(screen.getByLabelText("Search work"), {
    target: { value: "Buildmesh" },
  });
  expect(screen.getByTestId("node-7")).toBeTruthy();
});

it("keeps Capture mounted until a pending launch is confirmed", async () => {
  let finish!: (value: Response) => void;
  const onOpenNode = vi.fn();
  const fetcher = vi.fn((url: string) => {
    if (url === "/api/nodes/create")
      return new Promise<Response>((resolve) => {
        finish = resolve;
      });
    return Promise.resolve(
      response(
        url === "/api/nodes"
          ? [node]
          : url === "/api/meshes"
            ? [mesh]
            : url === "/api/providers"
              ? [provider]
              : { ticket: "test" },
      ),
    );
  });
  vi.stubGlobal("fetch", fetcher);
  render(
    <NodeList
      onOpenNode={onOpenNode}
      onOpenAgentNodes={vi.fn()}
      onOpenIssues={vi.fn()}
      onOffline={vi.fn()}
      onAuthFailed={vi.fn()}
    />,
  );
  await screen.findByTestId("node-list");
  fireEvent.click(screen.getByRole("button", { name: "Capture", exact: true }));
  fireEvent.change(screen.getByLabelText("Your idea"), {
    target: { value: "Do this once" },
  });
  fireEvent.click(screen.getByText("Start working on this"));
  const work = screen.getByRole("button", { name: "Work", exact: true });
  expect(work).toHaveProperty("disabled", true);
  fireEvent.click(work);
  expect(screen.getByLabelText("Your idea")).toBeTruthy();
  await act(async () => finish(response(node)));
  expect(onOpenNode).toHaveBeenCalledTimes(1);
  expect(localStorage.getItem("buildmesh_mobile_idea")).toBeNull();
  expect(work).toHaveProperty("disabled", false);
});

it("remembers the chosen destination when returning to a captured idea", () => {
  const props = {
    meshes: [mesh, { ...mesh, id: 2, name: "Website" }],
    providers: [provider],
    ...callbacks(),
  };
  const first = render(<CaptureIdea {...props} />);
  fireEvent.change(screen.getByLabelText("Work in"), {
    target: { value: "2" },
  });
  fireEvent.change(screen.getByLabelText("Your idea"), {
    target: { value: "Improve the website" },
  });
  first.unmount();
  render(<CaptureIdea {...props} />);
  expect(screen.getByLabelText("Work in")).toHaveProperty("value", "2");
  expect(screen.getByLabelText("Your idea")).toHaveProperty(
    "value",
    "Improve the website",
  );
});

it("makes stale destinations explicit and allows choosing the only available replacement", () => {
  localStorage.setItem(
    "buildmesh_mobile_idea_destination",
    JSON.stringify({ meshId: "99", providerId: "deleted" }),
  );
  localStorage.setItem(
    "buildmesh_mobile_idea",
    "An idea for the replacement mesh",
  );
  render(
    <CaptureIdea meshes={[mesh]} providers={[provider]} {...callbacks()} />,
  );
  expect(
    screen.getByText("Previous mesh unavailable — choose a mesh"),
  ).toBeTruthy();
  expect(
    screen.getByText("Previous agent unavailable — choose an agent"),
  ).toBeTruthy();
  expect(screen.getByText("Start working on this")).toHaveProperty(
    "disabled",
    true,
  );
  fireEvent.change(screen.getByLabelText("Work in"), {
    target: { value: "1" },
  });
  fireEvent.change(screen.getByLabelText("Agent"), {
    target: { value: "anthropic" },
  });
  expect(screen.getByText("Start working on this")).toHaveProperty(
    "disabled",
    false,
  );
});
