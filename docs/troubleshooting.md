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

The wait is bounded: a probe that does not answer is abandoned and reported as a
load failure with a **Retry** button rather than leaving the tab stuck. The
provider pickers stay disabled while the check runs and are enabled once it
finishes. If a load fails, use **Retry** in the banner; the rest of Settings
remains usable while it does.

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

## Codex reports “Hook failed”

Buildmesh's Codex attention hook sends lifecycle updates to the local app. The
hook is best-effort, so an unavailable app or an already archived node should
not stop Codex. Restart the node from Buildmesh to refresh its project hook
configuration. If Buildmesh is closed, Codex can continue, but its lifecycle
state cannot be updated until a later callback succeeds. If the error persists,
use [What to include in a report](#what-to-include-in-a-report) and include the
Codex version, node status, and relevant redacted log lines.

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

## The app fails to start or closes unexpectedly

For a release install, check the stable profile; for a development build, use
the dev profile. The usual Windows locations are:

- `%APPDATA%\com.alond.buildmesh\logs\buildmesh.log`
- `%APPDATA%\com.alond.buildmesh.dev\logs\buildmesh.log`
- `panic.log` in the matching profile's `logs` directory

Record the Buildmesh version from **Settings → General → About**, the OS
version, and the last action before the failure. Do not upload the whole log if
it contains prompts, paths, credentials, or tokens.

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
