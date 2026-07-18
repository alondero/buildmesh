// Shared types and HTTP client for the mobile SPA.
//
// Authentication (issue #500, extended by #502): the QR/pasted root token is
// POSTed to `/api/session` (Authorization: Bearer) by `login()`, which pairs
// this device — minting a persistent *per-device* token — sets an HttpOnly
// bm_session cookie, and returns that device token. We persist the device token
// (not the root token) in localStorage, so this phone is revocable on its own
// and re-`login()`s as itself after the cookie expires. Subsequent fetches send
// the cookie automatically via `credentials: "include"` — the token never rides
// a URL. WebSocket upgrades use a single-use `?ticket=` minted by
// `mintWsTicket()`. localStorage is the only "keystore" a browser SPA has; a
// per-device token there is strictly better than the shared root token it
// replaces (its leak exposes only this device, and the user can revoke it).

const TOKEN_STORAGE_KEY = "buildmesh_token";

// Mesh, AgentNode, ProviderInfo and the SessionStatus union are generated from
// the Rust structs (issue #359, issue #406) — the same structs the desktop
// Tauri path serialises. The mobile SPA reads only a subset of their fields,
// but the type now states the full wire shape, so a missing or renamed field
// becomes a compile error instead of a blank page (#360). NodeStatus is kept
// as an alias of the generated SessionStatus so existing mobile call sites
// keep working. `Provider` is kept as an alias of the generated `ProviderInfo`
// (issue #406) so `listProviders(): Promise<Provider[]>` and
// `import { Provider } from "../api"` call sites keep compiling — distinct
// from the generated `Provider` string union scoped inside `AgentNode.ts`
// (the provider-id enum, e.g. "anthropic"/"minimax").
import type { Mesh } from "../types/generated/Mesh";
import type { AgentNode } from "../types/generated/AgentNode";
import type { ProviderInfo as Provider } from "../types/generated/ProviderInfo";
import type { SessionStatus } from "../types/generated/SessionStatus";
export type { Mesh, AgentNode, Provider };
export type NodeStatus = SessionStatus;

export interface CreateNodeRequest {
  mesh_id: number;
  provider: string;
  rows?: number;
  cols?: number;
}

export function rememberToken(token: string) {
  try {
    localStorage.setItem(TOKEN_STORAGE_KEY, token);
  } catch {
    /* private-mode storage failure — fine, cookie still works for this session */
  }
}

export function readStoredToken(): string | null {
  try {
    return localStorage.getItem(TOKEN_STORAGE_KEY);
  } catch {
    return null;
  }
}

export function clearStoredToken() {
  try {
    localStorage.removeItem(TOKEN_STORAGE_KEY);
  } catch {
    /* ignore */
  }
}

class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

function fallbackMessage(status: number): string {
  if (status === 401 || status === 403) {
    return "Not authorized — reconnect with a fresh token from the desktop app.";
  }
  if (status >= 500) return "The desktop app hit an error handling this request.";
  return `Request failed (${status}).`;
}

async function apiFetch(path: string, init?: RequestInit): Promise<Response> {
  // Auth is the HttpOnly bm_session cookie, sent automatically via
  // credentials:include. The token never rides the URL (issue #500); the
  // cookie is minted once by `login()` (POST /api/session).
  const resp = await fetch(path, {
    credentials: "include",
    ...init,
  });
  if (!resp.ok) {
    // Routes report failures as {"error": "..."} — surface that text to the
    // user instead of a bare status code when it's available.
    let detail = "";
    try {
      const j = (await resp.json()) as { error?: unknown };
      if (typeof j?.error === "string") detail = j.error;
    } catch {
      /* non-JSON body */
    }
    throw new ApiError(resp.status, detail || fallbackMessage(resp.status));
  }
  return resp;
}

export function isAuthError(e: unknown): boolean {
  return e instanceof ApiError && (e.status === 401 || e.status === 403);
}

