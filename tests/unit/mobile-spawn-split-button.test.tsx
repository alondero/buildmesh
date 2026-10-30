/**
 * The default provider's brand mark on the phone's spawn split button.
 *
 * The desktop's spawn cluster draws the resolved default harness's brand mark
 * on its quick-spawn half (issue #2040): the mark is how the user tells
 * *which* provider the default half will run before they tap it. The phone
 * draws the same mark beside its label.
 *
 * The mark comes from the mesh's *pinned* default Spawn Option only. An
 * app-wide default is invisible to the phone, so an unpinned mesh keeps the
 * **Mesh default** label and shows no mark rather than a wrong one.
 */
import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { SpawnSplitButton } from "../../src/mobile/components/SpawnSplitButton";
import type { Provider } from "../../src/mobile/api";

/** A native Spawn Option with a registered brand mark. */
const claude = {
  id: "claude",
  label: "Claude Code",
  harness_id: "claude",
  provider_id: null,
  icon: "C",
} as Provider;

/** A custom harness profile: no registered brand, so the wire glyph stands in. */
const custom = {
  id: "custom-harness",
  label: "DeepSeek via Claude",
  harness_id: "custom-harness",
  provider_id: null,
  icon: "D",
} as Provider;

function renderSplitButton(provider: Provider | null, label: string) {
  return render(
    <SpawnSplitButton
      label={label}
      provider={provider}
      testId="issue-spawn-42"
      menuOpen={false}
      onStart={() => {}}
      onToggleMenu={() => {}}
    />,
  );
}

describe("the phone's spawn split button", () => {
  it("draws the default provider's brand mark beside the label", () => {
    renderSplitButton(claude, "Claude Code");

    const main = screen.getByTestId("issue-spawn-42");
    expect(main.querySelector(".spawn-split-icon")).not.toBeNull();
    // The registered brand mark itself, not the neutral fallback dot.
    expect(
      main.querySelector(".spawn-split-brand")?.tagName.toLowerCase(),
    ).toBe("svg");
    expect(
      main.querySelector(".spawn-split-label")?.textContent,
    ).toBe("Claude Code");
  });

  it("shows no mark when the mesh pins no default the phone can see", () => {
    renderSplitButton(null, "Mesh default");

    const main = screen.getByTestId("issue-spawn-42");
    expect(main.querySelector(".spawn-split-icon")).toBeNull();
    expect(
      main.querySelector(".spawn-split-label")?.textContent,
    ).toBe("Mesh default");
  });

  it("falls back to the wire glyph for a harness with no brand mark", () => {
    renderSplitButton(custom, "DeepSeek via Claude");

    const main = screen.getByTestId("issue-spawn-42");
    const slot = main.querySelector(".spawn-split-icon");
    expect(slot).not.toBeNull();
    expect(slot?.textContent).toBe("D");
  });

  it("drops the mark while the launch is in flight", () => {
    render(
      <SpawnSplitButton
        label="Claude Code"
        busyLabel="Spawning agent…"
        provider={claude}
        busy
        testId="issue-spawn-42"
        menuOpen={false}
        onStart={() => {}}
        onToggleMenu={() => {}}
      />,
    );

    const main = screen.getByTestId("issue-spawn-42");
    expect(main.querySelector(".spawn-split-icon")).toBeNull();
    expect(main.textContent).toContain("Spawning agent…");
  });
});
