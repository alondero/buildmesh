// Shared types and HTTP client for the mobile SPA.
//
// Authentication: on the initial `/v2?token=ROOT` load the server sets an
// HttpOnly bm_session cookie. Subsequent fetches send it automatically via
// `credentials: "include"`. The raw token stays in localStorage as a
// fallback for the WS handshake (some proxies strip cookies there) and so
// the browser can re-authenticate after the cookie expires.

const TOKEN_STORAGE_KEY = "buildmesh_token";

export interface Mesh {
  id: number;
  name: string;
  path: string;
  created_at: string;
}

export interface AgentNode {
  id: number;
  mesh_id: number;
  name: string;
  path: string;
  branch: string | null;
  provider: string;
  status: NodeStatus;
  cli_session_id: string | null;
  created_at: string;
}

export type NodeStatus =
  | "idle"
  | "running"
  | "suspended"
  | "error"
  | "awaiting_input"
  | "archived";

export interface Provider {
  id: string;
  label: string;
  color: string;
  icon: string;
}

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
    throw new ApiError(resp.status, `API ${resp.status} on ${path}`);
  }
  return resp;
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

export interface GitStatusEntry {
  path: string;
  status: string; // M / A / D / ?? / etc — git porcelain code
}

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

export interface DiscoveredSession {
  cli_session_id: string;
  first_message: string | null;
  branch: string | null;
  worktree_name: string | null;
  last_active_at: string | null;
  provider: string;
}

export interface GitHubIssue {
  number: number;
  title: string;
  body: string;
  url: string;
  state: string;
  labels: string[];
}

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
        cli_session_id: session.cli_session_id,
        branch: session.branch ?? "main",
        worktree_name: session.worktree_name ?? undefined,
        provider: provider ?? session.provider,
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
  const resp = await apiFetch(
    `/api/meshes/${meshId}/issues/${issue.number}/spawn`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        title: issue.title,
        body: issue.body ?? "",
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
