import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";

// Execute the exact JavaScript shipped into the harness's plugin directory.
const source = readFileSync(resolve("src-tauri/src/agent/provider/adapters/opencode_attention_plugin.js"), "utf8");
const load = new Function(source.replace("export const BuildmeshAttention", "const BuildmeshAttention") + ";return BuildmeshAttention;");

afterEach(() => { vi.unstubAllGlobals(); vi.unstubAllEnvs(); });

describe("OpenCode attention delivery", () => {
  it("forwards completion, permission and question as distinct events", async () => {
    vi.stubEnv("BUILDMESH_PORT", "2992");
    vi.stubEnv("BUILDMESH_SESSION_ID", "42");
    const fetch = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal("fetch", fetch);
    const plugin = await load()();
    for (const type of ["session.idle", "permission.asked", "question.asked"]) {
      await plugin.event({ event: { type, properties: { sessionID: "ses_Root", questions: [{ question: "Which branch?" }] } } });
    }
    expect(fetch.mock.calls.map(([url, init]) => [url, JSON.parse(init.body).hook_event_name]))
      .toEqual(["session.idle", "permission.asked", "question.asked"].map(type => ["http://localhost:2992/api/attention/42", type]));
    expect(JSON.parse(fetch.mock.calls[2][1].body)).toEqual({ hook_event_name: "question.asked", sessionID: "ses_Root", message: "Which branch?" });
  });

  it("does not let subagents steal the root session or emit its completion", async () => {
    vi.stubEnv("BUILDMESH_PORT", "2992");
    vi.stubEnv("BUILDMESH_SESSION_ID", "42");
    const fetch = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal("fetch", fetch);
    const plugin = await load()();
    await plugin.event({ event: { type: "session.created", properties: { info: { id: "ses_Root" } } } });
    await plugin.event({ event: { type: "session.created", properties: { info: { id: "ses_Child", parentID: "ses_Root" } } } });
    await plugin.event({ event: { type: "session.idle", properties: { sessionID: "ses_Child" } } });
    await plugin.event({ event: { type: "session.idle" } });
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(JSON.parse(fetch.mock.calls[1][1].body).sessionID).toBe("ses_Root");
  });

  it("delivers a resumed turn without requiring a session.created callback", async () => {
    vi.stubEnv("BUILDMESH_PORT", "2992");
    vi.stubEnv("BUILDMESH_SESSION_ID", "42");
    const fetch = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal("fetch", fetch);
    const plugin = await load()();
    await plugin.event({ event: { type: "question.asked" } });
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(JSON.parse(fetch.mock.calls[0][1].body).hook_event_name).toBe("question.asked");
  });

  it("preserves OpenCode permission ids across ask and reply", async () => {
    vi.stubEnv("BUILDMESH_PORT", "2992");
    vi.stubEnv("BUILDMESH_SESSION_ID", "42");
    const fetch = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal("fetch", fetch);
    const plugin = await load()();

    await plugin.event({ event: {
      type: "permission.asked",
      properties: { sessionID: "ses_Root", id: "perm-1", tool: { name: "Bash" } },
    } });
    await plugin.event({ event: {
      type: "permission.replied",
      properties: { sessionID: "ses_Root", requestID: "perm-1", reply: "once" },
    } });

    expect(JSON.parse(fetch.mock.calls[0][1].body)).toMatchObject({
      hook_event_name: "permission.asked",
      request_id: "perm-1",
      tool_name: "Bash",
    });
    expect(JSON.parse(fetch.mock.calls[1][1].body)).toMatchObject({
      hook_event_name: "permission.replied",
      request_id: "perm-1",
    });
  });
});
