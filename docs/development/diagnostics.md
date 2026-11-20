# Diagnostics: the resource sampler, the DevTools port, and the capture

Status: current

## What this area owns

`src-tauri/src/diagnostics/` owns the resource sampler that runs with the app,
the size-bounded logs, and the **opt-in DevTools (CDP) port** the hub window can
open. `scripts/diag-capture.mjs` owns the capture that reads that port.

The two halves answer different questions, and a slow hub usually needs both:

- the **sampler** answers "what is this process holding?" — memory, handles,
  threads, the PTY queue, the SQLite reader pool — in `logs/diagnostics.log`;
- the **capture** answers "what is the renderer doing?" — the compositor
  trace, the layer and animation state, the heap, the frame gaps — from
  inside the WebView, where a reload would have destroyed it.

## The opt-in DevTools port

Some of the decisive data only exists inside the WebView: the compositor's
trace categories, the layer tree, the animation state, the heap. That data is
reachable only through the Chrome DevTools Protocol, and only when WebView2
was started with a debugging port. Without a port, the only remedy —
reloading the window — destroys the evidence.

The port is therefore an **opt-in startup switch**, off by default:

- **Settings → General → Diagnostics** holds the port. It is stored on
  `AppPreferences::diag_cdp_port` (`src-tauri/src/preferences/model.rs`), and
  `commands::preferences::set_app_diag_cdp_port` is the one command that
  writes it.
- The port is read when the **window is created**, so a change takes effect
  on the next launch. The settings row says which port is *open* and which
  port is *asked for*, because those differ until the app restarts.
- While the port is open, `DevtoolsPortBanner` says so under the title bar:
  any program on this computer can attach to the window and drive it.
  Chromium binds the DevTools endpoint to `127.0.0.1`, so the port is not
  reachable from the network.
- A debug or `dev-tools` build may also take the port from
  `BUILDMESH_DIAG_CDP_PORT`. A **shipped release build does not compile that
  read**: a user launch is not steerable by a variable it inherited, the same
  rule as `BUILDMESH_PROFILE_DIR` and `BUILDMESH_PORT_OFFSET`. The preference
  is the shipped opt-in.

### Why the environment variable, and not a window builder

WebView2 reads `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` when it creates the
browser environment. Tauri creates the config window — and with it that
environment — on the event loop's first `Ready` event, and it does so
**before** it runs the `setup` hook. A port resolved in `setup` would name a
port that no process would be listening on.

So the port is resolved in `lib.rs` between `Builder::build` and `App::run`,
which is the last point that is still before the window exists:
`diagnostics::cdp::apply_startup_port`. The preference is read straight off
disk for that one field (`preferences::storage::read_for_startup`), because
the preferences store is not installed until `setup`.

The repository already used this lever before the setting existed:
`scripts/run-dev.ps1 -CdpPort` sets the same variable, and
`scripts/ui-shot.mjs` attaches to the real dev-profile window over it.

### The rules that decide the port

`diagnostics::cdp::resolve` is a pure function, so the rules are testable
without a process environment:

1. A launcher that already put a port in the browser arguments keeps it. Two
   `--remote-debugging-port` arguments in one string would leave Chromium
   using the first one, so the *effective* port has to be the one that is
   already there — and the UI must not claim a different number.
2. `BUILDMESH_DIAG_CDP_PORT` wins over the preference: it is the more
   specific act for this launch.
3. The preference is the shipped opt-in.
4. With no port at all, **no argument is added** to the browser arguments. That
   is the "preference off, no port is open" case, and it is asserted on the
   arguments themselves in `src-tauri/src/diagnostics/cdp_tests.rs`.

## `npm run diag:capture`

The capture attaches to a running hub over CDP and writes a timestamped
bundle:

```powershell
npm run diag:capture                    # the stable hub
npm run diag:capture -- --dev           # the dev profile (port 9223)
npm run diag:capture -- --port 9222     # a specific port
npm run diag:capture -- --out .tmp/diag # where the bundle lands
npm run diag:capture -- --seconds 10    # a longer trace
```

The port the script uses is the hub's, not the script's: an explicit
`--port`, else the port the profile's own preference asks for, else the
profile's default (9222 for the stable hub, 9223 for the dev profile).

### Run the capture BEFORE any reload

A reload clears the state the capture is about — the layer tree, the
animation state, the frame gaps, the heap profile. **When the UI is slow,
capture first and reload afterwards** (if at all). The bundle is the
evidence; a reloaded window is not.

### What the bundle contains

| File | What it is |
|---|---|
| `trace.json` | the raw CDP trace events (5 s by default) |
| `summary.json` | the machine-readable summary: compositor self time per event name, the page probe, the build stamp |
| `README.md` | the same content, readable |
| `renderer.log` | the latest renderer (frontend) lines from `buildmesh.log` |
| `diag.log` | the latest `DIAG` lines from `diagnostics.log` |

The raw trace is the big one — five seconds of a busy compositor is tens of
megabytes of JSON. It is the evidence, so it stays; when the folder is too
big to hand over, `summary.json` and `README.md` carry the findings on their
own.

The trace uses the DevTools Performance panel's own categories — `cc`,
`viz`, `benchmark`, `toplevel`, `blink`, `animation`, `gpu` and the
`disabled-by-default-cc.debug*` / `disabled-by-default-devtools.timeline*`
families — which is why the panel shows a single `RunTask` where the trace
names the compositor steps it records.

The **compositor summary** reports one row per event name: the count, the
inclusive duration, and the **self time** (the duration minus the work
nested inside it). Self time is the number that names the step that actually
burned the time; a total-duration ranking would put the frame wrapper above
the steps it contains. The two events the acceptance names —
`Scheduler::BeginImplFrame` and `TileManager::PrepareTiles` — are the ones a
capture that missed the compositor will not have, and the script reports
them as missing when they are absent.

The **page probe** reads what the page says about itself: DOM size, the
running animations, the canvases, the JS heap (`performance.memory`), the
frame gaps (the time between animation frames, sampled after the trace so
the measurement does not disturb it), and the idle-gate state
(`data-window-idle` on the root element — see
[the idle animation gate](probe-ui.md)). The build stamp is the
`git_sha` from the sampler's `DIAG session_start` line, so the bundle names
the build that produced it.

### Verifying a capture

The capture needs a real window with the port open, so it is not a unit
test. The parts that can be tested are tested: the trace summary, the option
resolution, the build stamp and the page picker in
`tests/unit/diag-capture.test.ts`; the port resolution rules in
`src-tauri/src/diagnostics/cdp_tests.rs`; and the command's wire shape in
`src-tauri/src/commands/devtools_tests.rs`.

For a real capture, start the dev profile with a port
(`scripts\run-dev.ps1 -Detach -CdpPort 9223`), run `npm run diag:capture --
--dev`, and read the bundle's `README.md` — the compositor summary must name
`Scheduler::BeginImplFrame` and `TileManager::PrepareTiles`.

## Related

- [The Probe panel](probe-ui.md) — the in-app view of the same surfaces.
- [Windows](../development/windows.md) and
  [startup and profiles](../development/startup-and-profiles.md) — the
  frameless window and the per-worktree profile the capture reads its logs
  from.
