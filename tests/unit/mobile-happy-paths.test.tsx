/**
 * Mobile happy-path coverage (issue #1262).
 *
 * The three core user-facing flows in the mobile SPA — PR creation, issue
 * spawn, archive resume — were only covered by *failure* tests
 * (`mobile-auth-expiry.test.tsx`). The success path was untested: a wire
 * shape drift, a malformed body field, or a missed `history.back()` would
 * ship unnoticed.
 *
 * Each flow is tested by:
 *   1. Stubbing `fetch` with a success response (the same
 *      `vi.stubGlobal("fetch", ...)` pattern as `mobile-auth-expiry.test.tsx`).
 *   2. Driving the screen-level submit/click.
 *   3. Asserting the success callback (`onCreated` / `onSpawned` / `onResumed`)
 *      fires with the right payload. The PR-creation test additionally
 *      mirrors App's `onCreated` body inline (success-toast render +
 *      `history.back()` call) so we can assert the App-level contract
 *      without mounting App.
 *
 * Wire-shape pinning lives in `mobile-api.test.ts` (the helpers' direct
 * callers); these tests assert the *screen-level* wiring on top.
 */
import { useState, type ReactNode } from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import CreatePrSheet from "../../src/mobile/screens/CreatePrSheet";
import IssuesScreen from "../../src/mobile/screens/IssuesScreen";
import ArchivedNodesScreen from "../../src/mobile/screens/ArchivedNodesScreen";
import type {
  AgentNode,
  ArchivedAgentNode,
  GitHubIssue,
  Mesh,
} from "../../src/mobile/api";

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
  branch: "main",
  provider: "anthropic",
  status: "running",
  cli_session_id: null,
  created_at: "2026-06-11T00:00:00Z",
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

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

