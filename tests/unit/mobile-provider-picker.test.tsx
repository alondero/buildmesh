/**
 * Mobile spawn picker (`ProviderPicker` in src/mobile/screens/NodeList.tsx).
 *
 * Issue #1086 — the picker rows must feed the live `listProviders()` `icon`
 * letter into `ProviderIcon` as `fallbackGlyph`, the same way `NodeRow` does
 * since #1082 (issue #948). A custom Proxied account (`claude:<slug>`) has no
 * entry in the brand registry, so without the fallback its picker chip renders
 * `ProviderIcon`'s bare dot instead of the wire glyph the user configured.
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

// One harness bucket: the native header row plus three Proxied children.
//   * `claude`                — registered brand (alias of `anthropic`).
//   * `claude:kimi`           — registered brand behind the composite id.
//   * `claude:custom-account` — no brand; wire glyph `"Z"`.
//   * `claude:blank-glyph`    — no brand; empty wire glyph.
//   * `homegrown`             — native row for an unregistered harness
//                               profile, wire glyph `"H"`. Custom harness
//                               profiles are user-named (ADR-0016), so the
//                               header row hits the same registry miss the
//                               Proxied children do.
// The glyph letters are deliberately not the providers' real ones so an
// assertion cannot be satisfied by anything else on the row.
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
  await screen.findByTestId("provider-claude:custom-account");
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

  it("renders the wire glyph in a custom Proxied child's 28px chip", async () => {
    await openPicker();

    const chip = screen.getByTestId("picker-avatar-claude:custom-account");
    // The indented child chip is the 28px one (the native header is 34px).
    expect(chip.style.width).toBe("28px");
    // `custom-account` has no brand registration, so nothing renders a mark…
    expect(chip.querySelector("img, svg")).toBeNull();
    // …and the live wire glyph is what fills the chip, over the live colour.
    expect(chip.textContent).toBe("Z");
  });

  it("renders the wire glyph for a native row whose harness has no brand", async () => {
    await openPicker();

    const chip = screen.getByTestId("picker-avatar-homegrown");
    expect(chip.style.width).toBe("34px");
    expect(chip.querySelector("img, svg")).toBeNull();
    expect(chip.textContent).toBe("H");
  });

  it("keeps a resolved brand mark ahead of the fallback glyph", async () => {
    await openPicker();

    // `ProviderIcon` renders exactly one inner node: a brand mark
    // (`<svg>`/`<img>`) or a `<span>` carrying the glyph or the dot. So a
    // mark with no `<span>` beside it is the precedence assertion — and it
    // survives the `<title>` the inline marks put in `textContent`.

    // Native header row: `claude` is an alias of the `anthropic` brand.
    const nativeChip = screen.getByTestId("picker-avatar-claude");
    expect(nativeChip.querySelector("img, svg")).toBeTruthy();
    expect(nativeChip.querySelector("span")).toBeNull();

    // Proxied child: `brandFor` reads the segment after the `:`, so
    // `claude:kimi` resolves the Kimi mark and the glyph must stay hidden.
    const childChip = screen.getByTestId("picker-avatar-claude:kimi");
    expect(childChip.querySelector("img, svg")).toBeTruthy();
    expect(childChip.querySelector("span")).toBeNull();
  });

  it("keeps the dot fallback when the wire glyph is empty", async () => {
    await openPicker();

    const chip = screen.getByTestId("picker-avatar-claude:blank-glyph");
    expect(chip.querySelector("img, svg")).toBeNull();
    expect(chip.textContent).toBe("");
    // ProviderIcon's neutral dot, not an empty glyph span.
    expect(chip.querySelector("span.rounded-full")).toBeTruthy();
  });
});
