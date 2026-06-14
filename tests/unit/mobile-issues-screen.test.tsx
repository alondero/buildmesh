/**
 * IssuesScreen: must render without crashing when the backend returns the
 * real Rust `GitHubIssue` shape (number, title, body) — which is missing
 * the `labels`, `url`, and `state` fields the mobile TS interface declares.
 *
 * Regression: the mobile screen used to do `issue.labels.length` and
 * `issue.labels.map(...)` directly; with the server response both threw
 * `TypeError: Cannot read properties of undefined`, the React render
 * crashed, and the user was left looking at a blank page below the
 * filter input.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import IssuesScreen from "../../src/mobile/screens/IssuesScreen";
import type { Mesh, GitHubIssue } from "../../src/mobile/api";

const mesh: Mesh = {
  id: 1,
  name: "buildmesh",
  path: "/tmp/repo",
  created_at: "2026-06-11T00:00:00Z",
};

/// Realistic backend payload: matches `src-tauri/src/commands/pr.rs`
/// `pub struct GitHubIssue { number, title, body }`. Note: no `labels`,
/// no `url`, no `state` — those are declared in the TS interface but
/// the Rust side does not serialise them.
const BACKEND_ISSUES: Pick<GitHubIssue, "number" | "title" | "body">[] = [
  { number: 101, title: "Add dark mode", body: "Please support dark themes." },
  { number: 102, title: "Refactor auth", body: "" },
  { number: 103, title: "Crash on launch", body: "Stack trace attached." },
];

/// Match only the list endpoint, not the per-issue spawn endpoint
/// `/api/meshes/{id}/issues/{n}/spawn` which would also match a naive
/// `url.includes("/api/meshes/1/issues")` — a future test that clicks the
/// Spawn button would then receive the issues list as a "successful
/// response" and navigate to a bogus empty node.
const LIST_ISSUES_PATH = /^\/api\/meshes\/1\/issues(\?|$)/;

function mockListIssues(issues: typeof BACKEND_ISSUES) {
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

  it("renders the issue list when the backend returns the 3-field Rust shape (no labels/url/state)", async () => {
    mockListIssues(BACKEND_ISSUES);

    render(
      <IssuesScreen
        mesh={mesh}
        onBack={noop}
        onSpawned={noop}
      />,
    );

    // Regression: the page must NOT go blank. All three issues must render.
    await waitFor(() => {
      expect(screen.getByTestId("issue-101")).toBeTruthy();
    });
    expect(screen.getByTestId("issue-102")).toBeTruthy();
    expect(screen.getByTestId("issue-103")).toBeTruthy();

    // Filter input must still be there (the user-visible part of the page
    // that survived before the bug was fixed).
    expect(screen.getByTestId("issues-filter")).toBeTruthy();

    // Pin the no-labels render path: the Rust payload omits `labels`, so
    // no label chips should render anywhere on the page. If a future
    // regression drops the `(issue.labels ?? [])` guard, this assertion
    // still passes (the page won't crash because the issue list itself
    // is the trigger), but the body assertion below catches the next
    // place `issue.body` is touched. Together they cover the two
    // crash sites.
    expect(screen.queryByText(/^bug$/)).toBeNull();
    expect(screen.queryByText(/^enhancement$/)).toBeNull();

    // Tapping an issue must expand it without crashing — the expanded view
    // is the second place `issue.body` and `issue.url` are touched, so this
    // catches a partial fix that handles the list view but not the
    // expanded card.
    await userEvent.click(screen.getByTestId("issue-101"));
    expect(screen.getByTestId("issue-body-101")).toBeTruthy();

    // The "View ↗" link must NOT render when the backend omits `url` —
    // it was previously rendered as a dead button. With url missing, the
    // link is hidden, so the only way the user can act on the expanded
    // issue is the Spawn button.
    expect(screen.queryByTestId("issue-view-101")).toBeNull();
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
});
