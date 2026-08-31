# 8. Coordinator Control API — external agents read and drive nodes

Status: proposed

Buildmesh exposes an **agent-agnostic control API** through which an external [Coordinator](../../CONTEXT.md) (the user's remotely-hosted Hermes Agent first; a future in-app Buildmesh superagent second) can *read* what every [Agent Node](../../CONTEXT.md) is doing and *drive* a chosen node. Buildmesh stays a "dumb" driver — the orchestration intelligence lives in the Coordinator. The read side ships first and stands alone; the drive side reuses the `AgentDriver` trait from the [#178 driving PRD](https://github.com/alondero/buildmesh/issues/178) and depends on it landing.

## Context

#178 makes Buildmesh able to *drive* a node without a human keystroke (provision → await-ready → send-prompt → verify), but only for one local consumer: the scheduler. Its PRD explicitly anticipates a v2 "super-agent" that supervises many nodes and widens the same `AgentDriver` trait with read-side methods. This ADR is that v2's architecture, prompted by the user wiring up Hermes (Nous Research's autonomous agent) as a remote coordinator.

Hermes is **external and remote** — its own process (local TUI, Docker, SSH, serverless), reachable over the network, driven by the user through a chat gateway. It calls HTTP APIs as tools and has a **native MCP client**. So the integration is unavoidably a network control API, and "any agent can coordinate" is a first-class goal, not Hermes-specific.

The read-side primitives mostly already exist: the attention system (`attention-needed`/`attention-cleared` + `SessionStatus::AwaitingInput`) is the "needs feedback" signal; `services/session_discovery.rs` already locates and parses Claude Code's on-disk JSONL transcripts; the embedded HTTP server (`src-tauri/src/http/`, ports 1992–1994, token-auth) is the existing network surface. The drive primitive is the PTY write the mobile WebSocket relay already performs.

## Decision

**1. Coordinator-agnostic, not Hermes-specific.** One control surface; Hermes is the first client, a future in-app superagent the second. Both go through the same API rather than growing parallel paths — the same single-execution-path argument #178 makes for the `AgentDriver` trait.

**2. REST-shaped-as-MCP now; MCP as a fast-follow.** The endpoints are designed as MCP *resources* (read) and *tools* (drive), but ship first as plain JSON-over-HTTP on the existing embedded server. Rationale: the protocol is the easy, swappable part; the uncertain part is the read model. Plain HTTP is human-`curl`-inspectable — the human and the Coordinator query the identical surface — so the read model can be iterated cheaply before any MCP dependency, transport, or remote-auth cost is taken on. Once the read model is proven, a thin MCP server (`rmcp`, streamable HTTP/SSE) wraps the same core; resource subscriptions then map `attention-needed` onto push notifications. (An MCP server is itself an HTTP server — this is a contract-shape decision, not a stack swap.)

**3. Layered, gracefully-degrading read model ([Node Digest](../../CONTEXT.md)).** An always-available spine from Buildmesh's own DB (lifecycle `status`, `needs_feedback` = `awaiting_input`, `waiting_since`) is enriched, **for harnesses with a wired transcript reader (currently Claude Code/Claude-compatible profiles, Codex, Cursor, AGY, Grok, and Command Code)**, with semantic content read from the JSONL transcript. A non-supporting provider, or a transcript that fails to parse, degrades to the spine with the enrichment **explicitly flagged `unavailable`** — never silently omitted, so the Coordinator can tell "node is quiet" from "rich layer is down". The rendered terminal/TUI is deliberately **not** a read source.

**4. Two-tier, raw read side.**
- `GET /nodes` → array of digests: the cheap "which nodes need me?" scan (`id`, `name`, `mesh`, `provider`, `status`, `needs_feedback`, `waiting_since`, `last_activity_at`, a short truncated `last_assistant_message`).
- `GET /nodes/{id}/log?tail=N` → on-demand raw recent transcript turns (assistant text + tool calls). **All JSONL brittleness is quarantined to this endpoint.**

Content is **raw and truncated, not pre-summarised**: the Coordinator is itself an LLM, so handing it raw material is cheaper, lower-latency, and less lossy than spending Buildmesh's own LLM (which is reserved for the human-facing UI, e.g. session naming). The single highest-value field: when a node is `awaiting_input`, its last assistant message *is the question it is blocked on*.

**5. Drive = thin skin over `AgentDriver`, any live node, honest verdict.** `POST /nodes/{id}/prompt` writes to the PTY via #178's `send_prompt` — the PTY's stdin *is* the input box, so there is no TUI element to locate (this is why #178 mandates provider-aware driving over screen-scraping). Any *live* node may be driven, not only `awaiting_input` ones, because Claude Code **queues** stdin sent to a busy agent (a legitimate "leave a follow-up" use). The response carries an honest verdict: `Delivered` when an `attention-cleared` transition confirmed consumption, `Unverified` when the write could not be confirmed (the queued-to-a-running-agent case). *Future:* the read side gives a stronger verifier — a queued prompt appears in the JSONL as a `user` message — so verification can later read the transcript rather than watch attention flags.

**6. Idempotency keys.** A Coordinator on a flaky network *will* retry a timed-out HTTP request, and #178's cardinal rule is "never let a prompt land twice." The Coordinator supplies an idempotency key per intended prompt; a duplicate key is a no-op returning the original verdict. This makes retries safe by construction.

**Hardened in #750.** v32 (issue #750) closed three deferred gaps from the initial review of #320's implementation:
- **Concurrent-retry double-send.** The pre-#750 `lookup → send → record` was non-atomic: two concurrent same-key requests could both miss the ledger and both write to the PTY. The fix is **claim-before-send** — a single transaction inserts a `pending` row (`INSERT OR IGNORE`), the winner drives, the loser sees `InProgress` and briefly waits (up to 5 s) for finalize or surfaces `409 + Retry-After: 1`. A `pending` row older than `PENDING_CLAIM_TIMEOUT_SECS` (30 s) is reclaimed by the next claim attempt — a crashed-mid-send row can't lock out the key forever.
- **Same key + different prompt.** Pre-#750 silently replayed the recorded verdict on a key-reuse-with-different-payload (a silent success-that-didn't-happen). The fix stores a SHA-256 `prompt_hash` alongside the row and rejects same-key-different-payload with `409 key_payload_mismatch` (Stripe-style). The Coordinator must mint a fresh key.
- **Bounded-age GC.** Pre-#750 the ledger grew one permanent row per `(node_id, key)` ever driven — unbounded across a long app lifetime. The fix is a dedicated background worker (`services::coordinator_ledger_maintenance::start_worker`) pruning rows older than `LEDGER_RETENTION_DAYS` (7 days) on a 30-minute cadence, plus an initial sweep on every launch.

**7. Security stance (see Consequences).** Buildmesh stays loopback/LAN-bound and the user owns the remote tunnel; the coordinator surface is **secure-by-default-off** behind a separate, capability-scoped (read-vs-drive) token; coordinator-originated prompts are attributed in the UI; a master kill-switch disables drive.

**8. Brittleness defence — *guarded*.** Defensive parsing (missing/renamed JSONL fields → `unavailable`, never panic) plus the degrade-and-flag rule, **plus a contract test against a checked-in real JSONL fixture** so a Claude Code format change turns a local test red instead of silently degrading Hermes in production. This is the read-side form of the existing serde-default-fragility lesson (required fields + a regression test asserting the old shape fails loud).

## Considered alternatives

- **MCP server first-class, now.** Rejected as the *first* step: it front-loads a heavyweight `rmcp` dependency, a new SSE transport on a deliberately framework-free server, and the least-mature part of MCP (remote auth) — all before the read model is proven. Kept as the fast-follow because Hermes speaks MCP natively and resource subscriptions fit the attention signal well.
- **Bespoke REST only, no MCP ever.** Rejected: forgoes auto-discovery and forces hand-maintained tool glue on every Coordinator; designing resource/tool-shaped now keeps the MCP wrap mechanical for near-zero cost.
- **Pre-summarise transcripts for the Coordinator.** Rejected: spends Buildmesh's tokens and discards detail an LLM consumer can reason over itself. Summarisation is for the human UI only.
- **Parse the rendered terminal/TUI for "what's going on".** Rejected (consistent with #178): visually brittle, breaks on every redraw, and semantically poorer than the JSONL.
- **Restrict driving to `awaiting_input` nodes only.** Rejected: Claude Code queues stdin, so leaving a follow-up for a busy agent is a real use; the honest `Unverified` verdict handles the unverifiable case without forbidding it.
- **Buildmesh becomes an internet-facing server (own TLS/hardening).** Rejected: signs the project up to maintain internet-grade security on a bespoke desktop HTTP server *that drives code-executing agents*. The tunnel approach keeps that burden in battle-tested software.

## Consequences

- **Sequencing.** The read side (items 3–4, 8) has **no dependency on #178** and is the lower-risk place to start (read-only cannot corrupt a run). The drive side (5–6) depends on #178's `AgentDriver`. These are separable issues.
- **Remote exposure is RCE-by-proxy.** A Coordinator that can drive nodes can make a tool-enabled agent run arbitrary code. The mobile threat model ("my phone, on my Wi-Fi, with a token") does **not** cover "an autonomous agent, possibly on a VPS, reachable from the internet." Hence: Buildmesh never opens an internet port (the user runs Tailscale/Cloudflare/WireGuard); the coordinator surface ships **off**, bound to loopback, with no token minted until explicitly enabled; the token is **capability-scoped** so a read-only token can be granted before any drive token; coordinator-driven prompts are attributed in the UI; a master kill-switch exists.
- **Distribution-readiness.** The user intends to share Buildmesh with colleagues. Secure-by-default-off + scoped token + attribution is precisely what makes a naive colleague's install safe by construction (not "safe if configured right"). The deeper multi-user story (per-user identity, who-drove-what across people) is deferred but **not blocked** — it builds on this foundation.
- **#178 cross-reference.** #178's PRD links a `docs/adr/0001-driving-agent-nodes.md` that was never created (the real ADR-0001 is auto-sync-on-spawn); this ADR is the architecture for the read + coordinator side of that work.
