# Troubleshooting

Use the smallest safe recovery first. If the problem involves credentials,
pairing, sandboxing, a worktree, or a remote connection, preserve the relevant
log before retrying and redact secrets before sharing it.

## No agent CLI appears in the Spawn Menu

Buildmesh only lists a harness it can detect in the selected runtime.

1. Run the CLI's version/help command in the same host or WSL environment where
   the Mesh will run.
2. Check that the executable is on that runtime's `PATH`. A Windows install is
   not automatically a WSL install, and vice versa.
3. Sign in or configure the provider according to the harness's own setup
   guide. For proxied providers, add the credential in **Settings → Providers**.
4. Restart Buildmesh after installing, removing, or moving a CLI.

The plain **Terminal** harness can still be used when no agent CLI is found.

## Settings → Providers is slow to load

The tab shows a spinner while Buildmesh checks which harnesses and providers it
can launch. That check runs a few short version and capability commands per
runtime, so it is normally well under a second. It takes noticeably longer when:

- you have proxied a provider over OpenAI (a Codex route), which adds Codex
  version, help, and location probes on both the Windows and WSL runtimes;
- the WSL distribution is stopped, so its first command pays the full VM start
  (this can take several seconds);
- `codex` or `wsl` is slow to start because the disk or the WSL service is busy.

The Codex probes do not all run at once. Within one runtime, the three
identity lookups - version, executable location, and `CODEX_HOME` - overlap each
other, and the two capability `--help` probes then overlap each other; the help
pair has to wait for the version, because it is keyed on the resolved install.
The Windows and WSL runtimes overlap each other throughout. So the wait per
runtime is roughly the slowest identity lookup plus the slowest help probe, and
a stopped WSL distribution usually costs one VM start rather than one per probe.

The wait is bounded: a probe that does not answer is abandoned and reported as a
load failure with a **Retry** button rather than leaving the tab stuck. The
provider pickers stay disabled while the check runs and are enabled once it
finishes. If a load fails, use **Retry** in the banner; the rest of Settings
remains usable while it does.

## A node shows an "Input queued" warning

A clock glyph next to a node's name means Buildmesh is holding your keystrokes
instead of delivering them, because the agent has stopped reading its input.
The badge appears only after the delay has lasted long enough to matter — a
brief pause while typing normally shows nothing at all.

Your keystrokes are **not lost**. Buildmesh keeps them in order and retries
automatically, and the badge clears itself as soon as the agent starts reading
again. Nothing needs to be re-sent unless the badge is still there after the
agent has caught up.

The usual cause is an agent process that is busy, paused at a debugger or
permission prompt, or blocked in a long synchronous call, so it is not reading
its input stream. Give the agent a moment or send it a control character
(usually Ctrl+C) to interrupt it; the queue drains as soon as it resumes.

