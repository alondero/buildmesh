# Buildmesh Context

Buildmesh is an orchestration platform for AI coding agents that work in parallel across meshes (repository roots) using Git worktrees.

## Language

**Mesh**:
A project workspace associated with a local Git repository root path.
_Avoid_: Project, repo, folder

**Agent Node**:
An interactive panel running a single agent execution process within a dedicated directory (either a worktree or the mesh root).
_Avoid_: Session, pane, terminal node

**Worktree Node**:
An Agent Node operating on an isolated Git worktree branch of its parent Mesh. (Used when the Mesh property use_worktree is true, unless overridden).

**Root Node**:
An Agent Node operating directly on the parent Mesh's root directory, bypassing worktree isolation. (Used when the Mesh property use_worktree is false, or when overridden via Alt-click).

**Node Working Directory**:
The directory an Agent Node's work physically lives in: its Worktree Node dir (`.claude/worktrees/<name>`) for a Worktree Node, or the Mesh root for a Root Node. The canonical "where is this node's stuff" rule (resolve `use_worktree` + a trimmed, non-empty `worktree_name`) lives in one place; callers pick the host form (Windows git2) or the spawn form (the path as the agent saw it — Linux for a WSL node, which is the form Claude Code encodes for its on-disk transcript directory).
_Avoid_: working path, repo path, node dir

**Node Turn**:
The point at which an Agent Node yields control back to the user — its agent has stopped and is waiting. Claude Code surfaces this as several hooks (the Stop hook = awaiting input, plus the catch-all Notification hook = idle prompt or permission prompt); Buildmesh treats them as one undifferentiated signal, because all are yields. A Node Turn is the single inbound fact that fans out to two independent reactions: marking the node for attention (status → `awaiting_input`, emit `attention-needed`) and considering an AI rename (session naming). The trigger is a clock tick, not a content source — naming's summary comes from the buffered PTY output, so the *kind* of yield never changes what gets named.
_Avoid_: turn signal, stop event, attention event, notification

**File Explorer Panel**:
A collapsible side panel displaying files and changes for a given Mesh or Agent Node.
_Avoid_: File tree panel, sidebar drawer

**Base Ref**:
The Git reference a new Agent Node's worktree is created from (default `origin/main`). Configured per Mesh; surfaced in the UI as "Fresh" (the Base Ref) vs "Head" (the Mesh's current checkout).
_Avoid_: Base branch, starting point, source branch

**Changed Files Section**:
A distinct view in the File Explorer Panel listing modified files with their addition/deletion line counts.
_Avoid_: Modified files list

**Drifted root**:
A Mesh whose root HEAD is not on the Base Ref's branch (e.g. the user parked the root on `feat/x` and forgot) — or is detached on a non-base commit. Surfaces as an amber `!` badge in the sidebar; one-click fix is "Restore root to base" in the mesh properties panel.
_Avoid_: Wrong branch, off branch, out of sync

**Base branch hostage**:
A condition where the Base Ref's branch (e.g. `main`) is checked out in one of the Mesh's worktrees, blocking `git checkout main` from the root. The health block names the holding worktree; the one-click fix is "Free base branch (worktree-name)".
_Avoid_: Branch locked, branch busy

**Unpushed commits on root**:
A Mesh whose root branch has local commits that aren't on its upstream — or has no upstream at all. The "Restore root to base" button refuses until the user pushes, branches, or resets the work, because a checkout would strand those commits in reflog.
_Avoid_: Local commits, un-pushed work

**Coordinator**:
An external, agent-agnostic supervisor that reads node state and drives nodes through Buildmesh's control API, rather than via the UI. The first coordinator is the user's remotely-hosted Hermes Agent (Nous Research); a future in-app "Buildmesh superagent" is intended to be a second coordinator on the same API. Buildmesh stays a "dumb" driver — the orchestration intelligence lives in the Coordinator.
_Avoid_: Supervisor, orchestrator agent, Hermes (Hermes is one instance of a Coordinator, not the category)

**Autopilot**:
An automated background execution mode for a Mesh that polls a remote issue tracker and automatically spawns Agent Nodes when matching issues/PRs are detected.
_Avoid_: Auto-worker, event listener

**Autopilot Policy**:
The set of configuration settings (trigger labels, concurrency limits, provider overrides, and success actions) that govern a Mesh's Autopilot behavior.

