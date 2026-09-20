# Buildmesh user guide

This guide covers the normal desktop workflow. Buildmesh runs the agent
programs you install; it does not install, authenticate, or upgrade those
programs for you.

## The mental model

- A **Mesh** is a repository workspace and its shared configuration.
- An **Agent Node** is one durable agent process, normally isolated in its own
  Git worktree and branch.
- An **Agent Harness** is the executable interaction surface, such as Claude
  Code, Codex, OpenCode, or Terminal.
- A **Model Provider** supplies credentials and model routing for compatible
  harnesses. A provider and a harness are not interchangeable.
- A **Worktree** is the directory Buildmesh creates for a node. The node's
  changes stay separate from the Mesh's parent checkout until you review and
  integrate them.

## Install and authenticate a harness

Buildmesh detects the executable you install; it does not install third-party
CLIs or provision their accounts. Install each CLI using its vendor's current
instructions, sign in in the same runtime where it will run, then restart
Buildmesh so the detection cache is refreshed.

| Credential path | Applies to | Where to configure it |
|---|---|---|
| Harness-owned login or API key | Claude Code, Codex, Antigravity, OpenCode, Cursor, Grok Code, Kimi Code, MiniMax Code, DeepSeek Harness, Command Code, Freebuff, and Meta Muse | The selected CLI's own login/configuration flow |
| Buildmesh provider account | Anthropic, MiniMax, and Kimi usage/quota integrations | **Settings → Providers** |
| Custom compatible endpoint | Claude-compatible model endpoints | **Settings → Providers**, then select the proxied profile for a harness |

The two columns are independent: a detected harness can run with its own
login, while a compatible provider profile can supply routing or usage data.
If a CLI is installed in both Windows and WSL, Buildmesh presents separate
runtime entries; configure credentials in the runtime you select.

A provider that isn't signed in right now keeps showing its last known usage for
up to seven days, marked **Last known value · …** on the Usage tab — so a fresh
start does not blank the meters you were watching. If nothing has been recorded
yet, its meter stays hidden.

## Your first session

