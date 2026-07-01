/**
 * `NodeRow` (src/mobile/screens/NodeList.tsx) renders a per-node avatar in
 * a 34×34 colored chip. The avatar's icon comes from `ProviderIcon`
 * (src/components/Providers/ProviderIcon.tsx) — the single source of
 * truth for the provider → {shape, color} mapping.
 *
 * This test pins the avatar's DOM shape per provider, so a regression
 * (e.g. someone re-introducing a hard-coded letter glyph or stripping
 * the icon to a placeholder) is caught immediately. The
 * `EXPECTED_BADGES` table is the same one that used to pin the now-
 * deleted `providerIcon` / `providerColor` helpers; here it asserts
 * the rendered tag, the chip color, and the icon's className.
 */
import { describe, it, expect } from "vitest";
import { render } from "@testing-library/react";
import { NodeRow } from "../../src/mobile/screens/NodeList";
import type { AgentNode } from "../../src/mobile/api";

function stubNode(provider: string, id = 1): AgentNode {
  return {
    id,
    mesh_id: 1,
    name: `node-${id}`,
    path: "/tmp",
    branch: null,
    provider,
    status: "idle",
    cli_session_id: null,
    created_at: "2026-06-14T00:00:00Z",
  };
}

const noop = () => {};

// Per-provider contract: which tag `ProviderIcon` should render inside
// the chip, and which color the chip's background should be (hex, lower
// case, to match what `ProviderIcon`'s `PROVIDER_CHIP_COLORS` emits).
type Avatar = "svg" | "img" | "span";
const EXPECTED: Record<string, { tag: Avatar; chipColor: string }> = {
  // Inline-SVG providers (fill="currentColor", renders white over the chip).
  anthropic: { tag: "svg",  chipColor: "#1d7cfc" },
  codex:     { tag: "svg",  chipColor: "#10a37f" },
  opencode:  { tag: "svg",  chipColor: "#f59e0b" },
  kimi:      { tag: "svg",  chipColor: "#00c4c4" },
  terminal:  { tag: "svg",  chipColor: "#9ca3af" },
  // Brand-color file assets — the chip color is the provider's UiMeta
  // color and the asset's own pixels sit on top.
  minimax:   { tag: "img",  chipColor: "#6366f1" },
  agy:       { tag: "img",  chipColor: "#10b981" },
};

describe("NodeRow avatar", () => {
  for (const [id, { tag, chipColor }] of Object.entries(EXPECTED)) {
    it(`renders a <${tag}> for "${id}" on a ${chipColor} chip`, () => {
      const { container } = render(
        <NodeRow node={stubNode(id)} onClick={noop} />,
      );

      const avatar = container.querySelector('[data-testid="node-avatar"]');
      expect(avatar).toBeTruthy();

      // The avatar must contain the expected icon tag — never a text
      // node with a single letter. (The old code rendered "A" / "M" /
      // etc. as text; the new code delegates to <ProviderIcon>.) We
      // use querySelector rather than `firstElementChild` so a future
      // wrapper element (e.g. a `<div>` for additional padding) is
      // detected via `children.length` rather than producing a
      // confusing "expected 'div' to be 'svg'" error from the
      // tagName check. The `textContent` route doesn't work because
      // the inline SVGs carry an accessible <title> child.
      const icon = avatar!.querySelector(tag);
      expect(icon).toBeTruthy();
      // The icon is the avatar's only direct child — no wrapper, no
      // stray text node siblings.
      expect(avatar!.children.length).toBe(1);
      expect(avatar!.firstElementChild).toBe(icon);

      // The chip's background must be the provider's brand color.
      // Style values are normalised to `rgb(...)` by the browser; we
      // compare the hex through a stable round-trip.
      const inlineStyle = (avatar as HTMLElement).style.background;
      expect(inlineStyle).toBeTruthy();
      // jsdom lower-cases hex inside rgb(). Compare against the
      // lower-cased form.
      const expectedRgb = hexToRgbString(chipColor);
      expect(inlineStyle).toBe(expectedRgb);
    });
  }

  it("falls back to a gray <span> for unknown providers", () => {
    const { container } = render(
      <NodeRow node={stubNode("mystery")} onClick={noop} />,
    );
    const avatar = container.querySelector('[data-testid="node-avatar"]');
    expect(avatar).toBeTruthy();
    const span = avatar!.querySelector("span");
    expect(span).toBeTruthy();
    expect(span!.className).toContain("bg-text-muted");
    // Unknown providers get a neutral chip background.
    const inlineStyle = (avatar as HTMLElement).style.background;
    expect(inlineStyle).toBe(hexToRgbString("#555"));
  });
});

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
