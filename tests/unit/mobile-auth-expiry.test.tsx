/**
 * Mobile auth-expiry recovery (issue #811).
 *
 * Every data screen must route an expired session through the same recovery
 * callback that clears the device token and returns the user to Connect, while
 * ordinary request failures must stay on the screen as its normal error.
 */
import { useState, type ReactNode } from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import ChangesScreen from "../../src/mobile/screens/ChangesScreen";
import DiffScreen from "../../src/mobile/screens/DiffScreen";
import IssuesScreen from "../../src/mobile/screens/IssuesScreen";
import ArchivedNodesScreen from "../../src/mobile/screens/ArchivedNodesScreen";
import CreatePrSheet from "../../src/mobile/screens/CreatePrSheet";
import {
  clearStoredToken,
  rememberToken,
  readStoredToken,
  type AgentNode,
  type ArchivedAgentNode,
  type GitHubIssue,
  type Mesh,
} from "../../src/mobile/api";

const node: AgentNode = {
  id: 5,
  mesh_id: 1,
  name: "node",
  path: "/tmp/worktree",
  branch: "main",
  provider: "anthropic",
  status: "running",
  cli_session_id: null,
  created_at: "2026-06-11T00:00:00Z",
};

const mesh: Mesh = {
  id: 1,
  name: "buildmesh",
  path: "/tmp/repo",
  created_at: "2026-06-11T00:00:00Z",
  scratchpad: "",
  sandbox: false,
};

const issue: GitHubIssue = {
  number: 101,
  title: "Add auth recovery",
  body: "Please recover expired mobile sessions.",
  url: "https://github.com/alondero/buildmesh/issues/101",
  state: "open",
  labels: ["bug"],
  blocked_by: [],
};

const archived: ArchivedAgentNode = {
  session_id: "session-101",
  first_message: "Recover the mobile session",
  branch: "main",
  cwd: "/tmp/repo",
  timestamp: "2026-06-11T00:00:00Z",
  worktree_name: "recover-session",
};

function response(status: number, body: unknown) {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
  };
}

function authResponse() {
  return response(401, { error: "session expired" });
}

function errorResponse(message: string) {
  return response(500, { error: message });
}

/** Mirrors App.handleAuthFailed's observable contract for screen tests. */
function AuthRecoveryHarness({
  children,
}: {
  children: (onAuthFailed: () => void) => ReactNode;
}) {
  const [connected, setConnected] = useState(true);
  if (!connected) {
    return <div data-testid="connect-screen">Connect</div>;
  }
  return (
    <>
      {children(() => {
        clearStoredToken();
        setConnected(false);
      })}
    </>
  );
}

function renderWithRecovery(
  screenFactory: (onAuthFailed: () => void) => ReactNode,
) {
  rememberToken("expired-device-token");
  return render(<AuthRecoveryHarness>{screenFactory}</AuthRecoveryHarness>);
}

