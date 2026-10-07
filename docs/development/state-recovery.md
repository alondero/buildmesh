# State recovery, logging, and crash handling

Status: current

## The durable-state lifecycle (issue #1537, [ADR 0040](../adr/0040-state-recovery-snapshot-export-restore.md))

`services::state_recovery` owns the profile's durable state lifecycle. It is the **only** module allowed to capture, replace, or validate `buildmesh.db` as a whole; domain modules (`db::mesh`, `db::agent_node`, …) read and write rows and must never touch the file as a file.

- **What "durable state" means here** — three things, not two: `buildmesh.db` (SQLite), `preferences.json` beside it, and `tls/` holding the LAN root CA **private key**. The service owns the first two end to end and deliberately does **not** own the third. Windows Credential Manager holds provider OAuth blobs outside any file and is not exportable. Provider API keys are held there too (#830) but are inlined back into a full-fidelity bundle's copy of `preferences.json`, so a snapshot or an unredacted export still restores them.
- **Capture uses `VACUUM INTO`, never a file copy.** A WAL-mode database's committed content may still live only in the `-wal` sidecar. The capture opens a **private** connection rather than `db::read_conn()` — `VACUUM INTO` writes its destination file, and the project rule is that filesystem I/O must never run while a pooled or shared DB handle is held (issue #1228). A private connection also cannot deadlock against the writer mutex it is capturing.
- **Start-up hooks live in `run_profile_startup`, not in `db::`.** `apply_pending_restore` runs first (before `db::init`, so no connection, reader pool, circuit worker, or PTY exists), then `snapshot_before_migration`. Both open their own read-only connections to read `schema_version`, which keeps the layering `commands → services → db` — nothing in `db/` grows a dependency on `services::`, and `db/seam_tests.rs`'s `ALLOWED_PUB_CRATE_FNS` allowlist needs no entry. Both are **non-fatal**: a recovery failure must not block launch, because the user needs the UI to reach Restore. Failures are logged and surfaced as a `RecoveryNotice`.
- **Exports are a versioned `.bmsnap` container, not a folder zip.** `magic | u32 header_len | JSON header | payload blob`, with a SHA-256 per section. A zip of the app-data directory would sweep in `tls/ca.key.der` and the cleartext `remote_access_token`; the container gives a format-version gate, per-section integrity, and an explicit section list so "this export contains no TLS keys" is a checkable fact.
- **Redaction runs against the copy, never the original.** `build_bundle` captures to a staging file and deletes the credential rows/fields from *that*, so a bug in the redaction path cannot reach live state. `preferences.json` redaction deliberately does **not** round-trip through the `AppPreferences` struct — an export must survive a file written by a newer build, and going through today's struct would silently drop every field it does not know about.
- **`tls/ca.key.der` is never exported and has no "include secrets" toggle.** Anyone holding that key can impersonate the HTTPS identity every paired device trusts. Terminal transcripts are absent because durable state never held any (scrollback is xterm.js; agent transcripts live in harness session directories outside the profile).
- **Restore is staged, never immediate.** `stage_restore` verifies → snapshots the current state for rollback → extracts the payload and fsyncs it → *then* writes the marker. The marker-last ordering is what makes a crash mid-stage inert. `apply_pending_restore` deletes the stale `buildmesh.db-wal`/`-shm` sidecars before moving the restored file in: without that, SQLite replays the *old* database's WAL frames onto the restored file and corrupts it. It also re-runs both `quick_check` and `integrity_check` on the staged payload, because the bytes may have changed since staging.
- **Never silently reset.** A failed pre-migration check still writes a snapshot; when `VACUUM INTO` cannot run on a damaged file it falls back to a raw byte copy (labelled `*-raw`, with a `RecoveryNotice` naming the path) because losing the only copy is the unacceptable outcome. Integrity checks only ever *report* — nothing in this module rewrites a database to make a check pass.
- **Retention is `SNAPSHOT_RETENTION = 3`**, ordered by the timestamp slug in the filename with mtime as a tiebreak. Manual snapshots share the cap.
- **Wire types are ts-rs derived** (`StateRecoveryInfo`, `StateSnapshot`, `StateIntegrityReport`, `StateExportResult`, `StateRestorePlan`, `RecoveryNotice`) — never hand-declared in TS (issue #359). Frontend wrappers live in the `src/lib/tauri/stateRecovery.ts` facet and route through the `_invoke` chokepoint (ADR-0010). Save/open dialogs resolve **in Rust** (matching `commands::mesh::pick_mesh_folder`), because the app-data directory is not readable from the frontend and the capability file grants no filesystem access. A cancelled dialog returns `null`, which is a no-op and not an error.
- **Tests:** `src-tauri/src/services/state_recovery/tests.rs` (parallel-safe; private temp dirs, never the process-global `DB`) and `tests/unit/data-recovery-settings.test.tsx`. The rejection cases all route through `assert_rejected_without_side_effects`, which asserts the live database and preferences are byte-identical afterwards and nothing was staged.

## Logging and Crash Handling

- Logs written to `buildmesh.log` via `tracing-appender`. The subscriber is installed by the startup
  bootstrap (below), before the database opens, and mirrors to stderr only until `promote()` runs at the
  end of startup, so a *normal* session is file-only while a failing one is on stderr as well
- Panic hook writes to `logs/panic.log` with thread name, thread ID, and full backtrace
- **`RUST_BACKTRACE=1` is the launcher's job.** `Backtrace::capture()` (lib.rs:364) reads the env var at runtime and returns the "disabled backtrace" placeholder when it's unset. All four launchers (`scripts/run.ps1`, `scripts/run-dev.ps1`, `scripts/run.sh`, `scripts/run-dev.sh`) set it before launching; `tests/unit/launch-script-backtrace.test.ts` pins the contract so a refactor can't silently drop the env var. Without it, the "full backtrace" bullet above is a lie — the file would have one placeholder line.
- **A launcher's exit code is the launch verdict and nothing else.** The Windows launchers (`scripts/run.ps1`, `scripts/run-dev.ps1`) run under `$ErrorActionPreference = "Stop"`, which is script-wide: any error record aborts the script and `powershell.exe` returns 1, including errors from work the verdict does not depend on. The app holds `buildmesh.log` open while writing, so a bare `Get-Content` on it can fail with a sharing violation. Both launchers dot-source `scripts/launcher-common.ps1`, whose `Read-LogFile` retries a transient sharing violation and returns an explicit `{ Readable; Lines }` result instead of aborting, and whose `Compare-LogGrowth` owns every line-count delta. The explicit result type is the load-bearing part: collapsing a failed read to zero lines makes `0 -gt N` false, so a panic the hook has already written would be reported as a clean launch (#158), and a failed baseline makes every existing line look new and false-panics a healthy launch (#2043). An unreadable result is therefore neither a verdict nor a clean bill of health: the launcher skips that source and falls through to the next piece of evidence. A second, host-level failure is not preventable from inside a script: if the *consumer* stops reading stdout early (`... 2>&1 | Select-Object -First N`), the pipe closes under `powershell.exe` and it returns non-zero after `exit 0`. The `OK - ` line is therefore the authoritative success signal, and all four launchers share that prefix so a caller needs one matcher.
- **`panic.log` vs `panic_early.log`** — two hooks, two files (`lib.rs:41-128` + `lib.rs:348-382`). The early hook is installed in `run()` BEFORE Tauri setup so it catches panics during Tauri-init that the main hook (installed later in `setup()`) can't. Bundle-id is derived from the binary name (`buildmesh-dev.exe` → `com.alond.buildmesh.dev`), so dev-profile crashes don't pollute the stable hub's logs. Both hooks `flush()` + `sync_all()` because `panic = "abort"` kills the process via `__fastfail` before the OS file buffer flushes.
- **`panic.log` is invisible to `buildmesh.log` pattern scanning.** The main panic hook writes to the file + `eprintln!`s but never pushes to the tracing pipeline. `/verify`'s full-tier log-scan (issue #158) tails `panic.log` and `panic_early.log` separately and treats any new line as an unconditional fail; the `scripts/run-dev.ps1` and `scripts/run-dev.sh` launchers also fast-fail on the same condition so a panic-only crash can't masquerade as a successful launch.
- **`watchdog.log` is intentionally out-of-process.** On Windows, the main process starts the same executable in private `--buildmesh-crash-watchdog` supervisor mode. The supervisor opens and retains a handle to the exact parent process before setup continues, then records the OS exit code and expected-exit marker after the parent dies. It cannot use `tracing-appender` because that pipeline dies with the process it observes, so each forensic line is appended and `sync_all()`'d directly. The external supervisor is the sole Windows relaunch owner; the in-process `WindowEvent::Destroyed` path deliberately defers to it, avoiding duplicate launches across an unavoidably non-atomic process-spawn boundary. An unexpected exit relaunches Buildmesh under the shared 60-second `auto_relaunched_at` crash-loop guard. `CloseRequested` and `ExitRequested` write a per-run expected marker, while non-Windows retains the guarded in-process WebView relaunch fallback. Set `BUILDMESH_DISABLE_CRASH_WATCHDOG=1` for debugger sessions that intentionally hard-kill the app.
- The main log is written through `startup::SharedLog`, a `Mutex<RotatingWriter>` held
  synchronously, **not** through `tracing_appender::non_blocking`. That wrapper cannot be
  used for a log a fatal failure depends on: `tracing-appender` 0.2 has no way to force its
  queue to disk (`NonBlocking::flush` is a no-op and `WorkerGuard` has no `flush`), and a
  failed startup ends the process moments later. Writing inline costs one uncontended mutex
  and one `write` syscall on the emitting thread, which is what the panic hooks and the
  diagnostics sampler already do, and it lets `Bootstrap::record_durably` `sync_all` the
  failure line before the error surface appears. A single shared handle also keeps rotation
  honest: two independent writers would each track their own byte count against the cap.
- `RotatingWriter::sync` exists for that one caller. `write_line` fsyncs every
  `SYNC_EVERY` lines, which suits a continuous sampler but not a record that must survive a
  process ending immediately after it.

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

**The file is scrubbed before it is written.** Frontend `console` lines enter through `log_frontend`, which
masks the message and then caps it. The subscriber writer masks every line again on the way into
`buildmesh.log`, and the bootstrap stderr mirror gets those same masked bytes. Masking covers provider
key shapes, bearer and private-key material, `#pair=` invitations, `ticket=` handshake values, and every
leaf under a JSON key that names a credential — a nested array, or a bare number such as a numeric PIN.
A frontend payload larger than 64 KiB is omitted rather than cut through the middle of a secret.
Ordinary diagnostic text is left in place. The structured pass is bounded per line (brackets resolved
once, then a JSON parse budget), so a payload of nothing but braces cannot stall a logging thread;
whatever falls outside that budget is still masked by the text rules.
Prompts and local paths are not secrets and stay in the file.

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

