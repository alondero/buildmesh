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

On the **Usage** tab, the **↗** link beside the provider name opens the provider's
usage page in your system browser — hover it to see which page it opens. The link
also works beside a last known reading or a fetch error, so you can check the
current figures directly. Where no direct dashboard is available, it opens the
provider's account console, quota documentation, or subscription settings instead.
Custom providers and externally managed billing omit the link. The browser uses
its own login; choose the same account or workspace as the meter.

## Your first session

The empty workspace and each selected empty repository offer three readiness
steps: add a repository, check your harness and runtime, then open a session.
**Check runtime and login** opens **Settings → Providers**. **Skip setup guide**
hides the guide across repositories; **Show setup guide** restores it.
Detection confirms an installation, so sign
in using the CLI in that runtime before launching an agent. **Start Terminal**
lets you start locally without agent credentials or completed runtime checks
once a repository is selected, using that repository's worktree setting.
It stays available when the guide is hidden or other repositories have agents.
Help contains the advanced keyboard shortcuts.

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

Changed files open the in-app diff from either list. Unchanged files in the
File Tree open in your configured editor. If a diff cannot load, an error
appears and the file is not presented as successfully reviewed.

Closing an active node that runs in the repository root asks for confirmation.
**Cancel** keeps the session and any scheduled prompt; **Close session** stops
its process and removes the node while preserving repository files. Worktree
nodes retain their separate confirmation for uncommitted or unpushed work.

The first successful loop is: **install → configure → spawn → inspect → verify
→ integrate**. If a step does not behave as described, start with
[Troubleshooting](troubleshooting.md).

## Open tools from the title bar

Use **More** at the right of the title bar to open **Project Files**,
**Agent Changes**, **Project Settings**, **Repository**, **GitHub Issues**,
**Pull Requests**, **Circuits**, **Agent History**, or **Notes** in the inspector.
The menu uses the same names, descriptions, and icons as the command palette.
**Usage** has its own title-bar button, alongside **Settings** and **Mobile**.

Settings opens with focus on **General**. Use Up/Down to select a section
(wrapping at either end), or Home/End to select the first/last section. Tab
enters that section; switching sections preserves unsaved drafts. If you try
to close with unsaved changes, **Keep editing** returns to your previous
control and **Discard changes** closes Settings.

Sidebar colour and provider-picker controls have enlarged targets around
their compact icons. Suspended agents show **Resume** and failed agents show
**Restart** when available, without needing to hover the row. The close icon
remains visible and identifies the node in its accessible label.

Select a tool to open it and close the menu. Use Tab and Enter to choose a tool,
or Escape to close the menu and return focus to **More**. Clicking outside also
closes the menu. On short windows, scroll the menu to reach every tool.

The inspector groups **Project Files** and **Agent Changes** under **Files &
changes**, and **GitHub Issues** and **Pull Requests** under **GitHub**. Switch
subviews inside the panel. The header shows the subject and whether it follows
your current selection. Files shows the working tree, while Agent Changes
compares against HEAD or the selected repository's base merge point. Commands
still open the exact requested view. The last Files subview is restored when
you restart.

**Agent History** finds lifecycle records across repositories, including
archived nodes. Search by name, repository, branch or session, and filter by
repository or lifecycle state. **Reopen** restores an archived node to the
workspace without starting its process; **Resume** starts it separately using
its saved session when available. **Discovered sessions** scans CLI history
for the explicitly selected repository.

## See which scope you are looking at

The control left of the view modes always names what the canvas is showing.
When a single Mesh is in scope it reads that Mesh's name; otherwise it reads
the view and the node count, such as **Pinned across meshes · 3 nodes** or
**Filtered across meshes · 2 of 5 nodes**. In **Single** it names the Agent
Node you are soloing and its Mesh. On narrow windows the name collapses to an
icon; hovering shows the same wording in full.

Choose **All Nodes** to leave Mesh scope. It is the only route out, and it
clears the Mesh selection.

Open the same control to choose a Mesh. The list shows every Mesh with a tick
beside the current one. Choosing a Mesh moves the canvas to that Mesh's nodes
and retargets the Mesh-scoped tools in the inspector — Files, Properties,
Worktree Manager, GitHub Issues and Archived Nodes all follow. Choosing the
Mesh you are already in returns the canvas to its grid. Escape or a click
outside closes the list and returns focus to the control, leaving the scope
unchanged.

Entering **Mesh Grid** with no Mesh selected asks which Mesh you meant rather
than guessing one. The canvas stays on its "no Mesh selected" state until you
choose, and the picker opens with focus inside it so you can tab straight to
the list. `Ctrl+Alt+G` (`Cmd+Alt+G` on macOS) rotates through the view modes
and asks the same way.

