/**
 * Mobile SPA → backend wire shape (issue #1262).
 *
 * The mobile SPA's API helpers (`createPr`, `createNode`, `importAndResume`,
 * `spawnFromIssue`) POST JSON bodies to fixed routes on the Rust backend.
 * Both sides are type-checked via `ts-rs` (issue #359), but a body field can
 * still be silently renamed (key collision vs. snake_case↔camelCase drift)
 * without a TS error — the request would 400 at runtime and surface to the
 * user as a generic "Request failed" message.
 *
 * Each test below pins the exact wire shape sent by one helper: the URL
 * path, the HTTP method, and the JSON body (post-stringification). If a
 * future refactor renames `base_branch` → `baseBranch` (or any other field),
 * these tests fail loudly, the same way they would in production.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  createPr,
  createNode,
  importAndResume,
  spawnFromIssue,
  type AgentNode,
  type ArchivedAgentNode,
  type GitHubIssue,
} from "../../src/mobile/api";

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

const archived: ArchivedAgentNode = {
  session_id: "session-abc",
  first_message: "Recover auth flow",
  branch: "fix/auth",
  cwd: "/tmp/repo",
  timestamp: "2026-06-11T00:00:00Z",
  worktree_name: "fix-auth-flow",
};

const issue: GitHubIssue = {
  number: 42,
  title: "Add dark mode",
  body: "Please support dark themes.",
  url: "https://github.com/alondero/buildmesh/issues/42",
  state: "open",
  labels: ["enhancement"],
  blocked_by: [],
};

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function okJson(body: unknown): Response {
  return jsonResponse(200, body);
}

describe("mobile api wire shape (issue #1262)", () => {
  let fetchMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  describe("createPr", () => {
    it("posts {title, body, base_branch} to /api/meshes/:id/pr", async () => {
      fetchMock.mockResolvedValue(okJson({ url: "https://github.com/x/y/pull/1" }));

      const { url } = await createPr(1, "Add dark mode", "Please.", "main");

      expect(url).toBe("https://github.com/x/y/pull/1");
      const [path, init] = fetchMock.mock.calls[0];
      expect(path).toBe("/api/meshes/1/pr");
      expect(init.method).toBe("POST");
      expect(init.credentials).toBe("include");
      expect(init.headers["Content-Type"]).toBe("application/json");
      expect(JSON.parse(init.body as string)).toEqual({
        title: "Add dark mode",
        body: "Please.",
        // snake_case — must match the Rust `CreatePrRequest` struct.
        base_branch: "main",
      });
    });

    it("round-trips an empty body string", async () => {
      fetchMock.mockResolvedValue(okJson({ url: "x" }));
      await createPr(1, "title", "", "main");
      expect(JSON.parse(fetchMock.mock.calls[0][1].body as string).body).toBe("");
    });
  });

  describe("createNode", () => {
    it("defaults rows=24, cols=80 when omitted and POSTs to /api/nodes/create", async () => {
      fetchMock.mockResolvedValue(okJson(node));

      const out = await createNode({ mesh_id: 1, provider: "anthropic" });

      expect(out).toEqual(node);
      const [path, init] = fetchMock.mock.calls[0];
      expect(path).toBe("/api/nodes/create");
      expect(init.method).toBe("POST");
      expect(JSON.parse(init.body as string)).toEqual({
        rows: 24,
        cols: 80,
        mesh_id: 1,
        provider: "anthropic",
      });
    });

    it("honours explicit rows/cols overrides", async () => {
      fetchMock.mockResolvedValue(okJson(node));
      await createNode({ mesh_id: 1, provider: "anthropic", rows: 40, cols: 120 });
      expect(JSON.parse(fetchMock.mock.calls[0][1].body as string)).toEqual({
        rows: 40,
        cols: 120,
        mesh_id: 1,
        provider: "anthropic",
      });
    });
  });

  describe("importAndResume", () => {
    it("maps session.session_id → cli_session_id and POSTs to the import-and-resume route", async () => {
      fetchMock.mockResolvedValue(okJson(node));

      const out = await importAndResume(1, archived, "anthropic");

      expect(out).toEqual(node);
      const [path, init] = fetchMock.mock.calls[0];
      expect(path).toBe("/api/meshes/1/agent-nodes/import-and-resume");
      expect(init.method).toBe("POST");
      expect(JSON.parse(init.body as string)).toEqual({
        // The wire key is `cli_session_id` (backend contract); the source
        // field on the mobile side is `session_id` (ts-rs naming, issue
        // #359). Renaming either side silently breaks resume.
        cli_session_id: "session-abc",
        branch: "fix/auth",
        worktree_name: "fix-auth-flow",
        cwd: "/tmp/repo",
        provider: "anthropic",
      });
    });

    it("falls back to branch='main' when the session has no branch", async () => {
      fetchMock.mockResolvedValue(okJson(node));
      // `ArchivedAgentNode.branch` is `string | null` (generated from the
      // Rust struct, issue #359), not `string | undefined` — the
      // `?? "main"` fallback handles both shapes.
      const noBranch: ArchivedAgentNode = { ...archived, branch: null };
      await importAndResume(1, noBranch);
      const body = JSON.parse(fetchMock.mock.calls[0][1].body as string);
      expect(body.branch).toBe("main");
    });

    it("drops provider when undefined so the backend uses its default", async () => {
      fetchMock.mockResolvedValue(okJson(node));
      await importAndResume(1, archived);
      const body = JSON.parse(fetchMock.mock.calls[0][1].body as string);
      // `provider` is intentionally undefined → JSON.stringify omits the key
      // so the backend sees a "no override" body rather than
      // `{provider: null}` and falls back to its default harness.
      expect("provider" in body).toBe(false);
    });

    it("surfaces a 207 partial response as a thrown 'Imported but spawn failed' Error", async () => {
      const partial = {
        node: { ...node, id: 99 },
        spawn_error: "agent binary missing",
      };
      // The 207 branch lives in `importAndResume` itself (api.ts:299-304),
      // bypassing `apiFetch`'s !resp.ok throw so the caller can distinguish
      // "node created, spawn failed" from "node never created".
      fetchMock.mockResolvedValue(jsonResponse(207, partial));

      await expect(importAndResume(1, archived)).rejects.toThrow(
        /Imported but spawn failed: agent binary missing \(node 99\)/,
      );
    });
  });

  describe("spawnFromIssue", () => {
    it("POSTs only the issue title hint and the optional provider to /api/meshes/:id/issues/:n/spawn", async () => {
      fetchMock.mockResolvedValue(okJson(node));

      const out = await spawnFromIssue(1, issue, "anthropic");

      expect(out).toEqual(node);
      const [path, init] = fetchMock.mock.calls[0];
      expect(path).toBe("/api/meshes/1/issues/42/spawn");
      expect(init.method).toBe("POST");
      // Backend derives the issue URL from the mesh's `origin` remote — we
      // only ship the title hint, not the body (avoids pushing a multi-KB
      // markdown blob through the Windows PowerShell -EncodedCommand argv
      // path). Renaming `title` would break the contract.
      expect(JSON.parse(init.body as string)).toEqual({
        title: "Add dark mode",
        provider: "anthropic",
      });
    });

    it("drops provider when undefined", async () => {
      fetchMock.mockResolvedValue(okJson(node));
      await spawnFromIssue(1, issue);
      const body = JSON.parse(fetchMock.mock.calls[0][1].body as string);
      expect(body).toEqual({ title: "Add dark mode" });
    });
  });
});
