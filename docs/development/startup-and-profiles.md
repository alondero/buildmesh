# Startup, profiles, and crash recovery

Status: current

## Profile ownership — one process per app-data profile (issue #1521)

Exactly one Buildmesh process may own an app-data profile, and the claim is taken in `setup` before `db::init` and before any worker starts. `instance_guard::with_profile_ownership` owns the ordering: everything that touches the profile lives in `run_profile_startup`, the continuation it only runs for the winner, so a second launch cannot open the database. That is the whole fix — the damage in a two-process profile was never the duplicate window, it was the loser's startup crash sweep running against a database full of live nodes it cannot see (`ProcessRegistry` is process-local), marking the winner's running Agent Nodes suspended so the frontend can resume a second harness in the same Worktree Node.

The claim is keyed on **bundle identifier plus canonicalised app-data dir**, so the stable hub and the `.dev` profile both run at once (they already differ in `http::port_offset` and in their data dir), and so two spellings of one path are one profile. Mechanism: a Win32 named mutex in the `Global\` namespace, created *without* initial ownership on Windows — `Global\` rather than `Local\` because a session-scoped object is invisible to a second Windows session, and an RDP reconnect or a scheduled task resolving the same profile must not become a second primary — the object's existence plus our open handle is the claim, so there is no release step to leak — and an exclusive non-blocking `flock` on `instance-<digest>.lock` in the profile dir elsewhere, where the digest is the same (identifier, profile) fingerprint. Both are released by the OS when the process dies, which is what lets the crash watchdog relaunch into a profile it can still claim.

A losing claim is retried for a moment before it is treated as a second launch, because Tauri spawns an update or restart successor from inside the *outgoing* process's `Exit` event: without that wait a relaunch would hand its activation to a process that is on its way out and leave the user with no app at all. A genuine losing launch restores and foregrounds the owner's window on Windows, appends a line to the profile's `logs/profile-ownership.log`, and exits 0; there is no portable window handle elsewhere, so macOS and Linux exit quietly and the user switches to the running window themselves. The lookup is by **process id**, not window handle: the owner publishes its pid in `instance-owner.pid` the moment it wins the claim, and the loser enumerates top-level windows for that pid, matching the one whose title starts with the shared `MAIN_WINDOW_TITLE_PREFIX` (`Buildmesh - `, set by `lib.rs`). A handle would have been the obvious choice and is the wrong one — `WebviewWindow::hwnd()` answers `RawHandleError(Unavailable)` while `setup` runs on Windows, because the native window is only created once the event loop starts, so a handle protocol has a hole in it that only shows up on a real second launch. A profile that cannot be resolved or claimed is a fatal, *visible* startup error (native message box, plus the same log), never permission to continue: an app that cannot say which profile it owns must not open that profile's database. `InstanceGuard` takes no `tauri::App` and no window, which is what lets a child-process test run the real gate — with a real `db::init` as its continuation — and prove the loser never reaches database initialization.

## Crash Recovery on Startup
`session_lifecycle::recover_from_crash()` (called from `lib.rs` setup) marks any agent nodes still showing `Running` status as `Suspended` during app startup, since a crash means no live process exists. These are then auto-resumed via the `auto_resume_agent_nodes` command on the frontend's first draw. A second sweep, `session_lifecycle::on_exit_sweep()`, runs from the `RunEvent::ExitRequested` callback to handle the graceful-shutdown case the same way; both wrappers live inside the `SessionLifecycle` module so the "exactly one place writes `agent_nodes.status` for suspend sweeps" invariant holds (issue #949, issue #132). The premise "a crash means no live process exists" is now enforced rather than assumed: the sweep sits behind the profile-ownership gate, so only the process that owns the profile reaches it (issue #1521).

## Startup readiness contract (issue #1524)
The window reaches the workspace only after the boot sequence returns a clean verdict, and `src/lib/bootSequence.ts` owns that contract. It runs the boot loaders concurrently and treats a failure as either a *rejection* or a *stored store error*. The second channel is not belt-and-braces: `meshStore.fetchMeshes` and `agentNodeStore.fetchAgentNodes` absorb their IPC failure, write `state.error`, and resolve, so `Promise.allSettled` alone reported a successful boot for a workspace that never loaded (it painted an empty workspace, which reads as data loss). Boot therefore uses the rejecting `meshStore.refreshMeshes` for the Mesh snapshot and cross-checks *both* stores' `error` fields once the loaders settle; both stores clear `error` when a load starts, so a non-null value belongs to the attempt that just ran. `isReady` is set only on a clean verdict, and every failure reaches `<BootErrorPanel>` as one `Source: message` line so the panel's **Retry** can re-run the load.
`agentNodeStore.initAttentionListeners` is the other half: listener registration is a three-state machine (`idle -> attaching -> attached`) with one shared in-flight promise, so React StrictMode's double-mount cannot register an event twice. `attached` is reached only after every `listen` resolves, and a mid-sequence failure makes `agentNodeListeners.attachAgentNodeListeners` roll back the already-registered handles before the store drops back to `idle`. The rollback isolates each unlisten handle: one that throws must not strand the remaining handlers, and must not replace the registration error Retry needs to report (it is logged and the original error is rethrown unchanged). The earlier boolean flag was set *before* the await, which is why a failed attachment was unrepairable: **Retry** short-circuited on the flag and the store stayed deaf to lifecycle events. Do not trade the state machine back for a "have we started?" boolean; the store has no unmount that could clean up a partial attachment, so a half-wired bus would be permanent.

## Startup bootstrap and fatal startup failures (issue #1525)

Diagnostics exist **before** the database does. `startup::bootstrap` runs first inside Tauri `setup` and does
three things in order: resolve the app-data profile, open the size-bounded `buildmesh.log`, and install the
tracing subscriber. Everything after it (`db::init`, preferences, the v19 repair migrations, harness
detection) is already inside a live `tracing` pipeline. Before this, all of that ran before any subscriber
existed, so its `warn!`/`error!` lines went nowhere: a user whose database would not open got no window, no
message, and no log, while the React Boot Error Panel (which needs the database to exist) nevertheless
promised that details had been written to `buildmesh.log`.

**One subscriber, installed once.** The fix is not a second, earlier subscriber, it is *this* subscriber,
installed as early as the profile directory allows and never replaced. So there is nothing to bridge and
nothing to rotate between two writers: early records are already in the same bounded `buildmesh.log` the rest
of the session appends to, and `try_init` is used rather than `init`, so a second attempt can never panic
with "a global default subscriber has already been set". The `tracing_subscriber::fmt().init()` that used to
sit in `setup` *after* `db::init` is gone; `Bootstrap::promote()` replaces it and only stops mirroring the log
to stderr. The writer is the same fixed-name `RotatingWriter` (`diagnostics::main_log_writer`) the rest of the
app uses, so a healthy startup still produces exactly one bounded log under the name the skills tail.

**Failures are typed, and retry is gated on stage.** `StartupFailure` carries a `StartupStage` (whose `label()`
is prose, because a modal dialog must not read `StartupStage::AppData`), a summary authored here rather than
derived from a driver error, a `SecretScrubber`-passed technical detail, and the resolved log and profile
paths. `StartupStage::is_retry_safe` is the behavioural line: `AppData` and `LogDirectory` fail before any
process-global state is installed, so re-running them is a real second attempt; everything from `Database`
onward is not, because `db::init` latches the global connection before it can fail and a second call
short-circuits to `Ok(())`. A "Retry" button on a database failure would therefore report success for a
database that is still broken.

**Corruption is reported, never repaired.** `startup::is_corruption` separates image damage (`DatabaseCorrupt`
/ `NotADatabase`) from reachability (`CannotOpen` / `PermissionDenied`), because the two send a user to
different places and a "your data is damaged" message aimed at a permissions problem would send them to
delete a perfectly good database. Nothing on this path renames, moves, or deletes a file: a corrupt database
is reported with its path and a `move ... .corrupt` command for the user to run themselves, and a test
asserts the file is byte-identical after classification.

**The error surface is native and pre-database.** `startup::present` shows a Win32 `MessageBoxW`, because a
Tauri command or the dialog plugin cannot help inside `setup` before the event loop starts (the plugin's
`blocking_show` deadlocks there, which is the same reason `instance_guard` raises a raw message box). The
button set is bound from the failure's action list by a pure, cross-platform-tested function, and the body
names what each Yes/No/Cancel does because Win32 will not relabel its buttons. The loop re-shows the dialog
until the user quits or a retry succeeds. `instance_guard::show_fatal_startup_error` is no longer used for
ownership failures: those now render through the same surface, so one failure reads like any other.

**Boundaries worth not re-litigating.** A `preferences.json` that will not parse is deliberately *not* a
fatal stage: the resolver accessors log and degrade to defaults by design, and refusing to launch over a
settings file would strand an app whose database is fine, so it is logged at `error` with the stage named
instead. Schema migration is not its own stage either, because `db::init` opens the connection and evolves
the schema in one call, so a failed migration *is* a `Database` failure by construction; only the
post-preferences v19 repair passes are separate, and those are non-fatal by design.

The frontend half is `commands::diagnostics::get_diagnostic_paths`, which hands `BootErrorPanel` the
resolved absolute locations from the bootstrap so the panel names a real path instead of a bare filename. It
performs no I/O of its own and cannot disagree with the files actually written.

## Environment Detection

- `env_for_path` — heuristics: `/mnt/`, `/home/`, `\\wsl$`, or `/` → WSL; everything else → Windows
- `to_host_path` — converts Linux paths to Windows UNC (`\\wsl$\Ubuntu\home\user`) for Windows-side file operations on WSL sessions

## Reproduction gotchas (Windows worktrees)

Preserve each file's encoding and line endings when editing; do not assume a repository-wide encoding. Prefer patch edits over shell whole-file rewrites. Inspect `git diff --check` and the actual diff for accidental encoding/whitespace churn. PowerShell double-quoted strings interpret backticks; use literal strings for Markdown/code snippets.

