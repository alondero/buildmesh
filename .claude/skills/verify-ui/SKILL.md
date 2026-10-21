---
name: verify-ui
description: Verify visible UI changes with mock IPC first, or the native dev profile when backend behavior is involved; capture functional and screenshot evidence for the PR
---

# `/verify-ui` — UI verification with screenshots

## When to use

The change affects something **visible in the UI** (desktop app or mobile SPA) and needs functional assertions and inspected screenshots. Complements `/verify` (build/lint/test/log-scan); mock checks are useful during iteration, and final completion still requires the applicable verification gates.

**Pick by the evidence the change needs:**

- **UI-only (`--mock`, first on every host).** Render the real frontend in Chromium with fixture IPC (`scripts/ui-mock/tauri-mock.mjs`), without a native build. Use a separate loopback port such as 3999. Assert the changed behavior, inspect the PNG and record elapsed time in the PR. See *Mock mode* below.
- **Backend / required real runtime (Windows CDP).** When backend behavior is part of the change or an acceptance criterion requires it, attach Playwright to the native dev-profile WebView2 window: real Tauri IPC and Rust backend. macOS/Linux WebViews have no CDP attach; report any missing real-runtime evidence there. Mock results prove frontend behavior with fixtures and cannot establish backend behavior.

## How it works (one paragraph)

For the native path, `scripts\run-dev.ps1 -Detach -CdpPort 9223 -TestBridge` builds **this working tree** into the dev profile (`buildmesh-dev.exe`, own data dir), launches with CDP on loopback, and enables the dev test bridge. From the main checkout those are 2992 (fallback 2993→2994) and 2991; from a linked worktree they are the `Dev ports:` line the launcher prints, because each worktree gets its own range so two can run at once. The bridge stays closed without `-TestBridge`. The launcher writes the bearer to `.tmp/test-bridge-token` and does not print it. `scripts/ui-shot.mjs` attaches Playwright to that window, sends `Authorization: Bearer` from that file, and runs functional steps before saving a PNG. The stable hub (`buildmesh.exe`, phone/Android on 1992) is never touched. The stable hub does not bind 1991.

## Procedure

### 0. Preconditions

- UI-only work takes the mock-mode procedure below first. The native procedure is for backend or required runtime evidence.
- Worktree has `node_modules` — if not: `npm ci`.
- Decide the shot: which screen, what fixture data it needs, and a CSS selector to crop to the changed region (full-window shots are huge and hide the change).

### 1. BEFORE screenshot (baseline)

Capture the baseline from the **pre-change tree**. Two cases:

