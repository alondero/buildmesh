# Buildmesh Context

Buildmesh is an orchestration platform for AI coding agents that work in parallel across meshes (repository roots) using Git worktrees.

## Language

**Agent Harness**:
The executor binary recipe (e.g. `claude` / `claude.exe`, `codex`, `agy`, `opencode`, `terminal`) that launches and communicates with an AI coding agent. Only the `Terminal` harness is always available; all others must be installed on the host and enabled by the user. A harness is distinct from the **Model Provider** whose models it runs (ADR-0014).
- **Model-Configurable Harness**: An **Agent Harness** that accepts dynamic model/provider overrides directly from Buildmesh's Mesh settings at spawn time (via CLI flags like `--model` or `--provider`). E.g., `Claude Code` (`anthropic`), `Codex`, `Antigravity` (`agy`), `OpenCode`.
- **Default-Only Harness**: An **Agent Harness** that ignores Buildmesh-level model/provider overrides and runs exclusively using its own local or global configuration file (e.g., `~/.pi/config.json`). E.g., `Pi Code` and the `Terminal` harness.
_Avoid_: Executor, runtime, provider (a harness is *not* a provider — see **Model Provider**).

**Model Provider**:
The credentials and billing identity a model request is served by (e.g. Anthropic, OpenAI, MiniMax, Kimi, DeepSeek, or a user-named generic key). Independent of the **Agent Harness** that runs it. Endpoint URL and model-tier remap live on the **Proxied Provider** pairing, not on the provider itself.
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
A user-defined **Model Provider** — a display name and an API key, with no registry entry. Compatible API surface, base URL, and model-tier remap are chosen per **Proxied Provider** attach (so one credential may attach under Claude Code and under Codex with different endpoints). Spawns fine, but has no brand icon (neutral fallback) and **no usage integration** (Buildmesh can't know its billing API), so its Providers-page card shows a "usage not tracked" state rather than an empty gauge.
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

**Spawn Context**:
The fully resolved state Buildmesh carries mid-spawn — the loaded `AgentNode`, the resolved **Spawn Source** (so the provisioner can branch on Issue / PR / Manual adoption mode without re-reading the node row), the mesh-derived `base_ref` (post-fetch for PR/Issue spawns, mesh `base_ref` otherwise), the resolved `worktree_mode` (`"branched"` or `"detached"`), the `use_worktree` flag, the optional **Pre-spawn Worktree** claim, and the resolved `host_path` (Windows form for git operations). It is the boundary between *resolving* what to spawn and *provisioning* the on-disk Worktree Node. The provisioner (`git::worktree::provision`) reads these fields and returns one of `Reused` / `Adopted` / `Upgraded` / `Created`. The provisioner also owns the warm-failure cold fallback and the post-success bookkeeping (`forget_after_spawn`, Manual name adoption, `post_spawn_maintenance` trigger) — three sibling inputs (`&ProvisionHooks` for decision flags + `&dyn ProvisionSink` for side effects) travel with the Spawn Context so the spawn pipeline (`agent::spawn::spawn_agent_inner`) doesn't need to thread the entry back out or rebuild the cold context for the retry. The WSL-spelled path the spawned agent will see stays off the Spawn Context and is consumed at command-build time in the launch phase (`agent/spawn/launch.rs`) after `provision_for_spawn` returns — putting it on the Spawn Context would duplicate env-shape logic that phase already owns. PTY size, prefill, and cascade overrides are likewise launch-phase inputs (`LaunchParams`), not Spawn Context fields; the orchestrator passes them straight to `launch_process` rather than couriering them through provision.
_Avoid_: spawn state, spawn config, resolved spawn params (these miss the temporal "between phases" sense); **Spawn Recipe** (that's the per-**Agent Harness** shell + env-var bundle, not the resolved state of one attempt).

**Spawn Source**:
The runtime classification of an **Agent Node** spawn — how it was triggered. One of three values: `Manual` (user clicked Spawn from the mesh panel — no `source_issue`, no `source_pr`), `Issue` (spawned from the Issues Probe — `source_issue` is set, the node carries a `gh{N}-` branch name), or `PullRequest` (spawned from the PR Probe — `source_pr` is set, the node carries a `pr{N}-` branch name). The Worktree Node provisioner uses Spawn Source to decide between the two Pre-spawn Worktree adoption modes: `Issue` and `PullRequest` move the pool's plain-slug directory to the node's `gh{N}-`/`pr{N}-` name (`git worktree move` + checkout to the resolved base SHA); `Manual` adopts the pool's pre-assigned slug as the node's own name and `git checkout -B` aligns the worktree's mode with the mesh's `worktree_mode`. Distinct from **Spawn Option** (which is a menu entry — what the user *could have* picked) and from **Spawn Context** (which is the resolved state of one attempt).
_Avoid_: spawn kind, spawn type, spawn trigger (these miss the runtime-vs-menu distinction).

**Probe Context Lens**:
The ownership perspective of a Probe destination: **Host** for machine-wide provider and account state, **Mesh** for one repository workspace and its configuration, or **Agent** for one Agent Node's changes and history. A lens names what the destination is about; a focused Agent Node may still provide a secondary working-tree view for the Mesh-owned File Explorer Panel.
_Avoid_: active context, Probe scope (both are too vague about ownership).


**Mesh**:
A project workspace associated with a local Git repository root path.
_Avoid_: Project, repo, folder

**Agent Node**:
An interactive panel running a single agent execution process within a dedicated directory (either a worktree or the mesh root).
_Avoid_: Session, pane, terminal node

**Worktree Node**:
An Agent Node operating on an isolated Git worktree branch of its parent Mesh. (Used when the Mesh property use_worktree is true, unless overridden).

**Pre-spawn Worktree**:
A warm Git worktree checked out to the latest commit of a Mesh's **Base Ref** in detached HEAD state, sitting in the **Pre-spawn Pool** ready to be adopted instantly by a newly spawned **Worktree Node**.
_Avoid_: Warm worktree, pre-warm worktree.

**Pre-spawn Pool**:
A persistent set of **Pre-spawn Worktrees** maintained on disk and tracked in SQLite to eliminate the cold checkout cost during agent spawn.
_Avoid_: Warm pool, worktree cache.

**Root Node**:
An Agent Node operating directly on the parent Mesh's root directory, bypassing worktree isolation. (Used when the Mesh property use_worktree is false, or when overridden via Alt-click).

**Node Working Directory**:
The directory an Agent Node's work physically lives in: its Worktree Node dir (`.claude/worktrees/<name>`) for a Worktree Node, or the Mesh root for a Root Node. The canonical "where is this node's stuff" rule (resolve `use_worktree` + a trimmed, non-empty `worktree_name`) lives in one place; callers pick the host form (Windows git2) or the spawn form (the path as the agent saw it — Linux for a WSL node, which is the form Claude Code encodes for its on-disk transcript directory).
_Avoid_: working path, repo path, node dir

**Node Turn**:
The point at which an Agent Node yields control back to the user — its agent has stopped and is waiting. Claude Code surfaces this as several hooks (the Stop hook = awaiting input, plus the catch-all Notification hook = idle prompt or permission prompt); each provider's payload is normalized (issue #1364) into a **Lifecycle Kind** (`turn_completed`, `input_required`, `permission_requested`, `question_requested`, `background_running`, `process_idle`, `session_exited`, `error`, `signal_unavailable`) carried on one `agent-lifecycle` event to both clients. A Node Turn fans out to independent reactions: the lifecycle transition (status → `ready` for a clean turn, `awaiting_input` when blocked on a permission/question, `running`-stay for background work), and considering an AI rename (session naming). The trigger is a clock tick, not a content source — naming's summary comes from the buffered PTY output, so the *kind* of yield never changes what gets named.
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
The desktop File Explorer Panel's distinct view listing modified files with their addition/deletion line counts, sourced from the working-tree `get_git_status` (HEAD-relative, uncommitted-only). For the mobile Changes view's full since-branch set, see **Node Change Set**.
_Avoid_: Modified files list

**Node Change Set**:
The set of files an **Agent Node** changed since it branched from its **Base Ref** — committed *and* uncommitted work — diffed against the merge-base of the node's `HEAD` and `base_ref` (ADR-0005). One baseline resolves it (`resolve_base_tree`), and both readers hang off that one seam: the file-list (`node_changed_files` / `node_changed_summary`) and the per-file diff (`diff_node_file_against_base`), so a node's mobile Changes tree, its header counts, and its tapped diffs always agree on what "changed" means. Distinct from the HEAD-relative working-tree status the desktop **Changed Files Section** shows for an arbitrary path.
_Avoid_: diff set, review set, changed files (ambiguous with the desktop HEAD-relative view).

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

**Device Session**:
A paired phone's own persistent credential on the Admin surface (issue #502, ADR-0018). Pairing — the first `POST /api/session` with the root token — mints a per-device token, stores its SHA-256 hash plus metadata (label, last IP, timestamps) in the `device_sessions` table, and hands the raw token back; the phone keeps it in its keystore (`localStorage` for the web SPA) and presents it thereafter instead of the root token. Because the token, not the IP, identifies the device, a phone keeps its session as it **roams** across networks; because each device holds a distinct token, the user can **revoke** one device — deleting its row blocks its next request, and a revocation signal force-closes any WebSocket it already holds — without disturbing the others or the root token. Surfaced in the desktop "Authorized Devices" panel and the remote `GET /admin/devices` + `POST /admin/devices/{id}/revoke` routes. A Device Session resolves to the **Admin** Role (above); the root token remains the pairing secret.
_Avoid_: device token (the token is one field of the session), login

**WS ticket**:
A short-lived (30s), single-use credential minted by the authenticated `POST /api/ws-ticket` and passed as `?ticket=` on a WebSocket upgrade (issue #500). It exists because a browser can't set headers on a WS upgrade and proxies strip cookies there; because a ticket can only be obtained through a cookie/header-protected fetch, a cross-site page cannot forge the upgrade. The ticket also carries the minting **Device Session**'s id, so a revocation can find and close the exact live socket that device opened. The ticket is **bound at mint time to a target** — a surface (`terminal` or `events`) and, for `terminal`, a specific node `id` (issue #551, in the request body); the upgrade rejects a ticket whose bound target doesn't match the requested one (`403`), and a target mismatch does **not** consume the ticket so a misrouted legitimate client can retry. The **binding** — not the 30s TTL — is the trust boundary: it narrows a leaked ticket from "any node the minting role can read" to "the single target the caller asked for", and makes a leak observable.
_Avoid_: WS token, handshake key

**Autopilot**:
An automated background execution mode for a Mesh that polls a remote issue tracker and automatically spawns Agent Nodes when matching issues/PRs are detected.
_Avoid_: Auto-worker, event listener

**Autopilot Policy**:
The set of configuration settings (trigger labels, concurrency limits, provider overrides, and success actions) that govern a Mesh's Autopilot behavior.

**Looping Autopilot**:
A per-Mesh Autopilot mode (selected via `autopilot_mode` = `"looping"`, set apart from `"issue_driven"`) that runs a single sequential Agent Node per loop iteration, executes the configured wrap-up sequence (session `finish.md` — verification + commit + push + open a draft PR if enabled), optionally injects a `loop_suffix_prompt` post-verification as a second prompt turn, then pauses `loop_interval_seconds` before the next iteration. Configuration lives on six nullable columns on the `meshes` row plus the `autopilot_mode` discriminator (wayfinder #990 ticket #991) and is edited through the dedicated **Autopilot** Probe tab (ticket #994); runtime state (Active / iteration N, Paused, Idle, Stopped) lives in process state until the loop scheduler (ticket #992) ships. Looping iterations respect the mesh's `use_worktree` setting; issue-driven autopilot always runs in a worktree (its poller overrides `use_worktree_override = Some(true)` — see `services/autopilot.rs`).
_Avoid_: Sequential autopilot, retry loop, cron autopilot

**Autopilot Circuit**:
A user-authored trigger-action graph on one Mesh — the composable generalisation of the two fixed Autopilot modes. Cycles are allowed only when every directed cycle is bounded by a `RetryLimit` or a `CollaboratorCheck` that requires approval. Newly created circuits start **disabled** (draft-first) so background pollers cannot fire while the graph is still being authored; **Trigger Now** still dry-runs a disabled circuit. The **blueprint** persists as `graph_json` on the `autopilot_circuits` row; a **Circuit Run** is one execution instance of it (`autopilot_circuit_runs`, deduped per `(circuit, trigger_identity)`); a **Circuit Step** is one circuit node's execution within a run (`autopilot_circuit_run_steps`; `pending_slot` = parked on a concurrency limit). Decisions come from a pure stepper (`autopilot::circuit::stepper`); the worker thread in `services::circuit_worker` observes live state and executes its effects. The ledger vocabulary above is the **stored** vocabulary; user-facing surfaces render a humanised label instead (`pending_slot` → "Queued", `blocked` → "Needs approval", `completed` → "Done") via `Circuits/runDiagnostics.ts`, keeping the raw token on a `data-run-state` / `data-step-status` attribute for tests and CSS (issue #1468). "Node" is overloaded: always qualify — *circuit node* (graph vertex) vs *agent node* (mesh session).
Pending Circuit Runs have a persisted, per-Mesh queue position and are admitted nearest-first; users may move or cancel them. The worker uses `circuit_run_capacity` as the per-Mesh run-admission policy and reserves the blueprint's declared SpawnAgentNode footprint in a durable per-run lease; the optional app-wide Autopilot pool remains a separate host-process backstop. Without that optional pool, there is no additional per-mesh agent-process cap: run capacity limits admitted runs, not fan-out. Admission accounts for durable worst-case leases, while Tick accounts for live circuit agents. Cancellation terminalises the run before attached Agent Nodes are retired; deleting a Circuit disables it and removes its ledger only after every retirement succeeds, retaining the disabled ledger for retry when cleanup fails.
_Avoid_: workflow graph, pipeline (when meaning a Circuit), flow (when meaning the blueprint)

**Node Digest**:
A coordinator-facing read summary of a single Agent Node answering "what's going on, and does it need feedback?". Layered: an always-available spine from Buildmesh's own DB (lifecycle `status`, "needs feedback" = `awaiting_input`) enriched, for harnesses with a wired transcript reader (currently Claude Code/Claude-compatible profiles, Codex, Cursor, AGY, Grok, and Command Code), with semantic content read from the agent's on-disk JSONL transcript. Non-supporting providers, or a transcript that fails to parse, degrade to the spine with the enrichment explicitly flagged unavailable (never silently omitted). The rendered terminal/TUI is deliberately **not** a digest source.
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
- A **First-class Model Provider** may publish several Compatible API surfaces and so attach across harnesses (e.g. MiniMax to both Claude Code and Codex); a **Generic Model Provider** picks surface per attach from the target harness
- A **Model Provider** can be proxied through **zero or more Agent Harnesses** (one spawn-menu entry per pairing). Usage follows the **credential**, not the pairing: proxying *one* credential through several harnesses is still **one Usage Meter**, but a provider may have **several Usage Meters** (e.g. an Anthropic subscription *and* an API wallet)
- The Providers page lists only configured providers: self-auth first-class rows are always present (enable + billing); keyed first-class rows appear after the user adds them from the catalog; generics after the user creates them. A **Usage Meter** still appears on the Usage tab when relevant (harness installed or key set)
- Every spawn surface (sidebar, Issues/PRs probes, archived-resume, mobile) renders the one **Spawn Menu** as-is — none re-orders or re-derives it; harness order is user-set (Terminal pinned last) and **Proxied Provider** options nest under their **Agent Harness**
- A **Proxied Provider**'s configuration splits by scope: the **credential (API key)** and (for first-class) **billing mode** are **global to the Model Provider**; the **Compatible API surface + endpoint URL + model-tier remap** are **per harness×provider pairing** and are edited only on the Harnesses page. Saving a key never auto-attaches a pairing — attach is explicit. A first-class provider's published surface→URL(+tiers) map prefills the attach form; the stored pairing is the source of truth at spawn
- A **Mesh** can have one or more **Agent Nodes**
- A **Mesh** can have **Autopilot** enabled, governed by its **Autopilot Policy**
- **Autopilot** automatically spawns **Agent Nodes** for matching issues or PRs, enforcing branched worktree mode
- An **Agent Node** operates on a child worktree or branch of its parent **Mesh**
- An **Agent Node** runs inside a configured **Sandbox Mode** to isolate execution from the host OS
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
