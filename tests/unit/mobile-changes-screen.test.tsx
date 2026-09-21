/**
 * Mobile ChangesScreen: verify status badge rendering against the canonical
 * diff vocabulary in src/lib/status.ts.
 */
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import ChangesScreen from "../../src/mobile/screens/ChangesScreen";
import type { AgentNode, GitStatusEntry } from "../../src/mobile/api";

const node: AgentNode = {
  id: 42,
  mesh_id: 1,
  name: "test-node",
  path: "/tmp/worktree",
  branch: "feature-branch",
  provider: "anthropic",
  status: "running",
  cli_session_id: null,
  created_at: "2026-06-11T00:00:00Z",
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("mobile ChangesScreen status badges", () => {
  it("renders canonical diff badge letters and colors for each status variant", async () => {
    const fakeEntries: GitStatusEntry[] = [
      { path: "added.ts", status: "added", additions: 10, deletions: 0 },
      { path: "modified.ts", status: "modified", additions: 5, deletions: 2 },
      { path: "deleted.ts", status: "deleted", additions: 0, deletions: 8 },
      { path: "renamed.ts", status: "renamed", additions: 1, deletions: 1 },
      { path: "untracked.ts", status: "untracked", additions: 20, deletions: 0 },
    ];

    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation(async (url: string) => {
        if (url.includes("/git/status")) {
          return {
            ok: true,
            status: 200,
            json: async () => fakeEntries,
          };
        }
        if (url.includes("/git/summary")) {
          return {
            ok: true,
            status: 200,
            json: async () => ({ added: 1, modified: 1, deleted: 1 }),
          };
        }
        if (url.includes("/git/branch")) {
          return {
            ok: true,
            status: 200,
            json: async () => ({ branch: "feature-branch" }),
          };
        }
        if (url.includes("/api/gh/auth")) {
          return {
            ok: true,
            status: 200,
            json: async () => ({ authenticated: true }),
          };
        }
        return { ok: true, status: 200, json: async () => ({}) };
      }),
    );

    render(
      <ChangesScreen
        node={node}
        onBack={() => {}}
        onOpenDiff={() => {}}
        onOpenPr={() => {}}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("changes-file-added.ts")).toBeTruthy();
    });

    const addedButton = screen.getByTestId("changes-file-added.ts");
    const addedBadge = addedButton.querySelector("span");
    expect(addedBadge?.textContent).toBe("A");
    expect(addedBadge?.style.color).toBe("rgb(34, 197, 94)"); // #22c55e

    const modifiedButton = screen.getByTestId("changes-file-modified.ts");
    const modifiedBadge = modifiedButton.querySelector("span");
    expect(modifiedBadge?.textContent).toBe("M");
    expect(modifiedBadge?.style.color).toBe("rgb(245, 158, 11)"); // #f59e0b

    const deletedButton = screen.getByTestId("changes-file-deleted.ts");
    const deletedBadge = deletedButton.querySelector("span");
    expect(deletedBadge?.textContent).toBe("D");
    expect(deletedBadge?.style.color).toBe("rgb(239, 68, 68)"); // #ef4444

    const renamedButton = screen.getByTestId("changes-file-renamed.ts");
    const renamedBadge = renamedButton.querySelector("span");
    expect(renamedBadge?.textContent).toBe("R");
    expect(renamedBadge?.style.color).toBe("rgb(139, 92, 246)"); // #8b5cf6

    const untrackedButton = screen.getByTestId("changes-file-untracked.ts");
    const untrackedBadge = untrackedButton.querySelector("span");
    // Crucial check: untracked must render "?" and muted grey, NOT "M" and amber
    expect(untrackedBadge?.textContent).toBe("?");
    expect(untrackedBadge?.style.color).toBe("rgb(122, 132, 146)"); // #7a8492
  });
});
