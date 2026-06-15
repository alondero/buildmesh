// Shared types and HTTP client for the mobile SPA.
//
// Authentication: on the initial `/v2?token=ROOT` load the server sets an
// HttpOnly bm_session cookie. Subsequent fetches send it automatically via
// `credentials: "include"`. The raw token stays in localStorage as a
// fallback for the WS handshake (some proxies strip cookies there) and so
// the browser can re-authenticate after the cookie expires.

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
  // The cookie handles auth for browsers that obey credentials:include. We
  // also append ?token=... when available so the very first request after
  // a fresh page load (cookie not yet set) succeeds against the API.
  const token = readStoredToken();
  const url =
    token && !path.includes("token=")
      ? path + (path.includes("?") ? "&" : "?") + "token=" + encodeURIComponent(token)
      : path;
  const resp = await fetch(url, {
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

/// Probe a token against the API before committing to the full-page
/// ?token= reload — a bad token would otherwise dead-end on the server's
/// raw 401 page with no way back to the connect form.
export async function validateToken(token: string): Promise<boolean> {
  const resp = await fetch("/api/meshes?token=" + encodeURIComponent(token), {
    credentials: "include",
  });
  if (resp.status === 401 || resp.status === 403) return false;
  if (!resp.ok) throw new ApiError(resp.status, fallbackMessage(resp.status));
  return true;
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

// Generated from the Rust `DiscoveredSession` struct (issue #359). The mobile
// SPA previously hand-declared `cli_session_id`/`last_active_at`/`provider` —
// none of which the wire sends (it sends `session_id` and `timestamp`, and no
// provider field at all), so those reads were always `undefined`. The generated
// type is the real wire shape.
import type { DiscoveredSession } from "../types/generated/DiscoveredSession";
export type { DiscoveredSession };

// Generated from the Rust `GitHubIssue` struct (src-tauri/src/commands/pr.rs) —
// the same struct the desktop Tauri path serialises (issue #359). #358 widened
// the struct to carry url/state/labels (guaranteed present — the upstream
// `services::github::Issue` deserialiser uses `#[serde(default)]`), so the
// generated type now includes them and IssuesScreen renders the labels and the
// "View ↗" link directly. No hand-maintained interface, no drift.
import type { GitHubIssue } from "../types/generated/GitHubIssue";
export type { GitHubIssue };

export async function discoverSessions(
  meshId: number,
): Promise<DiscoveredSession[]> {
  return (await apiFetch(`/api/meshes/${meshId}/sessions/discover`)).json();
}

export async function importAndResume(
  meshId: number,
  session: DiscoveredSession,
  provider?: string,
): Promise<AgentNode> {
  const resp = await apiFetch(
    `/api/meshes/${meshId}/sessions/import-and-resume`,
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

/// Build the WS URL for a given node id. We prefer the page's own host:port
/// so the v2 SPA works regardless of which fallback port (1992/1993/1994)
/// the embedded server bound to — the legacy mobile_app.html hardcoded 1992.
export function terminalWsUrl(nodeId: number): string {
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  const host = window.location.host; // includes :port
  const token = readStoredToken() ?? "";
  return `${proto}//${host}/ws/terminal/${nodeId}?token=${encodeURIComponent(token)}`;
}

export function eventsWsUrl(): string {
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  const host = window.location.host;
  const token = readStoredToken() ?? "";
  return `${proto}//${host}/ws/events?token=${encodeURIComponent(token)}`;
}

export type EventMsg =
  | { type: "attention-needed"; session_id: number }
  | { type: "attention-cleared"; session_id: number };

export { ApiError };
