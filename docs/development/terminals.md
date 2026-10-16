# Terminals and PTY transport

Status: current

## Terminal Persistence (CRITICAL)
Startup identity recovery (#1555) lives in `services/session_recovery.rs`. List all Suspended nodes before recovery, including NULL/empty CLI session IDs; never restore the old SQL filter or one-time Codex migration gate. Historic recovery requires an unambiguous workspace/time match and a conditional write against the original fresh-start timestamp. Fresh intent atomically clears identity and records that timestamp; Resume preserves it. Missing-ID legacy nodes may be approval-gated and must not be auto-started by transcript matching. See [node-resume-recovery.md](../learning/node-resume-recovery.md) for provider evidence, limits, and test coverage.

`TerminalManager` is a **singleton**. xterm.js instances survive React remounts via a hidden container stack. Never call `dispose()` on a terminal unless the agent node is explicitly deleted — see `src/components/Terminal/Terminal.tsx`. Disposing a terminal causes permanent blanking.

## Terminal creation is cancellable at every await (issue #2016)
`TerminalRegistry.getOrCreate` creates a `PendingCreation` alongside the in-flight `doCreate` promise, and every resource `doCreate` acquires is registered with that record as it is acquired: the xterm, its resize scheduler, the writer sink, the font-size and theme entries, both `listen` handles, and the binary PTY Channel. `dispose(nodeId)` marks an in-flight creation cancelled and releases what it already owns, so a node deleted while the lazy chunk fetch, a listener registration or the Channel subscription is still in flight never publishes a terminal and never retains a writer entry, a font-size or theme entry, or a listener. `doCreate` checkpoints after each await, abandons the creation, and — because the record is already closed — releases anything a late await acquires instead of parking it. A failure at any stage unwinds the same list, and the unsubscribe for the Channel is chained *behind* its subscribe so a deletion mid-registration still drops the Channel instead of racing it. The record is dropped when the creation settles; there is deliberately no deleted-id tombstone map, and `detach` never cancels anything — an unmount or mesh switch must leave the surviving node's terminal untouched. `destroy()` cancels in-flight creations and releases a module-level `listen` handle that resolves after teardown. `BuildRunTerminalRegistry` reaches the same outcomes through its own `PendingCreation` record plus the per-session queue.

## xterm + addon lazy-load (issue #1568)
`@xterm/xterm` and the five `@xterm/addon-*` packages add ~430 kB minified / ~100 kB gzip to the desktop entry chunk if statically imported — too big to pay for users who never open a canvas. `TerminalRegistry.doCreate` and `BuildRunTerminalRegistry.doCreate` keep the runtime imports behind `await import(...)` inside the function body and use `import type { Terminal }` (and the other addon classes) at module scope, so Rollup erases the types and the dynamic imports emit a separate chunk fetched only when a terminal pane is first attached. `WebglRendererPool.activate` dynamic-imports `loadWebglRenderer` (which carries `@xterm/addon-webgl`); the pool reserves the LRU slot synchronously and schedules the renderer attach inside the awaited promise. **Do not** revert to static `@xterm/xterm` imports — see the budget cap at `scripts/bundle-budget.json`; the entry chunk's ceiling is enforced by `scripts/check-bundle-size.mjs` and CI. The budget bounds *how large* the entry chunk may get, not *what belongs in it*, so the split itself is gated separately by `scripts/check-bundle-composition.mjs`: it fails when a forbidden module's lazy chunk disappears (its code was inlined into the entry chunk) or becomes reachable through a static `import` edge, and names the eager import that caused it. Its forbidden list is every module the app reaches through `import()` — the xterm packages plus the lazy `loadUnicode11Widths` wrapper and the lazy Probe tabs — so adding a terminal dependency that should stay lazy means adding it there too. Two pre-existing Vite warnings remain (`Terminal.tsx` and `agentNodeStore.ts` are both statically and dynamically imported) because the dynamic imports in `meshStore.deleteMesh` are load-bearing workarounds for the `meshStore → agentNodeStore → Terminal.tsx → uiStore → meshStore` ESM cycle (`src/stores/meshStore.ts:5`); clearing them requires restructuring the store layer, which is out of scope for the cold-only-surface mandate.

## Link clicks have exactly one owner
A press on a terminal link reaches two independent owners inside xterm. The `Linkifier` activates the link — an OSC 8 hyperlink through the `linkHandler` option (what Codex emits), a plain printed URL through `WebLinksAddon` — and, whenever the agent's TUI has turned mouse tracking on (`CSI ? 1003 h` with `CSI ? 1006 h`, which Codex does), xterm *also* forwards the press to the PTY as an SGR mouse report. Codex answers a press on its own OSC 8 hyperlink by opening that URL itself (`codex-rs/tui/src/tui/link_pointer.rs` → `open_url_in_browser`), so one click in a Codex session opened two browser tabs, while a harness printing plain URLs only ever reached ours.

The press cannot simply be left to the agent: xterm records the link under the pointer on `mousedown` and activates on `mouseup` only when that record survived, so suppressing the press also cancels our own activation. While the PTY app owns mouse input, `TerminalLinkRouter` (`src/components/Terminal/TerminalLinkRouter.ts`) therefore owns the whole gesture around a link. It tracks the hovered link from both providers' public `hover`/`leave` callbacks, stops a left press on a link in the capture phase on `term.element` — before xterm can forward it or record it — and opens the URI on the release, when press and release land on the same link.

Both halves of the gesture have to be withheld. xterm keeps a `_mouseDownLink` from earlier gestures and never clears it after activating, so a link clicked before the agent enabled mouse tracking leaves a release that can still activate it; letting that release through would reopen a link the router already opened. Clearing the hovered link after a click is equally wrong, because xterm only re-resolves the hovered link when the pointer moves to another cell — the next click on the same link would arrive with no hover event and escape to the agent. A press away from a link, and every non-primary button (Codex binds right-click to paste), is untouched and still reaches the agent.

"Hovered link" means the link's identity, not its text: the router keeps the hover `range` alongside the URI and compares both, as xterm's linkifier does (`Linkifier.ts:395-401`). A URI alone is not an identity, so the same URL printed twice is two links and a gesture that starts on one and ends on the other opens neither.

The router is dynamically imported alongside xterm and its addons in `doCreate` (see *xterm + addon lazy-load*), because it is only reachable from there and a static import of it broke the entry-chunk budget in `check:bundle`. It binds after `term.open()` (its press guard lives on `term.element`, its release guard on `document` so a release outside the terminal still ends the gesture) and is idempotent per element, so a re-attach re-parents the element without stacking listeners. `openUrl` stays the only sink, so the Tauri WebView never navigates, and `allowNonHttpProtocols` stays unset. Behaviour is pinned by `tests/unit/terminal-link-router.test.ts` and was checked against real xterm 6 for the Codex shape, repeated clicks without pointer movement, mouse tracking switched on after an untracked click, the plain-URL shape, a press off the link, a gesture released off the link, and a drag between two occurrences of the same URL.

## Command Code keyboard (Ink 7)
Command Code's Ink TUI can swallow keys when xterm.js leaves an unmatched bracketed-paste wrapper, or when the CLI's kitty-keyboard probe races ConPTY. Agent terminals for the `commandcode` harness set xterm.js `ignoreBracketedPasteMode`; other harnesses keep bracketed paste so a multi-line paste stays one prompt. Spawn sets `TERM_PROGRAM=vscode` so Command Code skips the probe. Every agent PTY is a real terminal: `wrap()` drops inherited `NO_COLOR` / `TERM=dumb` / `FORCE_COLOR=0` and sets `TERM=xterm-256color`, `COLORTERM=truecolor`, and `FORCE_COLOR=3` (and puts those keys on `WSLENV`).

## PTY input

Terminal handovers to Codex use `agent::handover::prepare` before prompt
delivery. Selections longer than 256 Unicode characters are written intact as
UTF-8 to a unique file under the app data directory's `handovers/node-<id>/`;
only a single-line instruction with the agent-visible path reaches the
terminal or initial launch prompt. Existing-node handovers and new-node
`SpawnIntent::Handover` launches share this policy, including saved Codex launch
configurations. Short selections and other harnesses retain inline delivery.
A save failure rejects the handover; it does not fall back to a long paste. If
staging fails for a newly created node, Buildmesh deletes that unstarted node;
if deletion also fails, the spawn error reports both failures.
When an existing-node PTY rejects a pointer before accepting it, only that
attempt's file is removed. Accepted pointers keep their files across process
exit and restart, and permanent node deletion removes the node's handover
directory.

Each `write_to_agent` call enqueues one buffer. The per-agent writer thread drains that buffer with a single `write_all`. xterm's paste is one data event, so a large or multi-line paste stays one write, including its bracketed-paste markers. Do not split or pace that write to work around a slow provider. The multi-second stall on a large Windows paste is the provider reading console input one record at a time; evidence and the upstream reader change are in [Large paste latency](../learning/large-paste-latency.md).

The queue is bounded twice: by message count (`PTY_WRITER_CHANNEL_CAPACITY`, 64) and by buffered bytes (`PTY_INPUT_QUEUE_BYTE_CAPACITY`, 1 MiB). The byte bound is tracked in an `InputQueueGauge` rather than read off the channel, because `SyncSender` exposes no depth introspection. A refusal is a typed `InputDisposition` — `Accepted | Backpressured | Closed` — returned to the caller, and is never dropped behind a success. `Backpressured` means the buffer was **not** queued: the input decoder does not advance, and no activity or first-user-input telemetry fires. That is what makes retrying the identical bytes safe — a refusal cannot duplicate. Only `Accepted` may clear attention, mark a coordinator prompt delivered, or record a first user input.

Each transport turns that one verdict into its own consequence. The desktop client holds the exact bytes in a single ordered retry buffer at the transport seam (`src/lib/terminalInputQueue.ts`) and surfaces a stalled-input badge only once bytes have been held past the buffer's own threshold, so an ordinary burst of typing shows nothing. The coordinator treats `Backpressured` as its own `DriveError` so `drive_node_idempotent` takes `release_claim` and never finalizes a `delivered` verdict; the circuit retries a bounded number of times and then fails loudly, because a refused Enter must not leave a prompt staged in an agent's input box forever. Mobile's HTTP route returns a typed body plus `Retry-After`, and its fire-and-forget WebSocket emits `terminal-input-stalled` on the existing broadcast, since the socket has no inbound channel to acknowledge on. Queue depth and bytes are on the `DIAG` line as `pty_qmsg=` / `pty_qbytes=`. A buffer larger than the whole byte cap is still admitted when the queue is empty, so the bound can never make an oversized paste undeliverable, and splitting one to fit would break the whole-paste rule above.

Desktop clipboard paste into native Windows Grok uses its Ctrl+V command
instead of streaming clipboard text. `TerminalRegistry.pasteClipboard` owns
keyboard and context-menu delivery; its persistent element captures browser
paste before xterm. `Terminal` selects this policy only for Grok on a Windows
host with a Windows node. WSL and mobile/remote paste must send their own text,
since they do not share the desktop clipboard. Programmatic `term.paste(text)`
retains its existing semantics.

Backend prompt injection (`circuit::delivery::PreparedPrompt`, used by
"Handover to node" and by Circuit prompt dispatches including turns, continuations,
and nudges) cannot use the clipboard, so it fixes the paste body instead. The
coordinator's `AgentDriver::send_prompt` is single-line by contract and bypasses
staged delivery. ConPTY turns every pasted byte into a key event: an LF is dropped
and a CRLF is an Enter. A terminal selection is CRLF-joined on Windows, so an
unmodified Grok handover submitted line 1 and queued the rest as separate messages.
A harness whose adapter returns true from `paste_requires_cr_newlines` (Grok and
Codex) gets CR line endings inside the bracketed paste, which is what xterm.js sends
for a manual paste. Other harnesses keep their line endings as given, but any prompt
containing CR or LF is treated as multiline: bracketed by `injection_payload` and
subject to non-generic rendered-paste gates (such as Codex and Muse). See the
[Grok paste investigation](../learning/grok-terminal-paste.md) for runtime evidence.

A harness with its own paste gate may need two texts rather than one. Codex's
split-marker gate keeps `codex_sanitized_text` (the payload: control-stripped, line
endings as `prepare` resolved them) separate from `codex_composer_text` (the proof:
the same text with CRLF and bare CR collapsed to LF, which is what Codex's
`apply_paste` holds and counts). Codex normalizes line endings *before* counting, so
`[Pasted Content N chars]` is identical for LF, CRLF and CR payloads — the CR the
wire needs costs the gate nothing, and no custom burst-boundary handling is needed.

## PTY output streaming (issue #1385 / #1393)

The backend avoids eager payload allocations in the PTY hot path. After session
capture is complete or preassigned, `ChunkCapture::feed` borrows valid UTF-8 from
the current read buffer; malformed bytes retain the per-chunk lossy decoding
behavior. Text released from the capture carry-over remains owned. The evaluator
consumes the text synchronously before the next read. The batcher keeps an
isolated chunk's original allocation and reserves the bulk budget when a second
chunk arrives. Both the size threshold and flush window still govern dispatch.
Run `node scripts/bench-session-capture.mjs` to measure decoder allocations and
batch capacity against the production modules, or pass `--baseline <commit>` for
a comparison. The standalone harness uses Cargo's cached dependencies offline.
Allocated bytes measure cumulative allocator traffic, not retained memory or app RSS.

Windows builds ship a pinned Microsoft ConPTY DLL and its matching native console
hosts. `build.rs` runs `scripts/prepare-conpty.mjs` to verify the NuGet package
checksum and stage the runtime beside Cargo binaries (including test binaries);
`tauri.windows.conf.json` packages the same files beside the installed executable.
The first Windows build needs Node.js and access to NuGet; subsequent builds use
the verified archive under `src-tauri/target/conpty/`. The sandbox's owned ConPTY
links the bundled DLL's Create/Resize/Close exports, while `portable-pty` loads
the same DLL for ordinary terminals. Keep the DLL and host versions together.
The inbox Windows console can forward DEC 2026 frame-end markers before flushing
its rendered text and cursor restoration, exposing intermediate cursor positions
in xterm. Do not compensate by disabling harness animations or delaying all PTY
output. Live Windows frame-order tests cover both PTY creation paths.

The PTY reader thread still sees every OS `read()` (session-id capture, auto-naming, autopilot). A sibling batcher coalesces those slices (8 ms window or 32 KiB = four 8 KiB PTY fills) and pushes **raw bytes** over a per-session Tauri `Channel`. The sink type lives in `pty::sink` (`OutputSink` / `OutputSinks`). Agent terminals use `pty::sink::AGENT` via `subscribe_agent_output` / `unsubscribe_agent_output` (`agent::output`). Build/Run terminals use a **sibling map** `pty::sink::BUILD_RUN` via `subscribe_build_run_output` / `unsubscribe_build_run_output` — both surfaces key by the same node id, so sharing a map would paint agent bytes into a Build/Run xterm. Bytes that arrive before subscribe are buffered on `OutputSink` and flushed in order — never mixed with the JSON event (those two IPC paths have no ordering). `agent-output` `line` and `build-run-output-{sessionId}` are test injection only. `pty::batch::with_batcher` drops the producer before join (otherwise the reader deadlocks on EOF). The Channel subscription belongs to the persistent terminal, not to one process incarnation: stale-process cleanup, PTY EOF, retry, resume, regenerate, and Build/Run `close_build_run` must preserve it. Agent Node deletion and explicit terminal disposal unregister it. Don't put production PTY bytes back on the JSON event.

The first-spawn path is the load-bearing one. The frontend subscribes as soon as the xterm exists, then `prepare_context` calls `kill_agent` even when there is no process. Unregistering there drops the Channel; the new reader's `ensure()` creates a disconnected pending sink and the viewport shows a cursor with no text. `kill_session` / the PTY-reader epilogue / `close_build_run` must not unregister. Pins: `kill_session_without_process_preserves_output_subscription`, `replacement_reader_reuses_the_live_channel`, `process_lifecycle_does_not_unregister_node_output_subscription`, `agent_and_build_run_maps_do_not_cross_talk`. Frontend: subscribe on `TerminalRegistry` / `BuildRunTerminalRegistry` create, unsubscribe only on `dispose` (never on remount, auto-spawn, detach, or `getOrCreate` reuse).

Tauri 2.11's raw Channel transport has a payload-shape boundary: frames smaller than 1 KiB reach JavaScript directly as an `ArrayBuffer`, while frames at or above 1 KiB use the fetch path and arrive as a `Response`. `subscribeAgentOutput` / `subscribeBuildRunOutput` (shared `subscribeRawPtyOutput`) must consume `Response.arrayBuffer()` asynchronously and serialize those reads with later frames so terminal bytes cannot overtake each other. The boundary-to-xterm regression is pinned in `tests/integration/agent-terminal-auto-spawn.test.tsx` and `tests/unit/build-run-terminal-persistence.test.tsx`.

`TerminalWriter` (`src/components/Terminal/TerminalWriter.ts`) buffers output per node behind one `requestAnimationFrame`, and it carries three independent budgets, because that flush scheduler suspends whenever the window is hidden. Bytes (`MAX_PENDING_BYTES`, 4 MiB) bound payload, chunk objects (`MAX_PENDING_CHUNKS`, 4096) bound the per-chunk headers a byte count cannot see, and in-flight parses (`MAX_INFLIGHT_WRITES`, 4) bound what xterm has been handed but not yet parsed. Eviction advances a head index and overwrites each dropped slot, so it is amortized constant-time and the discarded payload is released immediately; `slice` compaction keeps the backing array near twice the live window. The in-flight budget depends on xterm's parse-completion callback, so both registries register a completion-aware sink — a sink that ignores `done` cannot be budgeted and is therefore never deferred. While the cap is reached the writer holds its queue and does NOT re-arm a frame: that callback is the only wakeup, so a slow parse costs no polling and a finished burst costs no wakeup when nothing is waiting. The interactive fast path ignores the budget in both directions — a keystroke echo is never deferred and never consumes a bulk slot. The budgets bound queued work only: they must never dispose a hidden persistent terminal or shrink scrollback. `npm run bench:terminal-writer` measures append time, retained heap, queued chunks and in-flight depth against the real module; issue #2018 moved the paused-window case from ~11 s to ~65 ms for 200k lines and cut peak unparsed payloads from 200 to 4.

Terminal container resize is coalesced by the shared `TerminalResizeScheduler`, used by both agent and build/run registries. It waits for 200 ms without a size change before fitting, so pane dragging causes one PTY resize after the drag settles. A horizontal xterm resize reflows normal-buffer scrollback, and full-screen TUIs redraw for the final PTY size. Codex uses its fullscreen transcript mode rather than inline scrollback, avoiding a transcript clear and replay on width changes; history navigation belongs to Codex while it is running. Keep DOM measurement on the next animation frame and do not restore per-observation or per-frame `fit()` calls.

