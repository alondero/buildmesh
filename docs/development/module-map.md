# Rust backend module map

Status: current

A map of `src-tauri/src/` for someone who has never read it: which seams to
trust, where each kind of logic lives, and where the large files are still
growing. It is deliberately short — it names boundaries, it does not repeat
the owner documents.

The knowledge primer is the index; this is the map. Read the primer row for
your area first, then use the section below for the file to open.

## The four seams

Most backend bugs come from crossing one of these boundaries. Each one has an
owning directory; code that needs a capability from another seam calls its
public API instead of re-deriving the fact.

| Seam | Owner | The rule |
|---|---|---|
| **Commands are thin** | `commands/` | A `#[command]` is an adapter: validate, call one service or `db::` function, map the error to `String`. No policy, no SQL, no `git2` handles. Every new `#[command]` must be registered in `lib.rs` or it fails at runtime with "command not found". Files that still hold a `Repository` are [seam debt](#seam-debt), not the pattern to copy. |
| **Git access lives in `git/`** | `git/` | New `git2` use goes through `git/state.rs` (the capability-shaped reads — a `RepoState` at a named `Baseline`), `git/diff.rs`, `git/index.rs`, `git/branches.rs`, `git/remote.rs`, `git/tree.rs`, `git/worktree` and `git/sync` (ADR 0007). If you need "what changed and by how many lines", "which branch, how far ahead", "which files does this branch hold", or "stage this file", ask `git/` for it. Do not open a `Repository` in a command or in `circuit/`. |
| **`git/` never imports `services/`** | `git/` | The dependency between the service layer and the Git layer runs one way: `services/` → `git/`. A service that needs a Git capability calls `git/`'s public API; `git/` never reaches back into a service module. Where `git/` genuinely needs a service-side effect, the *consumer* declares the seam and the service implements it — today that is `git/worktree/warm::WarmClaim`, which the pre-spawn pool implements (issue #2533). A `services::` path under `src-tauri/src/git/` is a defect, not a shortcut. |
| **The stepper is pure** | `circuit/stepper.rs` | `advance(run, event) -> Transition` performs no I/O. SQLite state, PTY liveness, capacity counts and clocks arrive as `CircuitEvent`s from outside; the return value is `step_writes` plus `effects`. It is unit-testable with no database, which is why its tests run as a pure module. A change that needs a connection or a process handle does not belong here. |
| **The worker is not** | `services/circuit_worker/` | The worker owns every impure half of the circuit loop: observe → step → commit → execute effects, on a dedicated OS thread. It is the only place allowed to turn live state into `CircuitEvent`s and the only place that commits a `Transition`. Effect application (gate, claim, dispatch) is `circuit_worker/dispatch.rs`. GitHub calls stay in `circuit_worker/github.rs` and spawn overrides in `circuit_worker/spawn.rs`; a new effect kind extends dispatch, not the pass loop. |

Two more boundaries are load-bearing even though they are not "seams" in the
same sense:

- **`env/host_path.rs` is the only module allowed to build `\\wsl$\` paths.**
  Convert with `env::to_host_path` (`HostPath`) at the call site. A Linux or
  WSL path handed to a Windows API is the source of a whole class of
  "file not found" bugs.
- **A DB connection is passed through, never nested.** `db/mod.rs` owns the
  pool; public read fns check out `read_conn()`, public mutations take
  `write_conn()`, and each passes one connection into `_inner(&Connection)`
  helpers. Acquiring a second connection inside a call that already holds one
  deadlocks. `SCHEMA_VERSION` lives in `db/migrations.rs`, not in `db/mod.rs`.

## Seam debt

The rules above are the standard. No production file outside `git/` opens a
`git2` repository itself: the raw `Repository` handle stays inside the `git`
module, and callers ask for what they need through its capability-shaped
reads. `tests/agent-infra/git2-ownership.test.mjs` fails if a file outside
`git/` names `git2::`, and it fails if this list is not empty.

Test-only use (a `#[cfg(test)]` item, or a `*_tests.rs` / `tests.rs` file) is
not debt.

The seam was closed by issue #2526, which replaced the raw handle with
`git::state`'s `RepoState` / `Baseline` reads and the sibling `git::diff`,
`git::index`, `git::branches`, `git::remote` and `git::tree` modules. Do not
open a `Repository` in a command or in `circuit/`; do not copy a command that
does.

## Where things live

| Directory | Responsibility |
|---|---|
| `commands/` | Tauri IPC boundary. Thin adapters over services and `db/`. Every command registered in `lib.rs`. The git-owning exceptions are [seam debt](#seam-debt). |
| `services/` | Business logic between the command boundary and DB/IO. Long-running workers live here too (`circuit_worker`, `pool_worker`). |
| `db/` | SQLite pool, schema and migrations (`migrations.rs` owns evolution), plus domain query modules (`mesh`, `agent_node`, `warm_pool`, `circuit`). `db/mod.rs` owns the connection and `init` only; queries belong in a domain module. |
| `db/circuit/` | Circuit persistence: `ledger.rs` owns the `commit_circuit_advance` transaction, `evidence.rs` the outcome/evidence writes and reads, plus `queue.rs`, `leases.rs`, and the recovery modules. |
| `circuit/` | The circuit domain model and the pure decision core. `model.rs` is the graph AST; `stepper.rs` is `advance`; `vocabulary.rs` owns `RunState`/`StepStatus` strings; `capacity.rs` owns admission arithmetic. |
| `agent/` | Harness adapters (`provider/adapters/<id>.rs`), detection, launch recipes (`spawn.rs`, `spawn_environment.rs`), process supervision (`process.rs`), session lifecycle. |
| `git/` | New `git2` usage, plus the Git-domain coordination that belongs with it: `state.rs` (the capability-shaped reads — a `RepoState` at a named `Baseline`), `diff.rs` (the baseline tree and the diff walk), `status_stats.rs` (the one implementation of a changed file's line counts), `index.rs` (stage and revert), `branches.rs` (branch and worktree enumeration, branch deletion), `remote.rs` (repository identity and remote URLs), `tree.rs` (tree and blob plumbing), `sync_lock.rs` (per-Mesh fetch/sync lock), `fetch_freshness.rs` (per-Mesh fetch recency) and `sha.rs` (the crate's one SHA resolver, with an explicit source). `worktree/warm.rs` owns the pre-spawn pool's claim seam and the "usable warm worktree" rule. `git/` never imports `services/` — the dependency is one way (`services/` → `git/`). |
| `pty/` | PTY creation, lifecycle, registry, output sink. |
| `http/` | Loopback/LAN server: `server.rs`, `router.rs`, `auth.rs`, `tls/`, `ws.rs`, and `routes/` per resource. Routes are the second command boundary. |
| `env/` | Windows vs WSL detection, host-path conversion (`host_path.rs`), mesh row reads. |
| `preferences/` | Persisted user settings, loaded off the async pool. |
| `models/` | The shared DTOs. Split by domain (`mesh.rs`, `agent.rs`, `circuit.rs`, `git.rs`); `mod.rs` re-exports every type so `crate::models::…` paths stay stable. Wire types derive `TS` here and are generated, never hand-edited. |
| `sandbox/` | OS process confinement for agent PTY nodes — `restricted_token.rs`, `appcontainer.rs`, `acl.rs`, `conpty.rs`, and its own `spawn.rs`. |
| `session_naming/` | Backend-only node auto-naming from agent output (`engine.rs` observes output/turn boundaries, `words.rs`/`slug.rs` build the name, `repository.rs` persists it). PTY-side `session-id` capture does not belong here or in a caller. |
| `companion/` | Omnibar task lifecycle, bounded CLI inference and fleet tools; reuses `agent/background.rs`, coordinator reads and spawn services. |
| `coordinator/` | The agent-agnostic control-API read model (`node_digest.rs`, `enrichment.rs`, `drive.rs`) behind `http/routes/coordinator.rs`. |
| `diagnostics/` | Always-on resource diagnostics — the writer, the watchdog, and startup-failure capture. |
| `startup/` | Early-boot diagnostics for the failures that stop Buildmesh opening — an unwritable app-data directory, corrupt SQLite, a failed migration — captured before any logger exists. |
| `windowing/` | Native Windows caption-button affordances for the bespoke title bar (`snap_overlay.rs`). |

The `circuit/` and `services/circuit_worker/` pair is the reference example of
the pure/impure split: one domain, one pure decision module, one worker that
performs the effects.

## Where the large files still are

These modules are past the size where a change is easy to review. Treat each as
a candidate for the next extraction; the tests in them are the safety net that
makes a move behaviour-preserving.

| File | Non-blank lines | Notes |
|---|---|---|
| `services/circuit_worker/tests.rs` | 7,186 | The largest file in the crate — the circuit loop's tests. |
| `circuit/stepper/tests.rs` | 6,738 | The pure `advance` decision core's tests. |
| `circuit/stepper.rs` | 4,454 | The pure `advance` decision core. |
| `services/usage.rs` | 3,468 | Usage-meter orchestration. Per-provider logic already lives in `services/usage/adapters/`. |
| `db/circuit/ledger.rs` | 2,777 | The `commit_circuit_advance` transaction. |
| `db/circuit/evidence.rs` | 2,337 | Circuit evidence and outcome recording: `history`, `attention`, `record_outcome`, `commit_transition`, effect claiming. |
| `agent/process.rs` | 1,999 | Agent process supervision and teardown. |
| `services/circuit_worker/mod.rs` | 1,950 | The impure circuit loop. Effect dispatch lives in `dispatch.rs`. |
| `agent/provider/adapters/codex.rs` | 1,867 | One harness adapter. Attention-hook provisioning, install and runtime detection, native and WSL profile materialisation, then the `impl AgentProvider` trait surface. |
| `commands/diff.rs` | 1,841 | The per-file diff view: hunk building and syntax highlighting. The git2 side lives in `git/diff.rs` and `git/state.rs`. |
| `circuit/model.rs` | 1,840 | The graph AST and its validation and serialisation. |
| `commands/build_run.rs` | 1,733 | Build and run command orchestration. |
| `services/agent_node.rs` | 1,579 | Agent Node create, regenerate, and teardown orchestration. |
| `commands/pr.rs` | 1,283 | Pull-request commands. The git2 side lives in `git/remote.rs`. |
| `circuit/delivery.rs` | 940 | Delivering a step's prompt to a live agent. |

The counts are measured, not estimated. Re-measure before quoting one: the
numbers move as files are split, and a stale figure is worse than none.

When you split one of these, move code rather than rewriting it: an extraction
that keeps the same tests green and changes no runtime behaviour is the safe
kind. Prefer the cut the map above already names — a test module out of a
production file, or a named effect/adapter out of a loop — over inventing a new
layer.

## Adding a module

A new backend module should be reachable from one of the owners above and
declare its own boundary in a module-level doc comment (the pattern the existing
`git/`, `db/`, and `circuit/` modules use). If a new module introduces a new kind
of side effect, name the seam in this map too, so the next reader knows which
directory owns the capability.