Two scope changes announce themselves. A search that leaves a single Mesh
tells you the results span every Mesh, because the search matches Agent Node
names rather than one Mesh. Deleting the Mesh you are looking at tells you the
canvas moved to **All Nodes**. Both are reminders only — the view still
changes.

## Find agents that need attention

Choose **Attention** in the sidebar to see failed, waiting and suspended nodes
across repositories. Each row names its repository and offers **Open terminal**
and, where applicable, **Retry** or **Resume**. Return to **Workspace** to use
the existing repository and drag order.

In **Filtered** view, use the header's **Filters** button for provider, status,
sort and direction. The count shows matching nodes out of all loaded nodes.
Remove an active chip or use **Clear all** to clear search and filters and
restore the default sort. Your choices survive a restart, including a filter
with no matches.

## Configure Autopilot Circuits

In **GitHub Issues**, use the tag button beside an issue to change its GitHub
labels. Search the repository's labels, then check or uncheck one to add or
remove it. Changes save individually; permission and network errors
leave the displayed tags unchanged so you can retry. Create or rename repository
label definitions on GitHub.

Labels watched by an **enabled Autopilot Circuit on this mesh** appear first,
highlighted in violet with a lightning indicator. This includes `ready-for-agent`
when it is the configured issue trigger, and any custom trigger labels. Hover
for the Circuit name. Disabled Circuits and PR-label triggers do not highlight
issue tags. The indicator means eligible for pickup on a poll; dependencies,
existing runs, and capacity still determine when work starts. Removing a trigger
label does not cancel a run that has already started. Circuit status refreshes
while the Issues inspector is open and with **Refresh issues**.

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

A review started from the title bar's **Start review or circuit** control and an
issue-driven review flow run the same loop:

1. The implementation agent commits, pushes and opens a pull request before any
   review starts. An issue-driven flow does this in its wrap-up.
2. One reviewer agent reviews the work. When it requests changes, its findings go
   to the implementation agent, which commits and pushes fixes. The same reviewer,
   still open, is then asked to review again, up to the round limit.
3. On approval the reviewer is closed and the implementation agent is asked to
   squash and merge the pull request. It marks a draft ready, brings an
   out-of-date branch up to date with the base branch first, and waits for
   required checks before merging. It stops and reports instead of merging only
   if a check genuinely fails on the code or the merge stays blocked for any
   other reason. The circuit then finishes and hands the agent
   back to you. The agent stays open, so read its report to confirm the merge.

Legacy Autopilot controls are removed. Existing nodes, worktrees and history are
retained, and the previous global agent cap and custom wrap-up template are carried
forward. Old mesh-level Autopilot settings have no effect on Circuit launches.

## Read a node's status mark

Each node in the sidebar and on its card title bar has one small mark. The
**circle** is what the agent is doing: solid for running, a hollow ring for
idle, a dashed ring while starting, a ring with a dot (pulsing) for a node
that needs attention, a red cross for an error, a ring with a slash for lost,
half-filled for suspended, a ring with a dot for a node whose pull request is
open, and the same ring with a dot for a node that is just ready.

Idle and running share cyan but differ in shape (hollow ring vs solid
circle), so they are told apart by shape alone. `pending` and `spawning`
both draw a dashed muted ring (the same "Starting…" state from two code
paths); `completed` and `ready` both draw a green ringed dot (the
pre-existing ✓/green grouping). Hover the mark for its label.

When a Circuit is driving the node, a **ring** circles the mark. A violet comet
that moves round it means the Circuit is working. A held amber half ring means it
is waiting, a red dashed ring means it needs attention, and a closed green ring
means it finished. Hover the mark for a sentence describing both. If your
operating system is set to reduce motion, the comet stays still at the top of the
ring instead of circling. On a card title bar, click the mark to open that run in
the Circuits tab.

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

Each mesh is a card. The card's left edge is a full-height colour bar that does
two jobs: click it to change the mesh colour, drag it to reorder the mesh. From
the keyboard, <kbd>Enter</kbd> opens the colour picker and <kbd>Space</kbd> picks
the card up for reordering, so both jobs stay reachable without a mouse.

Under the mesh name, a row of status dots summarises the mesh's agents — one dot
per agent, coloured by that agent's status, with the count beside it. There are
no text status labels. Click that line to expand or collapse the mesh's agents;
a mesh holding an agent that needs your attention or has errored starts
expanded, so a mesh asking for you is never buried behind a click.