/// Pair this device / log in (issue #500, extended by #502). The token (root
/// token on first pair, or this device's own token on a refresh) is sent in the
/// `Authorization: Bearer` header — never the URL — to `POST /api/session`,
/// which sets the bm_session cookie and returns the effective *device* token.
/// Returns that device token (which the caller persists in place of whatever it
/// presented), `null` for a bad token (so the connect form can recover), and
/// throws only when the desktop app is unreachable.
export async function login(token: string): Promise<string | null> {
  let resp: Response;
  try {
    resp = await fetch("/api/session", {
      method: "POST",
      headers: { Authorization: `Bearer ${token}` },
      credentials: "include",
    });
  } catch {
    // Network-level failure (app down / wrong network) — distinct from a
    // rejected token, so surface it as a throw the caller turns into the
    // "can't reach the desktop app" message.
    throw new ApiError(0, fallbackMessage(0));
  }
  if (resp.status === 401 || resp.status === 403) return null;
  if (!resp.ok) throw new ApiError(resp.status, fallbackMessage(resp.status));
  // The server returns the persistent device token to store going forward. We
  // must NOT fall back to the token we presented: on a first pair that's the
  // root token, and persisting it would re-introduce the shared, unrevocable
  // credential #502 exists to remove. A 200 with no token is a server-contract
  // violation, so treat it as a failed login (null) rather than silently
  // downgrading to the root token.
  try {
    const j = (await resp.json()) as { token?: unknown };
    if (typeof j?.token === "string" && j.token.length > 0) return j.token;
  } catch {
    /* non-JSON / empty body — fall through to the null failure below */
  }
  return null;
}

export async function listNodes(): Promise<AgentNode[]> {
  return (await apiFetch("/api/nodes")).json();
}

export async function listMeshes(): Promise<Mesh[]> {
  return (await apiFetch("/api/meshes")).json();
}

export async function listProviders(): Promise<Provider[]> {
  return (await apiFetch("/api/providers")).json();
}

export async function createNode(req: CreateNodeRequest): Promise<AgentNode> {
  const resp = await apiFetch("/api/nodes/create", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ rows: 24, cols: 80, ...req }),
  });
  return resp.json();
}

// --- Stage 4: review & ship -------------------------------------------------

// Generated from the Rust `GitStatus` struct (issue #359); the mobile
// `/git/status` route serialises the same struct the desktop uses. Previously
// hand-declared as 2 fields ({path, status}), which dropped the line-count
// fields the wire sends and mislabelled `status` as a porcelain code (it is a
// word: "modified" | "added" | "deleted" | "renamed" | "untracked").
import type { GitStatus } from "../types/generated/GitStatus";
export type GitStatusEntry = GitStatus;

export interface GitSummary {
  added: number;
  modified: number;
  deleted: number;
}

export interface DiffLine {
  line_type: string; // "add" | "remove" | "context"
  content: string;
  old_line_no: number | null;
  new_line_no: number | null;
}

export interface DiffHunk {
  old_start: number;
  old_lines: number;
  new_start: number;
  new_lines: number;
  lines: DiffLine[];
  old_highlighted: string;
  new_highlighted: string;
}

export interface FileDiff {
  path: string;
  hunks: DiffHunk[];
}

export interface DiffResult {
  files: FileDiff[];
}

export async function gitStatus(agentId: number): Promise<GitStatusEntry[]> {
  return (await apiFetch(`/api/agents/${agentId}/git/status`)).json();
}

export async function gitSummary(agentId: number): Promise<GitSummary> {
  return (await apiFetch(`/api/agents/${agentId}/git/summary`)).json();
}

export async function gitBranch(agentId: number): Promise<{ branch: string }> {
  return (await apiFetch(`/api/agents/${agentId}/git/branch`)).json();
}

export async function diffFile(
  agentId: number,
  filePath: string,
): Promise<DiffResult> {
  const q = `path=${encodeURIComponent(filePath)}`;
  return (await apiFetch(`/api/agents/${agentId}/diff?${q}`)).json();
}

export async function ghAuthOk(): Promise<boolean> {
  const j = (await (await apiFetch("/api/gh/auth")).json()) as { ok: boolean };
  return j.ok;
}

export async function createPr(
  meshId: number,
  title: string,
  body: string,
  baseBranch: string,
): Promise<{ url: string }> {
  const resp = await apiFetch(`/api/meshes/${meshId}/pr`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ title, body, base_branch: baseBranch }),
  });
  return resp.json();
}

// --- Stage 5: kick off new tasks --------------------------------------------

// Generated from the Rust `ArchivedAgentNode` struct (issue #359 + #490;
// renamed to `ArchivedAgentNode` after PR #523 set the visible label to
// "Archive"). The mobile SPA previously hand-declared `cli_session_id`/
// `last_active_at`/`provider` — none of which the wire sends (it sends
// `session_id` and `timestamp`, and no provider field at all), so those
// reads were always `undefined`. The generated type is the real wire
// shape. Renamed from `DiscoveredAgentNode` in issue #490: the public
// surface uses "Agent Node"; the on-disk Claude Code session id
// (`session_id`) stays as-is.
import type { ArchivedAgentNode } from "../types/generated/ArchivedAgentNode";
export type { ArchivedAgentNode };

