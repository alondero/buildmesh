// Buildmesh attention plugin for OpenCode (issue #1295).
//
// Loaded by the OpenCode TUI as an ESM module from `.opencode/plugins/`.
// Forwards two harness events back to Buildmesh's local attention endpoint
// so the agent node reaches `awaiting_input` like every other harness:
//   - `session.idle`       — turn ended, agent is at its prompt
//   - `permission.asked`   — agent is blocked on a tool approval decision
//
// The endpoint URL and node id are resolved at runtime from `process.env`,
// set per-agent by `agent::spawn_environment` (`BUILDMESH_PORT` /
// `BUILDMESH_SESSION_ID`). We don't bake either value into the file
// because OpenCode's plugin loader imports it once per process and
// re-imports would defeat the per-node URL.
//
// Plugin contract: https://opencode.ai/docs/plugins
//   `event({ event })` is the typed-event dispatch. We consume exactly the
//   two event kinds Buildmesh turns into Node Turns; everything else is
//   dropped so OpenCode's own session/status chatter doesn't drown the
//   attention callback.

const logPath = process.env.BUILDMESH_PLUGIN_LOG;

function buildmeshUrl() {
  const port = process.env.BUILDMESH_PORT;
  const sid = process.env.BUILDMESH_SESSION_ID;
  if (!port || !sid) return null;
  return `http://localhost:${port}/api/attention/${sid}`;
}

// Forward the permission event's tool info when OpenCode provides it.
// Upstream shape varies across plugin-event versions; we tolerate `tool`
// or `toolName` at the top level or under `properties`, and accept
// either a bare string or a `{ name | id }` object, so the classifier
// can extract a semantic turn description (issue #1364 §1) regardless
// of which payload shape OpenCode ships today.
function pickToolInfo(event) {
  const candidates = [
    event.tool,
    event.toolName,
    event.properties?.tool,
    event.properties?.toolName,
  ];
  for (const c of candidates) {
    if (typeof c === "string" && c.length > 0) return c;
    if (c && typeof c === "object") {
      if (typeof c.name === "string" && c.name.length > 0) return c.name;
      if (typeof c.id === "string" && c.id.length > 0) return c.id;
    }
  }
  return undefined;
}

async function postAttention(body) {
  const url = buildmeshUrl();
  if (!url) return; // No Buildmesh runtime — silently drop.
  try {
    // Bound the request with an explicit timeout so a stuck loopback
    // socket can't leak an un-aborted promise into the OpenCode TUI's
    // event loop (matches the curl --connect-timeout / --max-time
    // pattern every other harness uses — see e.g. `agy::hook_command`).
    const res = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(2000),
    });
    if (!res.ok && logPath) {
      // Dynamic-import `node:fs/promises` so the diagnostic branch is
      // the only path that loads it; the steady-state plugin stays
      // free of FS dependencies on every OpenCode startup.
      const { appendFile } = await import("node:fs/promises");
      await appendFile(logPath, `attention ${res.status}\n`).catch(() => {});
    }
  } catch (_err) {
    // Network/loopback failure or timeout — swallow (Buildmesh's
    // autoclear safety net arms on every mark; a missed callback
    // self-heals within a few PTY output bursts).
    if (logPath) {
      try {
        const { appendFile } = await import("node:fs/promises");
        await appendFile(logPath, `attention error\n`).catch(() => {});
      } catch {}
    }
  }
}

export const BuildmeshAttention = async () => {
  return {
    event: async ({ event }) => {
      if (!event || typeof event.type !== "string") return;

      // `session.idle` — the agent finished its turn and is sitting at
      // the input prompt waiting. We use a dedicated `hook_event_name`
      // (`session.idle`) that mirrors the upstream OpenCode plugin event
      // type. The classifier has a dedicated rule that maps this event
      // to `InputRequired`. We deliberately do NOT post a
      // `transcript_path` because OpenCode has no Claude-style
      // transcript file — the classifier's transcript-scan fallback
      // would otherwise classify this as `Ready` (turn done, no pending
      // tasks).
      if (event.type === "session.idle") {
        await postAttention({
          hook_event_name: "session.idle",
          message: "OpenCode session idle — agent ready for input",
        });
        return;
      }

      // `permission.asked` — the agent is blocked on a tool approval
      // decision. We post the upstream event name verbatim
      // (`hook_event_name: "permission.asked"`) so the classifier has
      // an explicit, dedicated rule for it (issue #1295 wire honesty —
      // no borrowed Codex/Grok branch). The classifier also accepts
      // `notification_type: "permission_prompt"` as a parallel
      // structured signal (matches Grok/AGK semantics), and we forward
      // any tool info OpenCode exposes so the route's semantic turn
      // extractor can render a meaningful description.
      if (event.type === "permission.asked") {
        const toolName = pickToolInfo(event);
        const body = {
          hook_event_name: "permission.asked",
          notification_type: "permission_prompt",
          message: toolName
            ? `OpenCode is asking for permission: ${toolName}`
            : "OpenCode is asking for permission",
        };
        if (toolName) body.tool_name = toolName;
        await postAttention(body);
        return;
      }

      // Other events (`session.status`, `permission.replied`, ...) are
      // intentionally ignored — Buildmesh's turn pipeline doesn't need
      // them and forwarding every status transition would spam the
      // attention endpoint.
    },
  };
};
