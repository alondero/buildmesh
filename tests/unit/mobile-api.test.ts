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
  listProviders,
  getUsage,
  newOperationKey,
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
  // Issue #2332 — discovery stamps the originating harness on every entry.
  resumable: true,
  provider: "anthropic",
  node_id: null,
};

const issue: GitHubIssue = {
  number: 42,
  title: "Add dark mode",
  body: "Please support dark themes.",
  url: "https://github.com/alondero/buildmesh/issues/42",
  state: "open",
  labels: ["enhancement"],
  blocked_by: [],
  author: "alondero",
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
    it("posts the node identity and the pinned head to /api/meshes/:id/pr", async () => {
      // Issue #2024 rank 4 / #1567: `node_id` is REQUIRED. The old mesh-only
      // request could only resolve the mesh root, so it published the root's
      // branch — `main -> main`, or an unrelated feature branch.
      fetchMock.mockResolvedValue(
        okJson({
          url: "https://github.com/x/y/pull/1",
          head_branch: "agent/fix-x",
          base_branch: "main",
        }),
      );

      const result = await createPr(1, 7, "Add dark mode", "Please.", "main", "agent/fix-x");

      expect(result.url).toBe("https://github.com/x/y/pull/1");
      // The response echoes the branches actually used, so the sheet can show
      // what was created rather than what it guessed.
      expect(result.head_branch).toBe("agent/fix-x");
      expect(result.base_branch).toBe("main");
      const [path, init] = fetchMock.mock.calls[0];
      expect(path).toBe("/api/meshes/1/pr");
      expect(init.method).toBe("POST");
      expect(init.credentials).toBe("include");
      expect(init.headers["Content-Type"]).toBe("application/json");
      expect(JSON.parse(init.body as string)).toEqual({
        title: "Add dark mode",
        body: "Please.",
        // snake_case — must match the Rust `CreatePrRequest` struct.
        node_id: 7,
        base_branch: "main",
        head_branch: "agent/fix-x",
      });
    });

    it("omits base_branch and head_branch when they are not pinned", async () => {
      // Omitting the base lets the mesh's own `base_ref` decide it, so the
      // client never has to assume `main`.
      fetchMock.mockResolvedValue(
        okJson({ url: "x", head_branch: "a", base_branch: "trunk" }),
      );
      await createPr(1, 7, "title", "");
      expect(JSON.parse(fetchMock.mock.calls[0][1].body as string)).toEqual({
        title: "title",
        body: "",
        node_id: 7,
      });
    });

    it("round-trips an empty body string", async () => {
      fetchMock.mockResolvedValue(okJson({ url: "x" }));
      await createPr(1, 7, "title", "", "main");
      expect(JSON.parse(fetchMock.mock.calls[0][1].body as string).body).toBe("");
    });
  });

  describe("listProviders", () => {
    it("forwards an abort signal so a surface can cancel the read", async () => {
      // The Spawn Menu read is secondary: a surface unmounting mid-read must
      // be able to cancel it rather than resolve into a gone component.
      fetchMock.mockResolvedValue(okJson([]));
      const controller = new AbortController();

      await listProviders(controller.signal);

      const [path, init] = fetchMock.mock.calls[0];
      expect(path).toBe("/api/providers");
      expect(init.signal).toBe(controller.signal);
    });
  });

  describe("getUsage", () => {
    it("uses the paired usage route and forwards refresh and cancellation", async () => {
      const controller = new AbortController();
      fetchMock.mockResolvedValue(okJson([]));
      expect(await getUsage(true, controller.signal)).toEqual([]);
      expect(fetchMock.mock.calls[0][0]).toBe('/api/usage?refresh=true');
      expect(fetchMock.mock.calls[0][1].signal).toBe(controller.signal);
    });
    it("reports an older desktop's unknown-route response", async () => {
      fetchMock.mockResolvedValue(okJson({ error: 'not found' }));
      await expect(getUsage()).rejects.toThrow('updated desktop');
    });
  });

  describe("createNode", () => {
    // Issue #2331: the route answers a typed outcome, not a bare node, because
    // the server may commit the node row and then fail to start the process.
    const activeOutcome = { outcome: "active", node, operation_key: "op-1" };

    it("defaults rows=24, cols=80 when omitted and POSTs to /api/nodes/create", async () => {
      fetchMock.mockResolvedValue(okJson(activeOutcome));

      const out = await createNode({ mesh_id: 1, provider: "anthropic" });

      expect(out).toEqual(activeOutcome);
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
      fetchMock.mockResolvedValue(okJson(activeOutcome));
      await createNode({ mesh_id: 1, provider: "anthropic", rows: 40, cols: 120 });
      expect(JSON.parse(fetchMock.mock.calls[0][1].body as string)).toEqual({
        rows: 40,
        cols: 120,
        mesh_id: 1,
        provider: "anthropic",
      });
    });

    it("forwards the operation key so a retry resolves to the same node", async () => {
      fetchMock.mockResolvedValue(okJson(activeOutcome));
      await createNode({ mesh_id: 1, provider: "anthropic", operation_key: "op-1" });
      expect(JSON.parse(fetchMock.mock.calls[0][1].body as string).operation_key).toBe("op-1");
    });

    it("surfaces a committed-but-unstarted outcome with its node", async () => {
      // The headline contract: the client must be able to tell "committed but
      // not started" from "nothing created" and reach the node either way.
      fetchMock.mockResolvedValue(
        okJson({
          outcome: "start_failed",
          node,
          operation_key: "op-1",
          start_error: "claude CLI not found on PATH",
        }),
      );

      const out = await createNode({ mesh_id: 1, provider: "anthropic" });

      expect(out.outcome).toBe("start_failed");
      expect(out.outcome === "start_failed" && out.node.id).toBe(node.id);
      expect(out.outcome === "start_failed" && out.start_error).toBe(
        "claude CLI not found on PATH",
      );
    });
  });

  describe("newOperationKey", () => {
    it("mints a distinct key per submission and reuses crypto.randomUUID when present", () => {
      const first = newOperationKey();
      const second = newOperationKey();
      expect(first).not.toBe("");
      expect(first).not.toBe(second);
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

    it("sends the entry's own harness as the resume executor (issue #2332)", async () => {
      fetchMock.mockResolvedValue(okJson(node));
      // A Cursor conversation, resumed with no explicit override. The caller
      // is the mobile archive list, which cannot know anything the entry does
      // not say — before #2332 this request omitted `provider` and the backend
      // read the omission as "Anthropic", launching Cursor's conversation
      // through Claude Code.
      await importAndResume(1, { ...archived, provider: "cursor" });
      const body = JSON.parse(fetchMock.mock.calls[0][1].body as string);
      expect(body.provider).toBe("cursor");
    });

    it("lets an explicit provider argument override the entry's harness", async () => {
      fetchMock.mockResolvedValue(okJson(node));
      // The desktop picker's deliberate executor change reaches the same
      // helper, so the override has to keep winning.
      await importAndResume(1, { ...archived, provider: "cursor" }, "agy");
      const body = JSON.parse(fetchMock.mock.calls[0][1].body as string);
      expect(body.provider).toBe("agy");
    });

    it("omits provider only when the entry itself has no harness", async () => {
      fetchMock.mockResolvedValue(okJson(node));
      await importAndResume(1, { ...archived, provider: null });
      const body = JSON.parse(fetchMock.mock.calls[0][1].body as string);
      // `undefined` → `JSON.stringify` omits the key. The backend refuses an
      // absent provider with an actionable 400 rather than defaulting to
      // Anthropic, so omission is honest here, not a silent guess (#2332).
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

      const out = await spawnFromIssue(1, issue, {
        id: "anthropic",
        label: "Anthropic",
        provider: "anthropic",
      });

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
