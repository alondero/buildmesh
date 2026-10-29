import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";

// Execute the exact JavaScript shipped into the harness's plugin directory.
const source = readFileSync(resolve("src-tauri/src/agent/provider/adapters/opencode_attention_plugin.js"), "utf8");
const load = new Function(source.replace("export const BuildmeshAttention", "const BuildmeshAttention") + ";return BuildmeshAttention;");

afterEach(() => { vi.unstubAllGlobals(); vi.unstubAllEnvs(); vi.useRealTimers(); });

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
    const sent = fetch.mock.calls.map(([, init]) => JSON.parse(init.body));
    // The child is reported as owned work, never as the node's own lifecycle.
    expect(sent.map(body => body.hook_event_name))
      .toEqual(["session.created", "subagent_start", "subagent_stop", "session.idle"]);
    expect(sent.filter(body => body.hook_event_name.startsWith("session.")).map(body => body.sessionID))
      .toEqual(["ses_Root", "ses_Root"]);
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

  it("posts a permission ask after the hold, under the permission id", async () => {
    vi.useFakeTimers();
    vi.stubEnv("BUILDMESH_PORT", "2992");
    vi.stubEnv("BUILDMESH_SESSION_ID", "42");
    const fetch = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal("fetch", fetch);
    const plugin = await load()();

    await plugin.event({ event: {
      id: "evt_ask",
      type: "permission.asked",
      properties: {
        id: "per_external",
        sessionID: "ses_Root",
        permission: "external_directory",
        patterns: ["F:\\tmp\\*"],
        tool: { messageID: "msg_1", callID: "call_1" },
      },
    }});
    expect(fetch).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(750);

    const asked = JSON.parse(fetch.mock.calls[0][1].body);
    expect(asked.request_id).toBe("per_external");
    expect(asked.message).toContain("external_directory");
    expect(asked.message).toContain("F:\\tmp\\*");
    expect(asked.tool_name).toBeUndefined();
  });

  it("cancels a held permission ask when the auto-reply arrives first", async () => {
    vi.useFakeTimers();
    vi.stubEnv("BUILDMESH_PORT", "2992");
    vi.stubEnv("BUILDMESH_SESSION_ID", "42");
    const fetch = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal("fetch", fetch);
    const plugin = await load()();

    await plugin.event({ event: {
      id: "evt_ask",
      type: "permission.asked",
      properties: { sessionID: "ses_Root", id: "perm-1", tool: { name: "Bash" } },
    }});
    await plugin.event({ event: {
      id: "evt_reply",
      type: "permission.replied",
      properties: { sessionID: "ses_Root", requestID: "perm-1", reply: "once" },
    }});
    await vi.advanceTimersByTimeAsync(750);
    expect(fetch).not.toHaveBeenCalled();
  });

  it("posts the reply when it arrives after the ask was already delivered", async () => {
    vi.useFakeTimers();
    vi.stubEnv("BUILDMESH_PORT", "2992");
    vi.stubEnv("BUILDMESH_SESSION_ID", "42");
    const fetch = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal("fetch", fetch);
    const plugin = await load()();

    await plugin.event({ event: {
      type: "permission.asked",
      properties: { sessionID: "ses_Root", id: "perm-1", tool: { name: "Bash" } },
    }});
    await vi.advanceTimersByTimeAsync(750);
    await plugin.event({ event: {
      type: "permission.replied",
      properties: { sessionID: "ses_Root", requestID: "perm-1", reply: "once" },
    }});

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

  it("forwards the session error's name so a user abort is not a failure", async () => {
    vi.stubEnv("BUILDMESH_PORT", "2992");
    vi.stubEnv("BUILDMESH_SESSION_ID", "42");
    const fetch = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal("fetch", fetch);
    const plugin = await load()();
    await plugin.event({ event: {
      type: "session.error",
      properties: { sessionID: "ses_Root", error: { name: "MessageAbortedError", data: { message: "The operation was aborted." } } },
    }});
    await plugin.event({ event: { type: "session.error", properties: { sessionID: "ses_Root" } } });
    expect(JSON.parse(fetch.mock.calls[0][1].body)).toEqual({
      hook_event_name: "session.error",
      sessionID: "ses_Root",
      error: "MessageAbortedError",
    });
    expect(JSON.parse(fetch.mock.calls[1][1].body)).toEqual({ hook_event_name: "session.error", sessionID: "ses_Root" });
  });

  it("replaces a held ask for the same id instead of posting both", async () => {
    vi.useFakeTimers();
    vi.stubEnv("BUILDMESH_PORT", "2992");
    vi.stubEnv("BUILDMESH_SESSION_ID", "42");
    const fetch = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal("fetch", fetch);
    const plugin = await load()();

    await plugin.event({ event: {
      type: "permission.asked",
      properties: { id: "perm-1", sessionID: "ses_Root", permission: "bash" },
    }});
    await plugin.event({ event: {
      type: "permission.asked",
      properties: { id: "perm-1", sessionID: "ses_Root", permission: "edit" },
    }});
    await vi.advanceTimersByTimeAsync(750);
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(JSON.parse(fetch.mock.calls[0][1].body).message).toContain("edit");
  });
});

