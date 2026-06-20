/**
 * Mobile DiffScreen: hunks render with @@ headers so consecutive hunks
 * don't run together as one misleading block, and nodes sort newest
 * first in the archive screen helper.
 */
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import DiffScreen from "../../src/mobile/screens/DiffScreen";
import { sortAgentNodes } from "../../src/mobile/screens/ArchivedNodesScreen";
import type { AgentNode, DiffResult, ArchivedAgentNode } from "../../src/mobile/api";

const node: AgentNode = {
  id: 5,
  mesh_id: 1,
  name: "n",
  path: "/tmp",
  branch: null,
  provider: "anthropic",
  status: "running",
  cli_session_id: null,
  created_at: "2026-06-11T00:00:00Z",
};

const twoHunks: DiffResult = {
  files: [
    {
      path: "src/a.ts",
      hunks: [
        {
          old_start: 1,
          old_lines: 2,
          new_start: 1,
          new_lines: 3,
          old_highlighted: "",
          new_highlighted: "",
          lines: [
            { line_type: "context", content: "const a = 1;", old_line_no: 1, new_line_no: 1 },
            { line_type: "add", content: "const b = 2;", old_line_no: null, new_line_no: 2 },
          ],
        },
        {
          old_start: 40,
          old_lines: 1,
          new_start: 41,
          new_lines: 1,
          old_highlighted: "",
          new_highlighted: "",
          lines: [
            { line_type: "remove", content: "old()", old_line_no: 40, new_line_no: null },
          ],
        },
      ],
    },
  ],
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("DiffScreen", () => {
  it("renders one @@ header per hunk", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: true,
        status: 200,
        json: async () => twoHunks,
      }),
    );

    render(<DiffScreen node={node} filePath="src/a.ts" onBack={() => {}} />);

    await waitFor(() => {
      expect(screen.getByTestId("diff-body")).toBeTruthy();
    });
    const headers = screen.getAllByTestId("hunk-header");
    expect(headers).toHaveLength(2);
    expect(headers[0].textContent).toContain("@@ -1,2 +1,3");
    expect(headers[1].textContent).toContain("@@ -40,1 +41,1");
    expect(screen.getByTestId("diff-body").textContent).toContain("const b = 2;");
  });

  it("shows the empty note when the file matches HEAD", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: true,
        status: 200,
        json: async () => ({ files: [] }),
      }),
    );

    render(<DiffScreen node={node} filePath="src/a.ts" onBack={() => {}} />);

    await waitFor(() => {
      expect(screen.getByTestId("diff-empty")).toBeTruthy();
    });
  });
});

describe("sortAgentNodes", () => {
  it("sorts newest first, nodes without a timestamp last", () => {
    // Fields match the generated `ArchivedAgentNode` (issue #359; renamed from
    // DiscoveredAgentNode after PR #523): the sort key is `timestamp`, and
    // nodes are identified by `session_id` — not the phantom
    // `last_active_at`/`cli_session_id` the mobile type used to declare
    // (which the wire never sends).
    const s = (id: string, ts: string | null): ArchivedAgentNode => ({
      session_id: id,
      first_message: "",
      branch: null,
      cwd: null,
      timestamp: ts,
      worktree_name: null,
    });
    const sorted = sortAgentNodes([
      s("old", "2026-06-01T00:00:00Z"),
      s("none", null),
      s("new", "2026-06-10T00:00:00Z"),
    ]);
    expect(sorted.map((x) => x.session_id)).toEqual(["new", "old", "none"]);
  });
});
