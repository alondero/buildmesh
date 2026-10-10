import { act, createElement } from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import App from "../../src/mobile/App";

const appState = vi.hoisted(() => ({
  authFailed: null as (() => void) | null,
  connect: null as (() => void) | null,
  openIssues: null as (() => void) | null,
  lateSpawn: null as (() => void) | null,
  replyCompletion: null as (() => void) | null,
  // Retained so a test can replay a completion from a sheet that has
  // already been dismissed (issue #2024 rank 9).
  lastSheetCreate: null as ((url: string, sheetId: number) => void) | null,
  lastSheetId: 0,
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
    onOpenNode: (nextNode: typeof node, prompt?: string) => void;
    onOpenIssues: (mesh: typeof mesh) => void;
  }) => {
    appState.authFailed = props.onAuthFailed;
    appState.openIssues = () => props.onOpenIssues(mesh);
    return createElement(
      "div",
      { "data-testid": "mock-node-list" },
      createElement(
        "button",
        {
          "data-testid": "open-terminal",
          onClick: () => props.onOpenNode(node, "Check the preview deploy."),
        },
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

vi.mock("../../src/mobile/screens/NodeOverview", () => ({
  default: (props: {
    visitId: number;
    prompt?: string;
    draft?: string;
    replySending?: boolean;
    replyNotice?: string;
    onDraftChange: (draft: string, nodeId: number, visitId: number) => void;
    onReplySendingChange: (
      sending: boolean,
      nodeId: number,
      visitId: number,
    ) => void;
    onReplyNoticeChange: (
      notice: string,
      nodeId: number,
      visitId: number,
    ) => void;
    onChanges: () => void;
    onTerminal: () => void;
  }) => {
    appState.replyCompletion = () => {
      props.onDraftChange("", node.id, props.visitId);
      props.onReplyNoticeChange(
        "Reply delivered to the terminal.",
        node.id,
        props.visitId,
      );
      props.onReplySendingChange(false, node.id, props.visitId);
    };
    return createElement(
      "div",
      null,
      createElement("p", { "data-testid": "agent-request" }, props.prompt),
      createElement("p", { "data-testid": "reply-draft" }, props.draft),
      createElement(
        "p",
        { "data-testid": "reply-sending" },
        String(props.replySending ?? false),
      ),
      createElement("p", { "data-testid": "reply-notice" }, props.replyNotice),
      createElement(
        "button",
        {
          "data-testid": "edit-reply",
          onClick: () =>
            props.onDraftChange(
              "I will check the build logs.",
              node.id,
              props.visitId,
            ),
        },
        "Edit reply",
      ),
      createElement(
        "button",
        {
          "data-testid": "start-reply",
          onClick: () =>
            props.onReplySendingChange(true, node.id, props.visitId),
        },
        "Start reply",
      ),
      createElement(
        "button",
        { "data-testid": "open-changes", onClick: props.onChanges },
        "Review changes",
      ),
      createElement(
        "button",
        { "data-testid": "overview-terminal", onClick: props.onTerminal },
        "Open terminal",
      ),
    );
  },
}));

vi.mock("../../src/mobile/screens/TerminalScreen", () => ({
  default: (props: { onOpenChanges?: () => void }) =>
    createElement(
      "div",
      null,
      createElement("p", { "data-testid": "mock-terminal" }, "Terminal"),
      createElement(
        "button",
        { "data-testid": "terminal-open-changes", onClick: props.onOpenChanges },
        "open changes",
      ),
    ),
}));

vi.mock("../../src/mobile/screens/ChangesScreen", () => ({
  default: (props: {
    onOpenPr: (branch: string) => void;
    onBack: () => void;
  }) =>
    createElement(
      "div",
      null,
      createElement(
        "button",
        { "data-testid": "open-pr", onClick: () => props.onOpenPr("main") },
        "open PR",
      ),
      createElement(
        "button",
        { "data-testid": "changes-back", onClick: props.onBack },
        "back",
      ),
    ),
}));

vi.mock("../../src/mobile/screens/IssuesScreen", () => ({
  default: (props: { onSpawned: (nextNode: typeof node) => void }) => {
    appState.lateSpawn = () => props.onSpawned(node);
    return createElement("div", { "data-testid": "mock-issues" }, "issues");
  },
}));

vi.mock("../../src/mobile/screens/CreatePrSheet", () => ({
  default: (props: {
    onAuthFailed: () => void;
    onBusyChange?: (busy: boolean) => void;
    onCreated?: (url: string, sheetId: number) => void;
    sheetId: number;
  }) => {
    appState.lastSheetId = props.sheetId;
    appState.lastSheetCreate = props.onCreated ?? null;
    return createElement(
      "div",
      { "data-testid": "create-pr-sheet" },
      createElement(
        "button",
        { "data-testid": "sheet-auth", onClick: props.onAuthFailed },
        "expired",
      ),
      // Issue #2024 rank 9: the sheet publishes its in-flight state so the
      // app's back route honours the same dismissal decision.
      createElement(
        "button",
        {
          "data-testid": "sheet-busy",
          onClick: () => props.onBusyChange?.(true),
        },
        "busy",
      ),
      createElement(
        "button",
        {
          "data-testid": "sheet-complete",
          onClick: () => props.onCreated?.("https://example.test/pr/1", props.sheetId),
        },
        "complete",
      ),
    );
  },
}));

describe("mobile App auth recovery", () => {
  beforeEach(() => {
    localStorage.clear();
    localStorage.setItem("buildmesh_token", "device-token");
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(null, { status: 204 })));
    appState.authFailed = null;
    appState.connect = null;
    appState.openIssues = null;
    appState.lateSpawn = null;
    appState.replyCompletion = null;
    appState.connectRenders.mockClear();
    window.history.replaceState(null, "", "/");
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    localStorage.clear();
    vi.restoreAllMocks();
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

  it("keeps request text and an unsent reply across details, terminal, and changes", async () => {
    render(<App />);
    await screen.findByTestId("mock-node-list");
    await act(async () => screen.getByTestId("open-terminal").click());
    expect(screen.getByTestId("agent-request").textContent).toBe(
      "Check the preview deploy.",
    );
    await act(async () => screen.getByTestId("edit-reply").click());

    await act(async () => screen.getByTestId("overview-terminal").click());
    expect(screen.getByTestId("mock-terminal")).toBeTruthy();
    await act(async () =>
      window.dispatchEvent(new PopStateEvent("popstate")),
    );
    expect(screen.getByTestId("agent-request").textContent).toBe(
      "Check the preview deploy.",
    );
    expect(screen.getByTestId("reply-draft").textContent).toBe(
      "I will check the build logs.",
    );

    await act(async () => screen.getByTestId("open-changes").click());
    expect(screen.getByTestId("open-pr")).toBeTruthy();
    await act(async () =>
      window.dispatchEvent(new PopStateEvent("popstate")),
    );
    expect(screen.getByTestId("agent-request").textContent).toBe(
      "Check the preview deploy.",
    );
    expect(screen.getByTestId("reply-draft").textContent).toBe(
      "I will check the build logs.",
    );
  });

  it("settles a pending reply after navigating to changes", async () => {
    render(<App />);
    await screen.findByTestId("mock-node-list");
    await act(async () => screen.getByTestId("open-terminal").click());
    await act(async () => screen.getByTestId("edit-reply").click());
    await act(async () => screen.getByTestId("start-reply").click());
    expect(screen.getByTestId("reply-sending").textContent).toBe("true");

    await act(async () => screen.getByTestId("overview-terminal").click());
    expect(screen.getByTestId("mock-terminal")).toBeTruthy();
    expect(appState.replyCompletion).toBeTruthy();
    await act(async () =>
      window.dispatchEvent(new PopStateEvent("popstate")),
    );
    expect(screen.getByTestId("reply-sending").textContent).toBe("true");

    await act(async () => screen.getByTestId("open-changes").click());
    await act(async () => appState.replyCompletion?.());
    await act(async () =>
      window.dispatchEvent(new PopStateEvent("popstate")),
    );
    expect(screen.getByTestId("agent-request").textContent).toBe(
      "Check the preview deploy.",
    );
    expect(screen.getByTestId("reply-draft").textContent).toBe("");
    expect(screen.getByTestId("reply-sending").textContent).toBe("false");
    expect(screen.getByTestId("reply-notice").textContent).toBe(
      "Reply delivered to the terminal.",
    );
  });

  it("ignores a reply completion from a previous visit to the same agent", async () => {
    render(<App />);
    await screen.findByTestId("mock-node-list");
    await act(async () => screen.getByTestId("open-terminal").click());
    await act(async () => screen.getByTestId("edit-reply").click());
    await act(async () => screen.getByTestId("start-reply").click());
    const oldCompletion = appState.replyCompletion;
    expect(oldCompletion).toBeTruthy();

    await act(async () => window.dispatchEvent(new PopStateEvent("popstate")));
    await act(async () => screen.getByTestId("open-terminal").click());
    await act(async () => screen.getByTestId("edit-reply").click());
    expect(screen.getByTestId("reply-draft").textContent).toBe(
      "I will check the build logs.",
    );
    await act(async () => screen.getByTestId("start-reply").click());
    expect(screen.getByTestId("reply-sending").textContent).toBe("true");

    await act(async () => oldCompletion?.());

    expect(screen.getByTestId("reply-draft").textContent).toBe(
      "I will check the build logs.",
    );
    expect(screen.getByTestId("reply-sending").textContent).toBe("true");
    expect(screen.getByTestId("reply-notice").textContent).toBe("");
  });

  it("clears the token and transitions to Connect only once for duplicate failures", async () => {
    const removeItem = vi.spyOn(Storage.prototype, "removeItem");
    render(<App />);
    await screen.findByTestId("mock-node-list");
    const callback = appState.authFailed;
    removeItem.mockClear();
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
    removeItem.mockClear();
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

  it("pops the history entry when auth fails while a PR sheet is open (issue #1260)", async () => {
    // Issue #1260: openPrSheet pushes a history entry, but handleAuthFailed
    // used to only clear React state — the dead entry lingered, so the first
    // back press did nothing and iOS swipe-back read as broken.
    const historyBack = vi
      .spyOn(window.history, "back")
      .mockImplementation(() => undefined);
    render(<App />);

    await screen.findByTestId("mock-node-list");
    await act(async () => screen.getByTestId("open-terminal").click());
    await act(async () => screen.getByTestId("open-changes").click());
    await act(async () => screen.getByTestId("open-pr").click());
    expect(await screen.findByTestId("create-pr-sheet")).toBeTruthy();

    // Opening the sheet pushed a history entry — back must NOT have fired yet.
    expect(historyBack).not.toHaveBeenCalled();

    await act(async () => screen.getByTestId("sheet-auth").click());
    await screen.findByTestId("connect-screen");

    // Auth failure while a sheet was open should pop exactly one entry —
    // matching the onCreated success path's `window.history.back()`.
    expect(historyBack).toHaveBeenCalledTimes(1);
    expect(screen.queryByTestId("create-pr-sheet")).toBeNull();
  });

  it("keeps the PR sheet open on a back gesture while a create is in flight, and preserves history depth (issue #2024 rank 9)", async () => {
    // Cancel and the backdrop are gated by the sheet's own busy flag; the
    // OS/browser Back route is gated by App. Before the fix, back closed
    // the sheet unconditionally — the deferred request then completed into
    // an unmounted sheet and popped the *screen's* history entry.
    const historyBack = vi
      .spyOn(window.history, "back")
      .mockImplementation(() => undefined);
    const pushState = vi.spyOn(window.history, "pushState");

    render(<App />);
    await screen.findByTestId("mock-node-list");
    await act(async () => screen.getByTestId("open-terminal").click());
    await act(async () => screen.getByTestId("open-changes").click());
    await act(async () => screen.getByTestId("open-pr").click());
    expect(await screen.findByTestId("create-pr-sheet")).toBeTruthy();

    const pushedBeforeBusy = pushState.mock.calls.length;
    await act(async () => screen.getByTestId("sheet-busy").click());

    // The browser back gesture arrives while the sheet is busy.
    await act(async () => {
      window.dispatchEvent(new PopStateEvent("popstate"));
    });

    expect(screen.getByTestId("create-pr-sheet")).toBeTruthy();
    expect(historyBack).not.toHaveBeenCalled();
    // The consumed entry is re-pushed so the stack keeps its depth and the
    // user's next back press still lands where they expect.
    expect(pushState.mock.calls.length).toBe(pushedBeforeBusy + 1);

    pushState.mockRestore();
    historyBack.mockRestore();
  });

  it("closes the sheet on a back gesture when it is not busy", async () => {
    const historyBack = vi
      .spyOn(window.history, "back")
      .mockImplementation(() => undefined);
    const pushState = vi.spyOn(window.history, "pushState");

    render(<App />);
    await screen.findByTestId("mock-node-list");
    await act(async () => screen.getByTestId("open-terminal").click());
    await act(async () => screen.getByTestId("open-changes").click());
    await act(async () => screen.getByTestId("open-pr").click());
    expect(await screen.findByTestId("create-pr-sheet")).toBeTruthy();

    const pushedBefore = pushState.mock.calls.length;
    await act(async () => {
      window.dispatchEvent(new PopStateEvent("popstate"));
    });

    expect(screen.queryByTestId("create-pr-sheet")).toBeNull();
    // No re-push on the normal dismissal path — the gesture is allowed to
    // consume the entry the sheet pushed.
    expect(pushState.mock.calls.length).toBe(pushedBefore);

    pushState.mockRestore();
    historyBack.mockRestore();
  });

  it("pops the sheet's entry on success without navigating the screen below (issue #2024 rank 9)", async () => {
    const historyBack = vi
      .spyOn(window.history, "back")
      .mockImplementation(() => undefined);
    const pushState = vi.spyOn(window.history, "pushState");

    render(<App />);
    await screen.findByTestId("mock-node-list");
    await act(async () => screen.getByTestId("open-terminal").click());
    await act(async () => screen.getByTestId("open-changes").click());
    await act(async () => screen.getByTestId("open-pr").click());
    expect(await screen.findByTestId("create-pr-sheet")).toBeTruthy();

    await act(async () => screen.getByTestId("sheet-complete").click());

    // Exactly one back: the sheet's own entry. If completion were not
    // scoped, a stale sheet's completion would pop a second entry and
    // navigate the Changes screen out from under the user.
    expect(historyBack).toHaveBeenCalledTimes(1);
    expect(await screen.findByTestId("pr-success-toast")).toBeTruthy();

    pushState.mockRestore();
    historyBack.mockRestore();
  });

  it("ignores a completion from a sheet that is no longer the open one (issue #2024 rank 9)", async () => {
    const historyBack = vi
      .spyOn(window.history, "back")
      .mockImplementation(() => undefined);
    const pushState = vi.spyOn(window.history, "pushState");

    render(<App />);
    await screen.findByTestId("mock-node-list");
    await act(async () => screen.getByTestId("open-terminal").click());
    await act(async () => screen.getByTestId("open-changes").click());
    await act(async () => screen.getByTestId("open-pr").click());
    expect(await screen.findByTestId("create-pr-sheet")).toBeTruthy();

    // Dismiss the sheet, then replay the completion the retired sheet was
    // still holding. App must not treat it as its own.
    const retired = appState.lastSheetCreate;
    const retiredId = appState.lastSheetId;
    expect(typeof retired).toBe("function");
    const pushedBefore = pushState.mock.calls.length;
    await act(async () => {
      window.dispatchEvent(new PopStateEvent("popstate"));
    });
    await act(async () => retired!("https://example.test/pr/2", retiredId));

    expect(historyBack).not.toHaveBeenCalled();
    expect(screen.queryByTestId("pr-success-toast")).toBeNull();
    expect(screen.queryByTestId("create-pr-sheet")).toBeNull();
    expect(pushState.mock.calls.length).toBe(pushedBefore);

    pushState.mockRestore();
    historyBack.mockRestore();
  });

  it("does not pop history on auth failure when no PR sheet was open", async () => {
    // (no sheet) must NOT call window.history.back() — there is no entry
    // pushed by openPrSheet to pop, and a stray back() leaves the SPA.
    const historyBack = vi
      .spyOn(window.history, "back")
      .mockImplementation(() => undefined);
    render(<App />);

    await screen.findByTestId("mock-node-list");
    const callback = appState.authFailed;
    expect(callback).toBeTruthy();

    act(() => callback!());
    await screen.findByTestId("connect-screen");

    expect(historyBack).not.toHaveBeenCalled();
  });
});