The rest of the header carries the badges: `!` when the mesh has drifted or its
base branch is held hostage (click it to open the Worktree Manager), `↓N` when
the branch is that many commits behind upstream, and a sync icon only when a
sync from upstream has failed — click it to retry. The `+` button spawns an
agent. Right-click the card for Properties, File Explorer, Force sync from
upstream, Archive, GitHub Issues, and — for a mesh with a GitHub origin — View
on GitHub.

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

Project Settings becomes editable after the selected project's settings load.
If loading fails, use **Retry**; values from the previous project cannot be
saved into the newly selected one.

**Repository** covers maintenance, and separates repairing from deleting:

- **Health and recovery** reports drift, a base branch held by a node, and
  unpushed commits, and offers the one-click Restore and Free actions. A
  healthy project says so explicitly rather than showing nothing.
- **Branches and worktrees** lists local branches and worktrees per
  repository, selects the merged/orphaned/clean ones for you, and prunes
  remote-tracking references. Deleting asks for confirmation first, naming how
  many branches and worktrees it will remove.

Both views act on the project root, never on a focused node's worktree.

Switching projects clears Repository cleanup selections and any open deletion
confirmation. Wait for the current project's list to load before selecting
branches or worktrees to delete.

**Notes** is a separate project tool for scratch text. Its editor becomes
available after the project's notes load; a failed read offers **Retry**.
Typing saves after a short pause, and switching projects flushes pending
typing to its original project. Saves for the same project run in order;
returning to it waits for the latest save to finish. A save failure keeps the
current text visible so you can retry editing before leaving the tool.

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

If the selected configuration cannot be resolved, auto-naming retries on a later
turn after you repair it. On Windows, Claude Code background work also checks
the standard native and npm install locations when the app's PATH is stale.

## Settings that matter

Open **Settings** for app-wide defaults. A Mesh's **Project Settings** override
the app-wide value when both exist.

Tab and Shift+Tab stay within the visible pane. If you try to close with
unsaved edits, focus moves to **Keep editing**; only **Discard changes**
abandons the edits.

The selected Settings tab is the tablist's single Tab stop. Up/Down wrap
through tabs, and Home/End choose the first/last tab. Tab then enters the
selected panel. Switching tabs preserves unsaved edits.

Default, classifier, reviewer and naming choices load independently of live
CLI and WSL probes. Routes awaiting runtime verification explain that state
and remain unavailable until verified. Failed checks keep a **Retry** action;
defaults stay disabled if preferences failed to load, so a failed read cannot
overwrite saved choices.

## When Buildmesh cannot read your settings

If `preferences.json` is damaged, empty, truncated, or holds a field this
version does not understand, Settings opens with a warning panel instead of
silently showing defaults. Nothing is lost at that point: Buildmesh leaves
the file exactly as it found it and refuses to write over it, so your
provider accounts, API keys, provider pairings, harness defaults, and
Autopilot settings are still on disk. Settings controls stay disabled until
you choose one of three actions:

- **Restore last-known-good** puts back the settings Buildmesh last saved
  successfully. A copy of the unreadable file is kept either way. Offered
  only when such a backup exists; it is a plain file named
  `preferences.json.bak` next to `preferences.json`.
- **Open file location** opens the folder in Explorer or Finder, so you can
  copy the file somewhere safe or repair it in a text editor. Restart
  Buildmesh afterwards — Settings re-checks the file each time it opens, so
  a repaired file is picked up without touching anything else.
- **Reset to defaults…** replaces the file with defaults. It asks for
  confirmation first and names what is lost, and it also keeps a copy of the
  unreadable file (named `preferences.json.corrupt-<timestamp>`) in the same
  folder.

The most common cause is an interrupted write — a crash or a full disk during
a save. A file written by a *newer* Buildmesh can also fail to load if that
version changed the type of a field; **Restore last-known-good** is the right
answer there too.

| Settings area | Use it for |
|---|---|
| General | Appearance, quit confirmation, global Circuit agent capacity, the default worktree directory, and the Probe spawn prompts |
| Providers | Credentials, provider routing, accounts, and custom compatible endpoints |
| Launch Configurations | Named recipes, harness ordering, fallback defaults, and advanced provider routes |
| Remote Access | LAN/VPN exposure, the Coordinator Read API, certificate management, and paired-device revocation |

**Probe spawn prompts** (Settings → General) customise the initial prompt
for agents spawned from the Probe's GitHub Issues and Pull Requests tabs.
The built-in templates appear as editable text, so you can adjust their wording
directly. Changes save when you leave the editor. Click a placeholder button to
insert it at the cursor or replace selected text; each button explains its value
with an example, including for screen readers. Before an editor has been focused,
clicking a placeholder appends it to the prompt; after focusing, insertion uses
the last cursor position or selection. Expand **Example prompt** for a live
preview using fictional GitHub item #42 in `octocat/hello-world`.

