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
On Windows, the spawn menu offers Windows-capable harnesses only from a native
installation; a WSL-only installation is not substituted when that native CLI
is absent. A WSL entry is offered only for a harness without Windows support.
Saved WSL recipes for Windows-capable harnesses remain stored but do not appear
in spawn menus. Configure credentials in the runtime you use.

A provider that isn't signed in right now keeps showing its last known usage for
up to seven days, marked **Last known value · …** on the Usage tab — so a fresh
start does not blank the meters you were watching. If nothing has been recorded
yet, its meter stays hidden.

## Your first session

1. Install Buildmesh from the [latest release](https://github.com/alondero/buildmesh/releases/latest),
   or follow the [source-build instructions](../README.md#build-from-source).
2. Install at least one supported agent CLI on the runtime where it will run.
   Native Windows and WSL installations are detected separately, but the spawn
   menu offers WSL only for harnesses without native Windows support. Sign in
   to that CLI or configure its API key according to the CLI's own documentation.
3. Create or open a Mesh for the repository you want to work on. **Open folder**
   points at a repository already on disk; **Clone from GitHub** takes an
   `owner/repo` or github.com URL, clones it into a new subfolder under a parent
   folder you pick, and creates the Mesh from the clone in one step. Confirm that
   Git can read the repository and that the base ref is the one you expect.
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

## Configure Autopilot Circuits

Open **Circuits** to configure automated flows and their triggers. **Max concurrent
circuit runs** controls how many runs the selected mesh can admit at once.
Settings > General > **Circuit agent pool size** adds an optional cap on agents
across all meshes; leave it empty for no global cap, or use 0 to pause new launches.

Agent steps use their explicit launch configuration, then the mesh default, then
the application default. Review agents use their configured reviewer selection.
Settings > Providers > **Circuit classifier provider** selects the background
launch configuration used to classify reports. It defaults to Claude Code and
is independent of agent and reviewer defaults. Select host-native Claude Code
(including a compatible provider route) or native Codex. Saved model and effort
settings apply; for example, save a Codex configuration with model `gpt-6-luna`
and effort `low`, then select it here. Codex uses its existing login and supports
model and effort settings without extra CLI arguments.

After five unsuccessful classifications, the step remains **Unverified** with
the last failure displayed. Automatic retries stop, including after an app
restart. Restore the configured backend, then use **Recheck evidence** to retry.

Issue-driven review flows prepare a draft pull request. Customize prompts and
publication steps in the Circuit editor. The shared wrap-up template is stored in
`circuits/finish.md` under the application data directory.

Legacy Autopilot controls are removed. Existing nodes, worktrees and history are
retained, and the previous global agent cap and custom wrap-up template are carried
forward. Old mesh-level Autopilot settings have no effect on Circuit launches.

## Understand a circuit checkpoint

An **Unverified Checkpoint** means Buildmesh cannot currently establish the
evidence needed for the next circuit step. It does not mean the agent failed,
and a Done label on the agent does not by itself prove the circuit can advance.

Expand the run and inspect its reason and Circuit Run History. The reason can
identify missing session/report evidence, changed input, or uncertain input
tracking. Finish or clear a real draft before rechecking; answer an actual
question or permission request in its session. **Recheck evidence** asks
Buildmesh to inspect again without repeating an uncertain external action.
Unverified agent steps also continue observing for fresh evidence automatically.

A finished report can hand work to the next gate even when a harness cannot
expose all background-task ownership. Known unfinished work and actual input
requests still block progress. Review approval and merge requirements remain
separate. A slow classification or verification operation is queued independently
so other sessions can continue being observed.

## Group nodes into tabs

In a grid with **Custom** ordering, drag a node by its title onto another node
in the same Mesh. Drop on the title or upper centre to **Group as tabs**; drop
in the lower centre to **Swap** positions. The highlighted half previews the
action. Side edges below the title still insert before or after the target.
Dragging is disabled in Pinned and Single views and with other sort orders.

The PR pill can group a node for you: click the **PR #N** chip in an agent's
title and pick **Spawn reviewer agent…**, then a harness. Buildmesh launches an
agent on that pull request's head commit — the same spawn the Pull Requests
probe's **+** performs — in its own worktree (on Meshes that use worktrees) and
groups it as another tab on the node you clicked.

This is a plain agent on the pull request, not the review loop: it reads the PR
and writes its report in its own tab without sending findings back. Use the
title bar's **Start review or circuit** control for the automated
implementer/reviewer loop that returns findings to the agent for fixes.

A group shares one viewport, with named tabs for its agents. Review and
Build/Run/Terminal tabs stay with their owning agent. Dragging a grouped card
moves the whole group, and dropping it onto another group combines their tabs.
Use **Move selected node out of group** beside the tab strip to separate the
selected agent and its review/utility tabs again. Tab arrow keys, Home/End, and
the All sessions menu work in groups too.

Groups survive app restarts in this desktop profile. Grouping only changes
the layout: agents retain their own processes, worktrees, and close actions.
If the group's title node is archived or deleted, surviving members remain
accessible. Grouping does not combine nodes from different Meshes.

### Groups in the sidebar

The sidebar shows the same groups, so you can see a pairing from either side.
Paired agents appear as one cluster: the group's title node on top, the other
agents indented beneath it and joined by a connector rail. A slim marker on
the left of the title row and a hover tooltip naming every member (with its
role) make the pairing explicit. The marker's status is the group's combined
status — if any member needs attention or errors, the group does too, even when
the title node itself looks healthy.

An agent that is not paired is drawn exactly as a single row, with no rail or
indent. Every agent in a group stays individually clickable, renameable, and
deletable; grouping is a view, not a merged node.

## Find open work in the sidebar

Meshes that currently have at least one open agent node sit at the top of
the sidebar under an **Active** label, in your own drag order; every other
mesh follows below, dimmed, in the same manual order. Nothing is hidden — all
meshes stay visible and every row keeps its spawn control, so a mesh with
nothing open is still a working row you can spawn from.

Opening the first node in a mesh moves it into the top band; closing or
archiving its last node returns it to its manual position. Dragging a mesh
only changes your manual arrangement — it never changes which band the mesh
is in.

## Paste into a terminal

Use the terminal's **Paste** context-menu action or your usual paste shortcut.
For native Windows Grok sessions, Buildmesh asks Grok to read the desktop
clipboard directly, keeping long, multiline text in one pasted block. This
also applies to Ctrl+Shift+V and Shift+Insert. Pasting stages the text; press
Enter when you are ready to send it.

WSL sessions, other harnesses, and the mobile terminal continue to receive
clipboard text through the terminal connection.

## Hand work over to another node

Select text in an agent's terminal and right-click it to hand that selection on
from the context menu:

- **Handover to new Node** spawns another agent on the Mesh, pre-filled with the
  selection as its first turn.
- **Handover to node** sends the selection to an agent you already have. The text
  is staged as a single paste and then submitted, exactly as if you had pasted it
  and pressed Enter — so a multi-line review report arrives as one prompt instead
  of one prompt per line.

The agents this one already shares tabs with are offered first — the pair you
grouped, or a review agent spawned from this node — which keeps an
implementer/reviewer pair one click apart, and every row shows the target's
status. A handover needs a selection in the source terminal, and the target must
have a live agent process; without one the action reports the failure instead of
dispatching. If the harness does not take the submit, the text is left staged in
that node's input box, so you can press Enter (or edit it) yourself rather than
losing the handover.

## Harnesses and capabilities

The Spawn Menu lists detected harnesses. Each harness's configurations submenu
contains its saved native and compatible proxied provider recipes.
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
| MiniMax Code | Yes | Hook | Attention reports completed turns; the TUI rejects model/effort flags and is launched in Full Access |
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
  can be configured separately in Project Settings.
- Closing a node removes it from the UI first and cleans its worktree in the
  background. A cleanup warning means the directory may still exist; do not
  recreate or manually delete it until you have checked the warning.
- The Mesh may sync from its configured upstream before a new node is created.
  A sync warning does not silently discard the local worktree; inspect the
  Mesh health and Git status before retrying.
- Use the Repository view to understand which branch and path belong to which
  node. Do not move a live worktree behind Buildmesh's back.

## Project Settings and Repository

Two separate views cover everything you can do to a Mesh, and they never mix:
**Project Settings** is where you configure the project, **Repository** is
where you repair or clean it up. Open either from the command palette, or from
the **More** disclosure at the right of the title bar.

**Project Settings** is grouped so you can tell at a glance what kind of
change you are making:

| Section | Use it for |
|---|---|
| General | The project's display name and its directory on disk |
| Agent runtime | The default provider and whether agents run sandboxed |
| Build and run | Build and Run commands, plus optional Root overrides and project presets |
| Worktree strategy | Whether new nodes get a worktree, the starting point and mode, the pre-spawn warm pool, and the worktree directory |
| Danger zone | Deleting the project |

Everything here applies to the project root. A focused node's worktree is a
different path, and these settings never change it retroactively.

**Repository** covers maintenance, and separates repairing from deleting:

- **Health and recovery** reports drift, a base branch held by a node, and
  unpushed commits, and offers the one-click Restore and Free actions. A
  healthy project says so explicitly rather than showing nothing.
- **Branches and worktrees** lists local branches and worktrees per
  repository, selects the merged/orphaned/clean ones for you, and prunes
  remote-tracking references. Deleting asks for confirmation first, naming how
  many branches and worktrees it will remove.

Both views act on the project root, never on a focused node's worktree.

## Launch Configurations

Use **Settings → Launch Configurations** to name the recipes you launch. Choose
a harness, native authentication or a provider, then a model, supported effort,
and optional extra arguments. Known providers supply catalogue choices; generic
providers retain manual model entry. The same editor opens from **New configuration**
in a harness's spawn submenu. Add credentials in **Providers** first: enabled,
keyed providers can be selected even before a pairing exists. New pairings collect
their endpoint in the editor and are saved together with the configuration;
cancelling creates neither. **Advanced Provider Routes** edits existing shared
endpoints and model-tier overrides; **Providers** owns credentials and billing.
While a configuration is saving, the editor shows progress and keeps its fields
disabled. In the spawn menu, the status changes from **Saving configuration…**
to **Refreshing availability…** while provider availability is fetched. A failed
save shows an error and leaves the draft available to retry.

Model choices belong to the selected provider and API surface; **Custom model**
allows a model not yet in the catalogue. Effort choices are restricted to documented
provider/model and harness support. MiniMax M3 through Codex offers `none` (thinking
off) and `high` (thinking on), not graded reasoning depth. MiniMax through Claude
Code has no documented graded effort selector; use the harness's thinking toggle.
See MiniMax's [Codex guide](https://platform.minimax.io/docs/token-plan/codex) and
[Claude Code guide](https://platform.minimax.io/docs/token-plan/claude-code).
Model identifiers and argument formats vary by harness, provider, and account;
see [Supported model strings](research/supported-model-strings.md) for current
examples and each harness's live model-list command or picker.

Adding or enabling a known provider creates compatible routes; configurations
are only the recipes you save, so each harness submenu starts with none.
**Clone** creates an independent recipe. Deleted configurations are not recreated
automatically. In the desktop spawn menu, choose a harness directly for its
defaults or open its configurations submenu for a named recipe, including
provider routes such as MiniMax via Claude Code. Mobile groups the same recipes
under each harness.
Unavailable entries explain whether a harness, key, route, or verification needs
attention. Codex proxy routes still require verification for their exact model
and runtime before launch. Use **Verify provider and model** in the configuration
editor; it sends a small tool-call request using the stored credential without
saving the draft. Changing the model or endpoint requires verification again.

Explicit launch overrides win over the selected recipe, followed by Mesh and
application defaults for native authentication. Proxied configurations instead
use the provider route's model default; native harness model and effort defaults
do not cross into them. New nodes retain a secret-free snapshot: changing or
deleting a recipe does not change existing or archived nodes. Resume reads the
current credential for the saved account; restore a missing key in **Providers**.
Changing credentials or the Codex installation can require route verification
again. Regenerate with a different recipe deliberately replaces the snapshot.

Mobile exposes the same choices and **Manage Launch Configurations** in the
new-node picker. Background auto-naming and Circuit classification accept
host-native configurations whose harness supports one-shot background inference:
Claude Code, Codex, OpenCode, Kimi Code, Grok Code, Antigravity, Command Code, and
MiniMax Code. Claude Code also accepts configured provider routes; the other
background runners use the harness's native authentication. Saved model and
effort settings apply where the harness supports those controls. Configurations
with extra CLI arguments cannot run background inference. Unsupported choices
show the missing requirement in the settings picker and are rejected by the
backend. Usage remains attached to the account, not each recipe.

## Settings that matter

Open **Settings** for app-wide defaults. A Mesh's **Project Settings** override
the app-wide value when both exist.

| Settings area | Use it for |
|---|---|
| General | Appearance, quit confirmation, global Circuit agent capacity, the default worktree directory, and the Probe spawn prompts |
| Providers | Credentials, provider routing, accounts, and custom compatible endpoints |
| Launch Configurations | Named recipes, harness ordering, fallback defaults, and advanced provider routes |
| Remote Access | LAN/VPN exposure, the Coordinator Read API, certificate management, and paired-device revocation |

**Probe spawn prompts** (Settings → General) customise the initial prompt
for agents spawned from the Probe's GitHub Issues and Pull Requests tabs.
Both templates offer `{{number}}`, `{{url}}`, `{{owner}}`, and `{{repo}}`.
The Issues template additionally offers `{{title}}` and `{{title_suffix}}`
(the latter renders " — title", or nothing when the title is blank),
while the PR template additionally offers `{{policy}}` (the shared review
policy). Leave a template empty, or use **Reset to default**, to restore
the built-in wording.

The **Sandbox agent processes** option is per Mesh and is off by default. It is
an OS process boundary, not a VM or a promise that the agent cannot send data
over the network. Windows currently has weaker file-read/write confinement than
macOS; read [Agent sandboxing](../README.md#agent-sandboxing-security) before
enabling it for untrusted prompts.

## Build and Run

Configure **Build command** and **Run command** in Project Settings, or choose a
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

### Using Buildmesh on your phone

The phone opens on **Overview**, with counts for agents needing attention,
running agents, and all active agents. Errors and degraded status reporting
appear first, followed by agents waiting for input. Each agent card shows its
mesh name. Status reporting warnings mean the agent's activity may be out of
date; inspect its terminal to check.

Use the bottom navigation to switch between **Overview**, **Work**, and
**Capture**. Work searches agent names, meshes, providers, and branches, with
filters for attention, running, and idle agents. Each mesh has direct links
to GitHub Issues and its Archive, plus a button to start an empty agent.

**Capture** saves an idea draft on this phone as you type. Choose a mesh and
an available agent (your choices are remembered on this phone), then select **Start working on this**. The idea becomes
the new agent's initial prompt. Saved launch configurations are available;
only agents supporting initial prompts are offered. A failed launch retains
the draft. If the connection drops during launch, check Work before retrying:
the desktop may have accepted the request. Drafts are local to the browser,
are not a desktop backlog, and disappear when site data is cleared.

Tapping an agent opens **Work details**, with current status, base reference,
**Review changes**, **Open terminal**, and a reply field. Replies send text
and Enter to an idle or waiting agent; delivery confirmation means the text
reached the terminal, not that the agent completed the request. Inspect the
agent's actual request before answering it. Longer replies belong in the
terminal. Changes still supports file diffs and pull request creation. The
request and unsent reply remain available while you inspect changes or the
terminal. Back returns through the screen you came from.

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
