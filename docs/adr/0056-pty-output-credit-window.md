# ADR 0056: PTY output credit belongs before Channel storage

Status: accepted

Tauri can retain raw frames before the frontend finishes reading the first
response body. TerminalWriter's downstream limits cannot bound that storage.
The shared Agent/Build/Run output transport therefore admits at most 64 frames
and 256 KiB, with each frame capped at 32 KiB, before sending to a Channel.
It returns cumulative credit only
after ordered consumer delivery. The existing dedicated batcher waits for
credit, allowing its bounded reader queue and the OS PTY to backpressure the
process. A frontend-only queue cap would either lose live output or move the
unbounded backlog into Tauri; stop-and-wait would add an IPC round trip to
every frame. The window permits pipelining while keeping small-frame delivery
synchronous.

The terminal owns the Channel across process EOF and restart. Each process
owns a separate cancellable producer, so killing an incarnation wakes its
batcher without closing the persistent terminal or retaining a reader queue
for every retry. Subscription registration is also its cleanup identity:
delayed teardown and credit return carry the original Channel id rather than
looking up whichever subscription is newest. The budgets, memory accounting,
failure behavior and native measurement command live in
[the terminal transport contract](../development/terminals.md).
