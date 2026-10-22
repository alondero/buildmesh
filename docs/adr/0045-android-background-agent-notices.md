# 45. Background Agent Notices on the Android Client

Status: accepted

## Context

An agent running on the desktop can stop and wait: it asks a question, or it
asks for permission to run a command. The desktop records that state on the
node (`awaiting_input`) and publishes it on the `/ws/events` feed that the
phone already reads while it is open. What the phone could not do was stay
open: the event socket belonged to the Activity, so the moment the user left
the app the phone stopped hearing about the desktop, and the one thing the
user most wants to know — that something is blocked on them — was the thing
they stopped being told.

Issue #2316 asks for a notice on the phone when an agent needs a person while
the app is in the background. Three things have to be decided together: the
delivery mechanism, what "needs a person" means, and what the notice may
claim.

The desktop's own answer to the second question already exists and must not
be re-invented on the phone: `node.status` is the single source of truth for
`awaiting_input` — the desktop's `is_attention_pending` derives it from
`node.status` in `src-tauri/src/commands/attention.rs` — and the lifecycle
kind only says *why* the node stopped.

## Decision

1. **The delivery mechanism is a user-enabled foreground service.** The
   service (`AgentWatchService`) holds the desktop's event feed open and posts
   a notice when a node needs a person. It is off until the user turns it on
   in Connection settings, and turning it off stops the service and clears its
   notices.
2. **The phone holds one event feed for the whole process.** The dashboard and
   the watch service attach to the same `EventFeed`, which owns one socket and
   one reconnect timer. Two sockets would double the desktop's subscriber
   count, the phone's idle traffic and the ticket mint rate for no added
   information.
3. **The watch service runs as a `specialUse` foreground service.** Its
   subtype is declared in the manifest. `dataSync` was rejected because
   Android 15 caps that type at six hours per day, and a watch that silently
   stops after six hours is worse than no watch.
4. **"Needs a person" is the node's status.** A node needs a person when the
   desktop reports the status `awaiting_input` — the same rule the desktop's
   own `is_attention_pending` applies in `src-tauri/src/commands/attention.rs`
   — while the lifecycle kind (`question_requested`, `permission_requested`)
   picks the wording only. The notice and the node row behind it therefore say
   the same thing.
5. **One notice per node, on its own channel, deep-linking to that node.**
   The notice carries the node's name and the reason, and opens
   `buildmesh://node/<id>`. It is cleared when the node's state is resolved —
   including when the answer is given on the desktop, which is the case that
   matters: the user is not required to answer on the phone that was told.
6. **The socket is held for as long as the user has enabled the watch.** It is
   deliberately *not* gated on "any node is running": the desktop can start an
   agent at any moment, and a phone that closed its socket while idle would
   have no way to hear about that agent. The idle cost is one keepalive ping
   per 20-second interval (`pingInterval(20, TimeUnit.SECONDS)` in
   `Connection.kt`), which is fewer requests than the foreground dashboard's
   five-second snapshot poll of three routes in `BuildmeshViewModel.resume()`.
7. **The delivery bound is under one second while the socket is connected.**
   The desktop emits the lifecycle event at the moment the hook reports the
   yield; the phone receives it on the open socket, posts the notice, and —
   for a node the phone has not seen before — reads the node's name from
   `/api/nodes` first. That is two round trips on a LAN or VPN. If the socket
   was down, the notice arrives after the reconnect instead: the socket backs
   off 1, 2, 4, 8 and 16 seconds, so the bound becomes 16 seconds after the
   last failed attempt.

## Alternatives considered

- **A periodic WorkManager check.** Rejected: `PeriodicWorkRequest` has a
  fifteen-minute floor, which is two orders of magnitude outside any useful
  bound for "an agent is waiting for you".
- **A push relay.** Rejected as out of scope: it needs the desktop reachable
  from the internet and changes the security model of Remote Access, which is
  deliberately a trusted-LAN or user-managed-VPN capability.
- **Two sockets — one for the dashboard, one for the watch service.** Rejected
  because it doubles the phone's idle traffic, the desktop's subscriber count
  and the ticket mint rate, and buys no information the shared feed does not
  already carry.
- **A `dataSync` foreground service.** Rejected: Android 15 limits that type
  to six hours in any 24-hour period, so the watch would stop working in the
  middle of a long working day with no explanation on the phone.
- **Deciding "needs a person" from the lifecycle kind.** Rejected: the desktop
  derives attention from `node.status`, so the phone deciding from the kind
  would be a second, drifting source of truth.
- **Answering from the phone being the only way to clear the notice.**
  Rejected: the notice is cleared by the node's state, not by the client that
  saw it.

## Consequences

- The phone shows a persistent "Watching for agents" notification while the
  watch is on. That is the price of a foreground service, and it is also the
  honest answer to "is this phone listening?".
- The watch holds one desktop connection open while it is on. Its idle traffic
  is a WebSocket keepalive ping per 20-second interval
  (`pingInterval(20, TimeUnit.SECONDS)` in `Connection.kt`): about 4,320 pings a
  day at that interval, each a single small control frame, so the day's
  traffic is protocol overhead rather than payload. That is fewer requests
  than the foreground dashboard's five-second snapshot poll, which the watch
  replaces rather than adds to. The on-device battery figure still needs the
  physical-device acceptance run and is not claimed here.
- A revoked device stops the feed instead of reconnecting forever; pairing
  again is the only way back.
- The watch follows the credential. Forgetting the desktop stops the watch and
  clears its notices — the watch had a desktop to listen to, and now it does
  not — and pairing again starts the watch again if the user left it on.
- The phone's notice is a statement about the desktop's state, not a promise
  that the answer must be given on the phone.

## Not covered

The Circuit "Needs approval" gate is a separate person-needed state and is not
covered by this record; the notice here is scoped to Agent Nodes. The browser
and desktop clients already show the same state in their own surfaces.

The supported behaviour is documented in the
[Android client contract](../development/android.md#background-agent-notices).