// Generated from the Rust `GitHubIssue` struct (src-tauri/src/commands/pr.rs) —
// the same struct the desktop Tauri path serialises (issue #359). #358 widened
// the struct to carry url/state/labels (guaranteed present — the upstream
// `services::github::Issue` deserialiser uses `#[serde(default)]`), so the
// generated type now includes them and IssuesScreen renders the labels and the
// "View ↗" link directly. No hand-maintained interface, no drift.
import type { GitHubIssue } from "../types/generated/GitHubIssue";
export type { GitHubIssue };

export async function discoverAgentNodes(
  meshId: number,
): Promise<ArchivedAgentNode[]> {
  return (await apiFetch(`/api/meshes/${meshId}/agent-nodes/discover`)).json();
}

export async function importAndResume(
  meshId: number,
  session: ArchivedAgentNode,
  provider?: string,
): Promise<AgentNode> {
  const resp = await apiFetch(
    `/api/meshes/${meshId}/agent-nodes/import-and-resume`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        // The request key stays `cli_session_id` (the backend's expected body
        // field); the source is the session's `session_id` (issue #359 — the
        // mobile type used to read a non-existent `cli_session_id`).
        cli_session_id: session.session_id,
        branch: session.branch ?? "main",
        worktree_name: session.worktree_name ?? undefined,
        provider,
      }),
    },
  );
  // 207 means node created but spawn failed — surface as a throw the
  // caller can catch and report. The node is still in the DB.
  if (resp.status === 207) {
    const partial = (await resp.json()) as { node: AgentNode; spawn_error: string };
    throw new Error(
      `Imported but spawn failed: ${partial.spawn_error} (node ${partial.node.id})`,
    );
  }
  return resp.json();
}

export async function listIssues(meshId: number): Promise<GitHubIssue[]> {
  return (await apiFetch(`/api/meshes/${meshId}/issues`)).json();
}

export async function spawnFromIssue(
  meshId: number,
  issue: GitHubIssue,
  provider?: string,
): Promise<AgentNode> {
  // Backend derives the issue URL from the mesh's `origin` remote — we only
  // ship the title hint, not the body (avoids pushing a multi-KB markdown
  // blob through the Windows PowerShell -EncodedCommand argv path).
  const resp = await apiFetch(
    `/api/meshes/${meshId}/issues/${issue.number}/spawn`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        title: issue.title,
        provider,
      }),
    },
  );
  return resp.json();
}

// Generated from the Rust `WsTicket` / `WsTarget` structs — the body of, and the
// request to, POST /api/ws-ticket (issues #500, #551). No hand-declared
// interface, no drift.
import type { WsTicket } from "../types/generated/WsTicket";
import type { WsTarget } from "../types/generated/WsTarget";

/// Mint a single-use WebSocket handshake ticket (issue #500). The long-lived
/// token never rides the WS URL; instead this cookie-authenticated POST returns
/// a short-lived ticket the upgrade carries as `?ticket=`. The ticket is bound
/// at mint time to the `target` surface/node the caller will open (issue #551),
/// so a leaked ticket can only unlock that one target — the upgrade rejects any
/// other. A malformed/absent target is a 400; a bad cookie a 401/403, which
/// `isAuthError` turns into a bounce back to Connect.
export async function mintWsTicket(target: WsTarget): Promise<string> {
  const resp = await apiFetch("/api/ws-ticket", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(target),
  });
  const j = (await resp.json()) as WsTicket;
  return j.ticket;
}

// --- 429 back-off for ticket mint (issue #552) ---------------------------
//
// The HTTP layer caps `/api/ws-ticket` mints at 30/token/minute so a
// sustained flood on a valid token cannot churn the in-memory ticket map.
// A legitimate phone — say, one that reconnects in a tight loop because
// of a VPN flap — can still hit that cap. A 429 from the mint is not the
// same as a connectivity failure: it carries a server-suggested
// `Retry-After` (seconds). We honour that hint with a single brief retry
// (issue #552 AC: "brief back-off and surfaces a non-alarming toast; does
// not loop"), then surface a regular `ApiError(429)` so the caller can
// fall back to its usual reconnect/error path.

/// Cap how long we'll wait before retrying — a `Retry-After` of 60s would
/// freeze the UI. Two seconds is the smallest interval the server could
/// realistically ask for (cap-exhaustion usually reports ~55s, but a
/// fresh window opening up will frequently report a sub-second hint)
/// while still feeling responsive. The intent is "take the hint, but
/// don't ever make the user wait through the full window".
const MAX_RETRY_AFTER_MS = 2000;

/// True iff `e` is an `ApiError` carrying the 429 status. Mirrors
/// `isAuthError` so callers can branch.
export function isRateLimited(e: unknown): boolean {
  return e instanceof ApiError && e.status === 429;
}

