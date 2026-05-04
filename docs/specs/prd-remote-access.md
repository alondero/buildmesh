## Problem Statement

Users need to monitor and interact with their buildmesh agent nodes from a mobile phone when away from their desktop — for example, checking on a long-running build or responding to an agent prompt from another room — without disrupting the existing desktop session.

Today there is no mobile interface. Users must be physically at their PC to check on agent state or send input.

## Solution

An **embedded HTTP/WebSocket server** in the Tauri Rust backend (port 1992, fallback 1993) serves a lightweight mobile web app. The mobile app communicates with the same PTY processes already running on the desktop, so phone and desktop share the same agent session.

Authentication is per-mesh using a random 32-character hex token generated once at mesh creation. The desktop UI shows a QR code linking phone and mesh.

## User Stories

1. As a user, I want to open a QR code in the desktop sidebar so that I can connect my phone to the same mesh my desktop is using.
2. As a user, I want to scan a QR code with my phone camera so that the mobile web app opens automatically without typing addresses or tokens.
3. As a user, I want to see a list of all agent nodes in my mesh on my phone so that I can pick which one to interact with.
4. As a user, I want to see each node's current status (idle, running, suspended, error) on the node list so that I can quickly assess which nodes need attention.
5. As a user, I want to tap a node in the mobile list so that I am taken directly to that node's terminal view.
6. As a user, I want to see the full terminal output of an agent node on my phone so that I can follow its progress remotely.
7. As a user, I want to type input to an agent from my phone so that I can respond to prompts without being at my desk.
8. As a user, I want my input from phone or desktop to go to the same agent session so that I do not have to worry about which device I am on.
9. As a user, I want the mobile terminal to show me all scrollback history so that I can read what happened before I connected.
10. As a user, I want an auto-reconnect mechanism on the mobile terminal so that brief network hiccups do not interrupt my session.
11. As a user, I want a clear offline message when the desktop app is not running so that I understand why the connection fails.
12. As a user, I want the connection to persist across browser restarts on my phone so that I do not have to re-scan the QR code every time.
13. As a user, I want a minimal header in the mobile terminal showing node name, provider, and status so that I always know which node I am on.
14. As a user, I want a back button in the mobile terminal so that I can return to the node list easily.
15. As a desktop user, I want the phone icon in the quick toolbar so that remote access is discoverable but does not dominate the UI.
16. As a user, I want the QR code to show both the URL and a readable IP:port fallback so that I can type it manually if the camera fails.
17. As a user, I want the HTTP server to use port 1992 by default so that it does not conflict with the E2E test server on port 1991.
18. As a user, I want the HTTP server to fall back to port 1993 if 1992 is in use so that the feature works even in constrained environments.
19. As a user running multiple agent nodes in the same mesh, I want each node to be independently accessible from my phone so that I can switch between them.
20. As a user, I want input I send from my phone to be visible in the desktop terminal so that both devices see the same conversation.

## Implementation Decisions

### Modules to Build or Modify

**Rust Backend:**

- `src-tauri/src/http_server.rs` (new): Embedded HTTP/WebSocket server using `tiny_http` for HTTP and `tokio-tungstenite` for WebSocket. Runs on the Tokio runtime already present in Tauri. Serves the mobile web app bundle and handles the `/ws/terminal/{node_id}` WebSocket endpoint.
- `src-tauri/src/db/mod.rs`: Add schema migration v7 that adds a `mesh_token TEXT NOT NULL UNIQUE` column to the `meshes` table. `create_mesh` must generate a random 32-char hex token on insert. Token is never updated after creation.
- `src-tauri/src/db/mesh_token.rs` (new if extraction warranted): Token generation using `rand::Rng`. Pure function, easily unit-tested.
- `src-tauri/src/commands/mesh.rs`: Add `get_mesh_token(mesh_id: i64) -> Result<String, String>` Tauri command. Reads token from DB and returns it for QR generation.
- `src-tauri/src/lib.rs`: Start the HTTP server in the `.setup()` closure after DB init and crash recovery. Graceful shutdown on app exit.
- `src-tauri/src/commands/terminal_ws.rs` (new): WebSocket handler that upgrades connections to `/ws/terminal/{node_id}`. Validates token from query param, looks up the PTY writer via `PROCESS_REGISTRY`, and relays bytes between the WebSocket and the PTY. If the PTY reader thread is not running (suspended node), it resumes the node first.

