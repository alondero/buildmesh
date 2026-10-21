# PRD: Ubiquitous Companion AI (Buildmesh Fleet Coordinator)

Status: Draft / Planned (issue [#2508](https://github.com/alondero/buildmesh/issues/2508))
Cross-references:
- [ADR-0008: Coordinator Control API](../adr/0008-coordinator-control-api.md) (Anticipates the in-app superagent)
- [Design Contract](../../DESIGN.md) (Violet for AI actions, dark-first layered neutrals)
- [Domain Language](../../CONTEXT.md) (Mesh, Worktree Node, Agent Harness, Launch Configuration, Probe Context Lens)
- [Command Omnibar](../../src/components/CommandOmnibar/CommandOmnibar.tsx) (Omnibar actions & palette routing)

---

## 1. Problem Statement

Buildmesh enables developers to run parallel AI coding agents across multiple repositories using Git worktrees. However, as fleet size grows (5+ agent nodes across multiple meshes), orchestration cognitive load shifts back onto the user:
1. **Manual fleet triage:** The user must manually click between nodes, inspect PTY terminals or Probe tabs to see who is blocked on feedback (`awaiting_input`), who completed tasks, and what PRs were created.
2. **Repetitive multi-step spawning:** Dispatching agents onto the top open issues of a repository requires navigating to the GitHub Issues Probe, triaging tickets one by one, picking spawn options, and clicking spawn repeatedly.
3. **No conversational fleet intelligence:** There is no single natural language interface to ask high-level questions (*"give me a status report on all running nodes across meshes"* or *"what is node 3 stuck on?"*) or dispatch high-leverage batched instructions (*"spin up 3 agent nodes to fix the highest priority issues in repo X"*).
4. **Subscription billing protection:** Developers often utilize flat-rate subscriptions (such as Claude Pro/Max or Codex subscriptions through their CLI logins) rather than pay-per-token API keys. An orchestration assistant must respect these native subscription logins rather than incurring unexpected direct API token charges.

---

## 2. Solution Overview

Buildmesh introduces the **Companion AI** — a ubiquitous, host-level supervisory agent integrated directly into the **Command Omnibar**.

The Companion AI sits above all Meshes, continuously understanding fleet state and executing ad-hoc requests via Buildmesh's native capabilities. It bridges natural language commands with internal Tauri/Coordinator APIs through an agentic tool loop.

### Key Tenets
1. **Unified Omnibar UX:** The existing Command Omnibar (`Cmd/Ctrl+K`) remains the single entry point. Typed navigation and exact commands filter instantaneously as today. Natural language queries or explicit AI triggers (`?` or `Tab`) seamlessly hand off to the Companion AI in a progressive-disclosure execution sheet.
2. **Subscription-Safe Execution via Harness CLI:** The Companion executes through a configured **Launch Configuration** using a headless **Agent Harness CLI subprocess** (e.g. `claude --print`, `agy -p`, or equivalent headless harness). This honors the developer's pre-authenticated flat subscriptions (e.g. Claude Pro/Max) without requiring raw API keys or incurring surprising per-token bills.
3. **Autonomous Execution with Destructive Gating:** Read queries and constructive actions (status reports, issue retrieval, bulk worktree node spawning up to a safety cap, PTY prompt driving) execute autonomously with live streamed progress. Destructive actions (killing nodes, discarding worktree changes, mass reconfigurations) pause for explicit inline user confirmation.
4. **Asynchronous Background Resilience:** Closing the Omnibar modal does not abort an ongoing task. A subtle activity pill in the header/Omnibar indicates ongoing execution, allows reopening the task sheet, and offers an explicit cancel button.

---

## 3. User Stories

### Interaction & UX
1. **As a developer**, I want to open the Command Omnibar (`Cmd/Ctrl+K` or header button) and type natural language queries, so that I don't have to remember rigid command syntax to interact with Buildmesh.
2. **As a developer**, I want to type `?` or press `Tab` in the Omnibar to immediately switch into Companion AI mode with a distinct violet accent style, so that I clearly know when I am talking to the AI.
3. **As a developer**, I want to see an auto-suggested entry (`✦ Ask Companion: "<query>"`) when my typed search has no exact navigation matches, so that I can dispatch natural language commands with a single `Enter`.
4. **As a developer**, I want the Omnibar to progressively expand into an execution card showing live tool steps (e.g. `✓ Read issues from GitHub`, `⟳ Spawning node 1 of 3...`), so that I have complete transparency into what the Companion is doing.
5. **As a developer**, I want to be able to close or dismiss the Omnibar while the Companion is working in the background, so that my workflow is not blocked.
6. **As a developer**, I want to see an active execution indicator (pill) in the TitleBar / Omnibar trigger when a background task is running, so that I can reopen the task sheet and inspect progress or click Cancel at any time.
7. **As a developer**, I want to receive a standard desktop toast notification when a background Companion task finishes, so that I know when my fleet changes are ready.

### Fleet Querying & Supervision
8. **As a developer**, I want to ask *"give me a status report on all running nodes"*, and receive a structured overview of node health, duration, active branch, and current blocking questions.
9. **As a developer**, I want to ask *"what is node 4 waiting on?"*, and have the Companion read that node's `node_digest` and transcript tail to summarize the exact blocker.
10. **As a developer**, I want to ask *"summarize the latest circuit runs in mesh frontend"*, so that I can check workflow pipeline outcomes without opening the Circuits Probe.

### Fleet Action & Spawning
11. **As a developer**, I want to say *"spin up 3 agent nodes to tackle the top bug issues in buildmesh"*, and have the Companion fetch open issues from GitHub, pick the relevant ones, and provision isolated worktree nodes.
12. **As a developer**, I want newly spawned nodes to default to the target Mesh's configured default Launch Configuration, while allowing me to override the harness/model in the prompt (e.g. *"using MiniMax"*).
13. **As a developer**, I want a safety cap (default max 5 nodes, configurable up to 10) enforced on bulk spawning, so that an accidental prompt does not exhaust machine RAM or disk space.
14. **As a developer**, I want to say *"tell all blocked nodes to run unit tests and fix errors"*, and have the Companion write follow-up prompts into their PTYs via `AgentDriver`.
15. **As a developer**, I want the Companion to navigate the UI (focus a specific node or switch to Mesh Grid view) when appropriate, so that I am immediately oriented after a command finishes.

### Safety & Guardrails
16. **As a developer**, when a prompt involves destructive changes (e.g. *"kill all failed nodes"* or *"discard changes in node 2"*), I want the Companion to halt and show an explicit confirmation prompt (`[Confirm (Enter)] / [Cancel (Esc)]`), so that no work is lost unintentionally.

---

## 4. Architecture & Technical Design

### 4.1 System Diagram

```
 +-------------------------------------------------------------------+
 |                         Buildmesh Frontend                        |
 |                                                                   |
 |  [CommandOmnibar]  <--->  [CompanionSheet]  <--->  [TitleBar Pill] |
 |         |                         ^                               |
 +---------|-------------------------|-------------------------------+
           | tauri::invoke           | tauri::emit ("companion-event")
           v                         v
 +-------------------------------------------------------------------+
 |                   Rust Core (src-tauri)                           |
 |                                                                   |
 |  [companion::service]                                             |
 |       |                                                           |
 |       +--> [Harness Subprocess Worker]                            |
 |       |    (Spawns Launch Config binary, e.g. `claude --print`)   |
 |       |    (Communicates via loopback tools / stdio)              |
 |       |                                                           |
 |       +--> [Internal Coordinator Tools]                           |
 |            |-- list_meshes / list_nodes                           |
 |            |-- get_node_digest / transcript_tail                  |
 |            |-- github_issues / github_prs                         |
 |            |-- spawn_agent_nodes (worktree provisioning)          |
 |            |-- agent_driver::send_prompt (PTY write)              |
 |            |-- circuits_summary                                   |
 |            +-- kill_nodes (destructively gated)                   |
 +-------------------------------------------------------------------+
```

### 4.2 Execution Engine: Headless CLI Subprocess

To protect subscription models (e.g., Claude Code flat subscriptions without token billing):
1. **Configuration:** App Settings gains a new section: **Companion AI Configuration**:
   - `companion_launch_config_id`: Selected Launch Configuration (defaulting to the system default harness, e.g. Claude Code or Codex).
   - `bulk_spawn_limit`: Maximum number of nodes that can be spawned in a single command (default: `5`, hard ceiling: `10`).
2. **Subprocess Invocation:**
   - The Rust backend invokes the resolved binary with headless parameters (e.g., `claude --print --non-interactive` or `agy -p`).
   - The Companion system prompt defines its role as the Buildmesh Fleet Coordinator, equipping it with the JSON tool-calling schema described below.
   - Loopback tool execution: The subprocess executes tools by dispatching HTTP requests to Buildmesh's internal Coordinator API (`http://127.0.0.1:1992-1994`) or via structured JSON stdio protocol.

### 4.3 Tool Catalog (The Action Space)

#### Read Tools (Autonomous)
* **`get_fleet_overview()`**: Returns all registered Meshes, active nodes per mesh, and summary counts (`running`, `awaiting_input`, `failed`, `idle`).
* **`list_nodes(mesh_id?, status?)`**: Returns detailed node list (`id`, `name`, `mesh_id`, `status`, `waiting_since`, `branch`, `source_issue`).
* **`inspect_node(node_id)`**: Returns the node's `node_digest` and the last assistant message/question from the transcript.
* **`list_github_issues(mesh_id, state?, label?, limit?)`**: Queries issues for the repository using Buildmesh's GitHub credentials.
* **`list_github_prs(mesh_id, state?, limit?)`**: Queries open PRs for the repository.
* **`get_circuit_summary(mesh_id?)`**: Returns active or recent Circuit runs and their stage outcomes.

#### Drive Tools (Autonomous with Safety Caps)
* **`spawn_worktree_nodes(mesh_id, requests: [{ issue_number?, initial_prompt?, launch_config_id? }])`**:
  - Validates count <= `bulk_spawn_limit`.
  - Provisions branched Git worktrees via `git::worktree::provision`.
  - Spawns agent processes using the mesh default or requested Launch Configuration.
* **`drive_node_prompt(node_id, prompt)`**:
  - Writes prompt to the node PTY via `AgentDriver::send_prompt` (ADR-0008).
  - Returns honest delivery verdict (`Delivered` vs `Unverified`).
* **`navigate_ui(focus_node_id?, view_mode?, mesh_id?)`**:
  - Emits UI navigation event to focus an agent node or switch grid view.

#### Destructive Tools (Gated by Confirmation)
* **`terminate_nodes(node_ids: number[], discard_worktree: boolean)`**:
  - Emits a `confirmation_required` event to the frontend.
  - Suspends execution until the user responds with `approve` or `reject`.
* **`bulk_reconfigure_reviewers(node_ids: number[], provider_route: string)`**:
  - Updates review configurations across active nodes.
  - Requires confirmation before mass mutation.

---

## 5. UI & Interaction Design

### 5.1 Omnibar Dual-Mode Entry

1. **Standard Palette Mode (Default):**
   - User opens Omnibar with `Cmd/Ctrl+K`.
   - As the user types, existing exact match items (navigation, meshes, settings) render at the top.
   - At the bottom of the list, a special dynamic item is always present:
     ```
     ✦ Ask Companion: "<current input text>"                [Enter]
     ```
   - Pressing `Enter` on this item, or pressing `Tab` / prefixing with `?`, transitions the palette into Companion Mode.

2. **Companion Mode (Violet Theme):**
   - In accordance with [DESIGN.md](../../DESIGN.md), AI actions use the **violet** role.
   - The Omnibar border and glow shift from cyan (`#00d4ff`) to violet (`#8b5cf6` / `--accent-violet`).
   - The placeholder changes to: `Ask the Buildmesh Companion to inspect, spawn, or drive nodes...`

### 5.2 Progressive Disclosure Execution Sheet

When a command starts:
1. **Running State:** The Omnibar input stays anchored at the top, while the lower area transitions into an execution stream:
   - A pulsing violet status line: `✦ Analyzing fleet state across 3 meshes...`
   - Step checklist with real-time updates:
     - `✓ Fetched open issues from buildmesh (found 12)`
     - `✓ Selected top 3 bug tickets: #2104, #2115, #2118`
     - `⟳ Provisioning worktree for issue #2104...`
2. **Confirmation Gate (When needed):**
   - If a destructive tool is called, the sheet renders an amber/violet warning box:
     ```
     ⚠️ Destructive Action Requested
     The Companion wants to terminate 3 agent nodes and delete their worktrees:
     • gh-120-fix-auth
     • gh-121-api-leak
     • pr-44-refactor
     [Confirm Execution (Enter)]        [Cancel (Esc)]
     ```
3. **Completed State:**
   - Summarized answer in clean markdown.
   - Actionable direct deep-links (e.g. `[Focus Node: gh2104]`, `[Open Mesh Grid]`).

### 5.3 Background Execution & TitleBar Pill

* If the user presses `Esc` or clicks outside during execution:
  - The modal closes; the background task **does not cancel**.
  - A compact pill appears in the TitleBar (or right side of the Omnibar button):
    `✦ Companion (2/3) [●]` with an animated violet pulse.
  - Clicking the pill immediately re-opens the execution sheet at its current progress.
  - Inside the sheet, a prominent **"Stop / Cancel Task"** button is always available to kill the background runner.
* When finished in the background:
  - The pill transitions to `✓ Companion task complete`.
  - A desktop toast appears: `Companion finished spawning 3 nodes on buildmesh.`

---

## 6. Implementation Decisions

1. **Rust Module Location:**
   - New module: `src-tauri/src/companion/`
     - `mod.rs`: Public API and lifecycle.
     - `runner.rs`: Subprocess invocation and stdout/event parsing.
     - `tools.rs`: Tool dispatcher calling existing internal services (`coordinator`, `git::worktree`, `agent::spawn`).
     - `state.rs`: In-memory tracking of the current active background task (one active task at a time for v1).
2. **Tauri IPC Commands & Events:**
   - `companion_execute_prompt(prompt: String, mesh_context_id: Option<i64>) -> Result<TaskId, String>`
   - `companion_respond_confirmation(task_id: TaskId, approved: bool) -> Result<(), String>`
   - `companion_cancel_task(task_id: TaskId) -> Result<(), String>`
   - `companion_get_active_task() -> Option<CompanionTaskState>`
   - Events emitted: `companion://step`, `companion://confirmation-needed`, `companion://completed`, `companion://error`.
3. **Frontend Components & State:**
   - `src/stores/companionStore.ts`: Tracks active task state, streamed steps, confirmation requests, and drawer open/closed state.
   - `src/components/CommandOmnibar/CompanionMode.tsx`: Execution sheet and tool step presentation.
   - `src/components/TitleBar/CompanionPill.tsx`: Active background indicator in the header.

---

## 7. Testing Decisions

1. **Rust Unit & Integration Tests:**
   - `tools_test.rs`: Test tool implementations against in-memory SQLite and mock Git/PTY targets to ensure permissions, caps, and validations hold.
   - `safety_gate_test.rs`: Assert that destructive tools never execute without explicit `approved: true` input.
   - `bulk_cap_test.rs`: Assert that requesting more than `bulk_spawn_limit` nodes is clamped or rejected cleanly.
2. **Frontend Component Tests:**
   - Omnibar transition tests: Verify typing `?` or `Tab` switches modes; verify selecting `✦ Ask Companion` triggers execution.
   - Execution sheet render test: Verify tool progress steps and final markdown links render correctly.
   - Confirmation modal test: Verify Enter confirms and Esc rejects destructive operations.
3. **UI Verification:**
   - Run UI verification skill checking contrast tokens (WCAG AA compliance for violet on zinc surfaces).

---

## 8. Out of Scope for v1

1. **Long-running Multi-turn Conversational Memory:** v1 is strictly command-and-response / single-intent execution. Retaining months of conversational memory or chat threads is deferred.
2. **Direct Internet Web Surfing:** The Companion only interacts with the local machine, Buildmesh nodes, and configured GitHub repositories.
3. **Concurrent Background Companion Jobs:** Only one top-level Companion command runs at any given time in v1. Starting a new command cancels or waits on the previous one.
4. **Autonomous Git Push / PR Creation:** The Companion can spawn nodes that create PRs, but does not autonomously force-push or merge PRs directly on host meshes.

---

## 9. Next Steps for Implementation Session

1. **Phase 1 (Backend Foundation):** Implement `src-tauri/src/companion/tools.rs` wiring read/drive tools to the existing Coordinator and Worktree APIs.
2. **Phase 2 (Subprocess Runner):** Implement `runner.rs` to invoke the configured Launch Configuration CLI with tool calling.
3. **Phase 3 (Omnibar UI & Store):** Extend `CommandOmnibar.tsx` with Companion mode, progressive execution stream, and TitleBar indicator pill.
4. **Phase 4 (End-to-End Verification):** Verify bulk issue spawning, status reports, and destructive confirmation prompts in a live development harness.