/// Parse a `Retry-After` header value per RFC 7231 §7.1.3: an integer
/// (delta-seconds) is the only shape the server emits. Anything else
/// (HTTP-date, malformed) returns `null` and the caller falls back to a
/// small default.
function parseRetryAfter(value: string | null): number | null {
  if (!value) return null;
  const n = Number.parseInt(value, 10);
  if (!Number.isFinite(n) || n < 0) return null;
  return n;
}

/// Tiny sleep helper. Kept local so the test can stub the global
/// `setTimeout` (or skip it via `vi.useFakeTimers`) and the back-off is
/// fully deterministic.
function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/// Mint a WS ticket, retrying exactly once on 429 with a brief back-off
/// capped at [`MAX_RETRY_AFTER_MS`]. The retry never loops — if the
/// second attempt is also 429, throws `ApiError(429)` whose message is the
/// toast text the SPA surfaces.
///
/// On a 429, waits the server's `Retry-After` hint (bounded) before
/// retrying — gives the rate-limit window the best chance to drain. If
/// the hint is absent or unparseable, falls back to a small default
/// (`FALLBACK_RETRY_MS`).
///
/// Accepts an `options.sleep` override so unit tests can plug in a
/// promise that resolves immediately without touching the global
/// setTimeout clock — the same effect as `vi.useFakeTimers().advanceTimersByTime(...)`,
/// but with zero risk of another test bleeding fake-timer state.
interface MintOptions {
  sleep?: (ms: number) => Promise<void>;
}
const FALLBACK_RETRY_MS = 800;

/// One fetch + one classifier — the cheap half. Body parse on 200 happens
/// here too so the public surface can `await` it without further branching.
async function fetchMintTicket(target: WsTarget): Promise<Response | string> {
  const resp = await fetch("/api/ws-ticket", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    credentials: "include",
    body: JSON.stringify(target),
  });
  if (resp.status === 429) return resp; // carry the 429 (with headers) to the orchestrator
  if (!resp.ok) {
    const detail = await safeJsonError(resp);
    throw new ApiError(resp.status, detail || fallbackMessage(resp.status));
  }
  const j = (await resp.json()) as WsTicket;
  return j.ticket;
}

export async function mintWsTicketWithBackoff(
  target: WsTarget,
  options: MintOptions = {},
): Promise<string> {
  const snooze = options.sleep ?? sleep;
  const first = await fetchMintTicket(target);
  if (typeof first === "string") return first;
  // first is the 429 Response — honour its `Retry-After`, wait, retry
  // exactly once. NOT a loop.
  const hint = parseRetryAfter(first.headers.get("Retry-After"));
  const waitMs =
    hint === null ? FALLBACK_RETRY_MS : Math.min(hint * 1000, MAX_RETRY_AFTER_MS);
  await snooze(waitMs);
  const second = await fetchMintTicket(target);
  if (typeof second === "string") return second;
  // second 429 still — surface the canonical toast text so the SPA renders
  // it uniformly across terminal & events surfaces.
  throw new ApiError(429, "Server is busy — reconnecting.");
}

/// Best-effort JSON `{"error":"..."}` extraction that mirrors `apiFetch`'s
/// pattern. Shared with `fetchMintTicket` and `apiFetch` to keep the
/// failure-shape consistent.
async function safeJsonError(resp: Response): Promise<string> {
  try {
    const j = (await resp.json()) as { error?: unknown };
    if (typeof j?.error === "string") return j.error;
  } catch {
    /* non-JSON body */
  }
  return "";
}

/// Build the WS URL for a given node id. Mints a fresh ticket per connection
/// (single-use), bound to this node's terminal surface (issue #551), and prefers
/// the page's own host:port so the SPA works regardless of which fallback port
/// (1992/1993/1994) the server bound to.
///
/// The mint goes through [`mintWsTicketWithBackoff`] so a 429 from a brief
/// reconnect storm (issue #552) is handled with one bounded wait inside this
/// helper — callers don't see the transient 429 and don't have to plumb
/// retry logic into the screen's reconnect machinery.
export async function terminalWsUrl(nodeId: number): Promise<string> {
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  const host = window.location.host; // includes :port
  const ticket = await mintWsTicketWithBackoff({
    surface: "terminal",
    node_id: nodeId,
  });
  return `${proto}//${host}/ws/terminal/${nodeId}?ticket=${encodeURIComponent(ticket)}`;
}

export async function eventsWsUrl(): Promise<string> {
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  const host = window.location.host;
  const ticket = await mintWsTicketWithBackoff({
    surface: "events",
    node_id: null,
  });
  return `${proto}//${host}/ws/events?ticket=${encodeURIComponent(ticket)}`;
}

export type EventMsg =
  | { type: "attention-needed"; session_id: number }
  | { type: "attention-cleared"; session_id: number };

export { ApiError };
