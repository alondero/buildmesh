---
name: verify-ui
description: Autonomously verify a visible UI change in the real running app and capture before/after screenshots for the PR — CDP-driven Playwright against the dev-profile window
---

# `/verify-ui` — UI verification with screenshots

## When to use

The change affects something **visible in the UI** (desktop app or mobile SPA) and you want to (a) prove the feature works by driving the real app, and (b) capture before/after screenshots to embed in the PR. Complements `/verify` (build/lint/test/log-scan): run `/verify` first for the green bar, then this for the visual evidence.

**Two paths, pick by host:**

- **Windows (full fidelity, default).** The harness attaches Playwright to the real WebView2 window over the Chrome DevTools Protocol (CDP): real Tauri IPC, real Rust backend, real pixels. Use this whenever you're on Windows — it's the only path that proves the *backend* behaves.
- **Headless / non-Windows (`--mock`, visual smoke).** macOS/Linux WebViews have no CDP attach, and a headless host (Claude Code on the web, CI) has no WebView2 or backend at all — so the previous "skip screenshots and say so" gap. Instead, render the **real frontend** in the pre-installed headless Chromium against a plain Vite dev server, with a **fake Tauri IPC** injected before boot (`scripts/ui-mock/tauri-mock.mjs`). This proves the UI renders + reacts to fixture data and captures before/after PNGs; it does **not** exercise the Rust backend. Say "visual smoke (mock IPC), not backend-verified" in the PR. See *Headless mock mode* below.

## How it works (one paragraph)

`scripts\run-dev.ps1 -CdpPort 9223` builds **this working tree** into the dev profile (`buildmesh-dev.exe`, ports 2991/2992, own data dir) and launches it with WebView2's remote-debugging port open on `127.0.0.1:9223`. `scripts/ui-shot.mjs` then attaches Playwright to the real window: real Tauri IPC, real backend, real pixels. It can run a "steps" module (click/fill/assert with the Playwright `page` API) before saving a PNG. The stable hub (`buildmesh.exe`, ports 1991/1992) is never touched.

## Procedure

### 0. Preconditions

- Worktree has `node_modules` — if not: `npm ci`.
- Decide the shot: which screen, what fixture data it needs, and a CSS selector to crop to the changed region (full-window shots are huge and hide the change).

### 1. BEFORE screenshot (baseline)

Capture the baseline from the **pre-change tree**. Two cases:

- **You haven't changed code yet** (best): build + launch + shoot now, then start implementing.
- **Changes already made:** create an isolated detached worktree at the recorded pre-change commit with `git worktree add --detach <scratch-path> <base-commit>`; build/capture there. Preserve the active checkout and user edits. Do not commit solely to obtain a screenshot. If a baseline cannot run, report that gap. A new feature with no meaningful before-state may use after-only evidence.

### 2. Build + launch the changed tree

```powershell
powershell -File scripts\run-dev.ps1 -CdpPort 9223
```

Kills any previous `buildmesh-dev` (never the hub), rebuilds from the current tree, launches with CDP. Fresh launch each time = same window size = comparable screenshots.

### 3. Drive the feature + AFTER screenshot

Write a steps module (throwing fails the run — put your functional assertions here):

```js
// steps.mjs — context: { page, invoke }
export default async function ({ page, invoke }) {
  // Fixture data via the HTTP test bridge (port 2991; only the commands
  // routed in src-tauri/src/commands/test.rs are available):
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

## Headless mock mode (`--mock`) — non-Windows / web / CI

No WebView2, no CDP, no Rust backend needed. Renders the real desktop frontend in the pre-installed headless Chromium against a plain Vite dev server, with a fake Tauri IPC installed before the app boots. This is the path a Claude-Code-on-the-web (headless Linux) session takes when it can't drive the real window.

```bash
npm ci                      # if node_modules is missing (fresh web clone)
# One shot — self-hosts the dev server, screenshots, tears it down:
node scripts/ui-shot.mjs --out docs/pr-screenshots/<branch>/<slug>-after.png --mock --serve \
  --steps steps.mjs --selector "<css>"
