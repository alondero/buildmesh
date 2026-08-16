import { act, createElement } from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import App from "../../src/mobile/App";
import { rememberToken } from "../../src/mobile/api";

const appState = vi.hoisted(() => ({
  authFailed: null as (() => void) | null,
  connect: null as (() => void) | null,
  openIssues: null as (() => void) | null,
  lateSpawn: null as (() => void) | null,
  connectRenders: vi.fn(),
}));

const node = {
  id: 7,
  mesh_id: 1,
  name: "node-7",
  path: "/tmp/worktree",
  branch: "main",
  provider: "anthropic",
  status: "running",
  cli_session_id: null,
  created_at: "2026-06-11T00:00:00Z",
};

vi.mock("../../src/mobile/screens/Connect", () => ({
  default: (props: { notice?: string | null; onConnected: () => void }) => {
    appState.connect = props.onConnected;
    appState.connectRenders();
    return createElement(
      "main",
      { "data-testid": "connect-screen" },
      props.notice,
    );
  },
}));

vi.mock("../../src/mobile/screens/NodeList", () => ({
  default: (props: {
    onAuthFailed: () => void;
    onOpenNode: (nextNode: typeof node) => void;
    onOpenIssues: (mesh: typeof mesh) => void;
  }) => {
    appState.authFailed = props.onAuthFailed;
    appState.openIssues = () => props.onOpenIssues(mesh);
    return createElement(
      "div",
      { "data-testid": "mock-node-list" },
      createElement(
        "button",
        { "data-testid": "open-terminal", onClick: () => props.onOpenNode(node) },
        "open terminal",
      ),
    );
  },
}));

const mesh = {
  id: 1,
  name: "buildmesh",
  path: "/tmp/repo",
  created_at: "2026-06-11T00:00:00Z",
  scratchpad: "",
  sandbox: false,
};

vi.mock("../../src/mobile/screens/TerminalScreen", () => ({
  default: (props: { onOpenChanges?: () => void }) =>
    createElement(
      "button",
      { "data-testid": "open-changes", onClick: props.onOpenChanges },
      "open changes",
    ),
}));

vi.mock("../../src/mobile/screens/ChangesScreen", () => ({
  default: (props: { onOpenPr: (branch: string) => void }) =>
    createElement(
      "button",
      { "data-testid": "open-pr", onClick: () => props.onOpenPr("main") },
      "open PR",
    ),
}));

vi.mock("../../src/mobile/screens/IssuesScreen", () => ({
  default: (props: { onSpawned: (nextNode: typeof node) => void }) => {
    appState.lateSpawn = () => props.onSpawned(node);
    return createElement("div", { "data-testid": "mock-issues" }, "issues");
  },
}));

vi.mock("../../src/mobile/screens/CreatePrSheet", () => ({
  default: (props: { onAuthFailed: () => void }) =>
    createElement(
      "div",
      { "data-testid": "create-pr-sheet" },
      createElement(
        "button",
        { "data-testid": "sheet-auth", onClick: props.onAuthFailed },
        "expired",
      ),
    ),
}));

describe("mobile App auth recovery", () => {
  beforeEach(() => {
    localStorage.clear();
    rememberToken("device-token");
    appState.authFailed = null;
    appState.connect = null;
    appState.openIssues = null;
    appState.lateSpawn = null;
    appState.connectRenders.mockClear();
    window.history.replaceState(null, "", "/");
  });

  afterEach(() => {
    localStorage.clear();
  });

  it("closes an open PR sheet before rendering Connect", async () => {
    render(<App />);

    await screen.findByTestId("mock-node-list");
    await act(async () => screen.getByTestId("open-terminal").click());
    await act(async () => screen.getByTestId("open-changes").click());
    await act(async () => screen.getByTestId("open-pr").click());
    expect(await screen.findByTestId("create-pr-sheet")).toBeTruthy();

    await act(async () => screen.getByTestId("sheet-auth").click());

    expect(await screen.findByTestId("connect-screen")).toBeTruthy();
    expect(screen.queryByTestId("create-pr-sheet")).toBeNull();
  });

  it("clears the token and transitions to Connect only once for duplicate failures", async () => {
    const removeItem = vi.spyOn(Storage.prototype, "removeItem");
    render(<App />);
    await screen.findByTestId("mock-node-list");
    const callback = appState.authFailed;
    expect(callback).toBeTruthy();

    act(() => callback!());
    await screen.findByTestId("connect-screen");
    const rendersAfterFirstFailure = appState.connectRenders.mock.calls.length;

    act(() => callback!());

    expect(removeItem).toHaveBeenCalledTimes(1);
    expect(appState.connectRenders).toHaveBeenCalledTimes(rendersAfterFirstFailure);
    expect(screen.getByTestId("connect-screen")).toBeTruthy();
    removeItem.mockRestore();
  });

  it("ignores a late successful request after recovery", async () => {
    render(<App />);
    await screen.findByTestId("mock-node-list");
    const openIssues = appState.openIssues;
    const callback = appState.authFailed;
    expect(openIssues).toBeTruthy();
    expect(callback).toBeTruthy();

    act(() => openIssues!());
    await screen.findByTestId("mock-issues");
    const lateSpawn = appState.lateSpawn;
    expect(lateSpawn).toBeTruthy();
    act(() => callback!());
    await screen.findByTestId("connect-screen");
    act(() => lateSpawn!());
    await waitFor(() => {
      expect(screen.getByTestId("connect-screen")).toBeTruthy();
      expect(screen.queryByTestId("open-changes")).toBeNull();
    });
  });

  it("ignores a late auth failure from the old screen after reconnect", async () => {
    const removeItem = vi.spyOn(Storage.prototype, "removeItem");
    render(<App />);
    await screen.findByTestId("mock-node-list");
    const oldAuthFailed = appState.authFailed;
    expect(oldAuthFailed).toBeTruthy();

    act(() => oldAuthFailed!());
    await screen.findByTestId("connect-screen");
    expect(appState.connect).toBeTruthy();

    act(() => appState.connect!());
    await screen.findByTestId("mock-node-list");
    act(() => oldAuthFailed!());

    expect(screen.getByTestId("mock-node-list")).toBeTruthy();
    expect(removeItem).toHaveBeenCalledTimes(1);
    removeItem.mockRestore();
  });
});
