// Shared types and HTTP client for the mobile SPA.
//
// Pairing exchanges a short-lived invitation for a persistent HttpOnly cookie.
// The paired device token stays in `localStorage` as the recovery credential:
// the cookie is the session, but the token is the only credential JavaScript
// can re-present, so a browser that loses the cookie while its device is still
// paired can re-mint one instead of forcing a fresh re-pair (issue #1577).
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

import type { CreateNodeRequest } from '../types/generated/CreateNodeRequest';
export type { CreateNodeRequest };

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

/**
 * The in-flight `POST /api/session` recovery refresh, or `null` when none is
 * running.
 *
 * Screens fan several requests out at once (the changes screen loads status,
 * summary, branch and the GitHub probe together), so a lost cookie arrives as
 * a burst of 401s. They all join this one promise instead of each minting their
 * own cookie — one refresh, one replay per caller, one device row.
 */
let sessionRecovery: Promise<RecoveryOutcome> | null = null;

/**
 * What one recovery refresh attempt concluded.
 *
 * Only `refreshed` buys a replay. The two failure outcomes stay distinct
 * because only one of them says anything about the stored credential:
 *
 * - **rejected** — the server refused the token (401/403), or there was none to
 *   present. The caller's original 401 stands, so the screen may clear the
 *   credential and show Connect.
 * - **unavailable** — the refresh never reached a verdict: the desktop app was
 *   unreachable, rate-limited or erroring. Nothing is known about the token, so
 *   the caller surfaces this instead of the 401 and the credential survives —
 *   issue #1577 clears local credentials *only* when the refresh is rejected.
 */
type RecoveryOutcome =
  | { kind: "refreshed" }
  | { kind: "rejected" }
  | { kind: "unavailable"; error: ApiError };

/**
 * Re-mint the session cookie from the saved device token, coalescing
 * concurrent callers onto a single request.
 *
 * Never rejects: every failure is a value, so the shared promise cannot strand
 * a concurrent caller on an unhandled rejection.
 *
 * The token rides an `Authorization` header on this one refresh call and never
 * a URL — `exchangeSession` is the same code path the boot-time migration uses
 * (issue #500).
 */
function recoverSession(): Promise<RecoveryOutcome> {
  const token = readStoredToken();
  if (!token) return Promise.resolve({ kind: "rejected" });
  sessionRecovery ??= exchangeSession("/api/session", token)
    .then((minted): RecoveryOutcome =>
      // `exchangeSession` resolves `false` for a refusal and throws only for a
      // failure that is not a refusal, which is what keeps the two apart.
      minted ? { kind: "refreshed" } : { kind: "rejected" },
    )
    .catch((e: unknown): RecoveryOutcome => ({
      kind: "unavailable",
      error: e instanceof ApiError ? e : new ApiError(0, fallbackMessage(0)),
    }))
    .finally(() => {
      // Clear before the next caller can observe the settled promise, so a
      // later 401 gets a fresh attempt instead of a cached rejection.
      sessionRecovery = null;
    });
  return sessionRecovery;
}

async function apiFetch(path: string, init?: RequestInit): Promise<Response> {
  return sendRequest(path, init, false);
}

/**
 * One request plus its single recovery replay.
 *
 * `retried` caps the replay at exactly one attempt: a refreshed cookie that
 * still 401s means the device itself was revoked, and retrying again would
 * loop. `init` is replayed verbatim, which is safe because every caller
 * serialises its body to a string before it gets here.
 *
 * Replaying a POST is side-effect-free because the server denies at the auth
 * boundary before dispatching a handler: an `Admin` route returns its 401 from
 * `router::dispatch_matched` without ever reaching `run_handler`, and the
 * public `/api/ws-ticket` route authorizes before it mints. A 401 therefore
 * means the request did nothing, so the retry cannot double-apply it.
 *
 * Only 401 recovers. A 403 means the device authenticated and was refused for
 * a scope it does not hold — a refresh cannot grant it, so it stays an error.
 */
