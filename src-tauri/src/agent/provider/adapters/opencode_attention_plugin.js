// Buildmesh attention plugin for OpenCode (issues #1295 + #1294).
//
// Loaded by the OpenCode TUI as an ESM module from `.opencode/plugins/`.
// Forwards three harness events back to Buildmesh's local attention
// endpoint so the agent node reaches `awaiting_input` like every other
// harness AND captures the freshly minted `ses_<…>` session id at TUI
// boot, which is the primary path for `agent_nodes.cli_session_id`
// (issue #1294):
//   - `session.created`   — TUI minted a new `ses_<…>` id at boot;
//                            persistence only, no Node Turn.
//   - `session.idle`       — turn ended, agent is at its prompt.
//   - `permission.asked`   — agent is blocked on a tool approval decision.
//
// Every callback includes `sessionID` so the route's ordering-token
// fence (`attention.rs:690-702`) can compare against the stored id and
// reject stale callbacks from a previous OpenCode incarnation — Claude
// and Codex attach the session token to every lifecycle event for the
// same reason; without it a crashed-and-relaunched OpenCode would let
// its predecessor's turns silently overwrite the new state.
//
// The endpoint URL and node id are resolved at runtime from `process.env`,
// set per-agent by `agent::spawn_environment` (`BUILDMESH_PORT` /
// `BUILDMESH_SESSION_ID`). We don't bake either value into the file
// because OpenCode's plugin loader imports it once per process and
// re-imports would defeat the per-node URL.
//
// Plugin contract: https://opencode.ai/docs/plugins
//   `event({ event })` is the typed-event dispatch. We consume exactly the
// three event kinds Buildmesh routes or persists; everything else is
// dropped so OpenCode's own session/status chatter doesn't drown the
// attention callback.

const logPath = process.env.BUILDMESH_PLUGIN_LOG;

// Module-scoped cache for the most recently validated `ses_<…>` id.
// OpenCode's `session.created` event carries it (and is the primary
// capture path for `agent_nodes.cli_session_id`); `session.idle` and
// `permission.asked` may or may not carry it depending on the upstream
// plugin revision, but every callback MUST include the id so the
// route's ordering-token fence (`attention.rs:690-702`,
// `if hook != stored_cli_session_id_owned`) can compare against the
// stored id and reject stale callbacks from a previous OpenCode
// process. Without this, two OpenCode incarnations that share the same
// `cli_session_id` (a crashed-and-relaunched TUI, an explicit restart)
// both pass the gate and the second one's turn quietly overwrites the
// first — the exact poisoning Claude/Codex avoid by attaching the
// session token to every lifecycle event. We prefer the event-supplied
// id when present (the upstream plugin is the authority) and fall
// back to the cache so a future revision that drops the field from
// `session.idle` / `permission.asked` doesn't lose fencing.
let cachedSessionId = null;

function buildmeshUrl() {
  const port = process.env.BUILDMESH_PORT;
  const sid = process.env.BUILDMESH_SESSION_ID;
  if (!port || !sid) return null;
  return `http://localhost:${port}/api/attention/${sid}`;
}

// OpenCode session ids are `ses_<hex+base62>` (12 hex timestamp chars + 14
// base62 chars). We gate the id at the plugin edge so a malformed payload
// (e.g. a future plugin-event revision that ships `sessionID` of the
// wrong shape) never reaches Buildmesh's argv splice for
// `--session <id>`. The Rust side runs the same validator
// (`http::request::parse_opencode_session_id`), but a fail-fast here
// saves the round-trip and keeps the log clean.
//
// IMPORTANT — Base62 is case-sensitive. OpenCode's CLI rejects unknown
// ids and looks them up case-sensitively (`opencode export ses_…Zwp…`
// succeeds; `ses_…zwp…` returns `Session not found`), so a "helpful"
// `.toLowerCase()` fold here would destroy the id and break
// `AgentProvider::resume_args`. The validator only case-folds the
// 4-byte `ses_` prefix; the remainder character class is the
// case-sensitive `[0-9A-Za-z_]` (Base62 + underscore), exactly the set
// the Rust gate accepts.
function isValidSessionId(id) {
  if (typeof id !== "string") return false;
  // Length bounds match the Rust gate: 4-byte prefix + 1..124 remainder
  // → total length 5..129 inclusive. We reject `ses_` alone (no
  // remainder) as too short to be a real id.
  if (id.length < 6 || id.length > 129) return false;
  // Case-insensitive prefix check. We avoid `.toLowerCase()` because
  // it would force-allocate a 129-char string on every payload; a
  // charCodeAt compare is allocation-free and matches the Rust gate's
  // `eq_ignore_ascii_case` byte-level check.
  // 's'=0x73 'S'=0x53 'e'=0x65 'E'=0x45 '_'=0x5f
  if (
    (id.charCodeAt(0) !== 0x73 && id.charCodeAt(0) !== 0x53) || // s/S
    (id.charCodeAt(1) !== 0x65 && id.charCodeAt(1) !== 0x45) || // e/E
    (id.charCodeAt(2) !== 0x73 && id.charCodeAt(2) !== 0x53) || // s/S
    id.charCodeAt(3) !== 0x5f // _
  ) {
    return false;
  }
  // Remainder: case-sensitive `[0-9A-Za-z_]`. charCodeAt lets us write
  // the four numeric ranges inline — no regex, no String allocation.
  for (let i = 4; i < id.length; i++) {
    const c = id.charCodeAt(i);
    const ok =
      (c >= 0x30 && c <= 0x39) || // 0-9
      (c >= 0x41 && c <= 0x5a) || // A-Z
      (c >= 0x61 && c <= 0x7a) || // a-z
      c === 0x5f; // _
    if (!ok) return false;
  }
  return true;
}

