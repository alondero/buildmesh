/**
 * Mobile spawn picker (`ProviderPicker` in src/mobile/screens/NodeList.tsx).
 *
 * Spawn choices are harness parents, with saved Launch Configurations inside
 * touch disclosures. Raw Provider Routes stay out of the spawn picker even
 * when the last saved configuration is deleted. Native harness avatar glyphs
 * still come from the live `listProviders()` row.
 *
 * The picker is module-private, so these mount `NodeList` and open the sheet
 * through the mesh's "new node" button — the same route the #815 picker test
 * takes in mobile-node-list.test.tsx.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import NodeList from "../../src/mobile/screens/NodeList";
import type { AgentNode, Mesh, Provider } from "../../src/mobile/api";

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

function provider(over: Partial<Provider> & Pick<Provider, "id">): Provider {
  return {
    label: over.id,
    color: "#1d7cfc",
    icon: "A",
    resumable: true,
    harness_id: "claude",
    provider_id: null,
    is_proxied: false,
    group_key: "claude",
    ...over,
  };
}

// One harness bucket: the native header row plus three backend Provider Routes.
//   * `claude`                — registered brand (alias of `anthropic`).
//   * `claude:kimi`           — registered brand behind the composite id.
//   * `claude:custom-account` — no brand; wire glyph `"Z"`.
//   * `claude:blank-glyph`    — no brand; empty wire glyph.
//   * `homegrown`             — native row for an unregistered harness
//                               profile, wire glyph `"H"`. Custom harness
//                               profiles are user-named (ADR-0016), so the
//                               header row hits the same registry miss the
//                               Proxied children do.
// The route rows must not appear as flat spawn actions; their data remains
// available to configuration selectors elsewhere.
const PROVIDERS: Provider[] = [
  provider({ id: "claude", label: "Claude Code", icon: "A" }),
  provider({
    id: "claude:kimi",
    label: "Kimi Account",
    icon: "Q",
    provider_id: "kimi",
    is_proxied: true,
  }),
  provider({
    id: "claude:custom-account",
    label: "My Proxy",
    icon: "Z",
    provider_id: "custom-account",
    is_proxied: true,
  }),
  provider({
    id: "claude:blank-glyph",
    label: "Glyphless Proxy",
    icon: "",
    provider_id: "blank-glyph",
    is_proxied: true,
  }),
  provider({
    id: "homegrown",
    label: "Homegrown Harness",
    icon: "H",
    harness_id: "homegrown",
    group_key: "homegrown",
  }),
];

function mockApi(providers: Provider[]) {
  const nodes: AgentNode[] = [];
  const fn = vi.fn().mockImplementation(async (url: string) => {
    let body: unknown;
    if (url.includes("/api/meshes")) body = [mesh];
    else if (url.includes("/api/nodes")) body = nodes;
    else if (url.includes("/api/providers")) body = providers;
    else body = [];
    return { ok: true, status: 200, json: async () => body };
  });
  vi.stubGlobal("fetch", fn);
  return fn;
}

const noop = () => {};

/** Mount NodeList, open the spawn sheet, and wait for the live rows. */
async function openPicker(): Promise<void> {
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

  fireEvent.click(screen.getByTestId("new-node-1"));
  await waitFor(() => {
    expect(screen.getByTestId("provider-picker")).toBeTruthy();
  });
  // The listProviders() fetch is async: until it lands the picker renders
  // the hardcoded fallback list, which carries none of these rows.
  await screen.findByTestId("provider-homegrown");
}