describe("mobile auth-expiry recovery", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("returns ChangesScreen to Connect and clears the token on an auth error", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(authResponse()));

    renderWithRecovery((onAuthFailed) => (
      <ChangesScreen
        node={node}
        onBack={() => {}}
        onOpenDiff={() => {}}
        onOpenPr={() => {}}
        onAuthFailed={onAuthFailed}
      />
    ));

    await waitFor(() => {
      expect(screen.getByTestId("connect-screen")).toBeTruthy();
    });
    expect(readStoredToken()).toBeNull();
  });

  it("keeps ChangesScreen's normal error state for a non-auth error", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(errorResponse("changes failed")),
    );
    const onAuthFailed = vi.fn();

    render(
      <ChangesScreen
        node={node}
        onBack={() => {}}
        onOpenDiff={() => {}}
        onOpenPr={() => {}}
        onAuthFailed={onAuthFailed}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("changes-error").textContent).toContain(
        "changes failed",
      );
    });
    expect(onAuthFailed).not.toHaveBeenCalled();
    expect(screen.queryByTestId("connect-screen")).toBeNull();
  });

  it("returns ChangesScreen to Connect when the GitHub auth probe expires", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation(async (url: string) => {
        if (url.includes("/api/gh/auth")) return authResponse();
        if (url.includes("/git/status")) return response(200, []);
        if (url.includes("/git/summary")) {
          return response(200, { added: 0, modified: 0, deleted: 0 });
        }
        return response(200, { branch: "main" });
      }),
    );

    renderWithRecovery((onAuthFailed) => (
      <ChangesScreen
        node={node}
        onBack={() => {}}
        onOpenDiff={() => {}}
        onOpenPr={() => {}}
        onAuthFailed={onAuthFailed}
      />
    ));

    await waitFor(() => {
      expect(screen.getByTestId("connect-screen")).toBeTruthy();
    });
    expect(readStoredToken()).toBeNull();
  });

  it("returns DiffScreen to Connect and clears the token on an auth error", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(authResponse()));

    renderWithRecovery((onAuthFailed) => (
      <DiffScreen
        node={node}
        filePath="src/a.ts"
        onBack={() => {}}
        onAuthFailed={onAuthFailed}
      />
    ));

    await waitFor(() => {
      expect(screen.getByTestId("connect-screen")).toBeTruthy();
    });
    expect(readStoredToken()).toBeNull();
  });

  it("keeps DiffScreen's normal error state for a non-auth error", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(errorResponse("diff failed")),
    );
    const onAuthFailed = vi.fn();

    render(
      <DiffScreen
        node={node}
        filePath="src/a.ts"
        onBack={() => {}}
        onAuthFailed={onAuthFailed}
      />,
    );

    await waitFor(() => {
      expect(screen.getByText("diff failed")).toBeTruthy();
    });
    expect(onAuthFailed).not.toHaveBeenCalled();
    expect(screen.queryByTestId("connect-screen")).toBeNull();
  });

  it("returns IssuesScreen to Connect and clears the token on a list auth error", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(authResponse()));

    renderWithRecovery((onAuthFailed) => (
      <IssuesScreen mesh={mesh} onBack={() => {}} onSpawned={() => {}} onAuthFailed={onAuthFailed} />
    ));

    await waitFor(() => {
      expect(screen.getByTestId("connect-screen")).toBeTruthy();
    });
    expect(readStoredToken()).toBeNull();
  });

  it("keeps IssuesScreen's normal error state for a non-auth list error", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(errorResponse("issues failed")),
    );
    const onAuthFailed = vi.fn();

    render(
      <IssuesScreen
        mesh={mesh}
        onBack={() => {}}
        onSpawned={() => {}}
        onAuthFailed={onAuthFailed}
      />,
    );

    await waitFor(() => {
      expect(screen.getByText("issues failed")).toBeTruthy();
    });
    expect(onAuthFailed).not.toHaveBeenCalled();
    expect(screen.queryByTestId("connect-screen")).toBeNull();
  });

  it("returns IssuesScreen to Connect when spawning an issue hits auth expiry", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation(async (url: string) => {
        if (/\/api\/meshes\/1\/issues\/?$/.test(url)) {
          return response(200, [issue]);
        }
        return authResponse();
      }),
    );

    renderWithRecovery((onAuthFailed) => (
      <IssuesScreen mesh={mesh} onBack={() => {}} onSpawned={() => {}} onAuthFailed={onAuthFailed} />
    ));

    await userEvent.click(await screen.findByTestId("issue-101"));
    await userEvent.click(screen.getByTestId("issue-spawn-101"));

    await waitFor(() => {
      expect(screen.getByTestId("connect-screen")).toBeTruthy();
    });
    expect(readStoredToken()).toBeNull();
  });

  it("returns ArchivedNodesScreen to Connect and clears the token on a list auth error", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(authResponse()));

    renderWithRecovery((onAuthFailed) => (
      <ArchivedNodesScreen
        mesh={mesh}
        onBack={() => {}}
        onResumed={() => {}}
        onAuthFailed={onAuthFailed}
      />
    ));

    await waitFor(() => {
      expect(screen.getByTestId("connect-screen")).toBeTruthy();
    });
    expect(readStoredToken()).toBeNull();
  });

  it("keeps ArchivedNodesScreen's normal error state for a non-auth list error", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(errorResponse("archive failed")),
    );
    const onAuthFailed = vi.fn();

    render(
      <ArchivedNodesScreen
        mesh={mesh}
        onBack={() => {}}
        onResumed={() => {}}
        onAuthFailed={onAuthFailed}
      />,
    );

    await waitFor(() => {
      expect(screen.getByText("archive failed")).toBeTruthy();
    });
    expect(onAuthFailed).not.toHaveBeenCalled();
    expect(screen.queryByTestId("connect-screen")).toBeNull();
  });

  it("returns ArchivedNodesScreen to Connect when resuming a node hits auth expiry", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation(async (url: string) => {
        if (url.includes("/agent-nodes/discover")) {
          return response(200, [archived]);
        }
        return authResponse();
      }),
    );

    renderWithRecovery((onAuthFailed) => (
      <ArchivedNodesScreen
        mesh={mesh}
        onBack={() => {}}
        onResumed={() => {}}
        onAuthFailed={onAuthFailed}
      />
    ));

    await userEvent.click(await screen.findByTestId("node-session-101"));
    await userEvent.click(screen.getByTestId("node-resume-session-101"));

    await waitFor(() => {
      expect(screen.getByTestId("connect-screen")).toBeTruthy();
    });
    expect(readStoredToken()).toBeNull();
  });

  it("returns CreatePrSheet to Connect and clears the token on an auth error", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(authResponse()));

    renderWithRecovery((onAuthFailed) => (
      <CreatePrSheet
        meshId={mesh.id}
        currentBranch="feature/auth"
        onClose={() => {}}
        onCreated={() => {}}
        onAuthFailed={onAuthFailed}
      />
    ));

    await userEvent.type(screen.getByTestId("pr-title"), "Recover auth");
    await userEvent.click(screen.getByTestId("pr-submit"));

    await waitFor(() => {
      expect(screen.getByTestId("connect-screen")).toBeTruthy();
    });
    expect(readStoredToken()).toBeNull();
  });

  it("keeps CreatePrSheet's normal error state for a non-auth error", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(errorResponse("create PR failed")),
    );
    const onAuthFailed = vi.fn();

    render(
      <CreatePrSheet
        meshId={mesh.id}
        currentBranch="feature/auth"
        onClose={() => {}}
        onCreated={() => {}}
        onAuthFailed={onAuthFailed}
      />,
    );

    await userEvent.type(screen.getByTestId("pr-title"), "Recover auth");
    await userEvent.click(screen.getByTestId("pr-submit"));

    await waitFor(() => {
      expect(screen.getByTestId("pr-error").textContent).toContain(
        "create PR failed",
      );
    });
    expect(onAuthFailed).not.toHaveBeenCalled();
    expect(screen.queryByTestId("connect-screen")).toBeNull();
  });
});
