# Remote Access — Mobile Web App

## Status
Draft — awaiting implementation

## Goal
Access and interact with buildmesh agent nodes from a phone on the same network, continuing existing work seamlessly.

## Architecture

**Embedded HTTP/WebSocket server in Tauri Rust backend** — no separate process. The same backend that manages PTY processes also serves the mobile web app and handles WebSocket terminal connections.

```
Phone Browser ──HTTP/WSS──> Tauri Backend (port 1992)
                              │
                              ├── HTTP server (static web app)
                              ├── WebSocket server (PTY relay)
                              └── PTY management (existing)
```

**Port:** 1992. If 1992 is in use (e.g., by E2E test server on 1991), fallback to 1993.

**Stack:** `tiny_http` for HTTP server, `tokio-tungstenite` for WebSocket (already in tokio ecosystem).

---

## Authentication

### Token Format
Per-mesh token — a random 32-character hex string generated once when the mesh is created. Stored in the database. No expiry.

### Desktop: QR Generation
- Phone icon in **quick toolbar** below the logo, above the meshes list
- Icon: minimalist outline phone (Option A from design review)
- Click → modal shows QR code encoding: `http://<pc-ip>:1992/?token=<mesh-token>`
- QR also shows `192.168.x.x:1992` text below for manual entry fallback

### Mobile: Connection Flow
1. Phone camera scans QR → opens `http://<pc-ip>:1992/?token=xxx` in Chrome
2. Backend validates token against mesh DB
3. If valid → store token in `localStorage` → redirect to node list
4. If invalid → show "Invalid token — rescan QR" error

### Token Storage (Phone)
`localStorage` (not sessionStorage). Token persists across browser restarts.

Rationale: convenience for repeated use. Physical phone security is acceptable risk for MVP.

---

## Mobile Web App

### Pages

#### 1. Node List (`/`)
- Shows all agent nodes for the authenticated mesh
- Each node: name, provider, status with color indicator
  - idle → blue left border
  - running → green left border
  - suspended → gray left border
  - error → red left border
- Tap node → navigate to `/terminal/{node_id}`
- "Switch Mesh" button (future: not in MVP)

#### 2. Terminal View (`/terminal/{node_id}`)
- Full xterm.js terminal (same renderer as desktop)
- Minimal header: node name + provider, status dot, back button, close button
- Full scrollback history piped to phone on WebSocket connect
- Input bar at bottom: text input + Send button
- On disconnect: "Reconnecting..." overlay with retry countdown
- Auto-reconnect: up to 3 retries with exponential backoff (1s, 2s, 4s)
- On permanent failure: "Connection lost — tap to retry" message

#### 3. Offline State
If buildmesh backend is not running when phone tries to connect:
- HTTP connection refused → show "Desktop app is offline — start it to continue"
- Clear message, not technical jargon

---

## Terminal Behavior

### Shared PTY
Desktop and phone share the same PTY input stream. Typing from either device sends to the same agent. This matches the mental model: "I'm working from my phone, not a separate session."

### Scrollback History
On WebSocket connect, the backend sends the full xterm.js scrollback buffer to the phone. User can scroll up to see everything since agent start.

### Provider in Header
Minimal header shows: `agent-backend · Minimax`. No additional chrome.

---

## Data Flow

### WebSocket Connect
```
Phone connects: WSS://pc:1992/ws/terminal/{node_id}?token=xxx
Backend validates token + node belongs to mesh
Backend looks up PTY via ProcessRegistry
Backend spawns PTY reader relay (if not already streaming)
PTY output stream → WebSocket → xterm.js on phone
Phone keyboard input → WebSocket → PTY writer
```

### Note on ProcessRegistry
ProcessRegistry already exists as a static `HashMap<i64, Arc<AgentProcess>>`. The WebSocket handler needs access to the PTY writer for a given node_id. If the PTY reader thread isn't running (suspended node), resume it first.

---

## Security Considerations (MVP)

- Token in URL params is unencrypted — visible in browser address bar
- Token in QR is briefly visible on screen — acceptable for MVP
- No token rotation/expiry in MVP — see future work
- No per-node access control — token grants access to all nodes in mesh
- Network: assumes trusted network (VPN or home/office WiFi)

**Future:** HTTPS support, token expiry, per-node access tokens.

---

## Implementation Order

1. Add mesh token generation on mesh creation
2. Add `/api/mesh/{id}/token` Tauri command (returns token for QR display)
3. Add `tiny_http` HTTP server to Tauri app setup
4. Add WebSocket endpoint for PTY relay
5. Build mobile web app (single HTML/JS bundle served by embedded server)
6. Desktop: phone icon + QR modal
7. Auto-reconnect + offline state handling

---

## File Changes

### Rust (src-tauri/src)
- `lib.rs`: Start HTTP server in setup, add mesh token field
- `db/`: Add `mesh_token` column, token generation
- `commands/mesh.rs`: Add `get_mesh_token` command
- `commands/terminal.rs`: Add WebSocket PTY relay (new file or module)
- `http_server.rs`: New module for embedded HTTP/WebSocket server

### Frontend (src/)
- `components/PhoneIcon/`: Toolbar icon + QR modal
- `mobile/`: Node list page, terminal view page
- Mobile app served as static files from embedded HTTP server

### Config
- `tauri.conf.json`: Add `httpServer` port config (default 1992)
- No new permissions needed beyond existing shell/plugin configs

---

## Out of Scope (MVP)

- Token rotation/expiry
- HTTPS
- Per-node access tokens
- Multi-mesh selector on mobile
- Desktop "phone connected" indicator
- Tabbed multi-node on mobile
- Voice input (defer to phone's built-in keyboard voice)