# Application performance and memory review

Reviewed base: `997ea8aeb13ec55e1a11f06d711183c42b9338c2` (3 October 2026).
Scope: desktop startup/rendering, shared query caches, terminal lifecycle/output,
native watchers, Git/diff/file operations, Circuit/transcript workers, SQLite,
provider usage, and mobile transport/polling.

Evidence combines source inspection and controlled production-module regressions.
This session does not include an overnight multi-agent RSS/CPU profile or an
installed-app startup benchmark. Deferred issues specify the measurements needed.
Priority reflects the observed mechanism and possible impact, rather than a
measured ranking of CPU consumers.

## Fixed amplification and ownership defects

| Problem | Evidence and change | Preserved contract |
|---|---|---|
| Git event starts a request per subscriber | The cache deleted `pending` before each notification. With 20 subscribers the base starts 20 fetches; dispatch now invalidates once per client/key and starts one shared request. Manual notification and matching extra paths use the same rule. | Every matched subscriber is notified; different keys and clients stay independent. |
| Obsolete request changes cache ownership | Old completions could publish obsolete data/errors, delete a newer pending request, or cancel trailing work. Only the current promise may commit; superseded callers adopt the current request/value. | Empty results, errors, deduplication, manual refresh and latest-result rendering. |
| Slow request is repeatedly superseded during edits | Bus events during pending work defer one settled refresh. The running request publishes; its configured freshness window bounds the follow-up rate. | Slow Git/GitHub consumers become usable while edits continue; manual supersession still works. |
| Trailing work retains unmounted consumers | Keyed timers retained callbacks after unsubscribe. They now retain subscription objects; unsubscribe removes exactly its subscription and cancels a timer with no remaining consumers. | Distinct subscriptions sharing a callback still receive their settled refresh. |
| Watcher storms queue unlimited signals | Notify previously sent into an unbounded channel. A single-slot channel and non-blocking `try_send` retain one pending wake. The saturation regression stalls the leading emit and sends 20,000 more events. | Signals contain no file data. Leading, periodic, quiet-gap and disconnect trailing refreshes remain. |

Implementation: [pathInvalidatedCache.ts](../../src/lib/pathInvalidatedCache.ts)
and [file_watcher.rs](../../src-tauri/src/commands/file_watcher.rs).
The [cache regressions](../../tests/unit/path-cache-request-ownership.test.ts)
exercise the actual event dispatcher, both factories, manual invalidation, both
completion orders, rejection, unsubscribe and timers. The watcher saturation test
uses the production channel factory and coalescer.

### Agent-node selector and usage-cache miss amplification (#2021)

Measured before/after on the current tree, both with the production code paths.

**All-node selector.** Seven components subscribe to the full derived node
array. Counting every `nodesById` dereference the derivation performs (a
deterministic measure, not a timing one), one store notification that changes
no node cost:

| Nodes | Dereferences before | After |
|---|---|---|
| 100 | 700 | 0 |
| 500 | 3500 | 0 |
| 1000 | 7000 | 0 |

A notification that genuinely changes one node is unchanged (1001 dereferences
at 1000 nodes) because that derivation is required work. The derivation is now
memoized on the identity of `nodesById`/`nodeIds`, so unrelated writes cost
one `Object.is` pair per subscriber and a real change is derived once for all
subscribers instead of once each. Measured component-commit time for 50
single-node patches moved 3.1/6.4/10.2 ms at 100/500/1000 nodes before, but
timings on this machine vary by ~2x between runs, so the dereference counts
above are the load-bearing evidence. Per-row `memo` already bailed out
(row renders stayed at the patch count), so no row-level change was justified.