**Node Digest**:
A coordinator-facing read summary of a single Agent Node answering "what's going on, and does it need feedback?". Layered: an always-available spine from Buildmesh's own DB (lifecycle `status`, "needs feedback" = `awaiting_input`) enriched, for the Claude Code provider family only, with semantic content read from the agent's on-disk JSONL transcript. Non-supporting providers, or a transcript that fails to parse, degrade to the spine with the enrichment explicitly flagged unavailable (never silently omitted). The rendered terminal/TUI is deliberately **not** a digest source.
_Avoid_: Node summary, status payload, snapshot

**Blocked by**:
The list of GitHub issue numbers an open issue declares it depends on, parsed from the issue body's `**Blocked by**` markdown section (settext or ATX heading; `None` short-circuits to an empty list; `/pull/N` references are ignored — only `/issues/N` counts). Surfaces in the Issues Probe as a flag below the Spawn button when at least one referenced blocker is still in the repo's loaded open-issues set. The flag is a warn, not a gate — the Spawn button stays enabled so a user who's intentionally unblocking something can still proceed.
_Avoid_: depends on, dependency list, blocking issue (singular)

**Sandbox** (Agent Process Sandbox):
A per-Mesh opt-in confinement for Agent Node PTY processes, exposed as the "Sandbox agent processes" toggle in the Mesh properties. Off by default; when on, every Agent Node spawned in the Mesh runs inside an OS-level deny-by-default container keyed to that node — macOS Seatbelt (`sandbox-exec`, #497) and Windows AppContainer (#498) each implement their own backend, sharing the single `meshes.sandbox` column. The OS-specific spawn policy is decided at one seam (`sandbox::sandbox_enabled`) so the per-OS implementation is swappable; the Mesh/UI layer is OS-agnostic. Sandboxed nodes can read/write their own worktree, reach the network (`internetClient` / `sandbox.network`), and run a curated PATH; everything else — the rest of `%USERPROFILE%`, host `%TEMP%`, the registry, system tools not on the curated PATH — is denied by default. See `docs/adr/0012-windows-appcontainer-agent-sandbox.md` for the Windows half.
_Avoid_: container (when meaning OS-level confinement), jail, restricted shell

## Relationships

- A **Mesh** can have one or more **Agent Nodes**
- A **Mesh** can have **Autopilot** enabled, governed by its **Autopilot Policy**
- **Autopilot** automatically spawns **Agent Nodes** for matching issues or PRs, enforcing branched worktree mode
- An **Agent Node** operates on a child worktree or branch of its parent **Mesh**
- An **Agent Node** emits a **Node Turn** each time its agent yields control back to the user; attention-marking and session naming react to it independently
- A **File Explorer Panel** shows context for either a **Mesh** or an **Agent Node**
- A **Mesh** can have a **drifted root** if its root HEAD is not on the Base Ref's branch
- A **Mesh** can be in a **base branch hostage** state when one of its worktrees holds the Base Ref's branch
- A **Mesh** can have **unpushed commits on root** that block the recovery actions
- A **Mesh** can opt into a **Sandbox**; when on, every Agent Node spawned in the Mesh is confined to its worktree via the OS-level backend (macOS Seatbelt, Windows AppContainer)

## Example dialogue

> **Dev:** "When the user spawns a new **Agent Node** under a **Mesh**, does it create a new Git branch?"
> **Domain expert:** "Yes, it creates a dedicated worktree branch tracking the selected starting point of that **Mesh**."

> **Dev:** "When the Issues Probe shows a red flag under an issue's Spawn button, what does that mean?"
> **Domain expert:** "The issue's **Blocked by** list contains at least one issue that's still open in this repo — the flag is a warn, not a gate, so Spawn still works if the user is intentionally unblocking it."

> **Dev:** "What happens if I flip on 'Sandbox agent processes' on a Mesh that's already running agents?"
> **Domain expert:** "The flag is read at spawn time, so already-running Agent Nodes are unaffected. New Spawns from this Mesh on a sandboxing-capable host (macOS Seatbelt, Windows AppContainer) will run inside an OS-level deny-by-default container; on hosts with no sandbox backend yet, the flag is a no-op."

## Flagged ambiguities

- "session" and "node" were used interchangeably. Resolved: we canonicalize on **Agent Node** for the user interface and domain model, while database/backend can use "session" for process lifecycle records.
- "state pollution" in worktrees. Resolved: Git worktrees are fully isolated, so parent Mesh cleanliness is not required when spawning new Agent Nodes.
