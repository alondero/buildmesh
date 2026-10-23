# Frontend IPC

Status: current

Frontend contributors add and consume typed native commands through the owning
domain facet, keeping invocation and error handling at one chokepoint.

## Import and transport ownership

Typed wrappers live under `src/lib/tauri/`. New callers import the facet
directly. `src/lib/tauri.ts` contains deprecated compatibility re-exports;
ESLint freezes its legacy consumer list while callers migrate.

Only the facets may import `_invoke`, whose module owns raw invocation and
error logging. `src/lib/frontendLog.ts` is a deliberate raw-invoke peer:
routing the logging bridge back through the wrapper would introduce a logging
cycle. Wire types come from `src/types/generated/`; see the
[Rust conventions](rust-conventions.md) for their generation contract.

## Domain facets

| Facet | Owns |
| --- | --- |
| `agentNode` | Node CRUD, close safety, discovery, import and startup resume |
| `agent` | Session launch, attention, ordered PTY input, output Channels and build/run PTYs |
| `mesh` | Mesh CRUD, settings, scratchpad, worktree pool configuration and AI context |
| `git` | Status, branches, diffs, mesh health, pruning and deferred worktree cleanup |
| `github` | Authentication, issue/PR feeds and issue/PR node acceptance |
| `files` | File tree, editor/file manager, path conversion and clipboard |
| `app` | Application identity, diagnostics, title bar and window shutdown |
| `preferences` | App preferences, preference-file recovery and app defaults |
| `remoteAccess` | Coordinator settings, device sessions, pairing, network and certificates |
| `circuit` | Circuit CRUD, runs, queue, ownership and semantic turns |
| `provider` | Harnesses, launch configurations, credentials, OAuth, defaults and usage |

The `history`, `stateRecovery`, `circuitBlueprint` and `circuitEvidence` facets
retain their narrower interfaces and are imported directly.

## Live event streams

A **live event** is a backend event the frontend reacts to while the app
is running (`circuit-run-updated`, `provider-list-changed`,
`worktree-directory-changed`, `mesh-sync-warning`, the node-lifecycle
events). Each stream in the table below has exactly **one owner**: the
module that subscribes to it, converts its payload into a typed update,
and publishes that update to whoever cares.
`tests/unit/live-event-stream-ownership.test.ts` proves that property for
the streams listed here.

Owners live in `src/lib/streams/`. An owner owns four things:

1. **The event name.** The constant is declared next to the subscription
   that uses it, so the name and its subscriber are changed together; the
   ownership guard test fails if a second `listen` for that name appears.
   The emitters import the same symbol (`lib/tauri/mesh.ts` and
   `lib/tauri/preferences.ts` import `PROVIDER_LIST_CHANGED_EVENT` from
   `lib/streams/providerListStream.ts`).
2. **The ONE `listen`.** `src/lib/streams/streamSubscription.ts` holds the
   shared machinery: one backend subscription per stream however many
   consumers read it, released when the last consumer goes.
3. **The payload → typed update conversion.** A pure function
   (`toCircuitRunUpdate`, `toProviderListChange`, …) that turns the raw
   payload into something a consumer can use without inspecting the raw
   shape.
4. **The match rule.** "Is this update about *my* run / *my* mesh?"
   (`concernsRun`, `concernsMesh`, `concernsMeshPath`). It lives in the
   owner because it is the part that used to be copy-pasted into each of
   the old subscribers.

Consumers read the typed update. They do **not** call `listen` for a
stream they do not own, and they do not match raw payloads.

| Stream | Owner | Consumers |
| --- | --- | --- |
| `circuit-run-updated` | `src/lib/streams/circuitRunStream.ts` | agent-node store, run overview, next-steps panel, evidence history, flow editor, Probe tab |
| `provider-list-changed` | `src/lib/streams/providerListStream.ts` | shared provider list, spawn clusters, provider-list invalidation hook |
| `worktree-directory-changed` | `src/lib/streams/worktreeDirectoryStream.ts` | effective worktree dir hook, Probe project settings |
| `mesh-sync-warning` | `src/lib/streams/meshSyncWarningStream.ts` | app toast stack, sidebar mesh row |
| node lifecycle (`node-*`, `agent-lifecycle`, `attention-cleared`, `circuit-pr-ready`) | `src/stores/agentNodeListeners.ts` | agent-node store |

Some events already had exactly one subscriber and keep the owner they
had — the node-lifecycle events in `src/stores/agentNodeListeners.ts`,
and `circuit-runs-attention-dismissed` in the Probe tab.

Adding a consumer to an existing stream is a read: import the owner's
`use…` hook or `subscribe…` function. Adding a *new* stream means adding
an owner module under `src/lib/streams/` — never a second `listen` in a
component.

## Verification

`eslint.config.js` owns the legacy consumer list and restrictions on static
imports and re-exports. `tests/unit/tauri-ipc-seam.test.ts` exercises allowed
and denied imports through ESLint. Dynamic imports and `require()` are left
to review; there is no separate source scan of transport access or facet layout.
`tests/unit/live-event-stream-ownership.test.ts` walks `src/` and asserts
that every live event reaches the event bus from exactly one file — the
structural guard that keeps "one owner per stream" true.
`tests/unit/live-event-streams.test.ts` pins each owner's payload →
typed-update conversion (including the "not my run" case) and its
subscription contract.
Run `npx vitest run tests/unit/tauri-ipc-seam.test.ts` and `npm run lint` when
changing these seams. The decision rationale is in
[ADR-0010](../adr/0010-tauri-ipc-wrapper-over-codegen.md).
