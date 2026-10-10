/**
 * Mobile Create PR sheet (issue #2024 ranks 4 and 9).
 *
 * Rank 4 (#1567): the sheet displayed the NODE's branch as the source while
 * the request itself carried only a mesh id, so the backend resolved the mesh
 * ROOT — producing `main -> main`, or the root's unrelated feature branch. The
 * sheet now previews the resolved pair from the same backend resolver the
 * create path uses, and the request carries the node identity.
 *
 * Rank 9: dismissal had three routes — the Cancel button (disabled while
 * submitting), the sheet backdrop (never gated) and the OS/browser Back
 * gesture (never gated). Back and backdrop both dismissed a sheet with a
 * create request in flight; the deferred request then completed into an
 * unmounted sheet and popped the screen's history entry.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import CreatePrSheet from "../../src/mobile/screens/CreatePrSheet";
import { Sheet } from "../../src/mobile/ui";
import type { AgentNode, Mesh } from "../../src/mobile/api";

const mesh: Mesh = {
  id: 1,
  name: "buildmesh",
  path: "/tmp/repo",
  created_at: "2026-06-11T00:00:00Z",
  scratchpad: "",
  sandbox: false,
};

const node: AgentNode = {
  id: 7,
  mesh_id: 1,
  name: "fix-auth-flow",
  path: "/tmp/wt",
  branch: "feature/auth",
  provider: "anthropic",
  status: "running",
  cli_session_id: null,
  created_at: "2026-06-11T00:00:00Z",
};

const CREATED_URL = "https://github.com/alondero/buildmesh/pull/4242";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

/**
 * Stub the mobile HTTP layer. `source` is the backend's resolved pair;
 * `createGate` lets a test hold the create request open so the busy window
 * can be exercised.
 */
