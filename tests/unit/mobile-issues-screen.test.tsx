/**
 * IssuesScreen: wire shape contract with the Rust backend.
 *
 * History:
 *  - The Rust `GitHubIssue` struct used to only serialise
 *    `{number, title, body}`. The mobile TS interface declared
 *    `{labels, url, state}` too, so the screen was defensively coded
 *    (`issue.labels ?? []`, `issue.url && (<a.../>)`) — and the View
 *    link was hidden because `url` was never present.
 *  - Issue #358 widened the Rust struct + `services::github::Issue`
 *    so all six fields ship on the wire. The screen now reads them
 *    straight, and the regression test below pins the contract:
 *    the link IS visible and points at the issue URL, label chips
 *    render from the `labels[]` array.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import IssuesScreen from "../../src/mobile/screens/IssuesScreen";
import type { Mesh, GitHubIssue } from "../../src/mobile/api";

const mesh: Mesh = {
  id: 1,
  name: "buildmesh",
  path: "/tmp/repo",
  created_at: "2026-06-11T00:00:00Z",
  scratchpad: "",
  sandbox: false,
};

/// Full wire shape — matches the Rust `GitHubIssue` struct in
/// `src-tauri/src/commands/pr.rs` and the upstream `services::github::Issue`
/// deserialiser. Every field is now guaranteed present on the wire, so
/// the test uses the strict `GitHubIssue` type (no `Pick`/`?` elision).
const BACKEND_ISSUES: GitHubIssue[] = [
  {
    number: 101,
    title: "Add dark mode",
    body: "Please support dark themes.",
    url: "https://github.com/alondero/buildmesh/issues/101",
    state: "open",
    labels: ["enhancement", "good first issue"],
    blocked_by: [],
  },
  {
    number: 102,
    title: "Refactor auth",
    body: "",
    url: "https://github.com/alondero/buildmesh/issues/102",
    state: "open",
    labels: [],
    blocked_by: [],
  },
  {
    number: 103,
    title: "Crash on launch",
    body: "Stack trace attached.",
    url: "https://github.com/alondero/buildmesh/issues/103",
    state: "open",
    labels: ["bug"],
    blocked_by: [],
  },
];

/// Match only the list endpoint, not the per-issue spawn endpoint
/// `/api/meshes/{id}/issues/{n}/spawn` which would also match a naive
/// `url.includes("/api/meshes/1/issues")` — a future test that clicks the
/// Spawn button would then receive the issues list as a "successful
/// response" and navigate to a bogus empty node.
const LIST_ISSUES_PATH = /^\/api\/meshes\/1\/issues(\?|$)/;

function mockListIssues(issues: GitHubIssue[]) {
  const fn = vi.fn().mockImplementation(async (url: string) => {
    if (LIST_ISSUES_PATH.test(url)) {
      return { ok: true, status: 200, json: async () => issues };
    }
    return { ok: true, status: 200, json: async () => ({}) };
  });
  vi.stubGlobal("fetch", fn);
  return fn;
}

const noop = () => {};

describe("IssuesScreen", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("renders the full wire shape: list, label chips, and expanded body", async () => {
    mockListIssues(BACKEND_ISSUES);

    render(
      <IssuesScreen
        mesh={mesh}
        onBack={noop}
        onSpawned={noop}
      />,
    );

    // All three issues must render.
    await waitFor(() => {
      expect(screen.getByTestId("issue-101")).toBeTruthy();
    });
    expect(screen.getByTestId("issue-102")).toBeTruthy();
    expect(screen.getByTestId("issue-103")).toBeTruthy();

    // Filter input must still be there.
    expect(screen.getByTestId("issues-filter")).toBeTruthy();

    // Label chips render from the `labels[]` array. #101 has two,
    // #103 has one, #102 has none — exercising both the populated and
    // empty branches.
    expect(screen.getByText("enhancement")).toBeTruthy();
    expect(screen.getByText("good first issue")).toBeTruthy();
    expect(screen.getByText("bug")).toBeTruthy();
    // #102 has no labels — its card must not render any label chip.
    const card102 = screen.getByTestId("issue-102");
    expect(card102.textContent).not.toMatch(/enhancement|bug/);

    // Tapping an issue must expand it without crashing.
    await userEvent.click(screen.getByTestId("issue-101"));
    expect(screen.getByTestId("issue-body-101")).toBeTruthy();
  });

  it("renders the 'View ↗' link for every issue, pointing at issue.url", async () => {
    // Issue #358: the link used to be hidden because the Rust struct didn't
    // serialise `url`. Now that the backend ships it, the link must be
    // unconditionally visible and point at the right URL.
    mockListIssues(BACKEND_ISSUES);

    render(
      <IssuesScreen
        mesh={mesh}
        onBack={noop}
        onSpawned={noop}
      />,
    );

    // Wait for the list to load, then expand issue #101.
    await waitFor(() => {
      expect(screen.getByTestId("issue-101")).toBeTruthy();
    });
    await userEvent.click(screen.getByTestId("issue-101"));

    const link = screen.getByTestId("issue-view-101") as HTMLAnchorElement;
    expect(link).toBeTruthy();
    expect(link.tagName).toBe("A");
    expect(link.getAttribute("href")).toBe(
      "https://github.com/alondero/buildmesh/issues/101",
    );
    expect(link.getAttribute("target")).toBe("_blank");
    expect(link.getAttribute("rel")).toBe("noreferrer");
    expect(link.textContent).toMatch(/View/);
  });

  it("filters the list by title and shows the 'no matches' state", async () => {
    mockListIssues(BACKEND_ISSUES);

    render(
      <IssuesScreen
        mesh={mesh}
        onBack={noop}
        onSpawned={noop}
      />,
    );

    const filter = await screen.findByTestId("issues-filter");
    await userEvent.type(filter, "auth");

    await waitFor(() => {
      expect(screen.getByTestId("issue-102")).toBeTruthy();
    });
    expect(screen.queryByTestId("issue-101")).toBeNull();
    expect(screen.queryByTestId("issue-103")).toBeNull();

    // A filter that matches nothing should still render the empty note,
    // not crash.
    await userEvent.clear(filter);
    await userEvent.type(filter, "zzzz-no-match");
    expect(screen.getByTestId("issues-empty")).toBeTruthy();
  });

  it("renders an empty state when the backend returns no issues", async () => {
    mockListIssues([]);

    render(
      <IssuesScreen
        mesh={mesh}
        onBack={noop}
        onSpawned={noop}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("issues-empty")).toBeTruthy();
    });
    expect(
      screen.getByText(/No open issues/),
    ).toBeTruthy();
  });

  it("surfaces a fetch error in the screen rather than going blank", async () => {
    const fn = vi.fn().mockImplementation(async () => {
      return {
        ok: false,
        status: 500,
        json: async () => ({ error: "boom" }),
      };
    });
    vi.stubGlobal("fetch", fn);

    render(
      <IssuesScreen
        mesh={mesh}
        onBack={noop}
        onSpawned={noop}
      />,
    );

    // The filter input and AppBar still render, and the error text is
    // visible — proving the page is not blank.
    await waitFor(() => {
      expect(screen.getByText("boom")).toBeTruthy();
    });
    expect(screen.getByTestId("issues-filter")).toBeTruthy();
  });

  // Issue #1259 — the card announces role="button" but ignored Enter/Space,
  // violating WCAG 2.1.1 (Keyboard) and 4.1.2 (Name, Role, Value). These
  // tests pin the keyboard activation so a regression is caught at PR time
  // rather than by a screen-reader / switch-access user.
  describe("keyboard activation (issue #1259)", () => {
    /// Helper: render with `BACKEND_ISSUES` loaded and return the card for
    /// issue #101. Centralised so the four tests below only assert on the
    /// activation behaviour — not the render plumbing.
    async function loadAndGetCard() {
      mockListIssues(BACKEND_ISSUES);
      render(
        <IssuesScreen mesh={mesh} onBack={noop} onSpawned={noop} />,
      );
      return await screen.findByTestId("issue-101");
    }

    it("expands the card when Enter is pressed", async () => {
      const card = await loadAndGetCard();
      // The body pane should not be visible before activation.
      expect(screen.queryByTestId("issue-body-101")).toBeNull();

      fireEvent.keyDown(card, { key: "Enter" });

      expect(screen.getByTestId("issue-body-101")).toBeTruthy();
      expect(screen.getByTestId("issue-spawn-101")).toBeTruthy();
    });

    it("expands the card when Space is pressed", async () => {
      const card = await loadAndGetCard();
      expect(screen.queryByTestId("issue-body-101")).toBeNull();

      fireEvent.keyDown(card, { key: " " });

      expect(screen.getByTestId("issue-body-101")).toBeTruthy();
    });

    it("toggles the card closed when Enter is pressed a second time", async () => {
      const card = await loadAndGetCard();

      fireEvent.keyDown(card, { key: "Enter" });
      expect(screen.getByTestId("issue-body-101")).toBeTruthy();

      fireEvent.keyDown(card, { key: "Enter" });
      expect(screen.queryByTestId("issue-body-101")).toBeNull();
    });

    it("ignores keys other than Enter and Space", async () => {
      const card = await loadAndGetCard();

      // A random letter must NOT trigger expansion — only Enter / Space
      // are activation keys for a button role.
      fireEvent.keyDown(card, { key: "a" });
      expect(screen.queryByTestId("issue-body-101")).toBeNull();

      fireEvent.keyDown(card, { key: "ArrowDown" });
      expect(screen.queryByTestId("issue-body-101")).toBeNull();
    });

    it("does not collapse the card when Space bubbles up from the nested Spawn button", async () => {
      // Regression: a naive onKeyDown handler on the role="button" wrapper
      // collapses the card on Space bubbled up from the inner Spawn
      // button, doubling the activation (collapse + spawn). The fix
      // ignores keydowns whose target is not the card itself.
      const card = await loadAndGetCard();
      fireEvent.keyDown(card, { key: "Enter" });
      expect(screen.getByTestId("issue-spawn-101")).toBeTruthy();

      const spawnBtn = screen.getByTestId("issue-spawn-101");
      fireEvent.keyDown(spawnBtn, { key: " " });

      // Card must still be open — Space should NOT have collapsed it.
      expect(screen.getByTestId("issue-spawn-101")).toBeTruthy();
      expect(screen.getByTestId("issue-body-101")).toBeTruthy();
    });
  });
});
