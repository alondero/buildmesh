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

import { appendFile } from "node:fs/promises";

const logPath = process.env.BUILDMESH_PLUGIN_LOG;

function buildmeshUrl() {
  const port = process.env.BUILDMESH_PORT;
  const sid = process.env.BUILDMESH_SESSION_ID;
  if (!port || !sid) return null;
  return `http://localhost:${port}/api/attention/${sid}`;
}

async function postAttention(body) {
  const url = buildmeshUrl();
  if (!url) return; // No Buildmesh runtime — silently drop.
  try {
    const res = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!res.ok && logPath) {
      // Best-effort diagnostic only — never throw from a plugin handler;
      // OpenCode would surface the throw as a plugin fault and the user
      // would see a noisy error overlay.
      await appendFile(logPath, `attention ${res.status}\n`).catch(() => {});
    }
  } catch (_err) {
    // Network/loopback failure — swallow (Buildmesh's autoclear safety
    // net arms on every mark; a missed callback self-heals within a few
    // PTY output bursts).
  }
}

export const BuildmeshAttention = async () => {
  return {
    event: async ({ event }) => {
      if (!event || typeof event.type !== "string") return;

      // `session.idle` — the agent finished its turn and is sitting at
      // the input prompt waiting. We use a dedicated `hook_event_name`
      // (`session.idle`) that mirrors the upstream OpenCode plugin event
      // type, so a future OpenCode release that adopts a structured
      // notification type for the same semantic converges without a
      // plugin rewrite. The classifier has a dedicated rule that maps
      // this event to `InputRequired`. We deliberately do NOT post a
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
      // decision. Map to PermissionRequest so the classifier always marks
      // (rule 2 in `classify` — no transcript scan needed for permission
      // prompts).
      if (event.type === "permission.asked") {
        await postAttention({
          hook_event_name: "PermissionRequest",
          notification_type: "permission_prompt",
          message: "OpenCode is asking for permission",
        });
        return;
      }

      // Other events (`session.status`, `permission.replied`, ...) are
      // intentionally ignored — Buildmesh's turn pipeline doesn't need
      // them and forwarding every status transition would spam the
      // attention endpoint.
    },
  };
};
