# Agent Node lifecycle and process architecture

Status: current

## Agent Node Management

### Normalized node store and the derived full-array selector (#1384, #2021)

`useAgentNodeStore` keeps Agent Nodes normalized: `nodesById` (keyed lookup, per-id object identity preserved across a refetch) and `nodeIds` (canonical `(mesh_id, position)` order). `fetchAgentNodes` reconciles field-by-field and keeps the *previous* object reference for every row whose fields compare equal, so per-id subscribers (`state.nodesById[id]`) and memoized rows skip renders when only other nodes changed. Consumers that genuinely need the whole list call `useAllAgentNodes()`; there are seven (Sidebar, AttentionList, AgentHistoryTab, CommandOmnibar, ScopeIndicator, GridFilterPopover, AgentNodeView).

`useAllAgentNodes` derives that array through `deriveAgentNodes`, memoized on the *identity of both containers* — so a write that touches no node (Circuit ownership patch, `loading`, `error`, `stalledInputs`, `schedules`) costs one `Object.is` pair per subscriber instead of re-scanning the id list, and one genuine change is derived once for all subscribers rather than once per subscriber. Profiling at 100/500/1000 nodes measured 700/3500/7000 `nodesById` dereferences for a single unrelated write before this change, and 0 after. The derivation returns the *previous* array when the fresh one is element-wise identical, which is the guarantee `useShallow` previously provided: a `fetchAgentNodes` poll that changes no field keeps consumers' array reference stable. A row that actually changed yields a new array so downstream `useMemo` grouping recomputes, while untouched rows keep their object references so per-row memo still bails. The archived filter and any ordering/sorting/pinned/activity interpretation stay in the consumers — the selector returns every live row including archived ones, because the Archive tab needs them. The hook returns `readonly AgentNode[]`: every subscriber shares ONE instance, so an in-place `sort`/`reverse` would corrupt all seven consumers and the cache without triggering a render. The type makes that a compile error rather than a convention; a consumer that needs its own order copies first. `scopeNodesForMode`'s `all` case hands back that same shared array, so `DerivedScope.visibleNodes` is readonly too, while the filtering cases return fresh arrays. Prefer a per-id selector when a consumer only needs one node.

**Background inference** is an adapter capability, independent of interactive
prefill, turn hooks, transcripts, and resume. `AgentProvider::background_recipe`
returns both the invocation and its `BackgroundInferenceCapability`; the
generated catalog and settings pickers derive eligibility from that recipe.
`agent::background` owns launch validation, prompt transport (stdin, argument,
or file), authentication environment, and final-answer extraction (stdout,
result file, or structured events). Naming and Circuit classifiers share it.
Unknown harness identifiers are rejected instead of using the legacy database
parser's Claude fallback. Both consumers own descendant cleanup through
`BackgroundProcessGuard`; callers supply isolated directories and execution bounds.
Claude command resolution preserves PATH lookup and Windows native/npm install
fallbacks for both binary spellings, unless an executable override is supplied.
Extra CLI arguments are rejected because they can change this protocol;
provider routes require explicit support from the background recipe. Adding a
runner belongs in its adapter, without an orchestration allow-list. Naming
preflight failures release in-flight ownership without consuming an inference
attempt or discarding the buffer, so repaired settings can retry on a later turn.
`session_naming.rs` captures PTY output and auto-names agent nodes via LLM summarisation (slug-based, e.g. `fix-auth-flow`). Buffering is gated: `on_output` only starts collecting after the first `on_turn` (first idle-prompt webhook) fires, so the Claude Code startup chrome — banner, "Bypass Permissions" warning, plugin/skill listing — is discarded before it can reach the LLM. The rename runs async one turn later, against clean post-startup content.

