---
name: verify-ui
description: Autonomously verify a visible UI change in the real running app and capture before/after screenshots for the PR — CDP-driven Playwright against the dev-profile window
---

# `/verify-ui` — UI verification with screenshots

## When to use

The change affects something **visible in the UI** (desktop app or mobile SPA) and you want to (a) prove the feature works by driving the real app, and (b) capture before/after screenshots to embed in the PR. Complements `/verify` (build/lint/test/log-scan): run `/verify` first for the green bar, then this for the visual evidence.

**Windows only.** The harness attaches Playwright to the real WebView2 window over the Chrome DevTools Protocol (CDP). macOS/Linux WebViews don't support CDP attach — on those hosts, skip screenshots and say so in the PR.

## How it works (one paragraph)

`scripts\run-dev.ps1 -CdpPort 9223` builds **this working tree** into the dev profile (`buildmesh-dev.exe`, ports 2991/2992, own data dir) and launches it with WebView2's remote-debugging port open on `127.0.0.1:9223`. `scripts/ui-shot.mjs` then attaches Playwright to the real window: real Tauri IPC, real backend, real pixels. It can run a "steps" module (click/fill/assert with the Playwright `page` API) before saving a PNG. The stable hub (`buildmesh.exe`, ports 1991/1992) is never touched.

## Procedure

### 0. Preconditions

- Worktree has `node_modules` — if not: `npm install`.
- Decide the shot: which screen, what fixture data it needs, and a CSS selector to crop to the changed region (full-window shots are huge and hide the change).

### 1. BEFORE screenshot (baseline)

Capture the baseline from the **pre-change tree**. Two cases:

- **You haven't changed code yet** (best): build + launch + shoot now, then start implementing.
- **Changes already made**: commit them first (`git status` must be clean), then:
  ```powershell
  git switch --detach (git merge-base HEAD origin/main)
  powershell -File scripts\run-dev.ps1 -CdpPort 9223
  node scripts/ui-shot.mjs --out docs/pr-screenshots/<branch>/<slug>-before.png --steps <steps.mjs> --selector "<css>"
  git switch -            # back to your branch
  ```
  This costs a second full build (~5 min). If the feature is brand-new UI (no meaningful "before"), skip the baseline and note that in the PR.

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
  await page.reload({ waitUntil: 'networkidle' });
  // Drive the real UI with Playwright:
  await page.locator('[title="New session"]').first().click();
  // Assert the feature works — a failed expectation throws → exit 1:
  await page.locator('text=Terminal').first().waitFor({ state: 'visible', timeout: 5000 });
}
```

```powershell
node scripts/ui-shot.mjs --out docs/pr-screenshots/<branch>/<slug>-after.png --steps steps.mjs --selector "<css>"
```

Use the **same steps + selector** for before and after. Exit 0 + PNG written = feature verified. Then **look at the PNG yourself** (Read the file) — confirm it shows what you claim before attaching it to a PR.

Clean up any fixture data your steps created (`invoke('delete_mesh', { meshId })`) so repeat runs stay deterministic.

### 4. Scan the log

New lines in `$env:APPDATA\com.alond.buildmesh.dev\logs\buildmesh.log` since launch: ` ERROR ` / `panic` = fail, fix before proceeding (same rules as `/verify` full tier).

### 5. Embed in the PR

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

`Stop-Process -Name buildmesh-dev -Force` when done (or leave it up if the user may want to poke at it).

## Mobile SPA changes (`src/mobile/`)

The mobile UI is served over plain HTTP on loopback — no CDP needed:

```powershell
# Token via the test bridge (NOTE: use node fetch — PowerShell's Invoke-RestMethod
# sends the body in a second TCP segment the bridge's single-read parser never sees):
$tok = node -e "fetch('http://127.0.0.1:2991/invoke',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({cmd:'get_root_token',args:{}})}).then(r=>r.json()).then(j=>console.log(j.data.token))"
node scripts/ui-shot.mjs --out docs/pr-screenshots/<branch>/mobile-after.png --url "http://127.0.0.1:2992/v2?token=$tok" --viewport 390x844
```

## Hard rules & pitfalls

- **NEVER** stop the `buildmesh` process or use ports 1991/1992/1420 — that's the stable hub **you may be running inside**. This skill only ever touches `buildmesh-dev` (2991/2992, CDP 9223).
- **Do not** reach for `npm run test:e2e` to "verify UI" — Playwright's webServer boots `tauri dev` on the hub's ports (1991/1992) and needs the hub paused. That suite is for humans/CI, not autonomous verification.
- CDP attach fails → the app wasn't launched with `-CdpPort` (plain `/use` launches without it) or the window was closed. Re-run step 2.
- Don't minimize the dev window while shooting — Chromium throttles hidden renderers.
- No Windows Firewall prompt should appear: everything binds loopback (the test bridge was moved off `0.0.0.0` for exactly this). If a prompt appears, a wildcard bind regressed — investigate, don't just dismiss it.
- The steps `invoke()` bridge only knows the commands routed in `src-tauri/src/commands/test.rs`. Anything else: drive it through the UI, or add a route there (and to this list of pitfalls if it bites you).
- Fixtures seeded via the bridge don't show up in the live window until the stores refetch — `await page.reload({ waitUntil: 'networkidle' })` after seeding (most bridge handlers emit no frontend event).
- Talking to the bridge from PowerShell: `Invoke-RestMethod` hangs/fails against it (single-read parser vs 100-continue); use a `node -e "fetch(...)"` one-liner instead.
