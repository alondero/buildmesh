/**
 * The App-owned launch transition (issue #2327).
 *
 * `completeLaunch` is the single place the mobile shell decides whether a
 * launch or resume completion may take the user to its result. The integration
 * suite in `mobile-launch-authority.test.tsx` proves that the real screens keep
 * that right; this file pins the decision itself, with stubbed screens that
 * report a completion for whatever visit they were handed. A screen is not
 * trusted to have retired its own work correctly — the shell that owns
 * navigation says no.
 */
import { act, createElement } from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen } from "@testing-library/react";
import App from "../../src/mobile/App";
import type { AgentNode, Mesh } from "../../src/mobile/api";
import type { LaunchVisit } from "../../src/mobile/launchVisit";

const mesh: Mesh = {
  id: 1,
  name: "buildmesh",
  path: "/tmp/repo",
  created_at: "2026-06-11T00:00:00Z",
  scratchpad: "",
  sandbox: false,
};

const otherMesh: Mesh = { ...mesh, id: 2, name: "mesh-b" };

const node: AgentNode = {
  id: 99,
  mesh_id: 1,
  name: "created-node",
  path: "/tmp/wt/created",
  branch: "main",
  provider: "anthropic",
  status: "running",
  cli_session_id: null,
  created_at: "2026-06-11T00:00:00Z",
};

const appState = vi.hoisted(() => ({
  openIssues: null as ((mesh: Mesh) => void) | null,
  openArchive: null as ((mesh: Mesh) => void) | null,
  openNode: null as ((node: AgentNode) => void) | null,
  authFailed: null as (() => void) | null,
  /// Retained so a test can replay a completion long after the screen that
  /// reported it is gone.
  issuesCompletion: null as ((node: AgentNode, visit: LaunchVisit) => void) | null,
  archiveCompletion: null as ((node: AgentNode, visit: LaunchVisit) => void) | null,
}));

vi.mock("../../src/mobile/screens/Connect", () => ({
  default: () => createElement("main", { "data-testid": "connect-screen" }),
}));

vi.mock("../../src/mobile/screens/NodeList", () => ({
  default: (props: {
    onOpenNode: (node: AgentNode) => void;
    onOpenIssues: (mesh: Mesh) => void;
    onOpenAgentNodes: (mesh: Mesh) => void;
    onAuthFailed: () => void;
  }) => {
    appState.openIssues = (target) => props.onOpenIssues(target);
    appState.openArchive = (target) => props.onOpenAgentNodes(target);
    appState.openNode = (target) => props.onOpenNode(target);
    appState.authFailed = props.onAuthFailed;
    return createElement("main", { "data-testid": "mock-node-list" });
  },
}));

/// Reports whatever visit it was handed, including one the App has already
/// moved past — the shape a screen with no retirement logic produces.
vi.mock("../../src/mobile/screens/IssuesScreen", () => ({
  default: (props: {
    onSpawned: (node: AgentNode, visit: LaunchVisit) => void;
    visitId: number;
    mesh: Mesh;
  }) => {
    appState.issuesCompletion = (created, visit) => props.onSpawned(created, visit);
    return createElement("div", { "data-testid": "mock-issues" }, "issues");
  },
}));

vi.mock("../../src/mobile/screens/ArchivedNodesScreen", () => ({
  default: (props: {
    onResumed: (node: AgentNode, visit: LaunchVisit) => void;
    visitId: number;
    mesh: Mesh;
  }) => {
    appState.archiveCompletion = (created, visit) => props.onResumed(created, visit);
    return createElement("div", { "data-testid": "mock-archive" }, "archive");
  },
}));

vi.mock("../../src/mobile/screens/NodeOverview", () => ({
  default: (props: { node: AgentNode }) =>
    createElement("p", { "data-testid": "mock-overview" }, `node ${props.node.id}`),
}));