- **You haven't changed code yet** (best): build + launch + shoot now, then start implementing.
- **Changes already made:** use [isolated review checks](../../../docs/agents/development-harness.md#isolated-review-checks) at the recorded pre-change commit; run the build and capture in one task-local script so they share the helper's checkout. A manually managed detached worktree is also valid. Preserve the active checkout and user edits. Do not commit solely to obtain a screenshot. If a baseline cannot run, report that gap. A new feature with no meaningful before-state may use after-only evidence.

### 2. Build + launch the changed tree

```powershell
powershell -File scripts\run-dev.ps1 -Detach -CdpPort 9223 -TestBridge
```

Kills any previous `buildmesh-dev` (never the hub), rebuilds from the current tree, launches with CDP. Fresh launch each time = same window size = comparable screenshots.

### 3. Drive the feature + AFTER screenshot

Write a steps module (throwing fails the run — put your functional assertions here):

```js
// steps.mjs — context: { page, invoke }
export default async function ({ page, invoke }) {
  // Fixture data via the HTTP test bridge (dev profile, port 2991).
  // `invoke` sends Authorization: Bearer from `.tmp/test-bridge-token`.
  // Launch with -TestBridge or the request is connection refused, and a
  // request without the bearer is 401. Only the commands routed in
  // src-tauri/src/commands/test.rs are available:
  const mesh = await invoke('create_test_mesh', { name: 'Shot fixture' });
  // Bridge writes go straight to the DB and (mostly) emit no frontend
  // events — reload so the stores refetch and the fixture appears:
  // Vite keeps its HMR WebSocket open, so network-idle never settles here.
  await page.reload({ waitUntil: 'domcontentloaded' });
  // Drive the real UI with Playwright:
  await page.locator('[title="New session"]').first().click();
  // Assert the feature works — a failed expectation throws → exit 1:
  await page.locator('text=Terminal').first().waitFor({ state: 'visible', timeout: 5000 });
}
```

```powershell
node scripts/ui-shot.mjs --out docs/pr-screenshots/<branch>/<slug>-after.png --steps steps.mjs --selector "<css>"
```

Use the **same steps + selector** for comparable before and after states. A saved PNG proves capture only; functional assertions must establish the requested behavior. For Probe changes exercise 240px width, including loading/error states and button bounds per `docs/development/probe-ui-checklist.md`. Then **look at the PNG yourself** (Read the file) — confirm it shows what you claim before attaching it to a PR.

Clean up any fixture data your steps created (`invoke('delete_mesh', { meshId })`) so repeat runs stay deterministic.

### 4. Scan the log

Inspect new lines in the dev-profile `buildmesh.log`, `panic.log`, and `panic_early.log`; any new panic-file content fails. Follow the runtime evidence rules in `../verify/SKILL.md`.

### 5. Attach evidence when publishing is authorized

Screenshots live in the repo so GitHub can render them (repo is public):

1. Keep PNGs small: crop with `--selector`, aim under ~300 KB each.
2. Commit them under `docs/pr-screenshots/<branch>/`.
3. Pin URLs to the commit SHA (`git rev-parse HEAD`) so they keep rendering after the branch is deleted post-merge:

```markdown
| Before | After |
|---|---|
| ![before](https://raw.githubusercontent.com/alondero/buildmesh/<sha>/docs/pr-screenshots/<branch>/<slug>-before.png) | ![after](https://raw.githubusercontent.com/alondero/buildmesh/<sha>/docs/pr-screenshots/<branch>/<slug>-after.png) |
```

### 6. Cleanup

Stop the dev-profile PID launched by this verification when done (or leave it up if the user wants to inspect it). Do not stop another session's runtime.

## Mock mode (`--mock`) — UI-only checks on every host

No WebView2, CDP or Rust build needed. Renders the real desktop frontend in Chromium against Vite with fixture IPC installed before boot. Use loopback port 3999 to keep it separate from other development runtimes.

```bash
npm ci                      # if node_modules is missing (fresh web clone)
# One shot — self-hosts the dev server, screenshots, tears it down:
node scripts/ui-shot.mjs --out docs/pr-screenshots/<branch>/<slug>-after.png --mock --serve --mock-url http://127.0.0.1:3999 \
  --steps steps.mjs --selector "<css>"
```

- **Before/after** uses the isolated baseline checkout above. Start each Vite server from its intended checkout; record the commit and fixture used.
- **Repeated checks.** Use `npm run dev:bg -- --port 3999`, omit `--serve` from shots against that URL, then `npm run dev:stop`. Record elapsed time and functional results in the PR. One-shot `--serve` stops only the server it starts.
- **Fixtures.** `scripts/ui-mock/tauri-mock.mjs` seeds two meshes + agent nodes so the shell renders populated; unknown IPC commands resolve `null` (the screen renders empty, not a crash) and log `[tauri-mock] unmocked invoke: <cmd>` via `console.debug`, which avoids the frontend log bridge. Override with `--fixtures <file.mjs|json>` (merged over the defaults).
- **Steps** get `{ page, mock }` instead of `{ page, invoke }` — there's no HTTP bridge. Use `mock.on('cmd', value)` to set an IPC response and `mock.emit('event', payload)` to push a backend event to the app's listeners. Drive everything else through `page` (real Playwright clicks/asserts). A throwing steps module fails the run.
- **Look at the PNG yourself** (Read it) before attaching, same as always. Then say in the PR: *visual smoke via mock IPC — renders + reacts to fixtures, backend not exercised.*
- **Limits.** Anything that needs real backend output — terminal PTY content, a real git diff, live provider usage — will be blank/empty here. For those, the Windows CDP path is the only real verification; note the gap rather than claiming coverage.
- **Browser resolution.** On a host whose pre-installed Chromium doesn't match the build Playwright pins, the script falls back to `$PLAYWRIGHT_BROWSERS_PATH/chromium` (or `--chromium <path>` / `$BUILDMESH_CHROMIUM`). On a normal dev box the bundled browser is used with no flags.

## Mobile SPA changes (`src/mobile/`)

The mobile UI is served over plain HTTP on loopback — no CDP needed:

```powershell
# Token via the dev-profile test bridge on 2991. The app must have been
# launched with -TestBridge, which writes .tmp/test-bridge-token.
# Use node fetch — PowerShell's Invoke-RestMethod sends the body in a
# second TCP segment the bridge's single-read parser never sees.
$tok = node -e "const fs=require('fs'); const token=fs.readFileSync('.tmp/test-bridge-token','utf8'); fetch('http://127.0.0.1:2991/invoke',{method:'POST',headers:{'Content-Type':'application/json',Authorization:'Bearer '+token},body:JSON.stringify({cmd:'get_root_token',args:{}})}).then(r=>r.json()).then(j=>{ if(!j.ok) throw new Error(j.error||'bridge rejected the token'); console.log(j.data.token); })"
node scripts/ui-shot.mjs --out docs/pr-screenshots/<branch>/mobile-after.png --url "http://127.0.0.1:2992/v2?token=$tok" --viewport 390x844
```

## Hard rules & pitfalls

- Keep the stable `buildmesh` process and its phone/Android port 1992 untouched. The test bridge (1991 stable, 2991 dev) is off unless that process was launched with the test-bridge switch. Use 3999 for mock Vite checks on every host. The CDP path uses `buildmesh-dev` (bridge 2991, phone/Android 2992, CDP 9223) launched with `-TestBridge`; stop only the PID launched for this verification.
- Playwright's webServer starts Vite on 1420. `--project=verify-smoke` uses mock IPC; chromium specs have additional runtime requirements. Read those specs before running them and never stop the stable hub to satisfy a test.
- CDP attach fails → the app wasn't launched with `-CdpPort` (plain `/use` launches without it) or the window was closed. Re-run step 2.
- Don't minimize the dev window while shooting — Chromium throttles hidden renderers.
- No Windows Firewall prompt should appear: everything binds loopback (the test bridge was moved off `0.0.0.0` for exactly this). If a prompt appears, a wildcard bind regressed — investigate, don't just dismiss it.
- The steps `invoke()` bridge only knows the commands routed in `src-tauri/src/commands/test.rs`. Anything else: drive it through the UI, or add a route there (and to this list of pitfalls if it bites you).
- Fixtures seeded via the bridge don't show up in the live window until the stores refetch — `await page.reload({ waitUntil: 'domcontentloaded' })` after seeding (most bridge handlers emit no frontend event). Vite's HMR WebSocket keeps network-idle open indefinitely.
- Talking to the bridge from PowerShell: `Invoke-RestMethod` hangs/fails against it (single-read parser vs 100-continue); use a `node -e "fetch(...)"` one-liner instead.