If the badge never clears, check the diagnostics log for the `pty_qmsg=` and
`pty_qbytes=` fields on the `DIAG` line: a byte count that climbs and then
returns to zero is a transient stall, one that stays high means the agent is
genuinely stuck, and a non-zero count with no queued work means a stuck
counter. Include that line in a [report](#what-to-include-in-a-report).
## "Buildmesh couldn't read your settings"

Settings shows a warning panel because `preferences.json` could not be turned
into settings. The file is **not** damaged by this and is **not** replaced:
Buildmesh refuses every settings write while the file is unreadable, so your
provider accounts, API keys, pairings, and Autopilot settings are still on
disk. The three actions in the panel are described in
[Settings that matter](user-guide.md#when-buildmesh-cannot-read-your-settings).

The log line is content-free by design - a `preferences.json` can still hold
API keys (always so on a machine without Credential Manager), so nothing from the
file is ever written to the log:

```
WARN preferences::storage::read_state preferences.json is corrupt (invalid_json): the file is not valid JSON (malformed JSON at line 1 column 24) - left on disk untouched, settings writes are refused until it is recovered
```

The reason code is worth reading:

| Reason | Meaning |
|---|---|
| `invalid_json` | The file is damaged, empty, cut short, or not text - most often an interrupted write |
| `not_an_object` | The file holds a JSON array or a bare value instead of a settings object |
| `schema_mismatch` | A field has a type this version cannot read - usually a file written by a newer Buildmesh |

To recover without the UI, stop Buildmesh, copy
`%APPDATA%\com.alond.buildmesh\preferences.json` somewhere safe, then either
delete it (a fresh start) or replace its contents with
`preferences.json.bak` from the same folder. Start Buildmesh again; the log
line above disappears once the file parses.

## A provider account says its key is in `preferences.json`

Buildmesh keeps provider API keys in the **Windows Credential Manager**, not
in `preferences.json`. When the credential store cannot be used - typically
because Windows has no logon session for the process, or Credential Manager is
unreachable - the key is left in `preferences.json` rather than dropped. The
affected account's card in **Settings → Accounts** says so, because a key
sitting in plain text in your app data folder is worth knowing about.

Nothing is broken and no action is urgent. To clear it:

1. Sign in to Windows (or otherwise make Credential Manager reachable).
2. Open **Settings → Accounts**, expand **Edit credentials** on that account,
   re-enter the key, and save.

The next write moves the key into the credential store, and the notice
disappears. On macOS and Linux there is no credential store, so keys stay in
`preferences.json` by design and this notice is always shown.

## Command Code does not accept typing

Command Code 1.56 and later can draw the prompt and then ignore keys on
Windows (including inside Buildmesh). This is an upstream CLI regression.
After updating Buildmesh, spawn a **new** node — typing and colour both work
on new nodes. If an already-open node is stuck, press Ctrl+C once, then type.
Downgrading the CLI to 1.55 with `COMMANDCODE_SKIP_UPDATES=1` also restores
typing outside Buildmesh.

## A Circuit says terminal input tracking is uncertain

An **Unverified Checkpoint** at **Await task** means Buildmesh cannot yet bind
the source agent's report to a known terminal input boundary. It does not mean
the task failed. Keyboard navigation or an incomplete escape sequence can make
the prompt contents uncertain, even when the terminal appears idle.

Inspect the source agent's prompt. Submit it with Enter if you intend to send
it, or use the harness's clear/cancel action (normally Ctrl+C; this can also
interrupt running work). Buildmesh rechecks automatically. Clearing scrollback
or dismissing attention does not clear the prompt. Other evidence requirements
still apply before the Circuit can advance.

Updated builds recognize separate Enter and Ctrl+C key events after an unfinished Escape/CSI
keyboard sequence; older builds could consume that recovery key as part of the
sequence and remain uncertain. Alt+Enter, paste contents and terminal string payloads
cannot establish a submission boundary.

## A completed Codex agent is waiting for a usable harness report

A Circuit can remain **Unverified** after Codex finishes when an older Buildmesh
report reader rejects valid system or developer context messages, such as those
emitted during a model switch. The checkpoint says the transcript contains an
unrecognised or malformed record. Rechecking the same transcript with that
reader cannot clear the problem.

Update Buildmesh to a build containing the Codex context-message fix. The
worker automatically rechecks a running Circuit's checkpoint. Handoff still
requires a current source process, session and input boundary. If updating
restarts the source, inspect the checkpoint and follow its displayed recovery
action; a report from before the new process incarnation cannot authorize
handoff. Newer activity, unknown message roles, pending human requests and known
unfinished work still block review.

## An Agent Node will not resume

Terminal nodes and agents that have not captured a session id are
non-resumable. For a resumable node:

- confirm that the original worktree still exists and has not been moved;
- use the node's **Resume** action or the Resume menu after startup;
- verify that the same harness and runtime are installed (native vs WSL matters);
- inspect the node error and `logs\buildmesh.log` for the provider's launch
  failure.

Do not delete a suspended node until you have reviewed its worktree and copied
any changes you need.

## The attention badge is missing or stale

`Ready` means a turn finished and the agent can accept another instruction;
`Completed` is an automation outcome. `Waiting for background work` means known
child or background work is still pending. Questions and approvals have separate
labels. These observations survive reconnects; the mobile agent overview shows
the last observation time and event source.

`Signal degraded` means a callback could not be interpreted reliably.
`Signal unavailable` means setup failed or the harness has no supported observer.
An old observation is not a heartbeat. Where delivery is merely not confirmed
yet, the node's status tooltip says so instead of showing a warning badge — an
unproven signal is the normal state between turns, not a fault. See the
[status observation contract](development/node-status-observation.md) for limits.

The terminal is the source of truth. Attention signals depend on the harness
and its integration; some harnesses have no hook or passive watcher.

- Keep the node open and inspect its terminal output.
- Confirm the harness is running in the runtime Buildmesh shows for that node.
- For a cross-runtime Grok hook, check Windows/WSL interoperability and
  mirrored networking.
- Restarting the node lets Buildmesh reinstall or refresh supported hooks.
- If the badge claims attention after you answered, check the terminal and
  capture the node status plus the surrounding log entries for a report.

Buildmesh writes those hooks into the harness's configuration files; the list
of files and what each one POSTs is in [Attention hooks Buildmesh installs on
disk](user-guide.md#attention-hooks-buildmesh-installs-on-disk).

## A node reports “attention hooks unavailable”

Buildmesh could not write the harness's hook, usually because the configuration
file it merges into does not parse — a half-finished edit or a leftover comma in
JSON, or a syntax error in Codex's and Kimi Code's TOML. The refusal is
deliberate: your file is left exactly as it was rather than replaced.

- Open the file named in the node's error. For Claude Code it is
  `.claude/settings.local.json` in the project directory; Codex uses
  `.codex/config.toml` and Kimi Code a `config.toml` in its configuration home.
- Repair the file's syntax, or delete the file to let Buildmesh recreate it with
  just its own entry.
- Restart the node. The write is retried and the status mark clears when a
  callback is delivered.

## Codex reports “Hook failed”

Buildmesh's Codex attention hook sends lifecycle updates to the local app. The
hook is best-effort, so an unavailable app or an already archived node should
not stop Codex. Restart the node from Buildmesh to refresh its project hook
configuration. If Buildmesh is closed, Codex can continue, but its lifecycle
state cannot be updated until a later callback succeeds. If the error persists,
use [What to include in a report](#what-to-include-in-a-report) and include the
Codex version, node status, and relevant redacted log lines.

## Codex refuses to start over a proxied provider

A node opened from a proxied provider (a Codex route) fails immediately with a
startup error instead of a prompt, for example:

```
error: the argument '--model <MODEL>' cannot be used multiple times
```

Codex accepts `--model` only once, so a launch that passes it twice is rejected
before the session starts. Older builds could do this for any proxied Codex node
whose launch resolved a model — which is every node spawned from a saved Launch
Configuration, because a proxied provider's model is the route's. Update to a
current build and start the node again: the launch command is rebuilt for every
start, and the CLI session never started, so there is no work to lose.

On a current build the only remaining way to repeat the flag is the verbatim
extra-argument layer: a Launch Configuration or Circuit step that puts `--model`
in its extra arguments is forwarded to Codex untouched, alongside the model
Buildmesh already passes. Remove it from the extra arguments and choose the model
in the configuration's model field instead.

## GitHub feeds fail for a WSL mesh

On a Windows host, Buildmesh reads the repository through its WSL network
path, then uses Windows-side GitHub credentials to fetch issues and pull
requests. Signing in to `gh` inside WSL alone does not authenticate the desktop
app. Run `gh auth login` in Windows if the error reports a missing token.

If the error says the repository is **not owned by current user**, Windows
libgit2 cannot verify the Linux owner's identity. For a repository you trust,
add its exact Windows path to Windows Git's global configuration in PowerShell:

```powershell
git config --global --add safe.directory '//wsl$/Ubuntu/home/your-user/your-repo'
```

Use the distribution, repository path, and network hostname shown in the error;
`wsl$` and `wsl.localhost` are distinct trust entries. Configure this in Windows,
then refresh the Issues or Pull Requests tab. A linked worktree has its own path
and needs its own entry if it reports the same error.

An unreadable repository reports a load error. A readable repository with no
GitHub origin still shows an empty feed.

## A phone cannot connect

Check these in order:

1. In **Settings → Remote Access**, confirm the toggle is enabled and the
   status lists at least one actually exposed interface.
2. Put the phone and computer on the same trusted LAN/VPN. Guest Wi-Fi and
   client isolation commonly block the connection.
3. Generate a fresh pairing code. It is single-use and expires after five
   minutes; an old QR cannot be reused.
4. If the browser reports a certificate error, install the current root CA from
   the Remote Access modal. After a trusted-root reset, every phone must trust
   the new root again.
5. If the toggle is enabled but no interface is exposed, inspect
   `logs\buildmesh.log` for TLS or interface-binding errors and retry after the
   network is available.

Never work around a certificate warning by disabling browser security on a
shared network. Remote access exposes terminal content and input.

## MiniMax Code rejects a configured model

Buildmesh passes the Launch Configuration's **Model** as the session-only
`--model` option, including when resuming. The interactive flag was verified on
native Windows with `mcode.cmd 0.6.5`; macOS, Linux and WSL are unverified.
Buildmesh has no model-flag version gate. The accepted compatibility risk is that
an older CLI without interactive `--model` rejects the launch inside the terminal.

Check `--version` and `--help` using the **exact executable Buildmesh launches**,
recorded as `executable` in its MiniMax Code harness profile in `preferences.json`.
For native Windows, invoke that full `.cmd` path from PowerShell, for example
`& 'C:\path\to\mcode.cmd' --version` and `& 'C:\path\to\mcode.cmd' --help`.
Plain `mcode` in Git Bash can select a different extensionless shim and report a
different version. For WSL, use the selected distribution's executable; for
macOS/Linux, use the resolved native executable. If its interactive help has no
`--model`, run that same executable with `update` or clear the model settings.

Use mcode's `/model` picker to find a model available to your account and enter
its full `provider/model` reference, optionally with `#variant`. Buildmesh accepts
only ASCII letters, digits, `.`, `_` and `-` in each part. Invalid references,
including shell characters, fail before spawning even if they came from a
Circuit override or a previously stored default. Replace or clear that value.
Clearing the configuration's Model field restores the application default from
**Settings → Launch Configurations**, in the **Agent Harness defaults** section,
then mcode's own default. Clear the MiniMax Code default there too to omit
`--model` entirely. See the
[MiniMax CLI docs](https://agent.minimax.io/docs/cli/features#model-references).

## My standalone `mcode` sessions now run in Full Access

Expected. When you launch a MiniMax Code node, Buildmesh sets
`permissionMode: bypassPermissions` in mcode's own settings file
(`<dataDir>/config.yaml` — `%USERPROFILE%\.minimax` on Windows,
`$HOME/.minimax` on macOS/Linux). The interactive `mcode` TUI has no
permission flag, so that file is the only lever the CLI offers.

That file is shared with `mcode` sessions you start yourself, so those run
in Full Access too until you edit the key back:

```yaml
# %USERPROFILE%\.minimax\config.yaml  (or ~/.minimax/config.yaml)
permissionMode: bypassPermissions   # full | ask | auto | off
```

Buildmesh re-applies the setting on the next node launch, so editing it
only helps for sessions started outside Buildmesh. Buildmesh rewrites only
that one line — your comments, ordering, API key and model catalog are left
as they were. See
[the MiniMax Code capability notes](learning/mcode-harness-capabilities.md).

## Circuit classification keeps failing or reports an expired login

The classifier has its own selection in Settings > Providers > **Circuit
classifier provider**. Changing an implementer or reviewer does not change it.
Select a working host-native Claude Code or native Codex configuration. A Codex
configuration can use `gpt-6-luna` with `low` effort and the existing Codex login.

The step displays the classifier's last error and stops automatic inference
after five failures. Once authentication or configuration is restored, choose
**Recheck evidence**. A restart or new report does not reset the exhausted budget.

## A Circuit leaves its prompt in the Codex input box

Update Buildmesh to a build with split-paste confirmation. On Windows, Codex can
render the start of a large prompt inline and show a **Pasted Content** marker
for the rest. Older builds expected that marker to count the entire prompt and
waited out the submission budget without sending Enter.

For an already staged prompt, inspect the input box before submitting it manually
or retrying the step, so the same prompt is not sent twice. New deliveries confirm
sequences of inline text and counted paste markers before sending Enter
separately. Before writing a multiline Codex prompt, Buildmesh applies Codex's
newline normalization and control-character sanitization; counts and visible
text use that same transformed prompt.

## MiniMax Code attaches the wrong conversation or never captures one

Buildmesh routes MiniMax callbacks using the native conversation id and workspace,
including callbacks from older shared plugins with a numeric URL. It no longer
guesses session ownership from manifest creation times. Standalone conversations,
duplicate conversation ownership, and ambiguous workspaces are rejected.

Use separate worktrees for simultaneous fresh MiniMax agents. If an older run
already has a wrong conversation id, preserve its worktree and history and recover
the verified original conversation before rechecking the run. Rechecking alone
cannot establish which conversation belongs to the implementer. See
[the runs 276/277 investigation](archive/2026-10/circuit-runs-276-277.md).

## Muse fails to start with `os error 267` or `Not a directory`

`AGENTS.md` and `.agents/skills` are Git symlinks. On Windows checkouts
with `core.symlinks=false`, Git stores them as plain pointer files and Muse
refuses to start because it cannot traverse `.agents/skills` as a directory.

- Launching through Buildmesh repairs those links automatically before the
  session starts, on both the native Windows and WSL runtimes.
- When invoking `muse` manually outside Buildmesh, restore the links
  yourself (Windows Developer Mode must be on; PowerShell's `New-Item`
  still requires elevation, so use `python -c "import os, ..."` with
  `os.symlink`, which does not):
  `AGENTS.md` → `CLAUDE.md`, `.agents/skills` → `..\.claude\skills`
  (backslashes — a forward-slash directory target is untraversable on
  Windows). Confirm `git status --porcelain` shows no change for either
  path afterwards, then retry.

## Git and `gh` fail with 401 inside a Muse node

Older Buildmesh releases started Muse with its built-in OS sandbox enabled.
That sandbox blocks the system credential store `gh` and git use for GitHub
sign-in, so `gh` reported no credential and https push/fetch returned 401
while every other agent worked.

- Buildmesh now launches Muse with the sandbox disabled. Start a **new** Muse
  node; existing sandboxed sessions must finish their work and be re-spawned.
- If you must finish work in an old node, run its git/gh commands from a
  terminal outside the Muse session — the credential store is reachable
  there.

## A Mesh is stale or sync fails

Buildmesh's sync path is conservative around changes that an incoming
fast-forward would overwrite.

- Review the Mesh health indicator and the changed paths it reports.
- Commit, stash, or otherwise resolve the overlapping local changes yourself.
- Retry **Sync from upstream** only after confirming the target branch/ref.
- A fetch may succeed while the fast-forward is blocked; that does not mean
  local work was discarded.

Do not reset or delete a worktree as a first response to a sync warning.

## Closing a node leaves "Worktree cleanup blocked"

Closing a node removes it from the UI immediately; its worktree folder is then
removed in the background. Windows refuses to remove a folder while any process
holds a handle inside it, so the cleanup can stay blocked. When it does, the
**Worktree cleanup blocked** dialog opens with the node's name, the full folder
path, the step that failed and the OS error — for example "being used by another
process":

1. **What is holding it?** lists the processes whose working directory or
   executable sits inside that folder. Closing those folders in those programs
   (or the programs themselves) is usually the whole fix. Explorer parked on a
   folder is the classic case; a background dev server or a shell whose working
   directory is inside the worktree is the next most common.
2. **Retry** attempts the removal again immediately. It is safe to press more
   than once: failed retries are recorded and the automatic retry backs off
   rather than hammering the folder.
3. **Keep worktree** stops the cleanup and keeps the folder. If a removal had
   already moved the folder aside, Buildmesh moves it back and tells you if the
   result was incomplete; if it could not move it back, the entry stays queued
   and keeps retrying.
4. **Copy diagnostics** produces one text block (node, path, failed step, OS
   error, attempts and the processes found) for a bug report. The list refreshes
   from the queue, so the dialog always shows what the background cleanup knows.

Ending a process from that dialog is deliberate and yours: it names the process,
it is only offered for a process that is still holding that folder, and it also
ends the processes that one started. Buildmesh never closes an application by
itself.

If nothing is listed, the holder may be one the process-level read cannot see
(a program that opened a file handle without a working directory in the tree).
Copy the diagnostics and retry after closing the programs you have open on that
folder.

## Cloning a repository fails

**Clone from GitHub** in the New Mesh dialog runs a plain `git clone` with your
machine's own Git authentication, and reports Git's own error in the dialog.

- **`fatal: repository … not found`** — check the `owner/repo` spelling and that
  the repository exists and you can reach it from this machine.
- **`fatal: could not read Username` / `Authentication failed`** — a private
  repository needs credentials that already work from your shell: an SSH key, the
  Git credential manager, or `gh auth login` followed by `gh auth setup-git` for
  HTTPS. Buildmesh never stores a GitHub token in the new repository, so an
  unauthenticated clone fails immediately instead of prompting.
- **`A folder already exists at …` / `A file already exists at …`** — the chosen
  parent already holds an entry named after the repository. Pick a different
  parent, or move the existing entry aside.
- **The dialog stays on "Cloning…" and then fails** — a very large repository over
  a slow link can outlast the clone timeout (10 minutes). Clone it from a
  terminal, then use **Open folder** on the result.

## Build or Run fails

Open Mesh Properties and verify the command, working context, and runtime. The
ordinary commands run in the node worktree; optional Root commands run at the
Mesh root. Confirm the dependency manager and CLI are installed in that
runtime, then run the command manually in the same directory. Include the
command, exit status, OS/runtime, and a redacted output excerpt in a report.

## A stored-state check reports damage

**Settings → Data & Diagnostics → Run quick check** (or **Run full check**)
reporting damage means SQLite found a problem in `buildmesh.db`. Buildmesh does
not repair or delete anything in response — your data stays on disk exactly as
it is, so you can still act on it.

What to do, in order:

1. **Export a copy** first, before touching anything else. Even a damaged
   profile is worth preserving; the export is the artefact you would otherwise
   lose.
2. **Restore** from your newest good snapshot or export, if you have one.
   Settings → Data & Diagnostics lists the snapshots in the `snapshots\`
   folder; pick one, read what it contains, then stage it and restart.
3. If a check fails *and* a warning in the same pane says Buildmesh kept a raw
   copy, that copy is the damaged original preserved for exactly this reason.
   Its path is shown in the warning. It is not a consistent snapshot, so
   restoring it may not include the most recent changes — treat it as a last
   resort.

**A restore was rejected.** Corrupt, truncated, hand-edited files, and files
from a newer Buildmesh than the one you are running are all refused, and your
current data is left untouched. If a file you exported yourself is refused, it
was most likely written by a newer build — check the Buildmesh version that
produced it.

**A restore was staged but nothing happened.** It applies on the next
**launch**. Restart Buildmesh. If you would rather not, **Cancel the staged
restore** in the same pane.

**Paired devices stopped working after a restore.** Expected. A restore of a
redacted export mints a new remote-access token, so every paired device has to
sign in again. Re-share the pairing QR from **Settings → Remote Access**.

## The app fails to start or closes unexpectedly

Launching Buildmesh again while it is already running looks like nothing
happening, on purpose: one process owns each app-data profile. The second
launch brings the running window to the front (restoring it if it was
minimized) and exits, leaving the running instance's Agent Nodes untouched.
On Windows that focus is automatic; on macOS and Linux the second launch
exits quietly and you switch to the running window yourself. Each launch
appends a line to `logs/profile-ownership.log` in the same profile directory
as `buildmesh.log`; that line is the record of a launch being forwarded. The
stable and dev profiles are separate, so both can run at the same time.

If Buildmesh instead reports that it cannot confirm it owns its profile, it
starts nothing at all. Read that log line for the underlying error — in
practice the app-data directory could not be read or written. Fix the
directory's permissions (or free some disk space, which can also make a
directory unwritable), then launch again.

There is no lock file to delete if the message persists. The claim itself is
held by an operating-system object — a named mutex on Windows, a locked file on
macOS and Linux — and the operating system releases it the moment the owning
process ends, including a crash or a forced kill. So if Buildmesh cannot start
and no other Buildmesh is running, the cause is the directory, not a stale
claim: check that the profile directory still exists and is writable, and that
free disk space is available. (`instance-owner.pid` in the profile directory is
only a breadcrumb recording which process owns the profile; deleting it has no
effect on whether the profile can be claimed.)

For a release install, check the stable profile; for a development build, use
the dev profile. The usual Windows locations are:

- `%APPDATA%\com.alond.buildmesh\logs\buildmesh.log`
- `%APPDATA%\com.alond.buildmesh.dev\logs\buildmesh.log`
- `panic.log` in the matching profile's `logs` directory

Record the Buildmesh version from **Settings → General → About**, the OS
version, and the last action before the failure. Buildmesh masks recognized
credentials in `buildmesh.log` before they are written: API keys, bearer
tokens, private keys, pairing links, and values logged under credential-like
names. Prompts and local paths are still in the file, so read it before you
upload it.

## Buildmesh shows a startup error and never opens

Some failures happen before Buildmesh's database is open, which means before
the app window can show anything at all. When that happens you get a native
error dialog instead of a window, naming the stage that failed and the absolute
path of the log it wrote to.

The dialog's buttons depend on the failure:

- **Copy these details** puts the stage, the message, the technical error, the
  log path, the build, and a timestamp on your clipboard. Paste that straight
  into a bug report.
- **Open the log folder** opens the profile's `logs` directory in your file
  manager. Offered whenever that folder actually holds the record of the
  failure.
- **Try again** re-runs the step that failed, and appears only for failures
  that happen before Buildmesh has installed anything — an app-data or log
  directory it could not create or open, for example. It is deliberately not
  offered for a database failure: Buildmesh has already latched its database
  connection at that point, so a retry would report success for a database that
  is still broken. Fix the cause and launch again instead.
- **Quit** closes the dialog and ends the process.

The stages you will see, and what each means:

- **the app data directory** — Buildmesh could not create or write the folder
  it keeps its data in. Check permissions and free disk space, then try again.
- **the log directory** — Buildmesh could not create or open its `logs` folder.
  Ant-virus and backup tools sometimes hold it. This is the one failure with
  no log behind it, so the dialog says so and the details are on screen only.
- **the app data profile** — another Buildmesh process owns this profile and
  the claim could not be established. See the section above.
- **the local database** — `buildmesh.db` could not be opened. See below.
- **startup services** — a post-migration step failed. Restarting usually
  clears it; if it persists, attach the log.

### If the database is reported as damaged

When Buildmesh says the database "is not a valid database", the file is
unreadable as SQLite. **Nothing has been deleted or changed** — Buildmesh never
renames, moves, or replaces a database it did not create, and it will not do so
for you.

To start with an empty Buildmesh, move the file aside yourself and launch
again:

```
move "%APPDATA%\com.alond.buildmesh\buildmesh.db" "%APPDATA%\com.alond.buildmesh\buildmesh.db.corrupt"
```

If the database matters — your Meshes and Agent Nodes live in it — copy it
somewhere safe before moving it, and mention the file in your report. Starting
empty loses the Meshes and Agent Nodes it contained, so it is your call, not
Buildmesh's.

## Buildmesh reports “Couldn't initialize Buildmesh”

This is the other half of the above, and it looks different: the window *did*
open. Buildmesh only finishes starting once it has loaded both authoritative
snapshots — your Meshes and your Agent Nodes. If either one fails, the window
says so instead of showing a workspace, with one line per failed part of
startup:

- `Meshes: …` — the Mesh list could not be read.
- `Agent Nodes: …` — the Agent Node list could not be read.
- `Event listeners: …` — startup could not subscribe to lifecycle events.

**Nothing is lost when this appears.** A snapshot that failed to load is
reported as a failure rather than drawn as an empty workspace, and no Mesh
or Agent Node is deleted, archived, or rewritten. Copy the raw text from
the panel, then press **Retry**.

Retry re-runs the whole startup load. A listener failure is fully
repairable: the listeners registered before the failure are removed, so the
next attempt starts from a clean event bus and the grid receives exactly one
update per lifecycle event. In practice the underlying cause is a database
that is locked, full, or not writable, or a backend that was still starting —
fix that, and the real workspace appears on the next Retry.

The panel names the absolute path of the log it wrote to, so you can go
straight to it. If Retry keeps reporting the same error, keep that file and
report it with the version from **Settings → General → About**. If Buildmesh
never gets far enough to show this panel at all, you get a native dialog
instead — see the section above.

## What to include in a report

Use the [bug report template](../.github/ISSUE_TEMPLATE/bug.md) and include:

- Buildmesh version and release/dev profile;
- OS version and native/WSL runtime;
- harness/provider and whether the node was fresh, resumed, or remote;
- minimal reproduction steps and expected/actual behavior;
- relevant redacted log lines and screenshots;
- whether a safe workaround exists.

For a suspected vulnerability, do not open a public issue; follow
[`SECURITY.md`](../SECURITY.md).