// Pull the session id off an OpenCode `session.created` event.
// Upstream has shipped the field under a few different shapes across
// plugin-event revisions; we check every documented location so a
// nested payload shape doesn't silently yield `undefined` and drop the
// capture. Order matches the most-likely-to-be-populated field first
// (top-level `sessionID`, the documented upstream field).
function pickSessionId(event) {
  if (!event || typeof event !== "object") return undefined;
  if (typeof event.sessionID === "string" && event.sessionID.length > 0) {
    return event.sessionID;
  }
  if (typeof event.session_id === "string" && event.session_id.length > 0) {
    return event.session_id;
  }
  const props = event.properties;
  if (props && typeof props === "object") {
    if (typeof props.sessionID === "string" && props.sessionID.length > 0) {
      return props.sessionID;
    }
    if (typeof props.session_id === "string" && props.session_id.length > 0) {
      return props.session_id;
    }
    const info = props.info;
    if (info && typeof info === "object") {
      if (typeof info.id === "string" && info.id.length > 0) return info.id;
      if (typeof info.sessionID === "string" && info.sessionID.length > 0) {
        return info.sessionID;
      }
    }
  }
  const info = event.info;
  if (info && typeof info === "object" && typeof info.id === "string") {
    return info.id;
  }
  return undefined;
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

// Single shared log helper. Lazy-imports `node:fs/promises` exactly
// once across the plugin so every diagnostic branch reuses the same
// module reference (instead of three separate `await import(...)`
// calls scattered through the file).
let _appendFile = null;
async function appendLog(line) {
  if (!logPath) return;
  if (_appendFile === null) {
    try {
      ({ appendFile: _appendFile } = await import("node:fs/promises"));
    } catch {
      // If the dynamic import itself fails (very rare on a real
      // Node runtime), null it out so we don't try again on every
      // log call — silent drop matches the existing post-error
      // behaviour where a log failure is non-fatal.
      _appendFile = false;
      return;
    }
  }
  if (!_appendFile) return;
  await _appendFile(logPath, line).catch(() => {});
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
    if (!res.ok) {
      await appendLog(`attention ${res.status}\n`);
    }
  } catch (_err) {
    // Network/loopback failure or timeout — swallow (Buildmesh's
    // autoclear safety net arms on every mark; a missed callback
    // self-heals within a few PTY output bursts).
    await appendLog("attention error\n");
  }
}

export const BuildmeshAttention = async () => {
  return {
    event: async ({ event }) => {
      if (!event || typeof event.type !== "string") return;

      // `session.created` (issue #1294) — the TUI just minted a fresh
      // `ses_<…>` id and is about to start. We forward it so the
      // attention route's `Decision::Ignore` arm persists the id via
      // `set_cli_session_id_if_missing`. This is the primary capture
      // path for `agent_nodes.cli_session_id` — running in-process
      // with the TUI that just started, so it's the only unambiguous
      // source when two Root Nodes share a directory (the SQLite
      // poller in `services::opencode_session` can only match on
      // `directory`, which is identical for both). `hook_event_name`
      // and `sessionID` mirror the upstream OpenCode plugin event
      // shape; the classifier has a dedicated rule that maps the
      // event to `Ignore` (issue #1294 — capture-only, no Node Turn).
      //
      // Field-name handling: OpenCode's plugin event shape varies
      // across revisions (top-level `sessionID`, snake_case
      // `session_id`, or a nested `properties.info.id`). We try every
      // documented shape via `pickSessionId` so a future upstream
      // rename doesn't drop captures.
      if (event.type === "session.created") {
        const id = pickSessionId(event);
        if (!isValidSessionId(id)) {
          await appendLog("session.created missing or malformed id\n");
          return;
        }
        cachedSessionId = id;
        await postAttention({
          hook_event_name: "session.created",
          sessionID: id,
          message: "OpenCode session created",
        });
        return;
      }

      // `session.idle` — the agent finished its turn and is sitting at
      // the input prompt waiting. We use a dedicated `hook_event_name`
      // (`session.idle`) that mirrors the upstream OpenCode plugin event
      // type. The classifier has a dedicated rule that maps this event
      // to `InputRequired`. We deliberately do NOT post a
      // `transcript_path` because OpenCode has no Claude-style
      // transcript file — the classifier's transcript-scan fallback
      // would otherwise classify this as `Ready` (turn done, no pending
      // tasks).
      //
      // Generation fencing (issue #1294 round-2 review): `sessionID`
      // is attached so the route's `hook_uuid != stored_cli_session_id`
      // check can drop callbacks from a previous OpenCode incarnation.
      // Prefer the event-supplied id; fall back to the cached value
      // captured at `session.created` for revisions that drop the field.
      if (event.type === "session.idle") {
        const id = pickSessionId(event) ?? cachedSessionId;
        if (!isValidSessionId(id)) {
          await appendLog("session.idle missing or malformed id\n");
          return;
        }
        await postAttention({
          hook_event_name: "session.idle",
          sessionID: id,
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
      //
      // Generation fencing (issue #1294 round-2 review): `sessionID`
      // is attached so the route's ordering-token fence can drop
      // permission callbacks from a previous OpenCode incarnation that
      // share a `cli_session_id` with the running process.
      if (event.type === "permission.asked") {
        const toolName = pickToolInfo(event);
        const id = pickSessionId(event) ?? cachedSessionId;
        if (!isValidSessionId(id)) {
          await appendLog("permission.asked missing or malformed id\n");
          return;
        }
        const body = {
          hook_event_name: "permission.asked",
          sessionID: id,
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