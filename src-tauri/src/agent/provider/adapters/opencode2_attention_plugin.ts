// OpenCode 2 server plugin. Identity comes from config options, keyed by CLI
// session; server-wide environment variables never identify a node.
import { Plugin } from "@opencode/plugin";

const validSession = (id) => typeof id === "string" && /^ses_[0-9A-Za-z_]{2,125}$/.test(id);
const HOLD_MS = 750;
const MAX_TRACKED_CHILDREN = 512;

export default Plugin.define({
  id: "buildmesh.attention2",
  setup(ctx) {
    const controller = new AbortController();
    const pending = new Map();
    const sessions = ctx.options?.sessions ?? {};
    // Native subagents are ordinary child sessions. Track each child's root so
    // its events do not become the node's own lifecycle (issue #2636).
    const children = new Map();
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

    function trackChild(childID, parentID) {
      if (!validSession(childID) || !validSession(parentID) || childID === parentID) return;
      if (children.has(childID)) return;
      const root = sessions[parentID] ? parentID : children.get(parentID)?.root;
      if (!root || !sessions[root]) return;
      children.set(childID, { root, open: true });
      while (children.size > MAX_TRACKED_CHILDREN) {
        const closed = [...children].find(([, child]) => !child.open);
        if (!closed) break;
        children.delete(closed[0]);
      }
      post(root, { hook_event_name: "subagent_start", agent_id: childID });
    }

    function finishChild(childID) {
      const child = children.get(childID);
      if (!child?.open) return;
      child.open = false;
      post(child.root, { hook_event_name: "subagent_stop", agent_id: childID });
    }

    void (async () => {
      try {
        for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
          const data = event.data;
          const id = data?.sessionID;

          if (event.type === "session.created") {
            // The configured root is pre-created through the session API; there
            // is no capture. A child session carries its parentID instead.
            if (validSession(id) && sessions[id]) continue;
            trackChild(id, data?.parentID);
            continue;
          }

          // V2 questions are forms, not `question.asked`. Only a form owned by
          // this node's root session is reported; child and MCP global forms
          // belong to their own owner (issue #2655).
          if (event.type === "form.created") {
            const form = data?.form;
            const sessionID = form?.sessionID;
            if (!validSession(sessionID) || !sessions[sessionID]) continue;
            const field = form.fields?.[0];
            const body = {
              hook_event_name: "question.asked",
              message: field?.description || form.title || field?.title || "OpenCode is asking a question",
            };
            if (typeof form.id === "string") body.request_id = form.id;
            post(sessionID, body);
            continue;
          }
          if (event.type === "form.replied" || event.type === "form.cancelled") {
            if (!validSession(id) || !sessions[id]) continue;
            const body = {
              hook_event_name: event.type === "form.replied" ? "question.replied" : "question.rejected",
            };
            if (typeof data?.id === "string") body.request_id = data.id;
            post(id, body);
            continue;
          }

          // A child's lifecycle is owned work, not the node's turn.
          if (children.has(id)) {
            if (["session.execution.succeeded", "session.execution.failed",
                 "session.execution.interrupted", "session.deleted", "session.idle"].includes(event.type)) {
              finishChild(id);
            }
            continue;
          }

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
          if (type === "session.execution.interrupted") {
            for (const pendingKey of pending.keys()) if (pendingKey.startsWith(`${id}:`)) clear(pendingKey);
            const reason = data?.reason;
            // The process is going away, or a newer execution already owns the
            // turn; treat both as non-interrupting.
            if (reason === "shutdown" || reason === "superseded") continue;
            post(id, {
              hook_event_name: "session.error",
              error: reason === "user" ? "MessageAbortedError" : (typeof reason === "string" ? reason : "ExecutionInterrupted"),
            });
            continue;
          }
          const body = { hook_event_name: type };
          if (typeof requestID === "string") body.request_id = requestID;
          if (type === "permission.asked") {
            const action = typeof data?.action === "string" ? data.action : undefined;
            const resources = Array.isArray(data?.resources)
              ? data.resources.filter((resource) => typeof resource === "string" && resource.length > 0)
              : [];
            body.notification_type = "permission_prompt";
            if (action) body.tool_name = action;
            body.message = typeof data?.message === "string" && data.message
              ? data.message
              : action
                ? (resources.length ? `${action} (${resources.join(", ")})` : action)
                : "OpenCode is asking for permission";
            if (!key) { post(id, body); continue; }
            clear(key);
            const timer = setTimeout(() => { pending.delete(key); post(id, body); }, HOLD_MS);
            timer.unref?.();
            pending.set(key, timer);
          } else if (type === "permission.replied") {
            if (!clear(key)) post(id, body);
          } else if (["session.idle", "session.busy", "session.error", "question.asked", "question.replied", "question.rejected"].includes(type)) {
            if (type === "question.asked") body.message = data.questions?.[0]?.question ?? "OpenCode is asking a question";
            if (type === "session.error" && typeof data?.error?.name === "string") body.error = data.error.name;
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
