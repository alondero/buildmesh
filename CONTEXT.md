# Buildmesh Context

Buildmesh is an orchestration platform for AI coding agents that work in parallel across meshes (repository roots) using Git worktrees.

## Language

**Agent Harness**:
The executor binary recipe (e.g. `claude` / `claude.exe`, `codex`, `agy`, `opencode`, `terminal`) that launches and communicates with an AI coding agent. Only the `Terminal` harness is always available; all others must be installed on the host and enabled by the user. A harness is distinct from the **Model Provider** whose models it runs (ADR-0014).
_Avoid_: Executor, runtime, provider (a harness is *not* a provider — see **Model Provider**).

**Model Provider**:
The credentials and endpoint a model request is served by (e.g. Anthropic, OpenAI, MiniMax, Kimi, DeepSeek, or a custom base-URL + key). Independent of the **Agent Harness** that runs it; one provider may expose more than one **Compatible API surface**.
_Avoid_: Backend, service, vendor; account (reserve "account" for the stored `ProviderAccount` config row).

**Compatible API surface**:
The wire protocol a harness expects of its backend — Anthropic-compatible or OpenAI-compatible. A **Proxied Provider** is reachable by a harness only over a surface that harness speaks.
_Avoid_: Compatible provider (compatibility is a property of the connection, not the provider).

**Proxied Provider**:
A **Model Provider** that Buildmesh wires into a harness by injecting an API-compatibility shim at spawn — base URL + auth token + a model-tier remap — so a harness built for backend A serves models from provider B over a **Compatible API surface**. Buildmesh owns the glue and the credentials. This is the harness↔provider pairing Buildmesh's spawn menu enumerates. _Example_: *MiniMax via Claude Code* points `claude.exe` at MiniMax's Anthropic-compatible endpoint via `ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN`. (Generalises the former *Claude Code (Compatible API)* term.)
_Avoid_: Gateway, bridge, redirect.

**Native Provider**:
A **Model Provider** selected and authenticated *inside* a harness's own login (e.g. MiniMax configured within OpenCode). Buildmesh launches the harness and does **not** manage the credentials or surface the provider choice — provider selection stays in the harness, never in Buildmesh's spawn menu.
_Avoid_: direct provider; "built-in" (ambiguous — see Flagged ambiguities).
Buildmesh manages no native creds for *spawning*, but a **First-class Model Provider**'s fetcher may still read a harness-native subscription's **Usage Meter** transparently (e.g. the Claude Code subscription quota) when the harness is installed.