async function sendRequest(
  path: string,
  init: RequestInit | undefined,
  retried: boolean,
): Promise<Response> {
  // Auth is the HttpOnly bm_session cookie, sent automatically via
  // credentials:include. The token never rides the URL (issue #500); the
  // cookie is minted by `login()` (POST /api/pair) and refreshed on reconnect.
  const resp = await fetch(path, {
    credentials: "include",
    ...init,
  });
  if (resp.ok) return resp;
  if (resp.status === 401 && !retried) {
    const outcome = await recoverSession();
    if (outcome.kind === "refreshed") return sendRequest(path, init, true);
    // A refresh that never reached a verdict proves nothing about the cookie or
    // the stored token. Surface its own failure so the screen takes its ordinary
    // offline/error path — `NodeList` routes any non-auth error to `onOffline()`
    // — instead of reading this as a revoked device (issue #1577).
    if (outcome.kind === "unavailable") throw outcome.error;
    // Rejected: fall through and re-raise the original 401, which is the truth.
  }
  // Routes report failures as {"error": "..."} — surface that text to the
  // user instead of a bare status code when it's available.
  const detail = await safeJsonError(resp);
  throw new ApiError(resp.status, detail || fallbackMessage(resp.status));
}

export function isAuthError(e: unknown): boolean {
  return e instanceof ApiError && (e.status === 401 || e.status === 403);
}

/** Exchange a single-use desktop invitation. The credential stays in a cookie. */
export async function login(token: string): Promise<boolean> {
  return exchangeSession("/api/pair", token);
}

/**
 * Refresh the HttpOnly cookie; migrate an old per-device token once.
 *
 * A successful migration KEEPS the token: it is the same device credential the
 * cookie now carries, and retaining it is what lets a later cookie loss
 * recover instead of demanding a fresh re-pair (issue #1577). The token is
 * dropped only when the server rejects it — a revoked device must not keep a
 * credential local — and survives a network failure, where nothing is yet known
 * about its validity.
 */
export async function restoreSession(): Promise<boolean> {
  const restored = await exchangeSession("/api/session");
  if (restored) return true;
  const legacy = readStoredToken();
  if (!legacy) return false;
  const migrated = await exchangeSession("/api/session", legacy);
  if (!migrated) clearStoredToken();
  return migrated;
}

async function exchangeSession(path: string, token?: string): Promise<boolean> {
  let resp: Response;
  try {
    resp = await fetch(path, {
      method: "POST",
      headers: token ? { Authorization: `Bearer ${token}` } : undefined,
      credentials: "include",
    });
  } catch {
    // Network-level failure (app down / wrong network) — distinct from a
    // rejected token, so surface it as a throw the caller turns into the
    // "can't reach the desktop app" message.
    throw new ApiError(0, fallbackMessage(0));
  }
  if (resp.status === 401 || resp.status === 403) return false;
  if (!resp.ok) throw new ApiError(resp.status, fallbackMessage(resp.status));
  return resp.status === 204;
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

export const launchConfigurationApi = {
  list: async (): Promise<import('../types/generated/SpawnConfiguration').SpawnConfiguration[]> => (await apiFetch('/api/launch-configurations')).json(),
  targets: async (): Promise<import('../types/generated/LaunchTarget').LaunchTarget[]> => (await apiFetch('/api/launch-targets')).json(),
  save: async (value: import('../types/generated/SpawnConfiguration').SpawnConfiguration, route?: import('../types/generated/ProviderPairing').ProviderPairing): Promise<import('../types/generated/SpawnConfiguration').SpawnConfiguration> =>
    (await apiFetch('/api/launch-configurations/save', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ ...value, route }) })).json(),
  verify: async (value: import('../types/generated/SpawnConfiguration').SpawnConfiguration, route?: import('../types/generated/ProviderPairing').ProviderPairing): Promise<import('../types/generated/PairingVerification').PairingVerification> =>
    (await apiFetch('/api/launch-configurations/verify', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ ...value, route }) })).json(),
  remove: async (id: string): Promise<void> => { await apiFetch('/api/launch-configurations/delete', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ id }) }); },
};

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