1. Install Buildmesh from the [latest release](https://github.com/alondero/buildmesh/releases/latest),
   or follow the [source-build instructions](../README.md#build-from-source).
2. Install at least one supported agent CLI on the runtime where it will run.
   Native Windows and WSL installations are detected separately. Sign in to
   that CLI or configure its API key according to the CLI's own documentation.
3. Create or open a Mesh for the repository you want to work on. Confirm that
   Git can read the repository and that the base branch/ref is the one you
   expect.
4. Use the provider control in the sidebar to choose a harness, then create an
   Agent Node. `Ctrl+T` (`Cmd+T` on macOS) opens the new-node flow.
5. Give the node a focused task. Watch the terminal and the node status; an
   attention badge means the harness reported that it needs input, but the
   terminal remains the authoritative place to inspect the conversation.
6. Review the result in the File Tree or Changed Files panel. Run the Mesh's
   configured Build and Run commands when appropriate, then commit or create a
   pull request only after reviewing the worktree.

The first successful loop is: **install → configure → spawn → inspect → verify
→ integrate**. If a step does not behave as described, start with
[Troubleshooting](troubleshooting.md).

## Harnesses and capabilities

The Spawn Menu lists detected harnesses and any compatible proxied providers.
The current built-in catalog is:

| Harness | Resume after restart | Attention signal | Notes |
|---|---:|---:|---|
| Claude Code | Yes | Hook | Model, effort, extra-argument, and prefill controls may be available |
| Codex | Yes | Hook | Uses its own project trust and permission flow |
| Antigravity | Yes | Hook | Requires a supported CLI version for attention hooks |
| OpenCode | Yes | None | Transcript and attention behavior depends on the installed integration |
| Grok Code | Yes | Hook | Cross-runtime hooks need working Windows/WSL networking |
| Cursor | Yes | Hook | Effort control is not available through Buildmesh |
| Kimi Code | Yes | None | Use the terminal when no lifecycle signal is available |
| MiniMax Code | Yes | None | Model/effort controls depend on the selected harness configuration |
| DeepSeek Harness | Yes | None | Use the terminal for progress when no signal is available |
| Command Code | Yes | Passive watcher | Transcript-based lifecycle support is available. If typing does nothing, see [Command Code does not accept typing](troubleshooting.md#command-code-does-not-accept-typing) |
| Freebuff | Yes | None | Model and effort overrides are not available |
| Cline | Yes | None | Manage providers with `cline auth`; Buildmesh injects account credentials at spawn |
| Meta Muse | Yes | Passive watcher | Native on Windows since v1.3.0; WSL guest otherwise |
| Terminal | No | None | A plain shell; closing the app loses its live process |

Capabilities can change with the installed CLI and runtime. Buildmesh hides
controls that a harness cannot safely honor. After installing or removing a
CLI, restart Buildmesh so detection and runtime identity are refreshed.

For the complete harness-by-harness grid — resume, attention hooks, transcript
support, model and effort controls, and platform availability — see the
[harness capabilities matrix](learning/harness-capabilities-matrix.md).

On Windows, Buildmesh bundles Microsoft's ConPTY runtime so animated terminal
updates keep the text caret in place. Codex's Astra glitter and other harness
animations remain enabled. After upgrading Buildmesh, restart the app to use
the updated terminal runtime.

## Worktrees, branches, and review

By default, a new node receives a separate worktree. This prevents parallel
nodes from writing the same checkout, but it does not make the agent's work
correct or safe by itself:

- Review diffs before merging, pushing, or opening a pull request.
- A node's Build and Run utilities can run in the node worktree; root commands
  can be configured separately in Mesh Properties.
- Closing a node removes it from the UI first and cleans its worktree in the
  background. A cleanup warning means the directory may still exist; do not
  recreate or manually delete it until you have checked the warning.
- The Mesh may sync from its configured upstream before a new node is created.
  A sync warning does not silently discard the local worktree; inspect the Mesh
  health and Git status before retrying.
- Use the Worktrees view to understand which branch and path belong to which
  node. Do not move a live worktree behind Buildmesh's back.

## Settings that matter

Open **Settings** for app-wide defaults. A Mesh's **Project Settings** override
the app-wide value when both exist.

| Settings area | Use it for |
|---|---|
| General | Appearance, quit confirmation, global Autopilot capacity, and the default worktree directory |
| Providers | Credentials, provider routing, accounts, and custom compatible endpoints |
| Harnesses | Harness ordering and per-harness/proxied-provider defaults |
| Remote Access | LAN/VPN exposure, the Coordinator Read API, certificate management, and paired-device revocation |

The **Sandbox agent processes** option is per Mesh and is off by default. It is
an OS process boundary, not a VM or a promise that the agent cannot send data
over the network. Windows currently has weaker file-read/write confinement than
macOS; read [Agent sandboxing](../README.md#agent-sandboxing-security) before
enabling it for untrusted prompts.

## Build and Run

Configure **Build command** and **Run command** in Mesh Properties, or choose a
detected project preset. Optional Root Build and Root Run commands execute from
the Mesh root; leaving them blank falls back to the ordinary commands. Build
and Run use separate utility processes from agent processes and do not survive
an app restart.

## Remote access

Remote access is optional and off by default.

1. Open **Settings → Remote Access** and enable **Expose to LAN / VPN over
   self-signed TLS**. The setting applies immediately.
2. Open the **Remote Access** modal and scan the one-time pairing QR code from
   the phone you want to authorize. The invitation expires after five minutes.
3. For a fresh phone, use the Android or iOS **Install** tab to install and
   trust Buildmesh's root certificate, then use **Connect**. A self-signed
   certificate warning is expected until the root is trusted.
4. Manage paired phones under **Authorized Devices**. Revoking a device closes
   its active connections and requires a new pairing.

The loopback listener stays plain HTTP for local agent hooks. LAN/VPN interfaces
use HTTPS/WSS. The status panel lists the interfaces that are actually exposed;
an enabled toggle with no exposed interface is not a usable phone connection.
Do not share QR codes, pairing invitations, cookies, certificates, or logs.

The **Coordinator Read API** is a separate, read-only surface for an external
coordinator. It is off by default and uses its own capability-scoped token; it
is not the phone pairing credential. Use your own authenticated tunnel for
access outside the local machine or trusted LAN.

## AI context portability

Buildmesh can help a repository share `CLAUDE.md`, `.claude/skills`, and related
context with other supported agent tools through `AGENTS.md` and
`.agents/skills` links. Treat those files as code: review the diff before
committing and never put secrets in them. The project-level context for this
repository is described in [CONTEXT.md](../CONTEXT.md).

## Data, backup, and uninstall

Buildmesh stores meshes, prompts, and settings locally. See the README's
[data-location table](../README.md#data-location-logs-backup-uninstall) for
profile paths and logs. There is no automatic cloud backup. Back up the data
directory before removing an install, and redact credentials or tokens before
sharing diagnostic material.