**First-class Model Provider**:
A **Model Provider** Buildmesh ships built-in knowledge of — brand identity (icon, accent colour), billing model, and a **Usage Meter** fetcher (Anthropic, MiniMax, Kimi). Renders its own brand mark and live usage on the Providers page. (issue #566)
_Avoid_: "built-in" (ambiguous — see Flagged ambiguities), supported provider.

**Generic Model Provider**:
A user-defined **Model Provider** — a name, a **Compatible API surface** + base URL, an API key, and a model-tier map, with no registry entry. Spawns fine, but has no brand icon (neutral fallback) and **no usage integration** (Buildmesh can't know its billing API), so its Providers-page card shows a "usage not tracked" state rather than an empty gauge.
_Avoid_: Custom provider (acceptable synonym), unsupported provider.

**Usage Meter**:
One distinct usage reading of a **Model Provider** — either a subscription plan's rolling window (quota %) or a pay-as-you-go wallet (credit balance). A provider may have **more than one** (e.g. an Anthropic Claude subscription *and* an Anthropic API wallet). The wire shape is `ProviderUsage` (`UsageWindow` for a plan, `BillingBalance` for a wallet). Shown on the Providers page only when its harness is detected or its key is configured.
_Avoid_: Balance (wallet-only sense), billing identity, quota (plan-only sense).

**Spawn Option**:
A single launchable entry in the **Spawn Menu** — either an **Agent Harness** on its own (launched natively) or an Agent Harness paired with a **Proxied Provider**. The unit a user picks to start an **Agent Node**, and the identity recorded on the node.
_Avoid_: Provider row, launch option, harness profile (the existing `HarnessProfile` struct is harness-only — don't reuse).

**Spawn Menu**:
The single, backend-derived, **Agent Harness**-grouped, user-ordered list of **Spawn Options**. Every spawn surface renders this one menu as-is.
_Avoid_: Provider menu, launch dropdown.


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

**Sandbox**:
A per-Mesh toggle (off by default) that confines an Agent Node's execution process to its Node Working Directory, denying the agent access to the rest of the machine — notably the home folder's credential stores (`~/.ssh`, `~/.aws`). On macOS this is realised with Seatbelt (`sandbox-exec` + a generated `.sb` profile, issue #497); on Windows with a restricted token (issue #528, ADR-0014 — pivoted off the original AppContainer, which hung `claude.exe`). SSH agent forwarding (the `SSH_AUTH_SOCK` socket, not the private key) is granted into the sandbox so Git fetch/push still authenticate. Stored on the `meshes` row and read at spawn; ignored on hosts without a sandbox backend. (Windows read/write confinement is deferred to #542; the current Windows backend fixes the hang/loopback but does not yet deny home reads.)
_Avoid_: jail, container (it is not a container), isolation mode

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

**Role** (API):
The authorization tier a request to the embedded HTTP/WS server resolves to, from its credential (issue #500, ADR-0015). Two **disjoint surfaces**, not a hierarchy: **Admin** — the root token, owning the mobile `/api/*` surface and the WebSockets; and **Coordinator** — the read- or drive-scoped tokens, owning `/nodes*` (drive implies read). A credential is accepted only on its own surface: a Coordinator token on an admin route is `403 Forbidden` (valid but wrong role), distinct from `401 Unauthorized` (no valid credential). Credentials travel only in headers/cookies — `Authorization: Bearer` or the HttpOnly `bm_session` cookie — never a `?token=` URL parameter.
_Avoid_: permission level, scope (reserve "scope" for the read/drive split within the Coordinator role)

**WS ticket**:
A short-lived (30s), single-use credential minted by the authenticated `POST /api/ws-ticket` and passed as `?ticket=` on a WebSocket upgrade (issue #500). It exists because a browser can't set headers on a WS upgrade and proxies strip cookies there; because a ticket can only be obtained through a cookie/header-protected fetch, a cross-site page cannot forge the upgrade.
_Avoid_: WS token, handshake key

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
A per-Mesh opt-in confinement for Agent Node PTY processes, exposed as the "Sandbox agent processes" toggle in the Mesh properties. Off by default; when on, every Agent Node spawned in the Mesh runs under an OS-level confinement keyed to that node — macOS Seatbelt (`sandbox-exec`, #497) and the Windows restricted token (#528, ADR-0014) each implement their own backend, sharing the single `meshes.sandbox` column. The OS-specific spawn policy is decided at one seam (`sandbox::sandbox_enabled`) so the per-OS implementation is swappable; the Mesh/UI layer is OS-agnostic. On macOS the agent can read/write its own worktree and reach the network, with everything else denied by default. On Windows the restricted token currently fixes the AppContainer hang (#528) and loopback (#533) but does **not** yet deny home reads/writes — deny-by-default file confinement is tracked in #542 (a same-user token can't separate user files from the user-keyed kernel objects MSYS `bash` needs). See `docs/adr/0014-pivot-windows-sandbox-off-appcontainer.md` (current) and `0012-windows-appcontainer-agent-sandbox.md` (the superseded AppContainer attempt).
_Avoid_: container (when meaning OS-level confinement), jail, restricted shell

## Relationships

- An **Agent Harness** runs models from a **Model Provider** either natively (a **Native Provider**, owned by the harness) or as a **Proxied Provider** (Buildmesh injects the compatibility shim)
- A **Proxied Provider** is reachable by a harness only over a **Compatible API surface** both share
- A **Generic Model Provider** declares exactly **one Compatible API surface**; a **First-class Model Provider** may declare several and so attach across surfaces (e.g. MiniMax to both Claude Code and Codex)
- A **Model Provider** can be proxied through **zero or more Agent Harnesses** (one spawn-menu entry per pairing). Usage follows the **credential**, not the pairing: proxying *one* credential through several harnesses is still **one Usage Meter**, but a provider may have **several Usage Meters** (e.g. an Anthropic subscription *and* an API wallet)
- The Providers page shows a **Usage Meter** only when it's relevant to the host: a harness-native subscription meter appears when that **Agent Harness** is **detected/installed** (no API key needed); a keyed provider's meter appears when its key is configured; an uninstalled harness's native meter is never shown
- Every spawn surface (sidebar, Issues/PRs probes, archived-resume, mobile) renders the one **Spawn Menu** as-is — none re-orders or re-derives it; harness order is user-set (Terminal pinned last) and **Proxied Provider** options nest under their **Agent Harness**
- A **Proxied Provider**'s configuration splits by scope: the **credential (API key)** is **global to the Model Provider** (entered once, reused across pairings), while the chosen **Compatible API surface + endpoint URL + model-tier remap** are **per harness×provider pairing** (one provider may expose several surfaces; each harness speaks only one). A first-class provider publishes its surface→URL map so a pairing only names the surface; a custom provider's URL is typed per pairing.
- A **Mesh** can have one or more **Agent Nodes**
- A **Mesh** can have **Autopilot** enabled, governed by its **Autopilot Policy**
- **Autopilot** automatically spawns **Agent Nodes** for matching issues or PRs, enforcing branched worktree mode
- An **Agent Node** operates on a child worktree or branch of its parent **Mesh**
- An **Agent Node** emits a **Node Turn** each time its agent yields control back to the user; attention-marking and session naming react to it independently
- A **File Explorer Panel** shows context for either a **Mesh** or an **Agent Node**
- A **Mesh** can have a **drifted root** if its root HEAD is not on the Base Ref's branch
- A **Mesh** can be in a **base branch hostage** state when one of its worktrees holds the Base Ref's branch
- A **Mesh** can have **unpushed commits on root** that block the recovery actions
- A **Mesh** can opt into a **Sandbox**; when on, every Agent Node spawned in the Mesh runs under the OS-level backend (macOS Seatbelt, Windows restricted token)

## Example dialogue

> **Dev:** "When the user spawns a new **Agent Node** under a **Mesh**, does it create a new Git branch?"
> **Domain expert:** "Yes, it creates a dedicated worktree branch tracking the selected starting point of that **Mesh**."

> **Dev:** "When the Issues Probe shows a red flag under an issue's Spawn button, what does that mean?"
> **Domain expert:** "The issue's **Blocked by** list contains at least one issue that's still open in this repo — the flag is a warn, not a gate, so Spawn still works if the user is intentionally unblocking it."

> **Dev:** "What happens if I flip on 'Sandbox agent processes' on a Mesh that's already running agents?"
> **Domain expert:** "The flag is read at spawn time, so already-running Agent Nodes are unaffected. New Spawns from this Mesh on a sandboxing-capable host (macOS Seatbelt, Windows restricted token) will run under the OS-level backend; on hosts with no sandbox backend yet, the flag is a no-op."

## Flagged ambiguities

- "session" and "node" were used interchangeably. Resolved: we canonicalize on **Agent Node** for the user interface and domain model, while database/backend can use "session" for process lifecycle records.
- "state pollution" in worktrees. Resolved: Git worktrees are fully isolated, so parent Mesh cleanliness is not required when spawning new Agent Nodes.
- "Provider" was an avoided alias for **Agent Harness**. Resolved (ADR-0014): **Model Provider** and **Agent Harness** are distinct first-class concepts. "Compatible" moved off the provider onto the connection (**Compatible API surface**); a provider may expose more than one. The old **Claude Code (Compatible API)** term is now just a **Proxied Provider** instance.
- "built-in" was used for both *harness-native* providers (chosen inside the harness, e.g. OpenCode's own login) and *Buildmesh-shipped* providers (the #566 registry). Opposite concepts — resolved: say **Native Provider** for the former and **First-class Model Provider** for the latter; never the bare word "built-in".
