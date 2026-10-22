/**
 * Launch/resume navigation authority (issue #2327).
 *
 * A launch or resume spawns a real agent, so its completion arrives long after
 * the tap. The question these tests answer is the contract: *may* this
 * completion still take the user to its result? Only the visit that is still on
 * display may — a retired visit's work still happened, but it must not move
 * someone who has already navigated somewhere else.
 *
 * The first half runs the real `IssuesScreen` / `ArchivedNodesScreen` against a
 * real `App`; only the shell around them is stubbed, so the test can choose a
 * destination deterministically. Each request's response is settled by the test
 * through a deferred of its own, so "still pending" is a fact rather than a race
 * and two consecutive launches are two genuinely distinct requests.
 *
 * Before the fix `ArchivedNodesScreen.resume` held no lifetime or visit guard at
 * all: leaving Archive with a resume in flight unmounted the screen, and
 * `onResumed(node)` still fired when the import finished, replacing whatever the
 * user had navigated to in the meantime.
 */
import { act, createElement, useEffect, useState } from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "../../src/mobile/App";
import IssuesScreen from "../../src/mobile/screens/IssuesScreen";
import ArchivedNodesScreen from "../../src/mobile/screens/ArchivedNodesScreen";
import type { AgentNode, ArchivedAgentNode, Mesh } from "../../src/mobile/api";

const mesh: Mesh = {
  id: 1,
  name: "buildmesh",
  path: "/tmp/repo",
  created_at: "2026-06-11T00:00:00Z",
  scratchpad: "",
  sandbox: false,
};

/// The Agent Node the user deliberately opens after walking away from the launch
/// screen. Its completion has to survive the late one.
const chosenNode: AgentNode = {
  id: 42,
  mesh_id: 1,
  name: "mesh-b-node",
  path: "/tmp/wt/mesh-b",
  branch: "main",
  provider: "anthropic",
  status: "running",
  cli_session_id: null,
  created_at: "2026-06-11T00:00:00Z",
};

/// The node a background resume produces.
const resumedNode: AgentNode = {
  id: 99,
  mesh_id: 1,
  name: "resumed-archive",
  path: "/tmp/wt/archived",
  branch: "main",
  provider: "anthropic",
  status: "running",
  cli_session_id: "abc-001",
  created_at: "2026-06-11T00:00:00Z",
};

/// The node a background issue spawn produces.
const spawnedNode: AgentNode = {
  id: 77,
  mesh_id: 1,
  name: "issue-101-agent",
  path: "/tmp/wt/issue-101",
  branch: "feat/101",
  provider: "anthropic",
  status: "running",
  cli_session_id: null,
  created_at: "2026-06-11T00:00:00Z",
};

const archivedSession: ArchivedAgentNode = {
  session_id: "abc-001",
  first_message: "Investigate the flaky spawn",
  branch: "main",
  cwd: "/tmp/repo",
  timestamp: "2026-08-25T12:00:00Z",
  worktree_name: "wt-flaky",
  resumable: true,
  provider: "anthropic",
  node_id: null,
};

const issue = {
  number: 101,
  title: "Add dark mode",
  body: "Please support dark themes.",
  url: "https://github.com/alondero/buildmesh/issues/101",
  state: "open",
  labels: [],
  blocked_by: [],
  author: "octo",
};

type Deferred = {
  promise: Promise<Response>;
  resolve: (response: Response) => void;
  reject: (reason: unknown) => void;
  /// How many times `apiFetch` may still replay this request. It retries
  /// exactly once, and only after recovering the session cookie on a 401, so a
  /// replay must observe the same response instead of opening a second pending
  /// promise. Every other outcome ends the logical request.
  replaysLeft: number;
};

function deferred(): Deferred {
  let resolve!: (response: Response) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<Response>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject, replaysLeft: 0 };
}

function takeRequest(queue: Deferred[]): Promise<Response> {
  const last = queue[queue.length - 1];
  if (last && last.replaysLeft > 0) {
    last.replaysLeft -= 1;
    return last.promise;
  }
  const next = deferred();
  queue.push(next);
  return next.promise;
}

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