**Frontend:**

- `src/components/Sidebar/PhoneIcon.tsx` (new): Toolbar icon button in the quick toolbar area.
- `src/components/Sidebar/QrModal.tsx` (new): Modal showing the QR code encoding `http://<pc-ip>:1992/?token=<mesh-token>` and the raw IP:port text.
- `src/components/Sidebar/Sidebar.tsx`: Integrate phone icon and QR modal.
- `src/mobile/` (new directory): Mobile web app — a single-bundle React app served as static files. Uses xterm.js for terminal rendering and a WebSocket client for PTY relay.
  - `src/mobile/index.html`: Entry point.
  - `src/mobile/App.tsx`: Router (node list view vs terminal view based on URL path).
  - `src/mobile/NodeList.tsx`: Shows all agent nodes for the authenticated mesh with status indicators.
  - `src/mobile/TerminalView.tsx`: Full xterm.js terminal with input bar, auto-reconnect, and offline state handling.
- `src/lib/ip.ts` (new): Utility to discover the PC's LAN IP address for QR generation.

**Data and Schema:**

- `meshes` table gains `mesh_token TEXT NOT NULL UNIQUE` column (migration v7).
- The mobile app stores the token in `localStorage` under the key `buildmesh_mesh_token`.
- WebSocket URL format: `ws://<pc-ip>:1992/ws/terminal/{node_id}?token=<mesh-token>`

**ProcessRegistry Access:**

- The WebSocket handler reads from the existing `PROCESS_REGISTRY` static to get the PTY writer for a given `node_id`. If the node is suspended but has a `cli_session_id`, the handler resumes it before relaying.
- The `AgentProcess` struct already holds `writer: Arc<Mutex<Box<dyn std::io::Write + Send>>>` — no structural changes needed to the registry entry.

**Port Selection:**

- Port 1992 is tried first. If the bind fails (E2E test server, another instance), try 1993. If both fail, log an error and do not start the HTTP server (app continues without remote access).

**Stack:**

- HTTP: `tiny_http` (synchronous, easy to integrate with Tauri setup thread)
- WebSocket: `tokio-tungstenite` (already tokio-native, pairs well with the async runtime)
- Mobile app: React 19 with xterm.js 6.x — same terminal renderer as desktop

## Testing Decisions

**What makes a good test:** Only test external behavior — token format, WebSocket upgrade success/failure, authentication rejection, node list response shape. Do not test implementation details like internal mutable state.

**Modules to test:**

1. `db/mesh_token.rs` (new): Pure token generation — verify length (32 chars), charset (hex), uniqueness across N generations. No I/O or locking required.
2. `http_server.rs`: Integration test that binds a test port, makes an HTTP request for the node list endpoint with a valid and an invalid token, and verifies correct HTTP status codes.
3. `commands/terminal_ws.rs`: Unit test that validates a WebSocket connection with a correct token upgrades, and one with a bad token is rejected before any PTY interaction.

**Prior art in the codebase:**

- `src-tauri/src/db/mesh_tests.rs` — DB migration tests with in-memory connections
- `src-tauri/src/commands/agent_tests.rs` — command-level tests using mocked DB
- Vitest integration tests in `tests/integration/` — patterns for Tauri command invocation from tests

## Out of Scope

- Token rotation or expiry
- HTTPS (token will be visible in URLs on the local network — acceptable risk for MVP)
- Per-node access tokens (a mesh token grants access to all nodes in that mesh)
- Multi-mesh selector on mobile (MVP shows only the mesh the QR was generated for)
- Desktop "phone connected" indicator
- Tabbed multi-node view on mobile (one node at a time)
- Voice input on mobile (defer to phone's built-in keyboard voice)
- Any changes to the desktop terminal experience

## Further Notes

The design deliberately leverages the existing `ProcessRegistry` rather than creating a separate remote-access PTY pool. When a phone connects, it shares the exact same PTY that the desktop is using — no duplication of process state.

Port 1991 is already used by the Playwright E2E test server started in `lib.rs:67`. The fallback to 1993 keeps the two concerns decoupled.

Scrollback history is piped to the phone on WebSocket connect by having the backend PTY reader emit the full buffer contents at connection time — the xterm.js scrollback buffer is maintained by the reader thread and flushed to new WebSocket clients on connect.