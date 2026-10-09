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
| **Commands are thin** | `commands/` | A `#[command]` is an adapter: validate, call one service or `db::` function, map the error to `String`. No policy, no SQL, no `git2` handles. Every new `#[command]` must be registered in `lib.rs` or it fails at runtime with "command not found". |
| **Git access lives in `git/`** | `git/` | All direct `git2` use goes through `git/primitives`, `git/worktree`, `git/sync`, `git/health`. `commands/git.rs` and `commands/prune.rs` are adapters over it (ADR 0007). If you need "is the repo dirty", the ahead/behind count, or the short SHA, call `git::primitives` — do not open a second `Repository`. |
| **The stepper is pure** | `circuit/stepper.rs` | `advance(run, event) -> Transition` performs no I/O. SQLite state, PTY liveness, capacity counts and clocks arrive as `CircuitEvent`s from outside; the return value is `step_writes` plus `effects`. It is unit-testable with no database, which is why its tests run as a pure module. A change that needs a connection or a process handle does not belong here. |
| **The worker is not** | `services/circuit_worker/` | The worker owns every impure half of the circuit loop: observe → step → commit → execute effects, on a dedicated OS thread. It is the only place allowed to turn live state into `CircuitEvent`s and the only place that commits a `Transition`. GitHub effects are in `worker/github.rs`, spawn overrides in `worker/spawn.rs`; a new effect kind extends those, not the pass loop. |

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
  deadlocks.

## Where things live

| Directory | Responsibility |
|---|---|
| `commands/` | Tauri IPC boundary. Thin adapters over services and `db/`. Every command registered in `lib.rs`. |
| `services/` | Business logic between the command boundary and DB/IO. Long-running workers live here too (`circuit_worker`, `pool_worker`). |
| `db/` | SQLite pool, schema and migrations (`migrations.rs` owns evolution), plus domain query modules (`mesh`, `agent_node`, `warm_pool`, `circuit`). `db/mod.rs` owns the connection and `init` only; queries belong in a domain module. |
| `db/circuit/` | Circuit persistence: `ledger.rs` owns the `commit_circuit_advance` transaction, `evidence.rs` the outcome/evidence writes and reads, plus `queue.rs`, `leases.rs`, and the recovery modules. |
| `circuit/` | The circuit domain model and the pure decision core. `model.rs` is the graph AST; `stepper.rs` is `advance`; `vocabulary.rs` owns `RunState`/`StepStatus` strings; `capacity.rs` owns admission arithmetic. |
| `agent/` | Harness adapters (`provider/adapters/<id>.rs`), detection, launch recipes (`spawn.rs`, `spawn_environment.rs`), process supervision (`process.rs`), session lifecycle. |
| `git/` | All `git2` usage. See the seam table. |
| `pty/` | PTY creation, lifecycle, registry, output sink. |
| `http/` | Loopback/LAN server: `server.rs`, `router.rs`, `auth.rs`, `tls/`, `ws.rs`, and `routes/` per resource. Routes are the second command boundary. |
| `env/` | Windows vs WSL detection, host-path conversion (`host_path.rs`), mesh row reads. |
| `preferences/` | Persisted user settings, loaded off the async pool. |

The `circuit/` and `services/circuit_worker/` pair is the reference example of
the pure/impure split: one domain, one pure decision module, one worker that
performs the effects.

## Where the large files still are

These modules are past the size where a change is easy to review. Treat each as
a candidate for the next extraction; the tests in them are the safety net that
makes a move behaviour-preserving.

| File | Approx. lines | Notes |
|---|---|---|
| `circuit/stepper.rs` | ~3.3k | The pure `advance` decision core; its `#[cfg(test)]` tests now live in `circuit/stepper/tests.rs`. |
| `db/circuit/evidence.rs` | ~5.0k | Circuit evidence and outcome recording: `history`, `attention`, `record_outcome`, `commit_transition`, effect claiming. |
| `agent/provider/adapters/codex.rs` | ~3.5k | One harness adapter. Attention-hook provisioning (~25 helpers), install/runtime detection, native + WSL profile materialisation, then the `impl AgentProvider` trait surface. |
| `circuit/model.rs` | ~3.4k | The graph AST and its validation/serialisation. |
| `services/usage.rs` | ~3.2k | Usage-meter orchestration; per-provider logic already lives in `services/usage/adapters/`. |
| `agent/process.rs` | ~2.8k | Agent process supervision and teardown. |

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