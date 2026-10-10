/**
 * Mobile DiffScreen: hunks render with @@ headers so consecutive hunks
 * don't run together as one misleading block, and nodes sort newest
 * first in the archive screen helper.
 *
 * Issue #2024 rank 10: a hunk-less diff is NOT automatically "no changes".
 * The mobile client used to hand-declare `FileDiff` without `binary`,
 * `status`, `old_path` or the line counters, so a modified PNG, a
 * rename-only change and a mode-only change all rendered the same
 * "No diff (file matches HEAD)" note — a false claim about files that
 * demonstrably changed. The fixtures below are the real generated wire
 * shape (issue #359: never hand-mirror a Rust type).
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

/** A `FileDiff` with every wire field the generated type declares, so a
 *  future fixture edit can't silently drop the field under test. */
function fileDiff(overrides: Partial<DiffResult["files"][number]> = {}) {
  return {
    path: "src/a.ts",
    hunks: [],
    status: "modified",
    old_path: null,
    additions: 0,
    deletions: 0,
    binary: false,
    ...overrides,
  };
}

const twoHunks: DiffResult = {
  files: [
    {
      ...fileDiff({ additions: 2, deletions: 1 }),
      hunks: [
        {
          old_start: 1,
          old_lines: 2,
          new_start: 1,
          new_lines: 3,
          old_highlighted: "",
          new_highlighted: "",
          lines: [
            { line_type: "context", content: "const a = 1;", old_num: 1, new_num: 1 },
            { line_type: "add", content: "const b = 2;", old_num: null, new_num: 2 },
          ],
          lines_highlighted: [],
        },
        {
          old_start: 40,
          old_lines: 1,
          new_start: 41,
          new_lines: 1,
          old_highlighted: "",
          new_highlighted: "",
          lines: [
            { line_type: "remove", content: "old()", old_num: 40, new_num: null },
          ],
          lines_highlighted: [],
        },
      ],
    },
  ],
};

/** Stub the mobile HTTP layer with one diff payload. */
function stubDiff(diff: DiffResult) {
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => diff,
    }),
  );
}

async function renderEmptyNote(diff: DiffResult) {
  stubDiff(diff);
  render(<DiffScreen node={node} filePath="src/a.ts" onBack={() => {}} />);
  await waitFor(() => {
    expect(screen.getByTestId("diff-empty")).toBeTruthy();
  });
  return screen.getByTestId("diff-empty").textContent ?? "";
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("DiffScreen", () => {
  it("renders one @@ header per hunk", async () => {
    stubDiff(twoHunks);

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
    stubDiff({ files: [] });

    render(<DiffScreen node={node} filePath="src/a.ts" onBack={() => {}} />);

    await waitFor(() => {
      expect(screen.getByTestId("diff-empty")).toBeTruthy();
    });
  });
});

describe("DiffScreen — hunk-less diffs are not all 'no changes' (issue #2024 rank 10)", () => {
  it("announces a modified binary file instead of claiming the file matches HEAD", async () => {
    // A changed PNG: git reports the delta, libgit2 flags it binary, and
    // there is never a text hunk. Pre-fix this rendered "No diff (file
    // matches HEAD)" for a file that did change.
    const note = await renderEmptyNote({
      files: [fileDiff({ path: "assets/logo.png", status: "modified", binary: true })],
    });
    expect(note).toMatch(/binary/i);
    expect(note).not.toMatch(/matches HEAD/i);
  });

  it("announces a rename whose contents are unchanged", async () => {
    const note = await renderEmptyNote({
      files: [
        fileDiff({
          path: "src/new-name.ts",
          status: "renamed",
          old_path: "src/old-name.ts",
        }),
      ],
    });
    expect(note).toContain("src/old-name.ts");
    expect(note).toMatch(/renamed/i);
    expect(note).not.toMatch(/matches HEAD/i);
  });

  it("announces a metadata-only change", async () => {
    // A chmod +x or a symlink retarget: the delta is real, the text is not.
    const note = await renderEmptyNote({
      files: [fileDiff({ status: "modified", binary: false })],
    });
    expect(note).toMatch(/metadata only/i);
    expect(note).not.toMatch(/matches HEAD/i);
  });

  it("still reports a genuinely unchanged file as unchanged", async () => {
    const note = await renderEmptyNote({ files: [] });
    expect(note).toMatch(/no changes/i);
  });

  it("states the branch-point baseline rather than HEAD", async () => {
    // `diff_node_file_against_base` diffs against the node's merge base with
    // its mesh `base_ref` (ADR 0005), so the copy must not name HEAD.
    const note = await renderEmptyNote({ files: [] });
    expect(note).not.toMatch(/HEAD/);
  });
});

describe("sortAgentNodes", () => {
  it("sorts newest first, nodes without a timestamp last", () => {
    // Fields match the generated `ArchivedAgentNode` (issue #359; renamed from
    // DiscoveredAgentNode after PR #523): the sort key is `timestamp`, and
    // nodes are identified by `session_id` — not the phantom
    // `last_active_at`/`cli_session_id` the mobile type used to declare
    // (which the wire never sends). `resumable` (issue #1065) says whether a
    // discoverable transcript exists; these rows have one.
    const s = (id: string, ts: string | null): ArchivedAgentNode => ({
      session_id: id,
      first_message: "",
      branch: null,
      cwd: null,
      timestamp: ts,
      worktree_name: null,
      resumable: true,
    });
    const sorted = sortAgentNodes([
      s("old", "2026-06-01T00:00:00Z"),
      s("none", null),
      s("new", "2026-06-10T00:00:00Z"),
    ]);
    expect(sorted.map((x) => x.session_id)).toEqual(["new", "old", "none"]);
  });
});