# Coordinator API and remote access

Status: current

## Coordinator Read & Drive API

Buildmesh exposes an **HTTP surface** for an external **Coordinator** (the user's remotely-hosted Hermes Agent first; a future in-app superagent second) to scan every Agent Node across every Mesh, drill into any one (the **read** half, below) and **drive** a chosen node (the write half — see *Coordinator Drive*). Plain JSON over the existing embedded HTTP server, **off by default**, behind separate capability-scoped tokens distinct from the mobile root token, bound to loopback + LAN (the user owns the remote tunnel — Tailscale / Cloudflare / WireGuard). Hermes is one instance of a Coordinator, not the category.

- **Architecture & rationale:** [`docs/adr/0008-coordinator-control-api.md`](../adr/0008-coordinator-control-api.md). **Domain language:** *Coordinator*, *Node Digest* in [`CONTEXT.md`](../../CONTEXT.md).
- **User guide (how to enable + consume):** [`docs/development/coordinator-read-api.md`](coordinator-read-api.md). **Spec:** issue #312.
- **Two endpoints (both authenticated with the read-scoped bearer token):**
  - `GET /nodes` — array of layered Node Digests. Spine is always present (lifecycle `status`, `needs_feedback` = `awaiting_input`, `waiting_since`, `last_activity`); the transcript-derived rich layer is present for providers with a wired reader (Anthropic/Claude-compatible profiles, Codex, Cursor, AGY, Grok, Command Code, Muse) or explicitly flagged `unavailable` (degrade-and-flag, never a silent omission). Muse nodes may also include `observed_session_telemetry` when MSP token/context events have been ingested; the field is omitted when there are no observations and is never remaining account quota. Cheap to poll.
  - `GET /nodes/{id}/log?tail=N` — on-demand raw recent turns (assistant text + tool calls) for one node. Content is **raw, not pre-summarised** — the Coordinator is itself an LLM. An unknown node id is a 404; every other degrade path is a 200 carrying a structured `unavailable` envelope.
- **Module layout:** `src-tauri/src/coordinator/` (`node_digest.rs` is the pure digest builder; `enrichment.rs` owns provider capability, working-path resolution, secret scrubbing and Muse observed-session telemetry). `src-tauri/src/services/transcript_reader/mod.rs` preserves the public read signatures and shapes unavailable results once. The `TranscriptReader` seam in `transcript_reader/adapter.rs` dispatches to one module per wired format in `transcript_reader/readers/`; each owns location, parsing, digest and assistant-report reads, including OpenCode's SQLite store and Cline's whole-document history. Shared streaming and byte-window I/O lives in `transcript_reader/file.rs`. The `adapters` module alias preserves existing lifecycle and session-discovery imports. `src-tauri/src/http/routes/coordinator.rs` is a thin transport skin; `src-tauri/src/http/router.rs` enforces the off-by-default + read-token gate in the dispatcher.
- **Contract test guard:** reader-owned tests use real disk fixtures under `src-tauri/tests/fixtures/transcripts/<reader>/` and temporary native stores to cover tail selection, latest assistant text, malformed records, empty sessions, unavailable files and shape drift. `TranscriptFormat::for_harness` explicitly lists Claude-compatible profiles and returns `None` for unwired ids; transcript dispatch never borrows Claude content. A coordinator regression preserves the Node Digest spine with enrichment flagged unavailable. This is the read-side form of the project's serde-default-fragility lesson.
- **Coordinator Drive (PRD #313, ADR-0008 §5–6; D1 #319 + D2 #320).** The write half: `POST /nodes/{id}/prompt` writes a prompt into a live node's PTY through #178's `AgentDriver` (`send_prompt` → `verify_delivery`) — the PTY's stdin *is* the input box, nothing is screen-scraped, and **any live node** is drivable (Claude Code queues stdin for a busy agent). Requires the **drive scope** (distinct from read, off by default, under the master kill-switch); the read token can never drive. The response carries an **honest verdict** — `Delivered` on a confirmed `awaiting → cleared` transition, else `Unverified` (queued-but-unconfirmed) — never success without confirmation. All the drive logic lives in `src-tauri/src/coordinator/drive.rs` behind two seams (`DriveTarget` = PTY/DB/events, `IdempotencyStore` = the ledger) so it is unit-testable without a real PTY or DB; `http/routes/coordinator.rs::prompt` is a thin skin. Do **not** grow a parallel write path — the scheduler (#178) reuses the same `AgentDriver`.
- **Idempotency (D2 #320, hardened #750).** Each drive carries a mandatory caller-supplied `idempotency_key`; the `coordinator_drive_prompts` ledger (`PRIMARY KEY (node_id, idempotency_key)`, `db/mod.rs`, schema v32) records the verdict once and a duplicate key **replays** it instead of re-sending, so a Coordinator's retry over a flaky network never lands a prompt twice (#178's cardinal rule). v32 (issue #750) reshaped the row protocol from `lookup → send → record` (racy: two concurrent same-key requests could both send) to atomic **claim-before-send** — the claim transaction inserts a `pending` row, the winner drives, the loser sees `InProgress` and briefly waits for finalize (or surfaces `409 + Retry-After: 1`). A `pending` row older than `PENDING_CLAIM_TIMEOUT_SECS` (30 s) is reclaimed by the next claim attempt, so a crashed-mid-send row can't lock out the key. The row also carries a SHA-256 `prompt_hash` (Stripe-style item 2 hardening) — same key + *different* prompt is `409 key_payload_mismatch` rather than a silent 200-replay-of-different-prompt. Recording happens only after a successful send: `NotLive` / `WriteFailed` calls `release_claim` (so a retry re-attempts); `Delivered` / `Unverified` calls `finalize` (so re-sending is the double-delivery #178 forbids). Lookup **fails safe, not open** — an unreadable ledger returns `503`, never a silent re-send (a read error must never be mistaken for "key never seen"). GC: a dedicated background worker (`services::coordinator_ledger_maintenance::start_worker`) prunes rows older than `LEDGER_RETENTION_DAYS` (7 days) on a 30-minute cadence so the table's size is proportional to "unique drives per week" rather than "unique drives ever" (item 3). The pure `drive_node_idempotent(store, driver, …)` orchestrator encodes claim → send → finalize / release_claim and is tested against fakes (headline: same key twice = exactly one PTY write; same key + different prompt = `KeyPayloadMismatch`; concurrent same-key = exactly one delivery, the loser waits then sees Replay).
- **Tunnels are the user's job.** Buildmesh never opens an internet port — the threat model for the coordinator surface is "coordinating agent on a machine I control, reached over my own tunnel", not "autonomous agent on a public VPS reachable from the open internet". Reaching the read surface from outside the LAN is a deliberate user choice, not a Buildmesh feature.
- **Auth is two-tier and header-only (#500, ADR-0015).** Every request resolves to a [Role](../../CONTEXT.md) — **Admin** (root token, the mobile `/api/*` surface) or **Coordinator** (read/drive tokens, `/nodes*`) — as **disjoint surfaces**: a token works only on its own surface (wrong-surface valid token → 403, no creds → 401). Role resolution lives in `src-tauri/src/http/auth.rs` (`authorize`); the dispatcher (`http/router.rs`) calls `auth::authorize(.., scope)` per route, and `/admin/*` is reserved Admin-only. Credentials travel only in `Authorization: Bearer` or the `bm_session` cookie — never `?token=`. The mobile shell/assets are public; the client logs in via `POST /api/session` (sets the cookie) and mints a single-use `?ticket=` per WebSocket via `POST /api/ws-ticket` (`src-tauri/src/http/ws_ticket.rs`).
- **Device Sessions are the per-browser Admin credential.** Desktop IPC `create_pairing_ticket` mints a hashed, single-use five-minute invitation (`http/pairing.rs`). QR URLs put it in `#pair=`, outside the initial request. `POST /api/pair` consumes it and creates a hashed `device_sessions` row. `POST /api/session` refreshes existing devices only, including one-time migration of old localStorage device credentials. Neither endpoint accepts root-token pairing or returns credentials in JSON. Both set a 400-day HttpOnly, SameSite=Strict cookie (Secure over TLS). Mobile boot refreshes automatically. Site-data removal, address changes, cookie expiry/eviction, and desktop revocation can require re-pairing. Authentication never depends on client IP. Authorized Devices and remote admin revocation delete the device row and broadcast to close its WebSockets; WebSockets use per-target single-use tickets.

The native Android client in `android/` consumes the same remote HTTP routes and
per-device session protocol. Compose owns management screens; a bundled, isolated
xterm WebView renders only terminal bytes while Kotlin owns transport. Pairing
authenticates the desktop CA against the QR fingerprint before exchanging an
invitation. Android Keystore protects the persisted session. Foreground refreshes
coalesce with owner/revision fences, and forgetting cancels pending actions. See
[native Android client](android.md) for build and boundary details.

## Mobile frontend composition

`src/mobile/screens/NodeList.tsx` owns navigation, filtering, quick-create retry
keys and request-bound reply state. Its rendering modules live under
`src/mobile/components/nodeList/`: node rows, attention cards/deck and request
previews, overview, per-mesh sections, provider picker, mesh actions sheet and
snapshot/create notices. ESLint caps the screen at 600 lines.

`useNodeListSnapshot` owns the mesh/node/Circuit reads, provider loading,
visibility polling and WebSocket reconciliation. The screen passes its mounted
ref into the hook, which owns the refresh revision fence. A failed secondary
Circuit read keeps the last successful Circuit data, and an unavailable primary
read keeps the visible list with a stale notice. Reply markers and create operation keys stay
with the screen so extracting rendering modules does not reset pending work.

## LAN/VPN Exposure & Self-Signed TLS

The embedded server binds **loopback only by default** (#496). An off-by-default
**LAN / VPN Exposure** toggle (App Settings → `set_lan_exposure_enabled`, stored in
`app_settings.lan_exposure_enabled`) exposes it on the machine's interfaces; issue
#501, [`docs/adr/0017-opt-in-lan-exposure-and-self-signed-tls.md`](../adr/0017-opt-in-lan-exposure-and-self-signed-tls.md).

- **Loopback stays plain HTTP; only non-loopback interface IPs get TLS** (`http::bind_specs`). This is deliberate: the attention webhook posts plain `http://localhost/api/attention/...`, so forcing TLS on loopback would silently break every agent's "awaiting input" signal. Do **not** "simplify" this to a single `0.0.0.0` TLS bind.
- **TLS identities** use a stable root and a leaf covering localhost, loopback, and interface IPs. `http/tls/identity.rs` serializes writers, validates key/certificate matches and the leaf chain, stages and syncs a complete private generation, then atomically publishes `tls/current`. Readers resolve the pointer once. Legacy files migrate without changing a valid root; address changes renew only the leaf. Explicit reset publishes a fresh root. Prior complete generations remain available; incomplete staging never becomes active. Directories and private keys are owner-only, including the temporary iOS signing directory.
- **The toggle rebinds live** without an app restart. A serialized binding operation owns the setting snapshot and listener replacement. Each listener owns its TLS/HTTP/WebSocket tasks and aborts/drains them on shutdown; loopback listeners have separate ownership. Each listener admits at most 64 HTTP/handshake connections and 32 WebSockets, with a five-second TLS deadline. WebSocket output futures remain inside their connection. Disabling LAN also invalidates unused pairing invitations.
- **HTTP module layout (issue #1658):** `http/server.rs` (bind, TLS, accept, head parse, Host guard, body read, WS upgrade), `http/router.rs` (one `ROUTES` table + `dispatch(ParsedRequest)`), `http/state.rs` (app handle, snapshots, ports, listeners, interface cache), `http/instances.rs` (the machine-global registry of bound ports). `http/mod.rs` re-exports and exposes `start_http_server` / `reapply_binding`.
- **Ports and the offset.** `http/state.rs` owns `HTTP_PORT_START` 1992 / `HTTP_PORT_END` 1994 (the server binds the first of the three that is free and publishes it as `current_http_port`, which is what agent attention hooks and `BUILDMESH_PORT` point at) and `port_offset`, which shifts every listener — including the dev test bridge on 1991 — so the stable hub and the `*.dev` profile never contend. A debug or `dev-tools` build reads `BUILDMESH_PORT_OFFSET` ahead of the identifier so two dev instances can each have a range; see [startup-and-profiles](startup-and-profiles.md) for the override's gate, its fallback, and how the launchers allocate it per worktree.
- **`MaybeTls`** (`http::stream`) is the single concrete stream type (`Plain(TcpStream)` | `Tls(Box<TlsStream<TcpStream>>)`); WSS rides the same enum. The server owns the stream. Route handlers take `ParsedRequest` and return `Response` — they never touch `BufStream` / `MaybeTls`.
- **A read outcome is a value, not a `String` (issue #2330).** `src-tauri/src/read_outcome.rs` owns the vocabulary for an authoritative persistence read: `Present` (which includes a *successful* empty collection), `Absent` (a lookup that matched no row), and `Unavailable` (persistence could not answer). `src-tauri/src/http/read_status.rs` maps it to the wire — `200` / `404` / `503` — and `http::read_status::serialized` replaces every `serde_json::to_string(x).unwrap_or_else(|_| "[]")` fallback, so a body that cannot be encoded is a `500` rather than a successful empty shape. **Do not reintroduce** `Err(_) => "[]"`, `.ok()?`, or `if db::get_x(id).is_err() { 404 }` on a read route: each turns "the database could not be queried" into "the resource does not exist", which is how a phone loses its Work list and a Coordinator drops a live node. Only `rusqlite::Error::QueryReturnedNoRows` — and only on a by-id lookup — is absence; match the variant, never its `to_string()`.
- **Unavailability is 503, never 401/403.** The mobile client treats `401`/`403` as a revoked credential and bounces the user to the pairing screen, so a disk fault reported as an auth failure signs people out. `src/mobile/api.ts::isUnavailableError` is the client-side counterpart; a `NodeList` poll that hits one keeps its last-known snapshot and shows a *stale notice* rather than the full-screen "can't reach the desktop" overlay (the desktop answered — a transport failure is a different thing), and the next successful poll calls `onReachable` to retire a stale overlay and converge.
- **Graceful degradation is for optional content, not the authoritative answer.** A transcript no reader supports stays a `200 {"status":"unavailable"}` envelope (ADR-0008 §3), because that is content Buildmesh legitimately lacks. The node inventory, the mesh list and the discovery ownership read are not optional: an unreadable one is a `503`, never an empty success.
- **Crypto provider is `ring`.** The embedded server selects it explicitly via
  `builder_with_provider`. App startup also installs `ring` as rustls's
  process-default provider because reqwest 0.13 uses `rustls-no-provider` and
  its client builder reads that default. The embedded server remains bound to
  its explicit provider; `aws-lc-rs` is not in the dependency tree.

## Multiple instances on one machine

The hub, a `*.dev` profile and any `BUILDMESH_PORT_OFFSET` instance can all be
running at once, each on its own port. Anything a harness reads at **run time**
follows the node — `$BUILDMESH_PORT` in `spawn_environment` — but a harness that
must bake a URL into one file it shares with every other instance cannot name
one port: MiniMax Code's single user-global plugin manifest is the case
`http::instances` exists for (issue #2343).

- **The registry.** Each instance writes `<profile-parent>/buildmesh-instances/<port>`
  once the port is settled (`http/server.rs`, after `store_resolved_http_port`)
  and removes it on a clean exit. The parent of the profile directory
  (`%APPDATA%`, `~/Library/Application Support`, `$XDG_DATA_HOME`) is where the
  shipped profiles already meet — the hub and the `*.dev` profile differ only by
  the leaf directory name, so neither can see the other's data directory. One
  file per instance means no cross-process write race: nothing reads or rewrites
  another's entry.
  The body's pid and profile label are diagnostic only; the file *name* carries
  the port.
- **`live_ports(own)`** returns the sorted, deduplicated ports that answer a TCP
  connect, always including `own`. The probe exists because a crash leaves the
  file behind, and a stale entry must not accumulate in every hook. `own` is
  never gated behind the probe: it is the one address that cannot be wrong, and
  dropping it would silence the callbacks only this instance can answer.
- **Best-effort by construction.** `publish` failing (an unwritable parent, a
  read-only profile root) logs and returns; remote access itself never depends
  on it. A missing registry degrades to the old single-instance behaviour.
- **Why a fan-out is correct.** `/api/attention/*` is public and
  node-independent: it resolves the node from the payload's session id and cwd,
  so the instance that owns the node accepts a callback and every other instance
  discards it as unknown. That makes the port list safe to bake whenever it was
  written — an instance that owns a MiniMax node is by definition running, and
  provisioned its own manifest. See [Attention](attention.md) for the hook side.

## Fuzzing the request-read path

The server reads hostile bytes twice — the request head
(`http::server::read_request_head`, bounded by `REQUEST_HEAD_TIMEOUT` and
`MAX_HEADER_BYTES`) and the body (`http::request::read_body_with_cap`, bounded
by the route's `BodyPolicy` cap and `BODY_READ_TIMEOUT`) — and
`src-tauri/src/http/fuzz.rs` fuzzes them as one path. The harness feeds raw
client bytes into an in-memory duplex and calls those production functions; it
never re-implements a parse, so a fix that closes a finding cannot leave the
harness green.

```text
cd src-tauri && cargo test --lib http::fuzz
cd src-tauri && BUILDMESH_FUZZ_CASES=500000 cargo test --release --lib http::fuzz -- --nocapture
```

- **Seeds:** `src-tauri/fuzz/corpus/http_request/*.bin` are raw request byte streams. The `SEEDS` table in the harness pins each one's outcome — including the four streams the `read_body_with_cap` unit tests send over loopback TCP — and a corpus file with no pinned outcome fails the run.
- **Invariants:** no panic; never `TimedOut`, because the harness closes the client half before any read, so a read that still waits is itself a finding; a returned body is exactly `Content-Length` bytes, is the exact run of request bytes between the head and whatever the stream still has unread, and is no larger than the route cap; `TooLarge` only above the cap; a head past `MAX_HEADER_BYTES` always reports overflow. A violation panics with the input inline and writes it to `.tmp/fuzz-artifacts/` under a process-unique name.
- **Knobs:** `BUILDMESH_FUZZ_CASES` (default 2048) and `BUILDMESH_FUZZ_SEED` (default `0x2156_0005`, printed by every run) make a campaign reproducible.
- **Stable, not `cargo-fuzz`.** `cargo fuzz` needs nightly, and both the local toolchain set and `.github/workflows/verify.yml` pin stable, so such a target could not be built or run in either place. A coverage-guided target can replace the mutation loop without touching the reader — both would call the same three functions.
- **CI:** the `commands-http` shard's `http::` filter already runs the four `http::fuzz` tests, and `rust-nonshard` runs the bounded smoke (`BUILDMESH_FUZZ_CASES=2000`, `--nocapture`) so the campaign settings stay on the record.
- **Remaining surfaces (issue #2156):** the per-harness transcript readers (`services/transcript_reader/readers/`) and PTY input decoding (`agent/process.rs`). Each plugs into the same shape — seeds on disk, a pinned outcome table, invariants over the production parser — without changing this harness.