describe("mobile happy paths (issue #1262)", () => {
  beforeEach(() => {
    localStorage.clear();
    window.history.replaceState(null, "", "/");
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  // -----------------------------------------------------------------------
  // PR creation
  // -----------------------------------------------------------------------

  describe("PR creation", () => {
    it("renders the success toast and calls history.back() on a successful submit", async () => {
      const createdUrl = "https://github.com/alondero/buildmesh/pull/4242";
      vi.stubGlobal(
        "fetch",
        vi.fn().mockResolvedValue(jsonResponse(200, { url: createdUrl })),
      );
      const historyBack = vi
        .spyOn(window.history, "back")
        .mockImplementation(() => {});

      // Mirror App.tsx's `onCreated` body inline so the test can assert the
      // App-level contract (toast render + history.back) without mounting
      // App itself. The minimal `<div data-testid="pr-success-toast">`
      // wrapper only needs to reproduce the testid App renders — App's
      // full markup also includes the link, dismiss button, and styles
      // (`App.tsx:273-303`), which are out of scope here.
      function Harness({ children }: { children: (onCreated: (url: string) => void) => ReactNode }) {
        const [prCreatedUrl, setPrCreatedUrl] = useState<string | null>(null);
        return (
          <>
            {children((url) => {
              setPrCreatedUrl(url);
              window.history.back();
            })}
            {prCreatedUrl && (
              <div data-testid="pr-success-toast" className="toast success">
                <span>PR created: {prCreatedUrl}</span>
              </div>
            )}
          </>
        );
      }

      const onCreatedSpy = vi.fn();
      render(
        <Harness>
          {(onCreated) => (
            <CreatePrSheet
              meshId={mesh.id}
              currentBranch="feature/auth"
              onClose={() => {}}
              onCreated={(url) => {
                onCreatedSpy(url);
                onCreated(url);
              }}
            />
          )}
        </Harness>,
      );

      await userEvent.type(screen.getByTestId("pr-title"), "Recover auth");
      await userEvent.type(screen.getByTestId("pr-body"), "Please recover.");
      await userEvent.click(screen.getByTestId("pr-submit"));

      // The screen-level `onCreated` fires once with the URL — proves the
      // submit fetched, parsed, and routed the success.
      await waitFor(() => {
        expect(onCreatedSpy).toHaveBeenCalledWith(createdUrl);
      });
      // App's mirroring: the toast is now visible AND history.back was
      // invoked to pop the sheet's history entry.
      expect(screen.getByTestId("pr-success-toast")).toBeTruthy();
      expect(screen.getByTestId("pr-success-toast").textContent).toContain(
        createdUrl,
      );
      expect(historyBack).toHaveBeenCalledTimes(1);
      // No error was rendered.
      expect(screen.queryByTestId("pr-error")).toBeNull();
    });
  });

  // -----------------------------------------------------------------------
  // Issue spawn
  // -----------------------------------------------------------------------

  describe("Issue spawn", () => {
    it("calls onSpawned with the created node when the spawn button is clicked", async () => {
      vi.stubGlobal(
        "fetch",
        vi.fn().mockImplementation(async (url: string) => {
          if (/\/api\/meshes\/1\/issues(\?|$)/.test(url)) {
            return jsonResponse(200, [issue]);
          }
          // The spawn endpoint (note the `/spawn` suffix) — must be
          // distinguished from the list endpoint by suffix, not substring
          // alone (see also `mobile-issues-screen.test.tsx` for the same
          // gotcha).
          if (url.endsWith("/issues/101/spawn")) {
            return jsonResponse(200, node);
          }
          return jsonResponse(404, { error: "not found" });
        }),
      );

      const onSpawned = vi.fn();
      render(
        <IssuesScreen
          mesh={mesh}
          onBack={() => {}}
          onSpawned={onSpawned}
        />,
      );

      // Expand the issue card to reveal the Spawn button.
      await userEvent.click(await screen.findByTestId("issue-101"));
      await userEvent.click(screen.getByTestId("issue-spawn-101"));

      await waitFor(() => {
        expect(onSpawned).toHaveBeenCalledWith(node);
      });
    });

    it("does not call onSpawned when the spawn endpoint fails", async () => {
      vi.stubGlobal(
        "fetch",
        vi.fn().mockImplementation(async (url: string) => {
          if (/\/api\/meshes\/1\/issues(\?|$)/.test(url)) {
            return jsonResponse(200, [issue]);
          }
          if (url.endsWith("/issues/101/spawn")) {
            return jsonResponse(500, { error: "spawn failed" });
          }
          return jsonResponse(404, { error: "not found" });
        }),
      );

      const onSpawned = vi.fn();
      render(
        <IssuesScreen
          mesh={mesh}
          onBack={() => {}}
          onSpawned={onSpawned}
        />,
      );

      await userEvent.click(await screen.findByTestId("issue-101"));
      await userEvent.click(screen.getByTestId("issue-spawn-101"));

      await waitFor(() => {
        expect(screen.getByText("spawn failed")).toBeTruthy();
      });
      expect(onSpawned).not.toHaveBeenCalled();
    });
  });

  // -----------------------------------------------------------------------
  // Archive resume
  // -----------------------------------------------------------------------

  describe("Archive resume", () => {
    it("calls onResumed with the imported node when the resume button is clicked", async () => {
      vi.stubGlobal(
        "fetch",
        vi.fn().mockImplementation(async (url: string) => {
          if (url.includes("/agent-nodes/discover")) {
            return jsonResponse(200, [archived]);
          }
          if (url.includes("/import-and-resume")) {
            return jsonResponse(200, node);
          }
          return jsonResponse(404, { error: "not found" });
        }),
      );

      const onResumed = vi.fn();
      render(
        <ArchivedNodesScreen
          mesh={mesh}
          onBack={() => {}}
          onResumed={onResumed}
        />,
      );

      // First click expands the card, second commits the resume.
      await userEvent.click(await screen.findByTestId("node-session-101"));
      await userEvent.click(screen.getByTestId("node-resume-session-101"));

      await waitFor(() => {
        expect(onResumed).toHaveBeenCalledWith(node);
      });
    });

    it("surfaces a 207 partial response as a screen-level error and skips onResumed", async () => {
      // The 207 branch (api.ts:299-304) means "node created but spawn
      // failed". The screen must NOT call onResumed (the caller would
      // navigate to a terminal that immediately dies), and it MUST
      // surface the spawn_error so the user knows what to retry.
      const partialNode = { ...node, id: 99 };
      vi.stubGlobal(
        "fetch",
        vi.fn().mockImplementation(async (url: string) => {
          if (url.includes("/agent-nodes/discover")) {
            return jsonResponse(200, [archived]);
          }
          if (url.includes("/import-and-resume")) {
            return jsonResponse(207, {
              node: partialNode,
              spawn_error: "agent binary missing",
            });
          }
          return jsonResponse(404, { error: "not found" });
        }),
      );

      const onResumed = vi.fn();
      render(
        <ArchivedNodesScreen
          mesh={mesh}
          onBack={() => {}}
          onResumed={onResumed}
        />,
      );

      await userEvent.click(await screen.findByTestId("node-session-101"));
      await userEvent.click(screen.getByTestId("node-resume-session-101"));

      await waitFor(() => {
        expect(
          screen.getByText(/Imported but spawn failed: agent binary missing/),
        ).toBeTruthy();
      });
      expect(onResumed).not.toHaveBeenCalled();
    });
  });
});