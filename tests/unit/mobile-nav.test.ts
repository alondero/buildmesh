/**
 * Mobile SPA navigation hierarchy.
 *
 * The OS back gesture / Android back button is wired through popstate, and
 * the handler derives the previous screen from `parentOf` rather than stored
 * history payloads. These tests lock the hierarchy: a terminal entered
 * laterally (e.g. from the sessions screen after a resume) must still back
 * out to the node list, and diff → changes → terminal → list must unwind in
 * order.
 */
import { describe, it, expect } from "vitest";
import { parentOf, type Screen } from "../../src/mobile/App";
import type { AgentNode, Mesh } from "../../src/mobile/api";

const node: AgentNode = {
  id: 7,
  mesh_id: 1,
  name: "fix-auth-flow",
  path: "/tmp/wt",
  branch: "fix/auth",
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

describe("parentOf", () => {
  it("terminal backs out to the list", () => {
    expect(
      parentOf({ kind: "terminal", node, visitId: 1 }),
    ).toEqual({ kind: "list" });
  });

  it("sessions and issues back out to the list", () => {
    expect(parentOf({ kind: "sessions", mesh })).toEqual({ kind: "list" });
    expect(parentOf({ kind: "issues", mesh })).toEqual({ kind: "list" });
  });

  it("changes backs out to the same node's terminal", () => {
    expect(parentOf({ kind: "changes", node, visitId: 1 })).toEqual({
      kind: "terminal",
      node,
      visitId: 1,
    });
  });

  it("diff backs out to the same node's changes screen", () => {
    expect(
      parentOf({ kind: "diff", node, filePath: "src/a.ts", visitId: 1 }),
    ).toEqual({
      kind: "changes",
      node,
      visitId: 1,
    });
  });

  it("list and connect are roots (back leaves the app)", () => {
    expect(parentOf({ kind: "list" })).toEqual({ kind: "list" });
    expect(parentOf({ kind: "connect" })).toEqual({ kind: "connect" });
  });

  it("a full diff → list unwind terminates", () => {
    let s: Screen = { kind: "diff", node, filePath: "src/a.ts", visitId: 1 };
    const seen: string[] = [s.kind];
    for (let i = 0; i < 10 && s.kind !== "list"; i++) {
      s = parentOf(s);
      seen.push(s.kind);
    }
    expect(seen).toEqual(["diff", "changes", "terminal", "list"]);
  });
});


it("returns from direct changes to details, and through terminal when opened there", () => {
  const direct: Screen = {
    kind: "diff",
    node,
    filePath: "a.ts",
    fromOverview: true,
    visitId: 2,
  };
  expect(parentOf(parentOf(direct))).toEqual({ kind: "overview", node, visitId: 2 });
  const viaTerminal: Screen = {
    kind: "diff",
    node,
    filePath: "a.ts",
    terminalFromOverview: true,
    visitId: 3,
  };
  expect(parentOf(parentOf(viaTerminal))).toEqual({
    kind: "terminal",
    node,
    fromOverview: true,
    visitId: 3,
  });
  expect(parentOf(parentOf(parentOf(viaTerminal)))).toEqual({
    kind: "overview",
    node,
    visitId: 3,
  });
});

it("preserves the agent request and reply draft while backing out of work screens", () => {
  const detail = {
    kind: "overview",
    node,
    visitId: 4,
    prompt: "Please fix the preview deploy.",
    draft: "I will check the build logs.",
    replySending: true,
    replyNotice: "",
  } satisfies Screen;
  const directDiff: Screen = {
    kind: "diff",
    node,
    filePath: "src/deploy.ts",
    fromOverview: true,
    visitId: detail.visitId,
    prompt: detail.prompt,
    draft: detail.draft,
    replySending: detail.replySending,
    replyNotice: detail.replyNotice,
  };
  expect(parentOf(parentOf(directDiff))).toEqual(detail);

  const terminalDiff: Screen = {
    kind: "diff",
    node,
    filePath: "src/deploy.ts",
    terminalFromOverview: true,
    visitId: detail.visitId,
    prompt: detail.prompt,
    draft: detail.draft,
    replySending: detail.replySending,
    replyNotice: detail.replyNotice,
  };
  expect(parentOf(parentOf(parentOf(terminalDiff)))).toEqual(detail);
});
