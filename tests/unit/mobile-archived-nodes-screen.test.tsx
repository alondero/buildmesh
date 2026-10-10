/**
 * ArchivedNodesScreen (mobile "Archive" list of resumable sessions).
 *
 * History:
 *  - Originally written when discover_agent_nodes was the first mobile HTTP
 *    surface to ship; the screen focuses on the resume-button affordance
 *    (importing an archive spawns a real agent, so an accidental tap must
 *    not commit — first tap expands the card, the explicit Resume button
 *    triggers importAndResume).
 *  - Issue #1259 — the card announced role="button" but ignored Enter and
 *    Space, leaving switch-access / keyboard users unable to expand it.
 *    WCAG 2.1.1 (Keyboard) requires every action a mouse / touch user can
 *    do to be reachable from a keyboard activation; WCAG 4.1.2 (Name, Role,
 *    Value) requires that the role behave the way the role says it does.
 *    The keyboard-activation describe block at the bottom pins the fix.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import ArchivedNodesScreen, {
  sortAgentNodes,
} from "../../src/mobile/screens/ArchivedNodesScreen";
import type { Mesh, ArchivedAgentNode } from "../../src/mobile/api";

const mesh: Mesh = {
  id: 1,
  name: "buildmesh",
  path: "/tmp/repo",
  created_at: "2026-06-11T00:00:00Z",
  scratchpad: "",
  sandbox: false,
};

// Wire shape mirrors the Rust `ArchivedAgentNode` struct (see
// `src/types/generated/ArchivedAgentNode.ts`). All fields are present.
const ARCHIVE: ArchivedAgentNode[] = [
  {
    session_id: "abc-001",
    first_message: "Investigate flaky test in agent spawn path",
    branch: "main",
    cwd: "/tmp/repo",
    timestamp: "2026-08-25T12:00:00Z",
    worktree_name: "wt-flaky",
    resumable: true,
  },
  {
    session_id: "abc-002",
    first_message: "Refactor session_id capture",
    branch: "fix/session",
    cwd: "/tmp/repo",
    timestamp: "2026-08-20T12:00:00Z",
    worktree_name: null,
    resumable: true,
  },
];

// Issue #1065 — a durable archived row (DB row still present, transcript gone)
// arrives with `resumable: false`. It must still be listed, and its expanded
// card must explain that Resume is unavailable instead of offering a button
// that would spawn a harness against a missing conversation.
const DURABLE: ArchivedAgentNode = {
  session_id: "node-77",
  first_message: "brave-fox",
  branch: "feat/1065",
  cwd: "/tmp/repo/.claude/worktrees/brave-fox",
  timestamp: "2026-08-15T12:00:00Z",
  worktree_name: "brave-fox",
  resumable: false,
};

function mockDiscoverNodes(nodes: ArchivedAgentNode[]) {
  const fn = vi.fn().mockImplementation(async (url: string) => {
    if (url.includes("/api/meshes/1/agent-nodes/discover")) {
      return { ok: true, status: 200, json: async () => nodes };
    }
    return { ok: true, status: 200, json: async () => ({}) };
  });
  vi.stubGlobal("fetch", fn);
  return fn;
}

const noop = () => {};

describe("ArchivedNodesScreen", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("renders the archive list and sorts newest activity first", async () => {
    mockDiscoverNodes(ARCHIVE);

    render(
      <ArchivedNodesScreen
        mesh={mesh}
        onBack={noop}
        onResumed={noop}
      />,
    );

    // Both cards render with their session-id test id.
    await waitFor(() => {
      expect(screen.getByTestId("node-abc-001")).toBeTruthy();
    });
    expect(screen.getByTestId("node-abc-002")).toBeTruthy();

    // The newer session (#001, timestamp 2026-08-25) must come BEFORE
    // #002 (2026-08-20) in DOM order. Sort regression: the screen used
    // to render in backend order, which is arbitrary.
    const cards = screen.getAllByTestId(/^node-/);
    expect(cards.map((c) => c.dataset.testid)).toEqual([
      "node-abc-001",
      "node-abc-002",
    ]);

    // Clicking expands the card and reveals the Resume button.
    await userEvent.click(screen.getByTestId("node-abc-001"));
    expect(screen.getByTestId("node-resume-abc-001")).toBeTruthy();
  });

  it("sortAgentNodes pushes untimestamped entries to the bottom", () => {
    // Pure helper — exercises the sink-to-bottom fallback so a future
    // regression that lifts the timestamp to the top is caught here, not
    // only in the UI test above.
    const sorted = sortAgentNodes([
      { ...ARCHIVE[0], timestamp: null },
      ARCHIVE[1],
    ]);
    expect(sorted[0].session_id).toBe("abc-002");
    expect(sorted[1].session_id).toBe("abc-001");
  });

  it("surfaces a fetch error in the screen rather than going blank", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: false,
        status: 500,
        json: async () => ({ error: "boom" }),
      }),
    );

    render(
      <ArchivedNodesScreen
        mesh={mesh}
        onBack={noop}
        onResumed={noop}
      />,
    );

    await waitFor(() => {
      expect(screen.getByText("boom")).toBeTruthy();
    });
  });

  // Issue #1065 — the durable half of the archive.
  it("lists a durable archived row and explains the unavailable Resume", async () => {
    mockDiscoverNodes([...ARCHIVE, DURABLE]);

    render(
      <ArchivedNodesScreen
        mesh={mesh}
        onBack={noop}
        onResumed={noop}
      />,
    );

    // The row itself is listed — its identity outlives its transcript.
    const card = await screen.findByTestId("node-node-77");
    expect(card).toBeTruthy();
    expect(screen.getByText("brave-fox")).toBeTruthy();

    // Expanding shows the explanation, not a Resume button.
    await userEvent.click(card);
    expect(screen.queryByTestId("node-resume-node-77")).toBeNull();
    expect(screen.getByTestId("node-unresumable-node-77").textContent).toContain(
      "Resume unavailable",
    );
  });

  it("still resumes a discovered session whose transcript is readable", async () => {
    // The pre-#1065 behaviour must be untouched: a discovered session keeps
    // its Resume button and imports on tap.
    const fetchMock = mockDiscoverNodes([DURABLE, ARCHIVE[0]]);

    render(
      <ArchivedNodesScreen
        mesh={mesh}
        onBack={noop}
        onResumed={noop}
      />,
    );

    await userEvent.click(await screen.findByTestId("node-abc-001"));
    await userEvent.click(screen.getByTestId("node-resume-abc-001"));

    await waitFor(() => {
      const calls = fetchMock.mock.calls.map(([url]) => String(url));
      expect(
        calls.some((url) =>
          url.includes("/api/meshes/1/agent-nodes/import-and-resume"),
        ),
      ).toBe(true);
    });
  });

  // Issue #1259 — mirrors the IssuesScreen tests above.
  describe("keyboard activation (issue #1259)", () => {
    async function loadAndGetCard() {
      mockDiscoverNodes(ARCHIVE);
      render(
        <ArchivedNodesScreen
          mesh={mesh}
          onBack={noop}
          onResumed={noop}
        />,
      );
      return await screen.findByTestId("node-abc-001");
    }

    it("expands the card when Enter is pressed", async () => {
      const card = await loadAndGetCard();
      expect(screen.queryByTestId("node-resume-abc-001")).toBeNull();

      fireEvent.keyDown(card, { key: "Enter" });

      expect(screen.getByTestId("node-resume-abc-001")).toBeTruthy();
    });

    it("expands the card when Space is pressed", async () => {
      const card = await loadAndGetCard();
      expect(screen.queryByTestId("node-resume-abc-001")).toBeNull();

      fireEvent.keyDown(card, { key: " " });

      expect(screen.getByTestId("node-resume-abc-001")).toBeTruthy();
    });

    it("toggles the card closed when Enter is pressed a second time", async () => {
      const card = await loadAndGetCard();

      fireEvent.keyDown(card, { key: "Enter" });
      expect(screen.getByTestId("node-resume-abc-001")).toBeTruthy();

      fireEvent.keyDown(card, { key: "Enter" });
      expect(screen.queryByTestId("node-resume-abc-001")).toBeNull();
    });

    it("ignores keys other than Enter and Space", async () => {
      const card = await loadAndGetCard();

      fireEvent.keyDown(card, { key: "a" });
      expect(screen.queryByTestId("node-resume-abc-001")).toBeNull();

      fireEvent.keyDown(card, { key: "ArrowDown" });
      expect(screen.queryByTestId("node-resume-abc-001")).toBeNull();
    });

    it("does not collapse the card when Space bubbles up from the nested Resume button", async () => {
      // Regression: a naive onKeyDown on the role="button" wrapper would
      // collapse the card on a keydown bubbled from the inner Resume
      // button (collapsing the card on top of an import-and-resume
      // activation). The fix ignores keydowns whose target is not the
      // card itself.
      const card = await loadAndGetCard();
      fireEvent.keyDown(card, { key: "Enter" });
      expect(screen.getByTestId("node-resume-abc-001")).toBeTruthy();

      const resumeBtn = screen.getByTestId("node-resume-abc-001");
      fireEvent.keyDown(resumeBtn, { key: " " });

      // Card must still be open — Space should NOT have collapsed it.
      expect(screen.getByTestId("node-resume-abc-001")).toBeTruthy();
    });
  });
});