/// A refused device. `apiFetch` refreshes the cookie once, replays the request,
/// still sees the 401 and re-raises it — so this reaches the screen as the auth
/// error it is, without needing a live server.
function revokedDevice(): Response {
  return json({ error: "Device revoked" }, 401);
}

const counters = vi.hoisted(() => ({ overviewRenders: 0 }));

vi.mock("../../src/mobile/screens/Connect", () => ({
  default: () => createElement("main", { "data-testid": "connect-screen" }),
}));

/// Stands in for the home screen. It reads `/api/nodes` so a test can show that
/// acknowledged background work appears in refreshed data, and it exposes the
/// ways into a launch screen plus a way to open an Agent Node.
vi.mock("../../src/mobile/screens/NodeList", () => ({
  default: function NodeListMock(props: {
    onOpenNode: (node: unknown) => void;
    onOpenIssues: (mesh: unknown) => void;
    onOpenAgentNodes: (mesh: unknown) => void;
  }) {
    const [ids, setIds] = useState<number[]>([]);
    const [reads, setReads] = useState(0);
    useEffect(() => {
      void fetch("/api/nodes")
        .then((response) => response.json())
        .then((rows: { id: number }[]) => setIds(rows.map((row) => row.id)));
    }, [reads]);
    return createElement(
      "div",
      { "data-testid": "mock-node-list" },
      createElement(
        "p",
        { "data-testid": "listed-nodes" },
        ids.join(",") || "none",
      ),
      createElement(
        "button",
        {
          "data-testid": "list-refresh",
          onClick: () => setReads((n) => n + 1),
        },
        "refresh",
      ),
      createElement(
        "button",
        {
          "data-testid": "open-issues",
          onClick: () => props.onOpenIssues(mesh),
        },
        "open issues",
      ),
      createElement(
        "button",
        {
          "data-testid": "open-archive",
          onClick: () => props.onOpenAgentNodes(mesh),
        },
        "open archive",
      ),
      createElement(
        "button",
        {
          "data-testid": "open-chosen-node",
          onClick: () => props.onOpenNode(chosenNode),
        },
        "open a node",
      ),
    );
  },
}));

vi.mock("../../src/mobile/screens/NodeOverview", () => ({
  default: (props: { node: AgentNode }) => {
    counters.overviewRenders += 1;
    return createElement(
      "div",
      null,
      createElement(
        "p",
        { "data-testid": "mock-overview" },
        `node ${props.node.id}`,
      ),
    );
  },
}));

const RESUME_PATH = "/api/meshes/1/agent-nodes/import-and-resume";
const SPAWN_PATH = "/api/meshes/1/issues/101/spawn";

