import { readFileSync } from "node:fs";
import { afterEach, describe, expect, it, vi } from "vitest";

const source = readFileSync("src-tauri/src/agent/provider/adapters/opencode2_attention_plugin.ts", "utf8");
const load = new Function("Plugin", source.replace('import { Plugin } from "@opencode/plugin";', "").replace("export default Plugin.define", "return Plugin.define"));
type Event = { type: string; data?: Record<string, unknown>; properties?: Record<string, unknown> };

function plugin() {
  let wake: (() => void) | undefined;
  const events: Event[] = [];
  const fetch = vi.fn().mockResolvedValue({ ok: true });
  vi.stubGlobal("fetch", fetch);
  const definition = load({ define: (value: unknown) => value });
  const cleanup = definition.setup({
    options: { sessions: { ses_One: { nodeId: 1, port: 2992 }, ses_Two: { nodeId: 2, port: 2992 } } },
    event: { subscribe: async function* ({ signal }: { signal: AbortSignal }) {
      signal.addEventListener("abort", () => wake?.());
      while (!signal.aborted) {
        if (!events.length) await new Promise<void>(resolve => { wake = resolve; });
        while (events.length) yield events.shift();
      }
    } },
  });
  const emit = async (event: Event) => {
    events.push(event); wake?.();
    for (let i = 0; i < 12; i++) await Promise.resolve();
  };
  return { fetch, emit, cleanup, bodies: () => fetch.mock.calls.map(([, init]) => JSON.parse(init.body)) };
}

afterEach(() => { vi.unstubAllGlobals(); vi.useRealTimers(); });

describe("OpenCode 2 server attention plugin", () => {
  it("routes data payloads by session and ignores unidentified, stale and child sessions", async () => {
    const p = plugin();
    await p.emit({ type: "session.execution.succeeded", data: { sessionID: "ses_One" } });
    await p.emit({ type: "session.execution.started", data: { sessionID: "ses_Two" } });
    await p.emit({ type: "session.execution.succeeded", data: { sessionID: "ses_Stale" } });
    await p.emit({ type: "session.execution.succeeded", data: { sessionID: "ses_Child" } });
    await p.emit({ type: "session.execution.succeeded", properties: { sessionID: "ses_One" } });
    await p.emit({ type: "session.execution.succeeded", data: { sessionID: "--bad" } });
    expect(p.fetch.mock.calls.map(([url]) => url)).toEqual(["http://localhost:2992/api/attention/1", "http://localhost:2992/api/attention/2"]);
    expect(p.bodies()).toEqual([{ hook_event_name: "session.idle", sessionID: "ses_One" }, { hook_event_name: "session.busy", sessionID: "ses_Two" }]);
    p.cleanup();
  });

  it("correlates permission replies by request id and clears delayed asks on new work", async () => {
    vi.useFakeTimers();
    const p = plugin();
    await p.emit({ type: "permission.asked", data: { sessionID: "ses_One", id: "perm_1", permission: "shell" } });
    await p.emit({ type: "permission.replied", data: { sessionID: "ses_One", requestID: "perm_1" } });
    await vi.advanceTimersByTimeAsync(800);
    expect(p.fetch).not.toHaveBeenCalled();
    await p.emit({ type: "permission.asked", data: { sessionID: "ses_One", id: "perm_2", permission: "shell" } });
    await vi.advanceTimersByTimeAsync(800);
    expect(p.bodies()[0]).toMatchObject({ hook_event_name: "permission.asked", request_id: "perm_2", tool_name: "shell" });
    await p.emit({ type: "permission.replied", data: { sessionID: "ses_One", requestID: "perm_2" } });
    expect(p.bodies()[1]).toMatchObject({ hook_event_name: "permission.replied", request_id: "perm_2" });
    await p.emit({ type: "permission.asked", data: { sessionID: "ses_One", id: "perm_3" } });
    await p.emit({ type: "session.inbox.enqueued", data: { sessionID: "ses_One" } });
    await vi.advanceTimersByTimeAsync(800);
    expect(p.bodies().at(-1).hook_event_name).toBe("session.busy");
    p.cleanup();
  });

  it("cleans up subscriptions and timers and never reports failures as completed turns", async () => {
    vi.useFakeTimers();
    const p = plugin();
    await p.emit({ type: "session.execution.failed", data: { sessionID: "ses_One" } });
    expect(p.bodies()[0].hook_event_name).toBe("session.error");
    await p.emit({ type: "permission.asked", data: { sessionID: "ses_One", id: "perm_1" } });
    p.cleanup();
    await vi.advanceTimersByTimeAsync(800);
    expect(p.fetch).toHaveBeenCalledTimes(1);
  });

  it("cancels pending permission attention when the execution is cancelled", async () => {
    vi.useFakeTimers();
    const p = plugin();
    await p.emit({ type: "permission.asked", data: { sessionID: "ses_One", id: "perm_cancel" } });
    await p.emit({ type: "session.execution.cancelled", data: { sessionID: "ses_One" } });
    await vi.advanceTimersByTimeAsync(800);
    expect(p.bodies()).toEqual([{ hook_event_name: "session.error", error: "MessageAbortedError", sessionID: "ses_One" }]);
    p.cleanup();
  });
});
