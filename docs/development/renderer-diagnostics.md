# Renderer diagnostics

Status: current

The renderer health line is a passive, always-on record of what the
Buildmesh **WebView** was doing: how long a keystroke waited to be
processed, how long the main thread was blocked, how big the page had grown,
and what the user was doing while it happened. It is written once every ten
seconds and lands in its own bounded, rotating file.

## Why it exists

The native process has an always-on resource timeline
(`logs/diagnostics.log`). The WebView — where the interface actually runs —
had none. In the session that produced this feature, typing lagged for days:
a keystroke delay median around 65 ms and frame gaps up to 183 ms, with
nothing recording it. Diagnosing it took hand-run console probes, and the
reload that fixed it destroyed the evidence.

The renderer line closes that gap. It is the record that survives the
reload.

## Where the line goes

| | |
|---|---|
| File | `logs/renderer-diagnostics.log`, beside `logs/diagnostics.log` |
| Size | One structured line every 10 seconds |
| Rotation | 8 MiB per file, one rotated generation kept |
| Kill switch | `BUILDMESH_DIAG=0` — the line is dropped, not redirected |

The line travels through the ordinary frontend log bridge
(`src/lib/frontendLog.ts` → the `log_frontend` Tauri command). The native
side recognises the `RENDER_DIAG` prefix and writes the line into the
renderer sink instead of the main log, so the renderer timeline can be handed
back on its own.

If the renderer sink cannot be opened — an unwritable log directory, say —
the line falls back to the main log rather than being lost. The one case
where it is dropped is the kill switch: a user who turned diagnostics off
should not get a line every ten seconds in the main log either.

Read the file with any text editor, or scan it with `grep`:

```powershell
Select-String -Path "$env:LOCALAPPDATA\com.alond.buildmesh\logs\renderer-diagnostics.log" -Pattern 'RENDER_DIAG'
```

## The line format

One line per sample, `key=value` fields, a stable `RENDER_DIAG` prefix.

```
RENDER_DIAG age_ms=10000 time_origin=1699999990000 vis=visible focus=1 idle=0 dom=1234 anim=running:12/paused:3/finished:0/idle:0/pending:0 anim_age_ms=0 heap_mb=45 term=3 webgl=1 ev_kb=n:2/p50:10/p90:30/max:30/proc_p50:2/proc_p90:5/proc_max:5/pres_p50:5/pres_p90:8/pres_max:8 ev_ptr=n:1/p50:5/p90:5/max:5/proc_p50:3/proc_p90:3/proc_max:3/pres_p50:8/pres_p90:8/pres_max:8 lf=n:1/total_ms:210/max_ms:210 act=view_switch:1/terminal_attach:2/terminal_detach:0/webgl_eviction:0/node_status:0/dialog:0/toast:0/focus_change:0 ipc=circuit-run-updated:3,agent-lifecycle:2,git-changed:1,node-created:1 cost_ms=0.12 lf_src=http://localhost:1420/src/busy.ts lf_invoker=BUTTON#onClick
```

Numeric blocks use `label:value` pairs separated by `/`, the same convention
the native `DIAG` line uses. Two fields differ because their value is free
text rather than a number: `ipc=` separates its event names with `,`, and
the script attribution is a top-level field (`lf_src=`, `lf_invoker=`)
because a script URL contains `/` and would otherwise be indistinguishable
from the field separator.