describe("App.completeLaunch", () => {
  beforeEach(() => {
    localStorage.clear();
    localStorage.setItem("buildmesh_token", "device-token");
    window.history.replaceState(null, "", "/");
    vi.spyOn(window.history, "back").mockImplementation(() => undefined);
    appState.openIssues = null;
    appState.openArchive = null;
    appState.openNode = null;
    appState.authFailed = null;
    appState.issuesCompletion = null;
    appState.archiveCompletion = null;
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response(null, { status: 204 })),
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
    localStorage.clear();
  });

  async function boot() {
    render(<App />);
    await screen.findByTestId("mock-node-list");
  }

  async function pop() {
    await act(async () =>
      window.dispatchEvent(new PopStateEvent("popstate")),
    );
  }

  it("navigates when the reporting visit is still on screen", async () => {
    await boot();
    act(() => appState.openIssues!(mesh));
    await screen.findByTestId("mock-issues");

    act(() =>
      appState.issuesCompletion!(node, {
        kind: "issues",
        meshId: mesh.id,
        visitId: 1,
      }),
    );

    expect(screen.getByTestId("mock-overview").textContent).toBe("node 99");
  });

  it("drops a completion whose screen the user has left", async () => {
    await boot();
    act(() => appState.openArchive!(mesh));
    await screen.findByTestId("mock-archive");
    const stale = appState.archiveCompletion!;

    await pop();
    expect(screen.getByTestId("mock-node-list")).toBeTruthy();
    act(() => appState.openNode!(node));
    expect(screen.getByTestId("mock-overview").textContent).toBe("node 99");

    act(() =>
      stale(node, { kind: "sessions", meshId: mesh.id, visitId: 1 }),
    );

    expect(screen.getByTestId("mock-overview").textContent).toBe("node 99");
  });

  it("drops a completion from a visit the App has replaced", async () => {
    await boot();
    act(() => appState.openIssues!(mesh));
    await screen.findByTestId("mock-issues");
    const stale = appState.issuesCompletion!;

    // A second visit to the same screen: the App mints a new visit id, so the
    // request the user walked away from must not claim this one.
    await pop();
    act(() => appState.openIssues!(mesh));
    await screen.findByTestId("mock-issues");

    act(() =>
      stale(node, { kind: "issues", meshId: mesh.id, visitId: 1 }),
    );

    expect(screen.queryByTestId("mock-overview")).toBeNull();
    expect(screen.getByTestId("mock-issues")).toBeTruthy();
  });

  it("drops a completion aimed at a different Mesh", async () => {
    await boot();
    act(() => appState.openArchive!(mesh));
    await screen.findByTestId("mock-archive");
    const stale = appState.archiveCompletion!;

    await pop();
    act(() => appState.openArchive!(otherMesh));
    await screen.findByTestId("mock-archive");

    act(() =>
      stale(node, { kind: "sessions", meshId: mesh.id, visitId: 1 }),
    );

    expect(screen.queryByTestId("mock-overview")).toBeNull();
  });

  it("does not navigate across an authentication recovery", async () => {
    await boot();
    act(() => appState.openIssues!(mesh));
    await screen.findByTestId("mock-issues");
    const stale = appState.issuesCompletion!;

    act(() => appState.authFailed!());
    await screen.findByTestId("connect-screen");
    act(() =>
      stale(node, { kind: "issues", meshId: mesh.id, visitId: 1 }),
    );

    expect(screen.getByTestId("connect-screen")).toBeTruthy();
    expect(screen.queryByTestId("mock-overview")).toBeNull();
  });

  it("drops a completion that lands in the same tick as the auth failure", async () => {
    // The race the recovery-version guard exists for. `handleAuthFailed` bumps
    // its version synchronously and clears the credential, but React has not
    // re-rendered yet — so the ref mirroring the *displayed* screen still says
    // "Issues, visit 1" and the visit check on its own would let this through,
    // navigating straight over the Connect screen the user has to re-pair on.
    await boot();
    act(() => appState.openIssues!(mesh));
    await screen.findByTestId("mock-issues");

    act(() => {
      appState.authFailed!();
      appState.issuesCompletion!(node, {
        kind: "issues",
        meshId: mesh.id,
        visitId: 1,
      });
    });

    await screen.findByTestId("connect-screen");
    expect(screen.queryByTestId("mock-overview")).toBeNull();
    expect(localStorage.getItem("buildmesh_token")).toBeNull();
  });
});