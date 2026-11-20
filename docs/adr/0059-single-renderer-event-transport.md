# ADR 0059: Count delivered events at the renderer IPC boundary

Status: accepted

## Context

Renderer diagnostics need to report how many Tauri events a window receives
between health samples. Listeners previously subscribed directly to Tauri from
multiple modules, so there was no single place to count deliveries.

## Decision

`src/lib/tauri/eventListen.ts` owns inbound Tauri event subscriptions. It counts
each delivery before invoking the caller's handler and forwards the event and
unlisten function. It preserves the two-argument subscription when options
are absent and forwards the third argument when options are present. Handlers
keep their original ordering and error behavior.

The counter folds trailing numeric and UUID instance IDs into one key per
stream shape. Each health sample drains the counters, sorts by descending
count and uses the event name to break ties so successive lines are easy to
diff. Outbound `emit` remains a direct Tauri import. ESLint enforces ownership
of inbound `listen` in renderer source.

## Alternatives and consequences

Counting registrations would measure subscriptions, so an idle stream would
look active and a busy one would appear only once. Instrumenting every handler
would preserve direct imports but leave gaps whenever a call site was missed.
Keeping the full per-instance name would make diagnostics increasingly noisy
as short-lived sessions opened. Grouped keys preserve traffic totals and trade
away individual session attribution in the health line.

## Links

- [Frontend IPC](../development/frontend-ipc.md#the-event-transport)
- [Renderer health line](../development/diagnostics.md#the-renderer-health-line)
- [ADR 0010 on command transport](0010-tauri-ipc-wrapper-over-codegen.md)
