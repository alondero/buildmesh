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

## Command Code does not accept typing

Command Code 1.56 and later can draw the prompt and then ignore keys on
Windows (including inside Buildmesh). This is an upstream CLI regression.
After updating Buildmesh, spawn a **new** node — typing and colour both work
on new nodes. If an already-open node is stuck, press Ctrl+C once, then type.
Downgrading the CLI to 1.55 with `COMMANDCODE_SKIP_UPDATES=1` also restores
typing outside Buildmesh.

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