describe("mobile ProviderPicker fallback glyphs (issue #1086)", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.stubGlobal("WebSocket", FakeWebSocket);
    mockApi(PROVIDERS);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("sends the selected saved configuration from its harness submenu", async () => {
    const recipe = { id: "proxy-max", name: "Proxy Max", spawn_option_id: "claude:custom-account", model: "some-model", effort: "max", extra_args: null };
    const fetch = mockApi([...PROVIDERS, provider({ id: recipe.id, label: recipe.name, provider_id: "custom-account", is_proxied: true, configuration: recipe })]);
    await openPicker();
    expect(screen.queryByTestId("provider-claude:custom-account")).toBeNull();
    fireEvent.click(screen.getByText("Claude Code configurations"));
    fireEvent.click(screen.getByRole("button", { name: "Proxy Max" }));
    await waitFor(() => {
      const call = fetch.mock.calls.find(([url]) => String(url).includes("/api/nodes/create"));
      expect(call).toBeTruthy();
      expect(JSON.parse(call![1].body)).toEqual({ rows: 24, cols: 80, mesh_id: 1, provider: "claude:custom-account", configuration_id: "proxy-max" });
    });
  });

  it("groups generated launch configurations under the harness on mobile", async () => {
    const recipe = { id: "launch/claude:kimi", name: "Kimi via Claude", spawn_option_id: "claude:kimi", model: null, effort: null, extra_args: null };
    const fetch = mockApi([
      PROVIDERS[0], PROVIDERS[1],
      provider({ id: recipe.id, label: recipe.name, provider_id: "kimi", is_proxied: true, configuration: recipe }),
    ]);
    render(<NodeList onOpenNode={noop} onOpenAgentNodes={noop} onOpenIssues={noop} onOffline={noop} onAuthFailed={noop} />);
    await screen.findByTestId("node-list");
    fireEvent.click(screen.getByTestId("new-node-1"));
    await screen.findByTestId("provider-claude");
    expect(screen.queryByTestId("provider-claude:kimi")).toBeNull();
    fireEvent.click(screen.getByText("Claude Code configurations"));
    fireEvent.click(screen.getByRole("button", { name: "Kimi via Claude" }));
    await waitFor(() => {
      const call = fetch.mock.calls.find(([url]) => String(url).includes("/api/nodes/create"));
      expect(call).toBeTruthy();
      expect(JSON.parse(call![1].body)).toMatchObject({ provider: "claude:kimi", configuration_id: "launch/claude:kimi" });
    });
  });

  it("keeps direct routes out of the picker when no recipes remain", async () => {
    await openPicker();
    expect(screen.queryByTestId("provider-claude:kimi")).toBeNull();
    expect(screen.queryByTestId("provider-claude:custom-account")).toBeNull();
    expect(screen.queryByText("Claude Code configurations")).toBeNull();
  });

  it("never promotes a configuration row to a launchable harness parent", async () => {
    const recipe = { id: "only-recipe", name: "Codex Sol", spawn_option_id: "codex", model: "gpt-5.6-sol", effort: null, extra_args: null };
    const fetch = mockApi([provider({ id: recipe.id, label: recipe.name, harness_id: "codex", group_key: "codex", configuration: recipe })]);
    render(<NodeList onOpenNode={noop} onOpenAgentNodes={noop} onOpenIssues={noop} onOffline={noop} onAuthFailed={noop} />);
    await screen.findByTestId("node-list");
    fireEvent.click(screen.getByTestId("new-node-1"));
    await screen.findByTestId("provider-picker");
    expect(screen.queryByTestId("provider-only-recipe")).toBeNull();
    expect(screen.getByRole("heading", { name: "codex" })).toBeTruthy();
    fireEvent.click(screen.getByText("codex configurations"));
    fireEvent.click(await screen.findByRole("button", { name: "Codex Sol" }));
    await waitFor(() => {
      const call = fetch.mock.calls.find(([url]) => String(url).includes("/api/nodes/create"));
      expect(call).toBeTruthy();
      expect(JSON.parse(call![1].body)).toMatchObject({ provider: "codex", configuration_id: "only-recipe" });
    });
  });

  it("renders the wire glyph for a native row whose harness has no brand", async () => {
    await openPicker();

    const chip = screen.getByTestId("picker-avatar-homegrown");
    expect(chip.style.width).toBe("34px");
    expect(chip.querySelector("img, svg")).toBeNull();
    expect(chip.textContent).toBe("H");
  });

  it("keeps a resolved brand mark ahead of the native fallback glyph", async () => {
    await openPicker();

    // `ProviderIcon` renders exactly one inner node: a brand mark
    // (`<svg>`/`<img>`) or a `<span>` carrying the glyph or the dot. So a
    // mark with no `<span>` beside it is the precedence assertion — and it
    // survives the `<title>` the inline marks put in `textContent`.

    // Native header row: `claude` is an alias of the `anthropic` brand.
    const nativeChip = screen.getByTestId("picker-avatar-claude");
    expect(nativeChip.querySelector("img, svg")).toBeTruthy();
    expect(nativeChip.querySelector("span")).toBeNull();

  });
});