function stubApi(
  options: {
    source?: { head_branch: string; base_branch: string };
    createGate?: Promise<Response>;
  } = {},
) {
  const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
    if (String(url).includes("/pr/source")) {
      return jsonResponse(
        200,
        options.source ?? { head_branch: "feature/auth", base_branch: "main" },
      );
    }
    if (options.createGate) return options.createGate;
    return jsonResponse(200, {
      url: CREATED_URL,
      head_branch: (options.source ?? { head_branch: "feature/auth", base_branch: "main" })
        .head_branch,
      base_branch: (options.source ?? { head_branch: "feature/auth", base_branch: "main" })
        .base_branch,
    });
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

function submittedBody(fetchMock: ReturnType<typeof vi.fn>) {
  const call = fetchMock.mock.calls.find(
    ([url, init]) =>
      String(url).endsWith("/pr") && (init as RequestInit | undefined)?.method === "POST",
  );
  expect(call, "the sheet must POST to /api/meshes/{id}/pr").toBeTruthy();
  return JSON.parse((call![1] as RequestInit).body as string) as Record<string, unknown>;
}

function renderSheet(overrides: Partial<Parameters<typeof CreatePrSheet>[0]> = {}) {
  const props = {
    meshId: mesh.id,
    nodeId: node.id,
    sheetId: 1,
    currentBranch: node.branch ?? "feature/auth",
    onClose: vi.fn(),
    onCreated: vi.fn(),
    ...overrides,
  };
  render(<CreatePrSheet {...props} />);
  return props;
}

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("Sheet — dismissal ownership at the primitive (issue #2024 rank 9)", () => {
  // The sheet itself also guards its Cancel handler, so the primitive's gate
  // is the second, independent line of defence for any other caller that
  // passes a bare `onClose`. Both must hold, so both are pinned.
  it("ignores a backdrop tap when not dismissible", async () => {
    const onClose = vi.fn();
    render(
      <Sheet onClose={onClose} testId="s" label="Test" dismissible={false}>
        <button>inside</button>
      </Sheet>,
    );

    await userEvent.click(screen.getByTestId("s-backdrop"));
    expect(onClose).not.toHaveBeenCalled();

    await userEvent.click(screen.getByTestId("s-backdrop"));
    expect(onClose).not.toHaveBeenCalled();
  });

  it("dismisses on a backdrop tap by default", async () => {
    const onClose = vi.fn();
    render(
      <Sheet onClose={onClose} testId="s" label="Test">
        <button>inside</button>
      </Sheet>,
    );

    await userEvent.click(screen.getByTestId("s-backdrop"));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("moves focus into the sheet and restores it on unmount", async () => {
    const outside = document.createElement("button");
    document.body.appendChild(outside);
    outside.focus();
    expect(document.activeElement).toBe(outside);

    const { unmount } = render(
      <Sheet onClose={() => {}} testId="s" label="Test">
        <input aria-label="field" />
      </Sheet>,
    );
    await waitFor(() => {
      expect(document.activeElement).not.toBe(outside);
    });

    unmount();
    await waitFor(() => {
      expect(document.activeElement).toBe(outside);
    });
    outside.remove();
  });
});

describe("CreatePrSheet — node-scoped source (issue #2024 rank 4)", () => {
  it("sends the node id so the backend cannot fall back to the mesh root", async () => {
    const fetchMock = stubApi();
    renderSheet();

    await userEvent.type(screen.getByTestId("pr-title"), "Fix auth");
    await userEvent.click(screen.getByTestId("pr-submit"));

    await waitFor(() => {
      expect(fetchMock.mock.calls.some(([u]) => String(u).endsWith("/pr"))).toBe(true);
    });
    const body = submittedBody(fetchMock);
    expect(body.node_id).toBe(node.id);
    expect(body.mesh_id).toBeUndefined();
  });

  it("previews the resolved head/base and shows them before submitting", async () => {
    // The Changes screen showed `feature/auth`, but the backend resolves the
    // node worktree's branch. These can differ, and the preview is the point:
    // what the sheet shows is what the create path will use.
    stubApi({ source: { head_branch: "agent/fix-x", base_branch: "trunk" } });
    renderSheet({ currentBranch: "stale-branch-from-the-list" });

    await waitFor(() => {
      expect(screen.getByTestId("pr-head-branch").textContent).toBe("agent/fix-x");
    });
    // The base seeds from the mesh's own base_ref — the client never
    // hardcodes `main`.
    await waitFor(() => {
      expect((screen.getByLabelText("Base Ref") as HTMLInputElement).value).toBe("trunk");
    });
  });

  it("pins the previewed head so a moved worktree fails instead of silently switching", async () => {
    const fetchMock = stubApi({
      source: { head_branch: "agent/fix-x", base_branch: "main" },
    });
    renderSheet();

    await waitFor(() => {
      expect(screen.getByTestId("pr-head-branch").textContent).toBe("agent/fix-x");
    });
    await userEvent.type(screen.getByTestId("pr-title"), "Fix auth");
    await userEvent.click(screen.getByTestId("pr-submit"));

    await waitFor(() => {
      expect(fetchMock.mock.calls.some(([u]) => String(u).endsWith("/pr"))).toBe(true);
    });
    expect(submittedBody(fetchMock).head_branch).toBe("agent/fix-x");
  });

  it("reports a preview failure instead of silently assuming a source", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        jsonResponse(500, { error: "mesh root is on the base ref — nothing to compare" }),
      ),
    );
    renderSheet();

    await waitFor(() => {
      expect(screen.getByTestId("pr-source-error").textContent).toContain(
        "nothing to compare",
      );
    });
  });

  it("reports the backend's rejection in the sheet's own error slot", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string) => {
        if (String(url).includes("/pr/source")) {
          return jsonResponse(200, {
            head_branch: "agent/fix-x",
            base_branch: "main",
          });
        }
        return jsonResponse(500, { error: "worktree is on agent/moved-on" });
      }),
    );
    renderSheet();

    await userEvent.type(screen.getByTestId("pr-title"), "Fix auth");
    await userEvent.click(screen.getByTestId("pr-submit"));

    await waitFor(() => {
      expect(screen.getByTestId("pr-error").textContent).toContain("agent/moved-on");
    });
  });
});