**Usage cache misses.** Eight concurrent cold readers for one credential
identity issued **8 vendor fetches** before coalescing, because the 5-minute
TTL only collapses sequential reads. After per-`(provider, identity)`
single-flight, the same burst issues **1**. Preserved: explicit force-refresh
(skips the TTL read; simultaneous refreshes collapse to one live call),
failures (every waiter receives the leader's real outcome, including
`Rejected`, and the slot is released so the next caller retries), identity
switches (two keys for one provider each reach the vendor), and meter
visibility (followers receive the leader's `UsageOutcome`, not a value
re-derived from the cached wire triple, so the `NoCredential`/`Rejected`
distinction the gate depends on survives). No lock is held across the vendor
round-trip, and a different provider is not serialized behind a slow one.

Regression evidence: [agent-node-derived-selector.test.tsx](../../tests/unit/agent-node-derived-selector.test.tsx)
(counts derivations, pins array-reference stability, ordering, pinned/status/mesh
invariants and archived-row retention) and the issue #2021 tests in
[catalog.rs](../../src-tauri/src/services/usage/catalog.rs) (controlled delayed
adapters covering coalescing, identity switching, failure propagation,
force-refresh and cross-provider independence).
These fixes remove demonstrated amplification mechanisms. They do not establish
the complete cause of the historic machine lockup in
[#799](https://github.com/alondero/buildmesh/issues/799).

## Remaining work

P1 denotes resource ownership or potentially severe scaling behavior to investigate
next. P2 denotes boundedness/efficiency work requiring measurement and a separate
implementation contract.

| Priority | Finding and source | Tracking / next evidence |
|---|---|---|
| P1 | Pending agent terminal creation can finish after deletion; failed listener registration retains partial writer/theme/font resources. [TerminalRegistry.ts](../../src/components/Terminal/TerminalRegistry.ts) | [#2016](https://github.com/alondero/buildmesh/issues/2016): controlled deletion/failure ordering, cleanup accounting and repeated-cycle heap. Preserve surviving terminals. |
| P1 | Diff owns whole old/new text, line strings and highlighted representations. File/hunk cancellation cannot interrupt the earlier single-file computation. Renderer creates every DOM row. [diff.rs](../../src-tauri/src/commands/diff.rs), [Diff.tsx](../../src/components/Diff/Diff.tsx) | [#2020](https://github.com/alondero/buildmesh/issues/2020): large-file benchmarks, recoverable lazy loading, finer cancellation and virtualization. |
| P1 | Recursive watches include dependency/build directories, with one watcher/coalescer per node. Bounded wake queues leave native event volume unchanged. [file_watcher.rs](../../src-tauri/src/commands/file_watcher.rs) | Existing [#799](https://github.com/alondero/buildmesh/issues/799): diagnostic reproduction, retirement/unwatch audit and shared/scoped watches. Callback filtering alone leaves native overhead. |
| P2 | Settled Git-cache entries have no inactive cardinality limit; invalidation reaches mounted consumers. Values/errors can retain old entity/path state. [pathInvalidatedCache.ts](../../src/lib/pathInvalidatedCache.ts) | [#2017](https://github.com/alondero/buildmesh/issues/2017): bounded inactive retention, active keys pinned, late-completion fencing and revisit freshness. Audit per-mesh promise-map deletion too. |
| P2 | Frontend terminal cap excludes object overhead, repeatedly shifts arrays, and retains a lone oversized chunk. Asynchronous xterm parsing lacks a completion budget. [TerminalWriter.ts](../../src/components/Terminal/TerminalWriter.ts) | [#2018](https://github.com/alondero/buildmesh/issues/2018): hidden-window/small-chunk stress, amortized eviction and parser backlog. Preserve order, interactive echo and split UTF-8. |
| P2 | Mobile channel map lacks removal; cleared history retains allocation. Broadcast caps message count rather than bytes and clones payloads per receiver. [ws.rs](../../src-tauri/src/http/ws.rs) | [#2019](https://github.com/alondero/buildmesh/issues/2019): deletion versus restart, stale-producer fencing, retained capacity and slow-client/shared-byte measurements. |
| P2 | Project files enumerate four levels including ordinary `node_modules`/`target`. Sort comparisons repeat `path().is_dir()` filesystem work. [file_tree.rs](../../src-tauri/src/commands/file_tree.rs), [FileTree.tsx](../../src/components/FileTree/FileTree.tsx) | Existing [#1569](https://github.com/alondero/buildmesh/issues/1569): lazy children, ignore policy, cached entry metadata, structural freshness and preserved keyboard/selection state. |
| P2 | Transcript tail streams the full file; last-assistant uses a 256 KiB window with deliberate full-scan fallback. Codex locates sessions by directory walks; Circuit ticks can reread unchanged evidence. [file.rs](../../src-tauri/src/services/transcript_reader/file.rs), [codex.rs](../../src-tauri/src/services/transcript_reader/readers/codex.rs), [native_completion.rs](../../src-tauri/src/services/transcript_reader/native_completion.rs) | Existing [#1753](https://github.com/alondero/buildmesh/issues/1753): proven-path/unchanged-file caching and WSL I/O counts. Preserve append/truncate/rotation/session/freshness fences and older-answer fallback. |
| P2 | Circuit submission ordinals filter JSON in append-only history without a matching index. [evidence.rs](../../src-tauri/src/db/circuit/evidence.rs) | Existing [#1932](https://github.com/alondero/buildmesh/issues/1932): partial expression index, query-plan/migration/snapshot evidence and unchanged cross-run correlation. |
| P2 | App eagerly imports canvas, probes, omnibar and modals. Terminals/addons already load dynamically. [App.tsx](../../src/App.tsx), [vite.config.ts](../../vite.config.ts) | Existing [#1750](https://github.com/alondero/buildmesh/issues/1750): current entry/startup measurements before splitting remaining cold surfaces. |

## Safeguards to preserve

- Persistent agent terminals survive mesh switches and React unmounts. Retained
  live buffers are intentional; disposing or shrinking them changes functionality.
  The shared WebGL pool already caps active renderers at four.
- Native PTY batching has 256 slots of 8 KiB reads (about 2 MiB), with 8 ms/32 KiB
  coalescing. Desktop output uses binary channels. Pre-subscription sinks and
  frontend writers each cap pending payload at 4 MiB per session. These do not
  bound total app memory or xterm parser work.
- SQLite uses WAL, one serialized writer and eight readers with bounded checkout
  waits. Preserve one-connection helpers and lock-free I/O phases. Slow commands
  use `run_blocking`; async-boundary and process-spawn checks remain relevant.
  Existing spawn/I/O work is tracked in [#1752](https://github.com/alondero/buildmesh/issues/1752),
  [#1753](https://github.com/alondero/buildmesh/issues/1753) and
  [#1229](https://github.com/alondero/buildmesh/issues/1229).
- Provider HTTP already shares a finite-timeout client. Mobile polling uses chained
  timeouts, pauses scheduled polling while hidden and owns result tokens.
- Existing diagnostics record process-tree vitals and watcher/Git/sync counters.
  Use them to separate app/WebView retention from agent-child memory in #799.

## Verification boundaries

The new cache regressions reproduce amplification and ownership failures at the
recorded base. The focused new/existing cache, Open PR and mesh-health suites
cover 97 tests, including rendered hook loading/data and continuous slow edits.
The Rust watcher suite passes 11 tests, including saturation and the existing
leading/trailing/path cases. These prove request counts, ownership and event
behavior; installed-app RSS savings remain unmeasured.

Before Rust edits, `cargo fmt --all --check` failed at the recorded base with
3,651 diff hunks. The touched watcher module is formatted. Repository-wide
formatting is tracked in [#2022](https://github.com/alondero/buildmesh/issues/2022),
separately from strict-Clippy debt in [#1491](https://github.com/alondero/buildmesh/issues/1491).
The PR must report the actual harness outcome and separately executed checks;
focused passes cannot turn a failing full gate into a pass.

Start follow-up work with #2016's terminal ownership tests and #799's diagnostic
reproduction, then benchmark large diffs. Keep lifecycle, retention and visible
loading-contract changes independently reviewable.
