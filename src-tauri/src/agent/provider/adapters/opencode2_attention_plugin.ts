// OpenCode 2 server plugin. Identity comes from config options, keyed by CLI
// session; server-wide environment variables never identify a node.
import { Plugin } from "@opencode/plugin";

const validSession = (id) => typeof id === "string" && /^ses_[0-9A-Za-z_]{2,125}$/.test(id);
const HOLD_MS = 750;

export default Plugin.define({
  id: "buildmesh.attention2",
  setup(ctx) {
    const controller = new AbortController();
    const pending = new Map();
    const sessions = ctx.options?.sessions ?? {};
    let delivery = Promise.resolve();

    function post(sessionID, body) {
      const route = sessions[sessionID];
      if (!validSession(sessionID) || !Number.isSafeInteger(route?.nodeId) || route.nodeId <= 0 ||
          !Number.isInteger(route.port) || route.port <= 0 || route.port > 65535) return;
      // Preserve event order even when a prior loopback request is slow.
      delivery = delivery.then(async () => {
        if (controller.signal.aborted) return;
        try {
          await fetch(`http://localhost:${route.port}/api/attention/${route.nodeId}`, {
            method: "POST", headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ ...body, sessionID }),
            signal: AbortSignal.any([controller.signal, AbortSignal.timeout(2000)]),
          });
        } catch { /* The harness continues when Buildmesh is unavailable. */ }
      });
    }

    function clear(key) {
      const timer = pending.get(key);
      if (!timer) return false;
      clearTimeout(timer);
      pending.delete(key);
      return true;
    }

    void (async () => {
      try {
        for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
          const data = event.data;
          const id = data?.sessionID;
          // Do not cache a root id or attribute unidentified child events to it.
          if (!validSession(id) || !sessions[id]) continue;
          const requestID = data.requestID ?? data.id;
          const key = typeof requestID === "string" ? `${id}:${requestID}` : undefined;
          let type = event.type;
          if (type === "session.execution.succeeded") type = "session.idle";
          if (type === "session.inbox.enqueued" || type === "session.execution.started") type = "session.busy";
          if (type === "session.execution.failed") type = "session.error";
          if (["session.busy", "session.idle", "session.error"].includes(type)) {
            for (const pendingKey of pending.keys()) if (pendingKey.startsWith(`${id}:`)) clear(pendingKey);
          }
          if (type === "session.execution.cancelled") {
            for (const pendingKey of pending.keys()) if (pendingKey.startsWith(`${id}:`)) clear(pendingKey);
            post(id, { hook_event_name: "session.error", error: "MessageAbortedError" });
            continue;
          }
          const body = { hook_event_name: type };
          if (typeof requestID === "string") body.request_id = requestID;
          if (type === "permission.asked") {
            body.tool_name = data.permission;
            body.message = data.permission;
            if (!key) { post(id, body); continue; }
            clear(key);
            const timer = setTimeout(() => { pending.delete(key); post(id, body); }, HOLD_MS);
            timer.unref?.();
            pending.set(key, timer);
          } else if (type === "permission.replied") {
            if (!clear(key)) post(id, body);
          } else if (["session.idle", "session.busy", "session.error", "question.asked", "question.replied", "question.rejected"].includes(type)) {
            if (type === "question.asked") body.message = data.questions?.[0]?.question ?? "OpenCode is asking a question";
            post(id, body);
          }
        }
      } catch { /* Abort on unload; no detached rejected subscription promise. */ }
    })();

    return () => {
      controller.abort();
      for (const timer of pending.values()) clearTimeout(timer);
      pending.clear();
    };
  },
});