```

- **Before/after** uses the isolated baseline checkout above. Start each Vite server from its intended checkout; record the commit and fixture used.
- **Fixtures.** `scripts/ui-mock/tauri-mock.mjs` seeds two meshes + agent nodes so the shell renders populated; unknown IPC commands resolve `null` (the screen renders empty, not a crash) and log `[tauri-mock] unmocked invoke: <cmd>` via `console.debug`, which avoids the frontend log bridge. Override with `--fixtures <file.mjs|json>` (merged over the defaults).
- **Steps** get `{ page, mock }` instead of `{ page, invoke }` — there's no HTTP bridge. Use `mock.on('cmd', value)` to set an IPC response and `mock.emit('event', payload)` to push a backend event to the app's listeners. Drive everything else through `page` (real Playwright clicks/asserts). A throwing steps module fails the run.
- **Look at the PNG yourself** (Read it) before attaching, same as always. Then say in the PR: *visual smoke via mock IPC — renders + reacts to fixtures, backend not exercised.*
- **Limits.** Anything that needs real backend output — terminal PTY content, a real git diff, live provider usage — will be blank/empty here. For those, the Windows CDP path is the only real verification; note the gap rather than claiming coverage.
- **Browser resolution.** On a host whose pre-installed Chromium doesn't match the build Playwright pins, the script falls back to `$PLAYWRIGHT_BROWSERS_PATH/chromium` (or `--chromium <path>` / `$BUILDMESH_CHROMIUM`). On a normal dev box the bundled browser is used with no flags.

## Mobile SPA changes (`src/mobile/`)

The mobile UI is served over plain HTTP on loopback — no CDP needed:

```powershell
# Token via the test bridge (NOTE: use node fetch — PowerShell's Invoke-RestMethod
# sends the body in a second TCP segment the bridge's single-read parser never sees):
$tok = node -e "fetch('http://127.0.0.1:2991/invoke',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({cmd:'get_root_token',args:{}})}).then(r=>r.json()).then(j=>console.log(j.data.token))"
node scripts/ui-shot.mjs --out docs/pr-screenshots/<branch>/mobile-after.png --url "http://127.0.0.1:2992/v2?token=$tok" --viewport 390x844
```

## Hard rules & pitfalls

- **NEVER** stop the `buildmesh` process or use ports 1991/1992/1420 — that's the stable hub **you may be running inside**. The CDP path only ever touches `buildmesh-dev` (2991/2992, CDP 9223). (Exception: `--mock` mode runs a throwaway Vite dev server on 1420 — safe on a headless web/CI host where no hub exists, but don't use `--mock` on a Windows box that's running the hub; use the CDP path there instead.)
- Playwright's webServer starts Vite on 1420. `--project=verify-smoke` uses mock IPC; chromium specs have additional runtime requirements. Read those specs before running them and never stop the stable hub to satisfy a test.
- CDP attach fails → the app wasn't launched with `-CdpPort` (plain `/use` launches without it) or the window was closed. Re-run step 2.
- Don't minimize the dev window while shooting — Chromium throttles hidden renderers.
- No Windows Firewall prompt should appear: everything binds loopback (the test bridge was moved off `0.0.0.0` for exactly this). If a prompt appears, a wildcard bind regressed — investigate, don't just dismiss it.
- The steps `invoke()` bridge only knows the commands routed in `src-tauri/src/commands/test.rs`. Anything else: drive it through the UI, or add a route there (and to this list of pitfalls if it bites you).
- Fixtures seeded via the bridge don't show up in the live window until the stores refetch — `await page.reload({ waitUntil: 'domcontentloaded' })` after seeding (most bridge handlers emit no frontend event). Vite's HMR WebSocket keeps network-idle open indefinitely.
- Talking to the bridge from PowerShell: `Invoke-RestMethod` hangs/fails against it (single-read parser vs 100-continue); use a `node -e "fetch(...)"` one-liner instead.