// Issue #2636. OpenCode creates a native subagent as a child session
// (`session.created` with `info.parentID`). The plugin reports it to Buildmesh
// as owned work, fenced to the root session, and never as the node's own turn.
describe("OpenCode child sessions as owned work", () => {
  async function start() {
    vi.stubEnv("BUILDMESH_PORT", "2992");
    vi.stubEnv("BUILDMESH_SESSION_ID", "42");
    const fetch = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal("fetch", fetch);
    const plugin = await load()();
    const sent = () => fetch.mock.calls.map(([, init]) => JSON.parse(init.body));
    return { plugin, fetch, sent };
  }
  const created = (id: string, parentID?: string) =>
    ({ event: { type: "session.created", properties: { sessionID: id, info: { id, ...(parentID ? { parentID } : {}) } } } });
  const idle = (sessionID: string) => ({ event: { type: "session.idle", properties: { sessionID } } });

  it("reports a child's start and completion under the root session id", async () => {
    const { plugin, sent } = await start();
    await plugin.event(created("ses_Root"));
    await plugin.event(created("ses_Child", "ses_Root"));
    await plugin.event(idle("ses_Child"));
    expect(sent().slice(1)).toEqual([
      { hook_event_name: "subagent_start", sessionID: "ses_Root", agent_id: "ses_Child" },
      { hook_event_name: "subagent_stop", sessionID: "ses_Root", agent_id: "ses_Child" },
    ]);
  });

  it("does not end a child's work on its error alone, because OpenCode goes idle after every error", async () => {
    // Observed on OpenCode 1.18.3: `session.error`, then `session.status`
    // idle, then `session.idle`. Ending on the error could report still-running
    // work as finished; waiting for the idle cannot.
    const { plugin, sent } = await start();
    await plugin.event(created("ses_Root"));
    await plugin.event(created("ses_Child", "ses_Root"));
    await plugin.event({ event: { type: "session.error", properties: { sessionID: "ses_Child", error: { name: "UnknownError" } } } });
    expect(sent().map(body => body.hook_event_name)).toEqual(["session.created", "subagent_start"]);
    await plugin.event(idle("ses_Child"));
    expect(sent().at(-1)).toEqual({ hook_event_name: "subagent_stop", sessionID: "ses_Root", agent_id: "ses_Child" });
  });

  it("ends a deleted child's work so it cannot stay owned forever", async () => {
    const { plugin, sent } = await start();
    await plugin.event(created("ses_Root"));
    await plugin.event(created("ses_Child", "ses_Root"));
    // Shape observed on OpenCode 1.18.3 when a child session is deleted.
    await plugin.event({ event: { type: "session.deleted", properties: { sessionID: "ses_Child", info: { id: "ses_Child", parentID: "ses_Root" } } } });
    expect(sent().map(body => body.hook_event_name)).toEqual(["session.created", "subagent_start", "subagent_stop"]);
  });

  it("reports each child's end once", async () => {
    const { plugin, sent } = await start();
    await plugin.event(created("ses_Root"));
    await plugin.event(created("ses_Child", "ses_Root"));
    await plugin.event(idle("ses_Child"));
    await plugin.event(idle("ses_Child"));
    await plugin.event({ event: { type: "session.deleted", properties: { sessionID: "ses_Child", info: { id: "ses_Child" } } } });
    expect(sent().filter(body => body.hook_event_name === "subagent_stop")).toHaveLength(1);
  });

  it("does not announce a child that belongs to a different root than this node's", async () => {
    // The TUI can show another top-level session; its children are not this
    // node's work. The route would drop them as a stale session anyway.
    const { plugin, fetch } = await start();
    await plugin.event(created("ses_Root"));
    fetch.mockClear();
    await plugin.event(created("ses_OtherChild", "ses_Other"));
    await plugin.event(idle("ses_OtherChild"));
    expect(fetch).not.toHaveBeenCalled();
  });

  it("delivers a child's stop only after its start, even if events are dispatched without waiting", async () => {
    // A stop that overtook its start would be ignored as an unknown child, and
    // the later start would leave that child open forever.
    vi.stubEnv("BUILDMESH_PORT", "2992");
    vi.stubEnv("BUILDMESH_SESSION_ID", "42");
    let releaseStart!: () => void;
    const startDelivered = new Promise<{ ok: boolean }>(resolve => { releaseStart = () => resolve({ ok: true }); });
    const fetch = vi.fn()
      .mockResolvedValueOnce({ ok: true }) // root session.created
      .mockReturnValueOnce(startDelivered) // the child's start is slow
      .mockResolvedValue({ ok: true });
    vi.stubGlobal("fetch", fetch);
    const plugin = await load()();
    await plugin.event(created("ses_Root"));

    const starting = plugin.event(created("ses_Child", "ses_Root"));
    const stopping = plugin.event(idle("ses_Child"));
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(fetch.mock.calls.map(([, init]) => JSON.parse(init.body).hook_event_name))
      .toEqual(["session.created", "subagent_start"]);

    releaseStart();
    await Promise.all([starting, stopping]);
    expect(fetch.mock.calls.map(([, init]) => JSON.parse(init.body).hook_event_name))
      .toEqual(["session.created", "subagent_start", "subagent_stop"]);
  });

  it("reports each child's start once even if the creation event repeats", async () => {
    const { plugin, sent } = await start();
    await plugin.event(created("ses_Root"));
    await plugin.event(created("ses_Child", "ses_Root"));
    await plugin.event(created("ses_Child", "ses_Root"));
    expect(sent().filter(body => body.hook_event_name === "subagent_start")).toHaveLength(1);
  });

  it("tracks children independently", async () => {
    const { plugin, sent } = await start();
    await plugin.event(created("ses_Root"));
    await plugin.event(created("ses_ChildA", "ses_Root"));
    await plugin.event(created("ses_ChildB", "ses_Root"));
    await plugin.event(idle("ses_ChildB"));
    expect(sent().slice(1).map(body => [body.hook_event_name, body.agent_id])).toEqual([
      ["subagent_start", "ses_ChildA"],
      ["subagent_start", "ses_ChildB"],
      ["subagent_stop", "ses_ChildB"],
    ]);
  });

  it("attributes a nested child to the root session, not to its direct parent", async () => {
    const { plugin, sent } = await start();
    await plugin.event(created("ses_Root"));
    await plugin.event(created("ses_Child", "ses_Root"));
    await plugin.event(created("ses_Grand", "ses_Child"));
    await plugin.event(idle("ses_Grand"));
    expect(sent().slice(2)).toEqual([
      { hook_event_name: "subagent_start", sessionID: "ses_Root", agent_id: "ses_Grand" },
      { hook_event_name: "subagent_stop", sessionID: "ses_Root", agent_id: "ses_Grand" },
    ]);
  });

  it("reports a child of a resumed root the plugin never saw created", async () => {
    // A resumed OpenCode session fires no `session.created` for the root, so
    // the child's `parentID` is the only root identity available. The route
    // still fences it against the node's stored session.
    const { plugin, sent } = await start();
    await plugin.event(created("ses_Child", "ses_Root"));
    await plugin.event(idle("ses_Child"));
    expect(sent()).toEqual([
      { hook_event_name: "subagent_start", sessionID: "ses_Root", agent_id: "ses_Child" },
      { hook_event_name: "subagent_stop", sessionID: "ses_Root", agent_id: "ses_Child" },
    ]);
  });

  it("keeps a child's own status and turn events out of the node's lifecycle", async () => {
    // Without a cached root a child's `session.idle` would otherwise look like
    // the node's own completion.
    const { plugin, sent } = await start();
    await plugin.event(created("ses_Child", "ses_Root"));
    await plugin.event({ event: { type: "session.status", properties: { sessionID: "ses_Child", status: { type: "busy" } } } });
    await plugin.event(idle("ses_Child"));
    expect(sent().map(body => body.hook_event_name)).toEqual(["subagent_start", "subagent_stop"]);
  });

  it("ignores a child whose identity is malformed instead of guessing", async () => {
    const { plugin, fetch } = await start();
    await plugin.event(created("ses_Root"));
    fetch.mockClear();
    await plugin.event(created("not-a-session", "ses_Root"));
    await plugin.event(created("ses_Child", "not-a-session"));
    await plugin.event({ event: { type: "session.created", properties: { info: { parentID: "ses_Root" } } } });
    expect(fetch).not.toHaveBeenCalled();
  });

  it("does not report a child's end for a child it never announced", async () => {
    const { plugin, fetch } = await start();
    await plugin.event(created("ses_Root"));
    fetch.mockClear();
    await plugin.event(idle("ses_Stranger"));
    expect(fetch).not.toHaveBeenCalled();
  });

  // The events below were recorded from a real OpenCode 1.18.3 run in which
  // the `task` tool started the `general` subagent (see the fixture's
  // `source`). Replaying them proves the plugin reads the real payload shapes,
  // not just the shapes these tests would invent.
  describe("a recorded OpenCode 1.18.3 subagent run", () => {
    const recording = JSON.parse(
      readFileSync(resolve("tests/fixtures/opencode/subagent-session-run.json"), "utf8"),
    ) as { root: string; child: string; run: any[]; deleted: any };

    async function replay(events: any[]) {
      const { plugin, sent } = await start();
      for (const event of events) await plugin.event({ event });
      return sent();
    }

    it("reports the real child once at its start and once at its end, under the real parent", async () => {
      const sent = await replay(recording.run);
      const owned = sent.filter(body => body.hook_event_name.startsWith("subagent_"));
      expect(owned).toEqual([
        { hook_event_name: "subagent_start", sessionID: recording.root, agent_id: recording.child },
        { hook_event_name: "subagent_stop", sessionID: recording.root, agent_id: recording.child },
      ]);
      const names = sent.map(body => body.hook_event_name);
      expect(names.indexOf("subagent_start")).toBeLessThan(names.indexOf("subagent_stop"));
    });

    it("never lets the child's own events reach the node as its lifecycle", async () => {
      const sent = await replay(recording.run);
      for (const body of sent.filter(body => !body.hook_event_name.startsWith("subagent_"))) {
        expect(body.sessionID).toBe(recording.root);
      }
      // The root's own completion is still delivered exactly once, after the child.
      const names = sent.map(body => body.hook_event_name);
      expect(names.filter(name => name === "session.idle")).toHaveLength(1);
      expect(names.lastIndexOf("subagent_stop")).toBeLessThan(names.indexOf("session.idle"));
    });

    it("ends the child's work when the recorded session.deleted arrives instead of an idle", async () => {
      const withoutChildIdle = recording.run.filter(
        event => !(event.type === "session.idle" && event.properties.sessionID === recording.child),
      );
      const sent = await replay([...withoutChildIdle, recording.deleted]);
      expect(sent.filter(body => body.hook_event_name === "subagent_stop")).toEqual([
        { hook_event_name: "subagent_stop", sessionID: recording.root, agent_id: recording.child },
      ]);
    });
  });

  it("leaves the root's own lifecycle untouched while a child is open", async () => {
    const { plugin, sent } = await start();
    await plugin.event(created("ses_Root"));
    await plugin.event(created("ses_Child", "ses_Root"));
    await plugin.event(idle("ses_Root"));
    await plugin.event(idle("ses_Child"));
    expect(sent().map(body => body.hook_event_name))
      .toEqual(["session.created", "subagent_start", "session.idle", "subagent_stop"]);
  });
});