// Issue #2024 rank 10 — the diff types are the generated ones, not
// hand-declared. The hand-written interfaces below were wrong about the
// field names (`old_line_no`/`new_line_no` instead of the wire's
// `old_num`/`new_num`) and, worse, dropped `FileDiff.status`, `old_path`,
// `additions`, `deletions` and `binary` entirely. A changed binary file
// carries no text hunks, so the screen could only see "zero hunks" and
// announced "No diff (file matches HEAD)" for a file that demonstrably
// changed. Consuming the generated type makes the missing signal visible
// at compile time instead of at runtime.
import type { DiffLine } from "../types/generated/DiffLine";
import type { DiffHunk } from "../types/generated/DiffHunk";
import type { FileDiff } from "../types/generated/FileDiff";
import type { DiffResult } from "../types/generated/DiffResult";
// Issue #2024 rank 4 — PR wire types are generated too, so the client
// cannot drift from the Rust structs the routes serialise.
import type { PrSource } from "../types/generated/PrSource";
import type { CreatePrResult } from "../types/generated/CreatePrResult";
export type { DiffLine, DiffHunk, FileDiff, DiffResult };

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

export async function listPulls(meshId: number): Promise<import('../types/generated/GitHubPullRequestFeed').GitHubPullRequestFeed> {
  const feed = await (await apiFetch(`/api/meshes/${meshId}/pulls?state=open`)).json() as import('../types/generated/GitHubPullRequestFeed').GitHubPullRequestFeed;
  if (!Array.isArray(feed?.items) || typeof feed.completeness?.complete !== 'boolean') {
    throw new Error('The desktop sent an unexpected pull requests response. Update Buildmesh or refresh to retry.');
  }
  return feed;
}

export async function prMergeability(meshId: number, number: number): Promise<import('../types/generated/PrMergeability').PrMergeability> {
  const result = await (await apiFetch(`/api/meshes/${meshId}/pulls/${number}/mergeability`)).json() as import('../types/generated/PrMergeability').PrMergeability;
  if (!result || (result.mergeable !== null && typeof result.mergeable !== 'boolean') || typeof result.mergeable_state !== 'string') {
    throw new Error('The desktop sent an unexpected mergeability response. Refresh before trying again.');
  }
  return result;
}

export async function squashMerge(meshId: number, number: number, url: string): Promise<void> {
  await apiFetch(`/api/meshes/${meshId}/pulls/${number}/merge`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ url }),
  });
}

/**
 * Preview the source/base pair a create-PR would use.
 *
 * Issue #2024 rank 4 / #1567: the sheet used to show the node's branch as
 * the source while the request itself resolved the *mesh root*, so the
 * display and the result disagreed. Both now come from the same backend
 * resolver, called with the node's id.
 */
export async function prSource(meshId: number, nodeId: number): Promise<PrSource> {
  const resp = await apiFetch(
    `/api/meshes/${meshId}/pr/source?node_id=${nodeId}`,
  );
  return resp.json();
}

/**
 * Create a PR from an agent node's worktree.
 *
 * `nodeId` is required: without it the backend could only resolve
 * `mesh.path`, which published the mesh root's branch — a `main -> main`
 * PR, or the wrong feature branch entirely (issue #2024 rank 4 / #1567).
 *
 * `expectedHead` pins the branch that was previewed; if the worktree moved
 * in between, the backend rejects rather than silently publishing
 * something the user never saw. Omitting `baseBranch` lets the mesh's own
 * `base_ref` decide, so the client never has to assume `main`.
 */
