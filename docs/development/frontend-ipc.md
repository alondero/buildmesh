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

## Verification

`eslint.config.js` owns the legacy consumer list and restrictions on static
imports and re-exports. `tests/unit/tauri-ipc-seam.test.ts` exercises allowed
and denied imports through ESLint. Dynamic imports and `require()` are left
to review; there is no separate source scan of transport access or facet layout.
Run `npx vitest run tests/unit/tauri-ipc-seam.test.ts` and `npm run lint` when
changing these seams. The decision rationale is in
[ADR-0010](../adr/0010-tauri-ipc-wrapper-over-codegen.md).
