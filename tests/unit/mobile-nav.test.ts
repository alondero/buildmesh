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
};

describe("parentOf", () => {
  it("terminal backs out to the list", () => {
    expect(parentOf({ kind: "terminal", node })).toEqual({ kind: "list" });
  });

  it("sessions and issues back out to the list", () => {
    expect(parentOf({ kind: "sessions", mesh })).toEqual({ kind: "list" });
    expect(parentOf({ kind: "issues", mesh })).toEqual({ kind: "list" });
  });

  it("changes backs out to the same node's terminal", () => {
    expect(parentOf({ kind: "changes", node })).toEqual({
      kind: "terminal",
      node,
    });
  });

  it("diff backs out to the same node's changes screen", () => {
    expect(parentOf({ kind: "diff", node, filePath: "src/a.ts" })).toEqual({
      kind: "changes",
      node,
    });
  });

  it("list and connect are roots (back leaves the app)", () => {
    expect(parentOf({ kind: "list" })).toEqual({ kind: "list" });
    expect(parentOf({ kind: "connect" })).toEqual({ kind: "connect" });
  });

  it("a full diff → list unwind terminates", () => {
    let s: Screen = { kind: "diff", node, filePath: "src/a.ts" };
    const seen: string[] = [s.kind];
    for (let i = 0; i < 10 && s.kind !== "list"; i++) {
      s = parentOf(s);
      seen.push(s.kind);
    }
    expect(seen).toEqual(["diff", "changes", "terminal", "list"]);
  });
});