Both templates offer `{{number}}`, `{{url}}`, `{{owner}}`, and `{{repo}}`.
The Issues template additionally offers `{{title}}` and `{{title_suffix}}`
(the latter renders " — title", or nothing when the title is blank).
The PR template additionally offers `{{policy}}`, which inserts Buildmesh's
shared review instructions. The example prompt shows the full policy text that
an agent receives. Clear a template and leave the editor, or use
**Reset to default** (also available while editing unsaved defaults), to restore the built-in
wording in the editor. Writes to each prompt are saved in order, including resets;
edits made during a pending save remain available for the next save.

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
   self-signed TLS**, or choose **Enable remote access** from the Remote Access
   dialog. The setting applies immediately.
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
The Remote Access dialog distinguishes disabled access from a connection
failure and offers a retry with the actual TLS/interface status. Its header
stays visible while expanded certificate instructions scroll below it.
Do not share QR codes, pairing invitations, cookies, certificates, or logs.

### Using Buildmesh on your phone

The native Android app supports Android 8.0 and later. Install the APK built
from this repository, open **Buildmesh**, choose **Scan pairing QR**, and scan
the desktop's Remote Access Connect QR. Then select **Pair with desktop**.
The app verifies and stores the desktop certificate privately; it does not
require the browser's system certificate installation. On older desktops,
paste the full pairing URL and the **Root CA SHA-256** fingerprint shown in
the desktop's Certificate section. See [Android build and installation](development/android.md).

Android provides native Overview/Work, agent replies, task capture, issues,
archive resume, changes and diffs. Its terminal includes keyboard input,
Esc/Tab/Ctrl+C and arrow keys, and reconnects when the app returns to the
foreground. **Create PR** on a mesh uses that mesh's current branch; ask an
agent to open a worktree PR from its terminal. **Connection → Forget this
desktop** removes this phone's saved session. Revoke the device on the desktop
to remove its authorization. Certificate resets require fresh pairing.

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

## Data, backup, and restore

Buildmesh stores Meshes, Agent Nodes, Circuits, and preferences locally. See the
README's
[data-location table](../README.md#data-location-logs-backup-uninstall) for
profile paths and logs. There is no automatic cloud backup.

**Settings → Data & Diagnostics** is the supported way to back up, check, and
restore that state. It has three parts.

### Check your stored state

**Run quick check** is a fast structural check. **Run full check** is the
thorough version, for when the quick one is not enough. Both only report — a
failed check never repairs or deletes anything. If a check fails, your data is
still on disk and a restore is still available.

### Back up

Buildmesh keeps the **newest 3 snapshots** in the `snapshots\` folder inside
your data directory, and takes one **automatically before it upgrades your
stored data**. So a schema upgrade is always reversible, without you doing
anything.

**Create snapshot** takes one on demand. Snapshots contain everything,
including your credentials, and are readable only by your Windows account.

**Export a copy…** writes a single `.bmsnap` file somewhere you choose — for
moving to another machine, or for attaching to a bug report. Leave
*Leave credentials out of the export* ticked (the default) and the file
contains no:

- provider API keys or account credentials
- the remote-access root token, coordinator tokens, or paired-device sessions
- the LAN HTTPS certificate or its private key
- terminal scrollback and agent transcripts (never part of stored state)
- Windows Credential Manager entries

Untick it only when you are moving to a machine you control and want your keys
to come with you. There is deliberately **no** option to export the LAN
certificate's private key — a file containing it would let its holder
impersonate the HTTPS identity your paired devices already trust.

### Restore

1. **Choose a file…** and read what Buildmesh found in it. Nothing changes yet.
2. **Restore this state** stages it. Buildmesh first saves your current state
   as a snapshot, so the restore is undoable.
3. **Restart Buildmesh.** The restore is applied during startup, before any
   database connection or background worker exists.

If a file is corrupt, truncated, edited, or from a newer Buildmesh than the one
you are running, it is rejected outright and your current data is left exactly
as it was. If a restore is staged and you change your mind, **Cancel the
staged restore** in the same pane.

**Restoring a redacted export** brings back your Meshes, Agent Nodes, Circuits,
and preferences — but no credentials. After the restart you re-enter your
provider API keys, Buildmesh mints a **new** remote-access token, and paired
devices need to sign in again.

**Where this data lives** shows your data directory, and **Open data folder**
opens it in Explorer or Finder.

### Uninstall

To remove the app *and* all its data, uninstall from
*Windows Settings → Apps → Installed apps → Buildmesh → Uninstall*, **then**
delete the profile directory. The uninstaller removes the binary and registry
entries but not the data dir, so the two steps matter if you want a clean
slate. Export first if you want to keep anything.