describe("CreatePrSheet — dismissal ownership during submission (issue #2024 rank 9)", () => {
  it("ignores a backdrop tap while a create is in flight", async () => {
    let release: (r: Response) => void = () => {};
    const gate = new Promise<Response>((resolve) => {
      release = resolve;
    });
    stubApi({ createGate: gate });
    const props = renderSheet();

    await userEvent.type(screen.getByTestId("pr-title"), "Fix auth");
    await userEvent.click(screen.getByTestId("pr-submit"));

    // Cancel is disabled and the backdrop must be too — otherwise the sheet
    // vanishes mid-request and the completion lands nowhere.
    await waitFor(() => {
      expect(screen.getByTestId("pr-submit").textContent).toContain("Creating");
    });
    await userEvent.click(screen.getByTestId("create-pr-sheet-backdrop"));

    expect(props.onClose).not.toHaveBeenCalled();
    expect(screen.getByTestId("create-pr-sheet")).toBeTruthy();

    release(jsonResponse(200, { url: CREATED_URL, head_branch: "feature/auth", base_branch: "main" }));
    await waitFor(() => {
      expect(props.onCreated).toHaveBeenCalledWith(CREATED_URL, 1);
    });
  });

  it("still dismisses on a backdrop tap when it is not submitting", async () => {
    stubApi();
    const props = renderSheet();

    await userEvent.click(screen.getByTestId("create-pr-sheet-backdrop"));
    expect(props.onClose).toHaveBeenCalledTimes(1);
  });

  it("ignores Cancel while a create is in flight", async () => {
    let release: (r: Response) => void = () => {};
    const gate = new Promise<Response>((resolve) => {
      release = resolve;
    });
    stubApi({ createGate: gate });
    const props = renderSheet();

    await userEvent.type(screen.getByTestId("pr-title"), "Fix auth");
    await userEvent.click(screen.getByTestId("pr-submit"));
    await waitFor(() => {
      expect(screen.getByTestId("pr-submit").textContent).toContain("Creating");
    });

    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(props.onClose).not.toHaveBeenCalled();

    release(jsonResponse(200, { url: CREATED_URL, head_branch: "feature/auth", base_branch: "main" }));
    await waitFor(() => {
      expect(props.onCreated).toHaveBeenCalled();
    });
  });

  it("freezes the submitted fields while the request is in flight", async () => {
    let release: (r: Response) => void = () => {};
    const gate = new Promise<Response>((resolve) => {
      release = resolve;
    });
    stubApi({ createGate: gate });
    renderSheet();

    await userEvent.type(screen.getByTestId("pr-title"), "Fix auth");
    await userEvent.click(screen.getByTestId("pr-submit"));
    await waitFor(() => {
      expect(screen.getByTestId("pr-submit").textContent).toContain("Creating");
    });

    // Editing the title/base mid-flight would make the confirmed fields
    // disagree with the ones GitHub received.
    expect((screen.getByTestId("pr-title") as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByTestId("pr-body") as HTMLTextAreaElement).disabled).toBe(true);
    expect((screen.getByLabelText("Base Ref") as HTMLInputElement).disabled).toBe(true);

    release(jsonResponse(200, { url: CREATED_URL, head_branch: "feature/auth", base_branch: "main" }));
    await waitFor(() => {
      expect(screen.queryByTestId("pr-submit")).toBeTruthy();
    });
  });

  it("publishes busy state up so the shell's back route can gate on it", async () => {
    let release: (r: Response) => void = () => {};
    const gate = new Promise<Response>((resolve) => {
      release = resolve;
    });
    stubApi({ createGate: gate });
    const onBusyChange = vi.fn();
    renderSheet({ onBusyChange });

    await waitFor(() => {
      expect(onBusyChange).toHaveBeenCalledWith(false);
    });
    await userEvent.type(screen.getByTestId("pr-title"), "Fix auth");
    await userEvent.click(screen.getByTestId("pr-submit"));
    await waitFor(() => {
      expect(onBusyChange).toHaveBeenCalledWith(true);
    });

    release(jsonResponse(200, { url: CREATED_URL, head_branch: "feature/auth", base_branch: "main" }));
    await waitFor(() => {
      expect(onBusyChange).toHaveBeenLastCalledWith(false);
    });
  });

  it("exposes the sheet as a modal dialog for assistive tech", async () => {
    stubApi();
    renderSheet();

    const dialog = screen.getByRole("dialog");
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(dialog.getAttribute("aria-label")).toBe("Create Pull Request");
  });
});