**Name uniqueness is load-bearing for Worktree Nodes.** A Worktree Node's `name` is also its `worktree_name`, its worktree directory, and (branched mode) its local branch — see `services/agent_node.rs` (`worktree_db_name = session_name`). Issue/PR spawns and Circuit spawn steps derive that name deterministically (`issue_node_name` / `pr_node_name` / `circuit_step_node_name` → `gh{N}-{slug}` / `pr{N}-{slug}`, plus the review variants), so a second spawn for the same issue or PR derives the same worktree path as the first. `git::worktree::provision_for_spawn` cannot recover from that: its warm path refuses the adoption because the branch is already checked out there, and its cold path's path-exists short-circuit hands the same directory to both nodes. (The warm-failure cleanup used to read "path exists" as "our move created it" and delete the other node's live worktree; it now only removes a target it created itself.) Any new spawn source that derives a name deterministically must therefore make it unique per Mesh — `session_naming::disambiguate_node_name` is the helper, and the PR pill's reviewer spawn (`create_pr_node` with `reviewer`) is the worked example.

### Create and import share one runner

`services::agent_node` owns both creating a node from a desktop or HTTP request and importing a discovered session. Commands and HTTP routes parse the request and map `AgentNodeError`. New worktree placement is one helper: mesh override, then the application worktree directory, then `.worktrees`, then the node segment. `env::resolve_worktree_node_raw` stays in `env` because path joining belongs with `HostPath`. Its definition and the call inside `resolve_agent_path_in_dir` remain in `env/host_path.rs`. The only production caller outside that module is the placement helper.

Import stores the conversation's originating harness verbatim and **requires** it: `import_discovered_session` resolves the id through `resolve_resumable_harness` and refuses an absent, unknown or non-resumable value with `AgentNodeError::UnresumableHarness` before the first DB call, so a refusal leaves no row (see "Harness provenance travels with the archive entry" below). Import names the node with `session_naming::on_spawn` and does not snapshot a launch configuration. `use_worktree` is true whenever a worktree name was supplied, including an empty one. The stored path is resolved only for a non-empty trimmed name, from the path on the mesh row. Discovery recognizes `.worktrees` and legacy `.claude/worktrees` even after a directory-setting change. If the current container has no materialized checkout, import checks these built-in locations before placing a new path. The desktop import command still accepts a mesh path on the IPC surface and does not use it.

An initial prompt is a non-empty string after trim. When a create carries one, the runner checks `supports_prefill` on the harness the launch plan will actually start, after the saved configuration and the plan have resolved, and before any row is inserted. A harness that does not accept a prompt returns `AgentNodeError::InitialPromptUnsupported` with the sentence `Choose an agent that supports an initial prompt`. HTTP maps that to 400. Desktop commands return the same sentence. No row is written. The check is the adapter capability, not `available_providers()`. Install detection stays on spawn preflight, so a missing binary is not reported as a prompt error. A prompted create of a prefill-capable harness that is not currently in the available menu creates the node and lets spawn preflight fail. Terminal, Kimi, and DSH prompted creates are rejected on both surfaces. Issue spawn, pull-request spawn, handover, the desktop omnibar, and mobile idea capture all pass their prompt through this rule. Circuit creates pass no prompt, so they keep the spawn pipeline's warn-and-skip for a harness that cannot take one. That spawn-time drop remains for a node that already exists. It is not a second create policy.

An unknown mesh is `MeshNotFound` for both create and import, including when the request also carries a prompt. HTTP create maps that to 400 "Mesh not found". HTTP import maps it to 404, and any other import failure to 500 — except `UnresumableHarness`, which is a 400 because the request named an executor this build will not resume, and no row was written.

### Archive list — durable identity and resumability are separate (issue #1065)

The Archive view (desktop probe tab and mobile `ArchivedNodesScreen`, both fed
by `services::agent_node_discovery::discover`) is built from two sources that
answer different questions. **Identity** — does this archived thing exist, with
a row, a worktree and a branch? — lives in `agent_nodes`. **Resumability** — is
there a transcript the harness can pick up again? — lives on disk. The list
must never derive the first from the second: before #1065 it did, so an
archived Terminal node (no `cli_session_id`) or any node whose transcript had
been deleted simply disappeared from the archive while its row and worktree
were still there.

`discover` reads the mesh's nodes once and uses that one read for both jobs:
ids owned by non-archived nodes are excluded from the disk scan (they are
already tracked), and archived rows the disk scan found nothing for are added
as durable entries. A durable entry is keyed by its `cli_session_id` when it
has one and by `node-<id>` otherwise, is labelled with the node's name, and
carries `resumable: false`. An archived node whose transcript *is* on disk is
already in the discovered set, so it keeps the single resumable entry and is
never duplicated or downgraded.

`resumable` is the only thing the surfaces branch on: a `false` entry renders
the node with an explanation instead of a Resume control, because importing it
would create a node whose session id points at a conversation that is not
there. The resume path itself is unchanged, and `agent::spawn::resume_guard`
remains the one place that decides whether a stored `cli_session_id` is a live
resume target (see the anti-pattern in `docs/knowledge-primer.md`).

### Harness provenance travels with the archive entry (issue #2332)

A `session_id` and a readable transcript do not say which CLI wrote them. The
four harnesses with an archive scanner all write UUID session ids, so an entry
with no recorded origin is genuinely ambiguous — and the old import read the
ambiguity as `anthropic`, which is how a Cursor, Command Code or Antigravity
conversation was launched through Claude Code. AGY is the loudest case: it
resumes with `--conversation <id>`, a flag shape no other executor in the
catalogue speaks, so a misrouted AGY conversation has no argv that can express
the resume at all.

`ArchivedAgentNode::provider` is that record. Each scanner stamps its own
adapter id at the point it builds the entry (`anthropic`, `cursor`,
`commandcode`, `agy`), a durable entry carries its row's own `agent_nodes.provider`
verbatim — including a non-resumable one like `terminal`, which is what
explains *why* that entry has nothing to resume — and `None` means **unknown**,
never "assume Claude Code". `node_id` rides along for the same reason: an entry
that reconciles to an archived row with the same `cli_session_id` keeps that
row's reference, so a caller can tell reopening known work from importing
external work. When a disk entry and an archived row describe the same
conversation, the scanner's provenance wins (it just read the transcript) and the
row contributes `node_id`; the row's `provider` is not allowed to overwrite it,
because it can be a stale executor from an older session.

Import is where the value is enforced. `resolve_resumable_harness` splits a Spawn
Option id (`claude:minimax` → `claude`), validates the harness half with
`preferences::harness_capabilities_for` — the known-harness check from #1148,
whose `None` is what keeps an unknown value out of `Provider::from_db_str`'s
Anthropic fallback — and rejects a harness whose capabilities do not include
`supports_resume` (Terminal, DSH). It runs before the first DB call, so a
refused import writes nothing. The id is stored **verbatim** once validated:
a Proxied Provider row keeps its Model Provider choice.

The HTTP route resolves provenance *before* it gates the session id, because the
gate belongs to the harness: `parse_session_id_for_provider` is the shared
dispatcher, so a `ses_…` OpenCode id or an `mvs_…` mcode id is accepted from a
harness that uses that shape while a UUID harness is still refused on anything
else. This keeps the `--dangerously-skip-permissions` argv-injection guard from
#1237 intact, now per harness.

**Compatibility.** A mobile client from before this change sends no `provider`,
and there is no server-side guess that repairs it: only the client that scanned
the transcript knows which harness wrote it. Omission is therefore a 400 with an
explicit "this client is out of date" message, not a silent default. An
installed older APK therefore resumes nothing from the archive — a visible
failure it can be upgraded out of, which is the correct trade against resuming
conversations in the wrong harness. Desktop is unaffected: the probe tab always
had an executor to send.

**Desktop default versus explicit change.** The primary Resume button uses the
entry's own harness. A mesh default provider is kept only when it already belongs
to that same harness — that is how a Proxied Provider user (`claude:minimax`)
keeps running Claude Code through their chosen account. A default pointing at a
*different* harness is replaced, because that is the bug. The `▾` picker stays,
so deliberately launching a conversation under another executor remains an
explicit choice. An entry with no known harness, or one whose harness has no
option here (not installed), shows "Resume unavailable" with the reason in its
tooltip instead of a button the backend would refuse.

**Lifecycle is unchanged.** `node_id` is a reference, not an action: import
still creates a new row, and an entry that happens to match an archived row is
still resumed by importing. Reopening the existing row instead is a separate
decision about node identity and configuration, deliberately not taken here.

### Profile ownership — one process per app-data profile (issue #1521)

Exactly one Buildmesh process may own an app-data profile, and the claim is taken in `setup` before `db::init` and before any worker starts. `instance_guard::with_profile_ownership` owns the ordering: everything that touches the profile lives in `run_profile_startup`, the continuation it only runs for the winner, so a second launch cannot open the database. That is the whole fix — the damage in a two-process profile was never the duplicate window, it was the loser's startup crash sweep running against a database full of live nodes it cannot see (`ProcessRegistry` is process-local), marking the winner's running Agent Nodes suspended so the frontend can resume a second harness in the same Worktree Node.

The claim is keyed on **bundle identifier plus canonicalised app-data dir**, so the stable hub and the `.dev` profile both run at once (they already differ in `http::port_offset` and in their data dir), and so two spellings of one path are one profile. Mechanism: a Win32 named mutex in the `Global\` namespace, created *without* initial ownership on Windows — `Global\` rather than `Local\` because a session-scoped object is invisible to a second Windows session, and an RDP reconnect or a scheduled task resolving the same profile must not become a second primary — the object's existence plus our open handle is the claim, so there is no release step to leak — and an exclusive non-blocking `flock` on `instance-<digest>.lock` in the profile dir elsewhere, where the digest is the same (identifier, profile) fingerprint. Both are released by the OS when the process dies, which is what lets the crash watchdog relaunch into a profile it can still claim.

A losing claim is retried for a moment before it is treated as a second launch, because Tauri spawns an update or restart successor from inside the *outgoing* process's `Exit` event: without that wait a relaunch would hand its activation to a process that is on its way out and leave the user with no app at all. A genuine losing launch restores and foregrounds the owner's window on Windows, appends a line to the profile's `logs/profile-ownership.log`, and exits 0; there is no portable window handle elsewhere, so macOS and Linux exit quietly and the user switches to the running window themselves. The lookup is by **process id**, not window handle: the owner publishes its pid in `instance-owner.pid` the moment it wins the claim, and the loser enumerates top-level windows for that pid, matching the one whose title starts with the shared `MAIN_WINDOW_TITLE_PREFIX` (`Buildmesh - `, set by `lib.rs`). A handle would have been the obvious choice and is the wrong one — `WebviewWindow::hwnd()` answers `RawHandleError(Unavailable)` while `setup` runs on Windows, because the native window is only created once the event loop starts, so a handle protocol has a hole in it that only shows up on a real second launch. A profile that cannot be resolved or claimed is a fatal, *visible* startup error (native message box, plus the same log), never permission to continue: an app that cannot say which profile it owns must not open that profile's database. `InstanceGuard` takes no `tauri::App` and no window, which is what lets a child-process test run the real gate — with a real `db::init` as its continuation — and prove the loser never reaches database initialization.

### Crash Recovery on Startup
`session_lifecycle::recover_from_crash()` (called from `lib.rs` setup) marks any agent nodes still showing `Running` status as `Suspended` during app startup, since a crash means no live process exists. These are then auto-resumed via the `auto_resume_agent_nodes` command on the frontend's first draw. A second sweep, `session_lifecycle::on_exit_sweep()`, runs from the `RunEvent::ExitRequested` callback to handle the graceful-shutdown case the same way; both wrappers live inside the `SessionLifecycle` module so the "exactly one place writes `agent_nodes.status` for suspend sweeps" invariant holds (issue #949, issue #132). The premise "a crash means no live process exists" is now enforced rather than assumed: the sweep sits behind the profile-ownership gate, so only the process that owns the profile reaches it (issue #1521).

### Startup readiness contract (issue #1524)
The window reaches the workspace only after the boot sequence returns a clean verdict, and `src/lib/bootSequence.ts` owns that contract. It runs the boot loaders concurrently and treats a failure as either a *rejection* or a *stored store error*. The second channel is not belt-and-braces: `meshStore.fetchMeshes` and `agentNodeStore.fetchAgentNodes` absorb their IPC failure, write `state.error`, and resolve, so `Promise.allSettled` alone reported a successful boot for a workspace that never loaded (it painted an empty workspace, which reads as data loss). Boot therefore uses the rejecting `meshStore.refreshMeshes` for the Mesh snapshot and cross-checks *both* stores' `error` fields once the loaders settle; both stores clear `error` when a load starts, so a non-null value belongs to the attempt that just ran. `isReady` is set only on a clean verdict, and every failure reaches `<BootErrorPanel>` as one `Source: message` line so the panel's **Retry** can re-run the load.
`agentNodeStore.initAttentionListeners` is the other half: listener registration is a three-state machine (`idle -> attaching -> attached`) with one shared in-flight promise, so React StrictMode's double-mount cannot register an event twice. `attached` is reached only after every `listen` resolves, and a mid-sequence failure makes `agentNodeListeners.attachAgentNodeListeners` roll back the already-registered handles before the store drops back to `idle`. The rollback isolates each unlisten handle: one that throws must not strand the remaining handlers, and must not replace the registration error Retry needs to report (it is logged and the original error is rethrown unchanged). The earlier boolean flag was set *before* the await, which is why a failed attachment was unrepairable: **Retry** short-circuited on the flag and the store stayed deaf to lifecycle events. Do not trade the state machine back for a "have we started?" boolean; the store has no unmount that could clean up a partial attachment, so a half-wired bus would be permanent.

To break unrecoverable restart loops where an expired session ID would otherwise be retried indefinitely:
- **Retry Resume (`↻` inline button):** Re-attempts spawning via `spawnAgent(node.id, node.provider)` with the existing `cli_session_id` (`SpawnIntent::Resume`) for transient failures (e.g. network blips or race conditions).
- **Start Fresh (Context Menu item):** Invokes `restartFreshAgent(node.id)` which calls `spawn_agent` with `resume = null` (`SpawnIntent::Fresh`). The backend `spawn_with_intent` pipeline detects `intent_replaces_conversation(&intent)` and executes `db::clear_cli_session_id(node_id)`, resetting `cli_session_id` to `NULL` in SQLite and launching the agent fresh in the existing worktree with correct terminal dimensions.

**Never-persisted sessions are not resumed in a loop (issue #2137).** The two affordances above are user-driven, so they cannot break a loop on their own. The loop came from the automatic path: `prepare_context` pre-writes a UUID to `cli_session_id` before launching (`SessionIdMode::Assign`), and the frontend's spawn-intent resolver turns *any* non-empty `cli_session_id` into a `--resume <id>` request. A node whose harness died before writing its first transcript (transcript saving off, #2136) therefore advertised a resume target that resolves to nothing, exited immediately, and was relaunched with the same dead id every few seconds — ~160 relaunches observed before a manual kill.

`agent::spawn::resume_guard` owns that, and it is consulted in the PTY reader epilogue, which is the one place that observes both facts the decision needs (this process died, and it was launched with `--resume`). The decision itself is the pure `classify_post_exit`; the DB write and the emits stay at the call site.

- **`resume_attempt` is the gate, not "a fast exit".** Only a `SessionIdMode::Resume` launch can invalidate an identity — a fresh spawn's pre-assigned UUID is not evidence that the conversation it names is missing, so fresh spawns keep their existing `Error`/`Idle` verdict.
- **`UNUSABLE_RESUME_WINDOW` (30 s) is deliberately wider than `EARLY_EXIT_WINDOW` (3 s).** The 3-second window only picks the `Error`-vs-`Idle` verdict; a harness that boots, loads its config and *then* discovers the conversation is gone routinely outlives it. Both exits are the same defect, so both must trip the guard.
- **First strike drops the identity, and says so on the wire.** `note_unusable_resume` returns `DropIdentity` and the epilogue clears `cli_session_id`, so the next auto-spawn resolves to `fresh` and starts a real conversation instead of re-issuing the doomed `--resume`. Clients keep their own copy of that column, so the clearing `Idle` transition carries `cleared_session_id` (`session_lifecycle::on_pty_eof_clearing_session`) and `lifecycleNodePatch` nulls it. Without that, a stale store keeps asking to resume the id the backend just discarded; the backend's old answer was an error, which the frontend answers by rolling status back to `idle` — re-arming the very auto-spawn that made the request, now with no delay at all.
- **`MAX_CONSECUTIVE_UNUSABLE_RESUMES` (3) bounds the repeats** and returns `Exhausted`, which the epilogue answers with `session_lifecycle::on_resume_exhausted` → terminal `Lost`. `Lost` is in `FORBIDDEN_TERMINAL`, so the frontend's idle-keyed auto-spawn effect cannot pick the node up again, and the stale kill at spawn step 2 (`kill_agent_blocking` → `on_idle`) does not resurrect it either. This transition fences only `Archived`: the usual terminal set would block the very `Error` → `Lost` escalation it exists to perform, and it emits `resume-failed` only when the write landed.
- **An exit the guard judged sound resets the budget** — in the reader epilogue, not on the `Spawning → Running` promotion. That promotion fires at `EARLY_EXIT_WINDOW` (3 s), well inside `UNUSABLE_RESUME_WINDOW`, so resetting there wiped every strike from a resume that died at 5 s and the bound was never reached. Giving up also clears the budget, so a later deliberate user retry starts clean.

Two supporting decisions, both deliberate:

- **`spawn_with_intent` falls back to a fresh launch when asked to resume with no stored id** (`ResumeSkipDecision::NoSessionId`) instead of returning an error. There is nothing to resume, and the error path was a hot relaunch loop whenever a client held a stale id. The startup-cause sibling still skips, leaving the node `Suspended` so the user's Resume button stays available.
- **The `Error`-shaped early exit does not carry the cleared id**, because `on_resume_failed` writes through `write_status_unless_in` and emits no `agent-lifecycle` event at all. That path cannot loop (`Error` is not the `idle` the auto-spawn keys on), and the fresh-launch fallback above covers the stale id it leaves behind.

Do not move this guard into the frontend or into `resolveSpawnAgentIntent`: it depends on observing a process death next to the launch mode, which only the backend has. Equally, do not "fix" the loop by making `on_idle` respect `FORBIDDEN_TERMINAL` — that write is what clears the recovery `Error` so an explicit Retry Resume / Start Fresh can spawn at all.

### Resume causes, and why regenerate branches instead of continuing (issue #1550)
`SpawnIntent::Resume` carries a `ResumeCause` so argv composition can tell three resumes apart — they look identical until the flags don't:

- `Startup` — crash-recovery auto-resume (`auto_resume_agent_nodes`). Honours `auto_resume_on_startup()`.
- `Explicit` — the user pressed **Retry Resume** / **Resume**.
- `Regenerate` — the user swapped Model Provider on the **same** harness. `decide_resume` accepted the captured `cli_session_id`, so the conversation is still reachable — but the run must not *keep writing to* the transcript it starts from, or a regenerate overwrites the very history it is regenerating against.

`AgentProvider::resume_args_fork(id)` is the per-adapter opt-in for the last case and **defaults to `resume_args`**, so harnesses with no fork concept keep their existing in-place resume and no CLI sees a flag it does not know. OpenCode is the only override so far (`--session <id> --fork`). Two constraints hold the seam:

- The cause rides on `SessionIdModeRef::Resume { id, cause }` rather than as a separate launch flag, so "fork without a resume" is unrepresentable at the only place that builds argv.
- Fork is a per-call-site decision keyed on the resume cause, **not** a `HarnessCapabilities` control, so `capability_recipe_coherence` (which asserts recipe flags agree with the advertised descriptor) is unchanged. Do not promote it to a capability without that test changing too.

Cross-harness regenerate still starts fresh — the captured id is bound to the old harness's session format.

### `AgentNode.branch` is overloaded (base ref vs PR head ref)
The `branch` field on `AgentNode` (see Rust doc comment at `src-tauri/src/models/mod.rs`) means two different things depending on spawn source:

- **Issue-spawned, hand-spawned, and handover-spawned nodes** — `branch` holds the mesh's `base_ref` (resolved via `commands::git::get_default_branch`, typically `origin/main`).
- **PR-spawned nodes** (issue #420, where `source_pr.is_some()`) — `branch` holds the PR's `head_ref` instead. The worktree is cut from `origin/<head_ref>` (or `fork-<owner>/<head_ref>` for fork PRs, issue #443) so the agent lands on the same commits the PR is built from.

**Disambiguator:** `source_pr.is_some()`. When set, treat `branch` as the PR head ref; otherwise as the mesh's base ref.

**Canonical reader:** `provision_workspace` in `src-tauri/src/agent/spawn/provision.rs` (called from `spawn_agent_inner`) derives the actual worktree `base_ref` from this field — see the `worktree_base_ref` block that keys off `node.source_pr.is_some()`. New code that needs the worktree's base ref should NOT reimplement the overload — call into that resolution or use the same `if source_pr.is_some()` pattern. The `commands::agent::create_pr_node` row-creation comment ("the *row* (`source_pr` is set, `branch` is the head ref) and in stage-2's `git fetch origin <head_ref>` worktree adoption") is the other half of the contract: the write side chooses the head ref precisely so the read side's `if source_pr.is_some()` switch lands on the right branch.

## Agent Process Architecture

### Provider-binary preflight (issue #823)
`agent::spawn::preflight::ensure_spawn_binary` runs at the top of the launch phase, **before** the PTY opens, and resolves the executable the spawn is about to dispatch. A miss becomes a structured `provider-error` toast naming the harness and the remediation; the terminal is never opened, so a missing CLI no longer surfaces as `'claude' is not recognized` scrolled inside PTY output the user has to read to diagnose.

It checks exactly what `spawn_environment::wrap` will invoke, in the order that function prefers it: the routing's `executable_override` (an absolute path detection already resolved — probed with `is_file`, since `wrap` dispatches that path rather than re-resolving), else the adapter's recipe stem re-resolved through `detection::resolve_spawn_binary`. Because the enriched search (`PATH` + npm prefix bins + user bin dirs + NVM shims) is a **superset** of the GUI process `PATH` a shell lookup would use, a miss means the shell could not have found it either — the preflight never rejects a spawn that would have worked. Keep new harnesses on `resolve_spawn_binary` rather than a bare `which`, or the preflight stops matching what the spawn does.

Two exemptions are load-bearing, both cases where host-side probing would be wrong rather than merely redundant:
- **`Provider::Terminal`** — its binary is the user's shell, which is never missing and never something the user installs; a miss message would be unactionable.
- **Guest-side runtimes** (`EnvType::Wsl`, `EnvType::WindowsInterop`) — the stem resolves *inside* the guest login shell, or on the Windows side of an interop spawn. Probing a `/home/...` guest path against a Windows host reports a false miss and would block a working spawn. This mirrors `launch_routing::spawn_time_executable`, which drops its host override for the same runtimes.

`PreparedLaunchRouting::pinned_runtime()` is how the check learns the runtime when the routing pins one itself: `CodexProxy` selects its npm-shim install during prepare, so its binary must be checked on *that* runtime rather than the one the mesh path implies. `Native` / `Environment` return `None` and inherit the mesh path's runtime — they must not pin one, or a WSL mesh would silently exempt every harness. When adding a routing variant that resolves its own runtime, return it here or the preflight will check the wrong filesystem.

The stem comes from `recipe_binary_for(provider, env_type)` using the **resolved** runtime, never a hardcoded `EnvType` variant: that function maps a runtime onto a `Platform`, and the mapping is platform-dependent (Claude Code's recipe binary is `claude.exe` on Windows and `claude` on macOS/Linux). `ensure_spawn_binary_with` takes the resolver as an argument so tests can force the bare-stem branch without depending on which CLIs the runner has — a test that hardcoded `claude.exe` failed the Linux and macOS CI runners on the first draft of this change (#2160 review). Keep preflight tests off literal stem tables; derive the expectation from `recipe_binary_for` and assert the platform variance separately with `cfg!`.

### ProcessRegistry — Runtime State
Agent state lives in a **static** `ProcessRegistry`: `HashMap<i64, Arc<AgentProcess>>` using `once_cell::sync::Lazy`. The DB is **not** the source of truth for running agents — it's only used for `cli_session_id` persistence across restarts.

### AgentProcess Fields
Each entry holds:
- `child` — `Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>`
- Writer channel — takeable internally so teardown can close it before joining the writer thread (issue #1531). Callers enqueue through `write_bytes`; a live sender plus a `recv()`-blocked writer pays the two-second join fallback.
- `queue` — `Arc<InputQueueGauge>` — live message/byte accounting for the writer channel, shared with the writer thread so the drain side can release each buffer as it drains. Without it the byte ceiling could be neither enforced nor observed, since `SyncSender` reports no depth (issue #1530)
- `master` — `Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>` — `take()` on kill/EOF closes the ConPTY (issue #300)
- `reader_alive` — `Arc<AtomicBool>` — set to `false` on PTY EOF; used to detect if an agent is still alive
- `generation` — per-incarnation token assigned at `insert`. `kill_session` and the reader-EOF `reap_incarnation` compare-and-remove against it so an old EOF cannot delete a replacement process (issue #1531)
- `job` — `Option<process_util::JobHandle>` — a Windows Job Object containing the agent's whole process tree (see *Killing the process tree* below); `None` on non-Windows or if assignment failed

The PTY handles are behind `Arc<Mutex<...>>` so the PTY reader thread and Tauri command handlers can both access them safely.

### Killing the process tree (Windows)
`kill_session` must kill **everything** the agent spawned, or a survivor pins the worktree's directory (as its CWD or via an open handle) and blocks removal on close. `taskkill /T` alone is insufficient: it walks *live* parent→child links, so it misses any descendant whose parent already exited — e.g. a dev server the agent backgrounded then orphaned. The fix is a **Job Object** (`process_util::JobHandle`): at spawn we assign the PTY shell to a kill-on-close job, so every process it later spawns is *contained* however it detaches. `kill_session` calls `TerminateJobObject` first (reaches detached/orphaned descendants), then keeps `taskkill /T` + `child.kill()` as fallbacks for the rare case job assignment failed. Assign happens immediately after spawn, before the shell launches the agent CLI, so the whole tree is covered. FFI to `kernel32` is declared inline via `extern "system"` (same no-new-deps pattern as `services::usage.rs`).

Teardown is centralized (issue #1531): compare-and-remove (or `kill_session`'s take of the current entry) claims exclusive ownership of the `Arc`, then cancel input (`close_input`), close the master, terminate/reap the process tree, join worker threads, and revoke Windows restricted-token grants. Natural PTY EOF goes through `reap_incarnation` with the same compare-and-remove so an idle node does not keep a registry slot, writer thread, child handle, or sandbox ACE. When `insert` overwrites a previous incarnation, it runs the same handle/thread teardown but **skips** sandbox revocation — grants are keyed by `session_id`, and the replacement spawn already registered them before `insert`. `kill_session` is the only public way to drop a live entry; an unconditional `remove` would delete a replacement spawn. Node-scoped output Channels stay registered across process incarnations.

### Worktree Support (git2-based)
The Mesh path is the selected project folder. `git::project` discovers its
nearest containing Git working repository, including nested repositories and
linked worktrees, and validates the relative folder beneath that working root.
Discovery and canonicalization run on blocking threads outside DB connections.
Bare repositories cannot supply a project working directory. Source Git queries,
worktree creation and `.worktreeinclude` use the discovered repository; Mesh
settings and grouping continue to use the selected path. Sync locks and fetch
freshness use the canonical common Git directory, so sibling folder Meshes share
Git coordination while their pool inventories and base refs remain separate.

`AgentNode.worktree_path` remains the exact owned checkout root.
`project_scope` persists the relative project folder (schema v49); an empty
string records a root project and NULL retains legacy root-cwd behavior.
`env::node_worktree_path` and `node_git_path` resolve checkout ownership and
repository queries; `node_working_path` adds saved scope for provider launch,
hooks, transcripts, builds and directory consumers. These helpers perform no
repository discovery. File-watcher events and `getNodeGitPath` subscribe to the
checkout identity; Project Files browses `getNodeWorkingPath` while status and
diff subscriptions retain the Git identity. Cleanup, close-safety
and sandbox ownership operate on the whole checkout. Non-worktree nodes start
in the selected folder and continue to work without Git.

Cold and warm manual/Issue/PR provisioning validate scope after checkout and
`.worktreeinclude`. A folder absent from that ref fails with its name and ref;
the provider does not silently start at the root. New scoped worktree names
include the Mesh id to separate sibling branch namespaces. Deterministic
Issue/PR identities are also reserved against stored worktree names under the
creation transaction. A warm manual upgrade refuses an existing branch.

Existing materialized nodes retain their saved location and legacy cwd on
resume. An explicit fresh launch of a node without a materialized checkout can save
the newly resolved scope. Resume reconstructs a saved cwd without discovering
the selected source folder; Mesh setting changes do not move the owned checkout
or replace its saved scope.

Automatic launch rejection stays `error`, including when the failure event is
missing or arrives before/after the IPC rejection. A frontend failure fence
keeps idle polling and pane remounts from re-arming it; explicit Retry/Start
Fresh clears that fence. Request ownership prevents old promise settlements
from regressing a newer attempt or reviving a deleted/archived row. Refreshing
the node list after an acknowledged launch is separate from launch failure.
The unusable-resume recovery guard remains responsible for early process exit
and conversation-identity exhaustion.

Buildmesh creates a dedicated worktree per agent node **itself**, via `git2` in `git/worktree/mod.rs` (`create_git_worktree` → `add_worktree_impl`) — for **all** providers, not just cwrap. This prevents concurrent agent node conflicts when multiple agent nodes target the same git repository. Two modes: `branched` (default, a real branch per worktree) and `detached` (a throwaway detached HEAD); both are cut from the configured Base Ref (default `origin/main`), resolved via `resolve_base_commit` with a fall-back to local `HEAD` when the ref is unresolvable (#230). See `docs/adr/0003-buildmesh-owns-worktree-creation.md` for why this moved off the agent CLI's old `-w` flag, and `docs/adr/0007-extract-git-module.md` for why the worktree lifecycle now lives in the `git` module.

**Failed-add repair (issue #2195).** `add_worktree_impl` returns `AddWorktreeFailure`, which carries an `AddWorktreeFailureKind` alongside the user-facing text; `create_git_worktree` matches the *kind*, never the wording. `RegisteredWorktree` is the one repairable conflict — the target path is already registered as a worktree whose directory is gone — and it prunes stale worktrees then retries once. Everything else (`Other`: a branch-name clash, an unresolvable base ref, a spawn failure) is returned to the user untouched. The classification lives in `classify_worktree_add_failure`, inside the one function that shells out, because git's prose is not a stable interface: current git reports the stale registration as `'<path>' is a missing but already registered worktree`, which contains neither "already exists" nor "already checked out", so the old call-site substring match never fired and the user got raw git output. The exclusion is load-bearing too — `A branch named '<b>' already exists.` also says "already exists", but pruning removes worktree admin entries and never branches, so the old match pruned unrelated worktrees and then failed with the identical message. Pinned by `classify_worktree_add_failure_recognises_the_registered_worktree_conflict`, `classify_worktree_add_failure_excludes_a_branch_name_clash`, `create_repairs_a_stale_worktree_registration`, and `create_does_not_prune_or_retry_a_branch_name_clash`. Adding a new `git worktree add` call site means reusing this failure type, not re-matching git's stderr.

**Worktree Provisioner** (`git/worktree/provision.rs`) is the four-branch decision that turns a Spawn Context into an on-disk worktree (`Reused` / `Adopted` / `Upgraded` / `Created`). The spawn pipeline (`agent/spawn/`) is four phases coordinated by `spawn_agent_inner` (issue #1427): prepare returns **workspace params** (git/disk) and **launch params** (PTY size, prefill, cascade overrides) as separate values; provision takes only workspace params; launch takes the provisioned workspace *plus* launch params; streams register the process. Do not courier launch knobs through the provision DTO — git worktree provisioning does not care about terminal columns. `spawn_agent_inner` acquires the in-flight claim as a named local before the phase calls (never a discarded `_claim` tuple from prepare — that reopens the #650 race). `spawn_with_intent` is the sole owner of `node-spawn-failed` and session-lifecycle Error writes; phase modules return `Result` so Resume can still call `on_resume_failed`. Prepare reads the mesh/node/warm-pool rows into `WorkspaceToProvision`. Provision builds a `SpawnContext` from those fields, hands it to `provision_for_spawn`, and lets the provisioner own post-spawn bookkeeping (forget-after-spawn, name adoption, status writes) internally — a provision `Err` is returned to the orchestrator, not handled here. Base-ref resolution (`SpawnContext.base_ref`) reaches the cold-path `add_worktree_impl` end-to-end via this seam (pinned by `provision_for_spawn_cold_created_uses_spawn_context_base_ref_not_local_head` for issues #230, #248). `.worktreeinclude` is applied by `apply_worktree_include` (`git/worktree/mod.rs`) on every cold-path `Created` and warm-path `Upgraded` outcome — including **recursive directory copy** as of #248 (previously log-and-skipped; pinned by `apply_worktree_include_copies_directory_recursively`).

**Resume:** worktrees are created only when the directory does not already exist (the `if !host_path.exists()` guard in `agent/spawn/provision.rs`), so resume simply re-spawns inside the existing worktree — no re-creation, and none of the old `-w` "already checked out" failures.

**Spawn-path subprocess budgets (issue #1229).** Every git CLI call in `git/worktree/` — the three mutators in `mod.rs` and the fetch helpers in `provision.rs` — runs on, or directly feeds, the click-to-terminal path (the provisioner's four branches, the PR-creation command that reuses the same primitives, and the warm-pool refresh), so none of them may use `Command::output()`/`status()`: both wait forever. The two wedges are a full checkout (`git worktree add` runs smudge/clean filters, and git-LFS pulls over the network) and a contended `.git/index.lock`. All of them route through `git::worktree::run_bounded_git`, a seam over `process_util::run_command_with_timeout` that kills the process tree on the deadline, records the wedge via `diagnostics::record_git_subprocess_timeout`, and names the command in its error — so the spawn-failure toast says which command gave up rather than showing a bare git error. Budgets are `GIT_TREE_TIMEOUT` (120s — `worktree add` and `checkout`, which can legitimately write a whole tree), `GIT_LOCAL_TIMEOUT` (30s — `worktree move`, `reset --hard`, the `remote get-url/add/set-url` trio, `rev-parse`), and `git::sync::SPAWN_FETCH_TIMEOUT` for the fork-head fetch. `reset --hard` is deliberately on the cheaper tier despite writing a tree, because a wedged reset leaves a warm-pool entry in `refreshing` with no path back to `available`: a bounded failure the pool retries beats an unbounded one it cannot. Adding a git shell-out under this module means `run_bounded_git`, not `run_command_with_timeout` directly — `spawn_path_git_shellouts_are_all_budgeted` scans both modules' production code so an unbudgeted call site fails the suite, and `run_bounded_git_surfaces_a_hung_command_as_a_named_err` pins the behaviour with a real wedged process.

**Auto-sync on spawn (issue #213, relaxed by ADR 0020):** before creating a *new* worktree (resume doesn't re-sync), `git::sync::fetch_origin` runs `git fetch <remote>` + `git pull --ff-only --no-rebase` on the parent mesh. The `--no-rebase` is required to defeat a global `pull.rebase=true` config — a rebase on a diverged history would write conflict markers to the working tree, silently mutating the user's local branch on what's supposed to be a read-only step. The sync is best-effort: dirty parents, no-origin repos, and already-up-to-date branches are silent; a fetch failure, diverged history, or unreadable repo surfaces a `mesh-sync-warning` toast (frontend label: `Sync`) and spawn proceeds from local HEAD. See `docs/adr/0001-auto-sync-mesh-on-node-spawn.md` and [[buildmesh-pull-rebase-default]].

**Spawn-time fetch TTL + background mesh sync (ADR 0020):** the spawn-time auto-sync is SKIPPED when the mesh was successfully synced within `services::fetch_freshness::SPAWN_FETCH_TTL` (5 min) — the background worker (`services::pool_worker`) re-fetches every idle worktree-enabled mesh once per `BACKGROUND_SYNC_INTERVAL` (3 min, gated on last *attempt* so an offline machine doesn't hammer retries) and triggers `warm_pool::on_fetch_completed` when the ref advances, so both the mesh and its warm pool stay continuously fresh without a network round-trip on the click-to-terminal path. All fetch paths stamp the registry via the `locked_*` wrappers in `git::sync`; the PR-head fetch is never skipped (correctness, not freshness); the manual Sync command is the "latest right now" override. Do NOT add a new fetch call site that bypasses `locked_fetch_origin` / `locked_do_sync` — it would fetch without stamping freshness and reintroduce redundant spawn-time fetches.

**Warm pool default (schema v24, ADR 0020):** `pre_spawn_pool_size` defaults to `1` (pool ON) for new meshes, with a one-time flag-gated backfill (`pool_default_backfill_v24`) for existing worktree-enabled meshes. Opt out per mesh via the Worktrees Probe.

**Meshes that are not git working repositories are not poolable (issue #2348):** `warm_pool::mesh_repo_unusable` is the precondition both git-shelling pool paths consult before doing any work — the idle fill and the ADR 0020 background sync. It answers with `git::project::resolve`, the same gate `create_git_worktree` applies before it touches anything else, so a Mesh whose folder cannot be opened as a git working repository is skipped outright: it can back neither a warm worktree nor a background fetch. The wording is "cannot be opened as" rather than "is not" because `resolve` also rejects a missing or unreachable folder (an unplugged drive) and a bare repository — the reason travels with the single log line, and no caller reports success. The probe is a cheap libgit2 open (no subprocess, no DB) and is only reached when work is actually due — a fill deficit, or a background-sync interval — so an at-target Mesh pays nothing for it. It is not latched: a Mesh whose folder is re-mounted, moved or `git init`-ed mid-session becomes poolable again on the next pass with no restart, and only the *log* is latched — one WARN per `(mesh_id, path)` per app run, so a re-pointed Mesh still reports itself and a second Mesh on the same folder is not silenced. `refresh_stale_warm_worktrees` is deliberately not gated: it runs only from a fetch that advanced the ref (unreachable for an unpoolable Mesh now) or from a post-spawn maintenance pass, and reports once per spawn rather than once per tick. Any failure that is not a rejected folder (a locked repo, a mid-checkout mesh) still falls through to the caller's existing error path. Before the gate, one such Mesh produced a doomed `git worktree add` plus a WARN on every idle tick *and* a `fetch_origin: path ... is not a usable git repo` WARN every `BACKGROUND_SYNC_INTERVAL` — 982 lines in one day of hub log, drowning the diagnostics people read while investigating status problems.

**Close/removal (optimistic + deferred):** closing a node is split in two. Phase 1 (`services::agent_node::delete`) kills the process tree (via the Job Object — see *Killing the process tree* above, so a dev server the agent spawned can't keep the directory pinned) and, in one transaction, deletes the `agent_nodes` row *and* enqueues the worktree into `pending_worktree_removals` — fast and authoritative, so the UI drops the node at once. The slow recursive directory delete (`remove_one_worktree`) runs as a background drain (`process_pending_removals`) that dequeues only on success; an app quit mid-cleanup is resumed by the startup reconcile in `lib.rs` `setup()`. Net: "node gone from UI" no longer implies "directory gone" — close is eventually-consistent on disk. See `docs/adr/0004-optimistic-node-close-deferred-worktree-removal.md`.

**Blocked cleanup (issue #2139, ADR 0043):** a removal that cannot complete is a *blocked cleanup*, not a toast that repeats. The drain (`process_pending_removals`) records the failure on the queue row — which step failed (the `git::worktree` operation vocabulary), the OS error, the attempt count and a backoff deadline — and asks for an event only when the blocker's *signature* (`operation: error`) is new, so an unchanged block warns once and then at most once an hour while it persists, instead of once per drain. Entries inside their backoff window (5 s doubling to 5 min) are not attempted at all. A removal whose working directory is already absent still reclaims a leftover `<path>.removing` staging sibling and prunes the orphaned git admin entry, so an interrupted removal can no longer leak either; failure to finish either half keeps the queue entry alive. The `worktree-cleanup-failed` event carries the node identity, the full path, the failed operation, the error, the attempts and the next retry, and records a toast plus an actionable card in the Host Notifications Probe. The bell sits immediately left of Zoom; the recovery dialog opens only after the user chooses Resolve. That dialog lists the live cleanup queue from `list_pending_worktree_removals` and offers Copy path / Copy diagnostics / What is holding it? / Retry / Keep worktree. "Keep worktree" is not a no-op: if a removal had already moved the worktree to its `<path>.removing` staging name, the staged copy is moved back, and the reply says whether the restore was complete. Diagnosis (`worktree_blockers`) is read-only and process-level (executable inside the tree, or a working directory inside it, including 32-bit WOW64 processes) — it names candidates the user can look at. `retry_worktree_cleanup` honours the warm-pool claim guard exactly as the drain does, so a manual retry never removes a live agent's worktree; and `release_worktree_cleanup_blocker` re-diagnoses and refuses a pid that is not a current blocker, so an id-reused process cannot be terminated. The removal path itself never terminates anything.

