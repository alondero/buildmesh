/**
 * HTTP-based Tauri invoke for Playwright E2E tests.
 *
 * Problem: `invoke()` from `@tauri-apps/api/core` requires `window.__TAURI__`
 * which is only available inside a Tauri webview. Playwright navigates to
 * http://localhost:1420 (Vite dev server) via HTTP, never getting Tauri globals.
 *
 * Solution: The Rust `test` module spawns an HTTP server on port 1991 that
 * forwards JSON-RPC calls to `app.invoke()`. Use `invokeViaHttp()` instead of
 * `invoke()` in Playwright tests.
 */

const TEST_SERVER_URL = 'http://127.0.0.1:1991';

// Track created test projects for cleanup
const createdProjects: number[] = [];

export interface InvokeOptions {
  cmd: string;
  args?: Record<string, unknown>;
}

/**
 * Call a Tauri command via HTTP POST to the test server on port 1991.
 */
export async function invokeViaHttp<T = unknown>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  const response = await fetch(`${TEST_SERVER_URL}/invoke`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ cmd, args }),
  });

  if (!response.ok) {
    const text = await response.text();
    throw new Error(`HTTP ${response.status}: ${text}`);
  }

  const json = await response.json() as { ok: boolean; data?: T; error?: string };

  if (!json.ok) {
    throw new Error(json.error || 'Unknown error from test server');
  }

  return json.data as T;
}

/**
 * Check if the HTTP test server is running and responding.
 * Handles BOTH pure "OK" (correct) and raw HTTP response (legacy).
 */
export async function isTestServerReady(): Promise<boolean> {
  try {
    const response = await fetch(`${TEST_SERVER_URL}/health`, { method: 'GET' });
    if (!response.ok) return false;
    const text = await response.text();
    // The body should be "OK" or "OK\r\n" etc (pure body), but if it contains
    // the full HTTP response, we check for "OK" substring anywhere
    return text.trim() === 'OK' || text.includes('HTTP/1.1 200 OK');
  } catch {
    return false;
  }
}

/**
 * Poll `/health` until the test server binds its socket. Returns `false`
 * on timeout. `fetch` carries a 1 s `AbortSignal` so a half-open socket
 * after `taskkill` doesn't pin the loop for the full timeout.
 */
export async function waitForTestServer(timeout = 15000): Promise<boolean> {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`${TEST_SERVER_URL}/health`, { signal: AbortSignal.timeout(1000) });
      if (res.ok && (await res.text()).trim().startsWith('OK')) return true;
    } catch {
      // not ready
    }
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  return false;
}

/**
 * Tauri commands won't work until `setup()` has run and the exe is
 * listening on the test server port. Today that's the same check as
 * `waitForTestServer` — kept as a separate symbol so the two can
 * diverge if the readyness check ever grows.
 */
export async function waitForTauriReady(timeout = 30000): Promise<boolean> {
  return waitForTestServer(timeout);
}

/**
 * Poll a TCP port until a TCP probe (or `fetch /health`) succeeds.
 * `fetch` carries a 1 s `AbortSignal` so a half-open socket after
 * `taskkill` doesn't pin the loop for the full timeout.
 */
export async function waitForPort(host: string, port: number, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`http://${host}:${port}/health`, { signal: AbortSignal.timeout(1000) });
      if (res.ok) return true;
    } catch {
      // not ready
    }
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  return false;
}

/**
 * Inverse of `waitForPort`: returns `true` once the port refuses
 * connections. Replaces the historical `setTimeout(500)` "let the OS
 * release the port" guess in `beforeEach` after `taskkill`.
 */
export async function waitForPortClosed(host: string, port: number, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      await fetch(`http://${host}:${port}/health`, { signal: AbortSignal.timeout(1000) });
    } catch {
      return true;
    }
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  return false;
}

/**
 * Create a test session via HTTP.
 * Uses the real Rust backend via the test server on port 1991.
 */
export async function createTestSessionViaHttp(index: number): Promise<void> {
  const tauriReady = await waitForTauriReady(5000);
  if (!tauriReady) {
    throw new Error('Tauri backend not ready - ensure `npm run tauri dev` is running');
  }

  // Create a test project first (uses temp directory)
  const project = await invokeViaHttp<{ id: number; name: string; path: string; created_at: string }>(
    'create_test_mesh',
    { name: `Test Project ${index}` }
  );

  // Then create a session linked to that project
  await invokeViaHttp('create_agent_node', {
    meshId: project.id,
    name: `Session ${index}`,
    path: project.path,
    branch: 'main',
  });

  // Track for cleanup
  createdProjects.push(project.id);
}

/**
 * Cleanup all test projects and sessions created via createTestSessionViaHttp.
 * Call this in afterEach or afterAll to prevent test data from polluting the app.
 */
export async function cleanupTestProjects(): Promise<void> {
  for (const meshId of createdProjects) {
    try {
      await invokeViaHttp('delete_mesh', { meshId });
    } catch (e) {
      console.warn(`Failed to cleanup project ${meshId}:`, e);
    }
  }
  createdProjects.length = 0;
}

/**
 * Reset the created projects tracker (useful between test files).
 */
export function resetCreatedProjects(): void {
  createdProjects.length = 0;
}