export async function createPr(
  meshId: number,
  nodeId: number,
  title: string,
  body: string,
  baseBranch?: string,
  expectedHead?: string,
): Promise<CreatePrResult> {
  const payload: Record<string, unknown> = { title, body, node_id: nodeId };
  if (baseBranch) payload.base_branch = baseBranch;
  if (expectedHead) payload.head_branch = expectedHead;
  const resp = await apiFetch(`/api/meshes/${meshId}/pr`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
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

export async function listIssues(meshId: number): Promise<import('../types/generated/GitHubIssueFeed').GitHubIssueFeed> {
  return (await apiFetch(`/api/meshes/${meshId}/issues`)).json();
}

export async function issueLabelCatalog(meshId: number): Promise<import('../types/generated/IssueLabelCatalog').IssueLabelCatalog> {
  return (await apiFetch(`/api/meshes/${meshId}/issue-labels`)).json();
}

export async function setIssueLabel(meshId: number, issueNumber: number, label: string, present: boolean): Promise<void> {
  await apiFetch(`/api/meshes/${meshId}/issues/${issueNumber}/label`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ label, present }),
  });
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
///
/// This cannot go through `apiFetch`: the 429 branch needs the response's
/// headers to reach `mintWsTicketWithBackoff`. It therefore rejoins the session
/// recovery seam explicitly rather than silently skipping it — a lost cookie
/// hits the terminal and events sockets first, and they are the surfaces a
/// phone reconnects to most often.
async function fetchMintTicket(
  target: WsTarget,
  retried = false,
): Promise<Response | string> {
  const resp = await fetch("/api/ws-ticket", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    credentials: "include",
    body: JSON.stringify(target),
  });
  if (resp.status === 429) return resp; // carry the 429 (with headers) to the orchestrator
  if (!resp.ok) {
    if (resp.status === 401 && !retried) {
      const outcome = await recoverSession();
      if (outcome.kind === "refreshed") return fetchMintTicket(target, true);
      if (outcome.kind === "unavailable") throw outcome.error;
    }
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

/// Best-effort JSON `{"error":"..."}` extraction that mirrors `sendRequest`'s
/// pattern. Shared with `fetchMintTicket` to keep the failure-shape consistent.
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

/// Fire a raw keystroke sequence into a node's PTY (issue #1377). The
/// triage deck's Approve/Reject chips use this to answer a permission prompt
/// without opening the terminal.
///
/// The previous implementation opened the full terminal WebSocket (which
/// does snapshot generation, broadcast subscription, and write-task
/// spawning) just to push two bytes (`"y\r"` / `"n\r"`) and closed it again.
/// That meant every tap on Approve executed heavyweight machinery the call
/// had no use for, AND raced the server's read loop with the client's
/// immediate close — the `ws.send()` could land before the server entered
/// its read loop, silently dropping the keystroke while the user saw a
/// green "Sent ✓".
///
/// The HTTP `POST /api/nodes/{id}/input` route is the clean replacement:
///   * one round-trip, no snapshot, no broadcast subscription, no spawned task
///   * the 200 OK response IS the delivery proof — the bytes hit the PTY
///     before the response is written
///   * the server-side attention autoclear (a CR/LF in the payload) runs
///     through the same code path the WS does, so `y\r` flips
///     `awaiting_input` exactly as a typed Enter would
///
/// The auth path is the same HttpOnly `bm_session` cookie every other
/// mobile API call rides, so `isAuthError` handles 401/403 the same way it
/// does for `createPr` / `spawnFromIssue`. A 503 (PTY not running — process
/// killed, spawn failed) bubbles up as `ApiError(503)` whose message is the
/// toast the user sees; a 404 (node deleted mid-tap) is the same shape.
export async function sendNodeKeys(nodeId: number, seq: string): Promise<void> {
  if (!seq) {
    throw new Error("seq must be non-empty");
  }
  await apiFetch(`/api/nodes/${nodeId}/input`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ seq }),
  });
}

// The wire shape is generated from the Rust `http::events::EventMsg` enum
// (ts-rs) — never hand-declare it (CLAUDE.md shared-types rule).
export type EventMsg = import("../types/generated/EventMsg").EventMsg;

export { ApiError };