describe("launch and resume visit authority (issue #2327)", () => {
  /// Nodes `/api/nodes` reports. The background-completion tests append the
  /// node the backend really created, so the list can show that the work
  /// survived even though its navigation was retired.
  let visibleNodes: AgentNode[];
  /// One deferred per request, so two launches are two pending promises rather
  /// than one promise resolved twice.
  let resumeRequests: Deferred[];
  let spawnRequests: Deferred[];
  /// Request inits seen per path, so a test can prove the backend call really
  /// ran and was never cancelled.
  let requested: Record<string, RequestInit | undefined>;

  beforeEach(() => {
    localStorage.clear();
    localStorage.setItem("buildmesh_token", "device-token");
    window.history.replaceState(null, "", "/");
    // The App's Back button calls `history.back()`; jsdom's implementation is
    // asynchronous, so the popstate that gesture would raise is delivered
    // explicitly below. Stubbing it keeps a second, out-of-order popstate from
    // walking the screen hierarchy behind the test's back.
    vi.spyOn(window.history, "back").mockImplementation(() => undefined);
    counters.overviewRenders = 0;
    visibleNodes = [];
    resumeRequests = [];
    spawnRequests = [];
    requested = {};
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const path = new URL(String(input), "http://bm.test").pathname;
        requested[path] = init;
        if (path === "/api/session") return new Response(null, { status: 204 });
        if (path === "/api/nodes") return json(visibleNodes);
        if (path === "/api/meshes") return json([mesh]);
        if (path === "/api/meshes/1/agent-nodes/discover")
          return json([archivedSession]);
        if (path === RESUME_PATH) return takeRequest(resumeRequests);
        if (path === "/api/meshes/1/issues")
          return json({
            items: [issue],
            completeness: {
              returned: 1,
              pages_fetched: 1,
              complete: true,
              incomplete_reason: null,
              reported_total: null,
            },
          });
        if (path === "/api/meshes/1/issue-labels")
          return json({ labels: [], autopilot_triggers: {} });
        if (path === SPAWN_PATH) return takeRequest(spawnRequests);
        if (path === "/api/providers") return json([]);
        return json({ error: `no fixture for ${path}` }, 404);
      }),
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
    localStorage.clear();
  });

  async function settleResume(response: Response) {
    const last = resumeRequests[resumeRequests.length - 1];
    // A 401 comes back through the transport's single authenticated replay.
    if (response.status === 401) last.replaysLeft = 1;
    await act(async () => last.resolve(response));
  }

  async function settleSpawn(response: Response) {
    const last = spawnRequests[spawnRequests.length - 1];
    if (response.status === 401) last.replaysLeft = 1;
    await act(async () => last.resolve(response));
  }

  /// Presses the Archive AppBar's Back and delivers the popstate that gesture
  /// produces. Archive has no in-flight dismissal gate, so this really does
  /// leave the screen while the resume is still running.
  async function leaveArchiveWithBack() {
    await act(async () => screen.getByRole("button", { name: "Back" }).click());
    await act(async () =>
      window.dispatchEvent(new PopStateEvent("popstate")),
    );
  }

  describe("through the App's navigation transition", () => {
    async function boot() {
      render(<App />);
      await screen.findByTestId("mock-node-list");
    }

    async function startArchiveResume() {
      await act(async () => screen.getByTestId("open-archive").click());
      await userEvent.click(await screen.findByTestId("node-abc-001"));
      await act(async () => {
        screen.getByTestId("node-resume-abc-001").click();
      });
      await waitFor(() => expect(resumeRequests).toHaveLength(1));
    }

    async function startIssueSpawn() {
      await act(async () => screen.getByTestId("open-issues").click());
      await userEvent.click(await screen.findByTestId("issue-101"));
      // The split button's main half starts the launch straight away — the
      // phone confirms nothing (issue #2385).
      await act(async () => screen.getByTestId("issue-spawn-101").click());
      await waitFor(() => expect(spawnRequests).toHaveLength(1));
    }

    it("keeps the screen chosen after Back when a background resume finishes", async () => {
      await boot();
      await startArchiveResume();

      // Back out of Archive while the import is still running, then open an
      // Agent Node — the deterministic sequence from the issue.
      await leaveArchiveWithBack();
      expect(screen.queryByTestId("discovered-nodes-screen")).toBeNull();
      await act(async () => screen.getByTestId("open-chosen-node").click());
      expect(screen.getByTestId("mock-overview").textContent).toBe("node 42");
      // The stack the user is actually holding. A retired completion that
      // navigated — or pushed an entry of its own — would move this.
      const depthBeforeCompletion = window.history.length;

      visibleNodes = [chosenNode, resumedNode];
      await settleResume(json(resumedNode));

      // The late completion must not steal the user's place...
      expect(screen.getByTestId("mock-overview").textContent).toBe("node 42");
      expect(window.history.length).toBe(depthBeforeCompletion);
      // ...and history still agrees with the rendered screen: one Back from
      // here reaches the node list, not the dead Archive entry.
      await act(async () =>
        window.dispatchEvent(new PopStateEvent("popstate")),
      );
      expect(screen.getByTestId("mock-node-list")).toBeTruthy();
    });

    it("surfaces completed work in refreshed data rather than navigating to it", async () => {
      await boot();
      await startArchiveResume();
      await leaveArchiveWithBack();

      // The backend really did the work, and nothing cancelled it.
      expect(requested[RESUME_PATH]).toEqual(
        expect.objectContaining({ method: "POST" }),
      );
      await settleResume(json(resumedNode));
      expect((requested[RESUME_PATH] as RequestInit).signal).toBeUndefined();

      expect(screen.getByTestId("listed-nodes").textContent).toBe("none");
      visibleNodes = [chosenNode, resumedNode];
      await act(async () => screen.getByTestId("list-refresh").click());
      await waitFor(() =>
        expect(screen.getByTestId("listed-nodes").textContent).toBe("42,99"),
      );
      // Still on the list the user chose: the completion surfaced the node
      // rather than moving them.
      expect(screen.queryByTestId("mock-overview")).toBeNull();
    });

    it("does not let an old resume claim a second visit to the same Archive screen", async () => {
      await boot();
      await startArchiveResume();

      await leaveArchiveWithBack();
      await act(async () => screen.getByTestId("open-archive").click());
      expect(await screen.findByTestId("discovered-nodes-screen")).toBeTruthy();

      await settleResume(json(resumedNode));

      // Returning to Archive mints a new visit, so the request the user walked
      // away from has nowhere to navigate.
      expect(screen.getByTestId("discovered-nodes-screen")).toBeTruthy();
      expect(screen.queryByTestId("mock-overview")).toBeNull();
    });

    it("navigates exactly once when the initiating visit is still on screen", async () => {
      await boot();
      await startArchiveResume();

      await settleResume(json(resumedNode));

      expect(screen.getByTestId("mock-overview").textContent).toBe("node 99");
      expect(screen.queryByTestId("discovered-nodes-screen")).toBeNull();
      expect(counters.overviewRenders).toBe(1);
    });

    it("drops a rejection from a retired visit instead of writing it into the new screen", async () => {
      await boot();
      await startArchiveResume();

      await leaveArchiveWithBack();
      await act(async () => screen.getByTestId("open-archive").click());
      expect(await screen.findByTestId("discovered-nodes-screen")).toBeTruthy();

      await act(async () =>
        resumeRequests[resumeRequests.length - 1].reject(
          new Error("import failed"),
        ),
      );

      expect(screen.queryByText("import failed")).toBeNull();
      expect(screen.queryByTestId("mock-overview")).toBeNull();
    });

    it("navigates to the created node when the issue launch stays on screen", async () => {
      await boot();
      await startIssueSpawn();

      await settleSpawn(json(spawnedNode));

      expect(screen.getByTestId("mock-overview").textContent).toBe("node 77");
      expect(screen.queryByTestId("issues-screen")).toBeNull();
      expect(counters.overviewRenders).toBe(1);
    });

    it("keeps a retired visit's 401 from tearing down the live session", async () => {
      await boot();
      await startArchiveResume();
      await leaveArchiveWithBack();
      await act(async () => screen.getByTestId("open-chosen-node").click());
      expect(screen.getByTestId("mock-overview").textContent).toBe("node 42");

      await settleResume(revokedDevice());

      // The refusal belongs to a screen the user left, so it must not clear the
      // credential or drop them on Connect. Centralized recovery still fires
      // for every request belonging to the session they are in.
      expect(screen.getByTestId("mock-overview").textContent).toBe("node 42");
      expect(screen.queryByTestId("connect-screen")).toBeNull();
    });

    it("still follows centralized recovery for the current visit's 401", async () => {
      await boot();
      await startArchiveResume();

      await settleResume(revokedDevice());

      await waitFor(() =>
        expect(screen.getByTestId("connect-screen")).toBeTruthy(),
      );
    });
  });

  describe("a live screen instance moved onto a new visit", () => {
    // The read effect's cleanup aborts, and `visitId` is one of its deps, so a
    // new visit retires the previous one exactly as an unmount does. These
    // tests hold the component mounted and move the visit out from under the
    // pending request, which is the case the App's transition has to arbitrate
    // and which a sync reset flag alone would miss.

    let onSpawned: ReturnType<typeof vi.fn>;
    let onResumed: ReturnType<typeof vi.fn>;
    let onAuthFailed: ReturnType<typeof vi.fn>;

    beforeEach(() => {
      onSpawned = vi.fn();
      onResumed = vi.fn();
      onAuthFailed = vi.fn();
    });

    function issues(visitId: number) {
      return (
        <IssuesScreen
          mesh={mesh}
          visitId={visitId}
          onBack={() => {}}
          onSpawned={onSpawned}
          onAuthFailed={onAuthFailed}
        />
      );
    }

    function archive(visitId: number) {
      return (
        <ArchivedNodesScreen
          mesh={mesh}
          visitId={visitId}
          onBack={() => {}}
          onResumed={onResumed}
        />
      );
    }

    /// Expands the issue card and starts a launch, expanding first only when
    /// the current visit has the card collapsed. The split button's main half
    /// starts the launch with the mesh default — there is no confirmation
    /// step on the phone (issue #2385).
    async function confirmLaunch() {
      await screen.findByTestId("issue-101");
      if (screen.queryByTestId("issue-spawn-101") === null) {
        await userEvent.click(screen.getByTestId("issue-101"));
      }
      const before = spawnRequests.length;
      await act(async () => screen.getByTestId("issue-spawn-101").click());
      await waitFor(() => expect(spawnRequests).toHaveLength(before + 1));
    }

    /// The Archive equivalent: expands the card only if it is not already open.
    async function clickResume() {
      await screen.findByTestId("node-abc-001");
      if (screen.queryByTestId("node-resume-abc-001") === null) {
        await userEvent.click(screen.getByTestId("node-abc-001"));
      }
      const button = await screen.findByTestId("node-resume-abc-001");
      await waitFor(() =>
        expect((button as HTMLButtonElement).disabled).toBe(false),
      );
      const before = resumeRequests.length;
      await act(async () => button.click());
      await waitFor(() => expect(resumeRequests).toHaveLength(before + 1));
    }

    it("drops an issue launch whose visit has been retired", async () => {
      const view = render(issues(1));
      await confirmLaunch();

      view.rerender(issues(2));
      await settleSpawn(json(spawnedNode));

      expect(onSpawned).not.toHaveBeenCalled();
    });

    it("does not write a retired visit's rejection into the new visit", async () => {
      const view = render(issues(1));
      await confirmLaunch();

      view.rerender(issues(2));
      await act(async () =>
        spawnRequests[spawnRequests.length - 1].reject(
          new Error("spawn failed"),
        ),
      );

      expect(screen.queryByText("spawn failed")).toBeNull();
      expect(onAuthFailed).not.toHaveBeenCalled();
    });

    it("still launches and reports from the current visit", async () => {
      const view = render(issues(1));
      view.rerender(issues(2));

      await confirmLaunch();
      await settleSpawn(json(spawnedNode));

      expect(onSpawned).toHaveBeenCalledExactlyOnceWith(spawnedNode, {
        kind: "issues",
        meshId: 1,
        visitId: 2,
      });
    });

    it("routes the current visit's 401 to pairing but not a retired visit's", async () => {
      const view = render(issues(1));
      await confirmLaunch();

      view.rerender(issues(2));
      await settleSpawn(revokedDevice());
      // Retired: the user has moved on, so a background failure must not tear
      // down the session they are now in.
      expect(onAuthFailed).not.toHaveBeenCalled();

      await confirmLaunch();
      await settleSpawn(revokedDevice());
      // Current visit: genuine current-session loss still follows the
      // centralized recovery policy.
      expect(onAuthFailed).toHaveBeenCalledTimes(1);
    });

    it("drops a resume whose visit has been retired, and reports from a current one", async () => {
      const view = render(archive(1));
      await clickResume();

      view.rerender(archive(2));
      await settleResume(json(resumedNode));
      expect(onResumed).not.toHaveBeenCalled();

      view.rerender(archive(3));
      await clickResume();
      await settleResume(json(resumedNode));

      expect(onResumed).toHaveBeenCalledExactlyOnceWith(resumedNode, {
        kind: "sessions",
        meshId: 1,
        visitId: 3,
      });
    });

    it("stops reporting a resume once the screen is gone", async () => {
      // The original defect in its simplest form: Archive held no lifetime
      // guard, so an unmounted screen still navigated on completion.
      const view = render(archive(1));
      await clickResume();
      view.unmount();

      await settleResume(json(resumedNode));
      expect(onResumed).not.toHaveBeenCalled();
    });
  });
});