| Field | Meaning |
|---|---|
| `age_ms` | Milliseconds since the document started. A reload shows up here as a reset. |
| `time_origin` | `performance.timeOrigin` — the epoch milliseconds the document began at. |
| `vis` | `document.visibilityState`. |
| `focus` | Whether the OS window has focus. |
| `idle` | Whether the idle animation gate is closed. |
| `dom` | Elements in the document. |
| `anim` | Live animations by `playState`: `running`, `paused`, `finished`, `idle`, `pending`. |
| `anim_age_ms` | How old the animation census is. The census runs on its own slower cadence — see [Cost](#cost). |
| `heap_mb` | `performance.memory.usedJSHeapSize`, or `off` where the browser does not expose it. |
| `term` | Terminals currently attached to a DOM container. |
| `webgl` | Live WebGL renderers. |
| `ev_kb` | Keyboard interaction latency: count, input-delay p50/p90/max, processing-time p50/p90/max, presentation-delay p50/p90/max. |
| `ev_ptr` | The same, for pointer interactions. |
| `lf` | Long frames: count, total duration, longest single frame. |
| `lf_src`, `lf_invoker` | The script that occupied the longest frame, and what called it. |
| `act` | Activity counters since the previous line. |
| `ipc` | The five busiest IPC event names since the previous line, and their counts. |
| `cost_ms` | What this sample's own main-thread work cost. |

The three latency numbers per interaction kind are the three halves of the
same event: `processingStart - startTime` (how long the event waited before
the app touched it), `processingEnd - processingStart` (how long the handler
ran), and `startTime + duration - processingEnd` (how long before the frame
reached the screen).

Input delay and processing time are exact. **Presentation delay is
approximate**: Chromium rounds an Event Timing entry's `duration` to the
nearest 8 ms, so the presentation figure carries up to ±4 ms of error. That
is enough to see "the frame was late" and not enough to measure a 2 ms
regression — the long-frame figures are the precise ones.

Percentiles are **nearest-rank** — the value actually observed at that rank,
not an interpolation between two observations. A 200 ms busy loop shows up
as `lf=n:1/max_ms:200` with the script that ran it named in `lf_src`.

## What Event Timing does and does not report

Event Timing reports an interaction only when its total duration exceeds the
observer's `durationThreshold`, and **Chromium's floor for that threshold is
16 ms** — the API will not go lower. A keystroke the app handled in under
16 ms is therefore never reported at all.

That decides what a zero means, so it is worth stating plainly:

* `ev_kb=n:0` across a session of fast typing means **nothing was slow**,
  not that the observer was broken.
* A non-zero count means at least one interaction was slower than 16 ms —
  which is the pathology this line exists to catch (issue #2830 measured a
  65 ms median keystroke delay).

The same floor applies to the pointer split.

## What is counted, and where

The sampler's collection lives in `src/lib/diagnostics/rendererHealth.ts`.
The counters it reads are owned by the module that knows the fact:

| Counter | Owner | Commit point |
|---|---|---|
| View switches | `src/stores/uiStore.ts` | `setViewMode`, after its idempotency check |
| Terminal attach / detach | `src/components/Terminal/TerminalRegistry.ts`, `BuildRunTerminalRegistry.ts` | the DOM attach and detach paths |
| WebGL pool evictions | `src/components/Terminal/WebglRendererPool.ts` | the pool's eviction loop |
| Node status transitions | `src/stores/agentNodeStore.ts` | `patchAgentNode`, when the patch carries a status |
| Dialogs opened | `src/components/shared/Modal.tsx` | the mount effect — mounting *is* the open |
| Toasts opened | `src/stores/toastStore.ts` | `addToast`, for a toast that is shown rather than only recorded |
| IPC events received | `src/lib/tauri/eventListen.ts` | the one `listen` wrapper, counting each delivered event |
| Window focus changes | `src/lib/diagnostics/rendererHealth.ts` | the OS focus subscription, counting transitions only |

Two rules the counters follow:

* **A counter is not a store field.** They live in
  `src/lib/diagnostics/rendererHealthCounters.ts` as module-level numbers. A
  store write notifies subscribers, so counting a view switch inside a store
  would wake every consumer of that store for a fact none of them reads.
* **A gauge is a provider, not an import.** The sampler reads "how many
  terminals are attached" through a function the owning module registers. The
  dependency points from the sampler to the owner, never the other way.

## Cost

The sampler is passive: it installs `PerformanceObserver`s and lets the
browser push entries into it. There is **no `requestAnimationFrame` loop**
and no per-frame work. The only timer is a single `setTimeout` that fires
once per window to write the line and re-arm itself.

The one unavoidable main-thread cost is reading the page's own size — the
DOM element count, the animation list and the JS heap — because those facts
exist only on the main thread. The line reports what that cost
(`cost_ms`), so the budget can be checked from the log rather than taken on
trust.

### The animation census is on its own cadence

A Chrome trace of the sampler ticking on its ten-second cadence measured the
individual reads at these costs (median, cold — a tick that runs once every
ten seconds never gets optimised by the JIT):

| Read | Cost |
|---|---|
| `document.getAnimations()` | ~0.47 ms |
| `document.getElementsByTagName('*').length` | ~0.02 ms |
| `performance.memory.usedJSHeapSize` | ~0.04 ms |
| `document.visibilityState`, `hasFocus()` | ~0.01 ms |

Reading the animation list on every window would put the sampler over the
0.5 ms budget the issue sets, so the census runs **once a minute** and the
line reports how old it is in `anim_age_ms`. Six windows of amortisation put
the census at ~0.08 ms per window, and a run of animations still shows up
within a minute.

The animation figures are therefore *not* contemporaneous with the rest of
the line. `anim_age_ms` says so explicitly; a reader must not treat a stale
`anim=` as the state at the moment the line was written.

`cost_ms` is measured with `performance.now()`, which a WebView coarsens to
100 µs, so the figure is quantised to a tenth of a millisecond and carries
up to 100 µs of error at each end. A Chrome trace is the precise
instrument.

The figure covers the sampler's own work — the gauge reads, the aggregation
and the formatting. It does not cover the IPC hop that carries the line:
that is the log bridge's cost, already paid by every other frontend log
line.

### What the trace says the sampler costs

Measured on the app's own sampler, in the running app, with an empty timer
callback on the same cadence alongside it to establish the trace's floor:

| | Median | Mean |
|---|---|---|
| The empty callback (the trace's floor) | 0.05 ms | 0.06 ms |
| The sampler's tick | 0.41 ms | 0.50 ms |
| **The sampler's work, over the floor** | **0.36 ms** | **0.44 ms** |

Fifteen consecutive windows, one tick each, the window that also takes the
animation census among the most expensive (0.80 ms) and the very first tick —
cold, before the JIT has seen the code at all — the most expensive of all
(1.62 ms). That is inside the 0.5 ms budget the issue sets, and it is the
figure that the census cadence buys: with the animation list read on every
window the same measurement came out at 0.73 ms over the floor.

### Measuring the cost honestly

Three traps make a self-reported `cost_ms` the wrong instrument on its own,
and all three are worth knowing about before anyone "optimises" this line:

1. **A tight loop measures the wrong thing.** Benchmarking `sample()` 500
   times in a row measures JIT-optimised code. The real sampler ticks once
   every ten seconds, so its code never gets hot. The same source is an order
   of magnitude slower cold.
2. **The trace has a floor.** A `FunctionCall` trace event for a timer
   callback that does *nothing* still reads ~0.05 ms. That is the cost of the
   callback dispatch under tracing, not work the sampler did.
3. **The trace does not inflate the sampler.** Measuring `sample()` with the
   trace running and not running gives the same figure (0.032 ms against
   0.030 ms), so tracing is not what makes the sampled work look expensive.

The measurement that settles it subtracts the floor: the sampler's callback
against an empty callback on the same cadence, under the same trace, in the
same page.

## Verification

`tests/unit/renderer-health.test.ts` drives the sampler with a mocked
`PerformanceObserver` and asserts the whole line, field for field. The
native side is covered by the Rust tests in
`src-tauri/src/diagnostics/renderer.rs` and
`src-tauri/src/commands/frontend_log.rs`.

## Related

- [Diagnostics](diagnostics.md) — the native resource sampler, the opt-in
  DevTools port and the capture bundle. The capture answers "what is the
  renderer doing?" once, on demand; this line answers it every ten seconds,
  always on.
- [Frontend IPC](frontend-ipc.md) — the event transport that counts the IPC
  events the line reports.
- [Terminals](terminals.md) — the xterm and WebGL lifecycle the terminal
  gauges read.
