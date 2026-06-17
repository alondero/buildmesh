# Autopilot ModeSpec — Event-Driven Agent Node Provisioning and Self-Correction Loop

## Problem Statement

The problem that the user is facing, from the user's perspective:
Developers running multiple AI agents in parallel on local repositories face high cognitive overhead in manually checking ticket trackers (like GitHub Issues), creating dedicated branches and worktrees, launching individual agent CLIs, pasting context, and running verification steps. When running multiple parallel tasks, it is easy to lose track of which issue matches which agent node, leading to manual copy-pasting, directory collisions, and local machine resource bottlenecks. Furthermore, when agents yield after compiling broken code or failing test suites, developers must intervene manually to input the test failures and tell the agent to fix them.

## Solution

The solution to the problem, from the user's perspective:
Buildmesh will introduce **Autopilot Mode**, a background worker daemon that monitors a remote issue tracker (like GitHub) for tagged tickets, automatically provisions isolated parallel **Agent Nodes** (using branched Git worktrees), and drives them through the complete implementation, self-correction, and pull request lifecycle. Buildmesh delegates the entire code review, verification, and PR creation task to the agent process itself by injecting a standardized wrap-up prompt (derived from the user's custom `/finish` command) to the PTY stdin upon completion. This keeps the developer at a high, supervisory level—only stepping in when human feedback is explicitly requested or when automated correction attempts fail.

## User Stories

A LONG, numbered list of user stories:

1. As a developer, I want to enable Autopilot Mode on a Mesh, so that Buildmesh can automatically handle task ingestion and agent execution for that repository.
2. As a developer, I want to configure an Autopilot Policy for a Mesh (including trigger label, concurrency limits, and default provider), so that I can control how and when automated tasks are spawned.
3. As a developer, I want Buildmesh to poll my GitHub repository for open issues with a specific label (e.g. `buildmesh:run`), so that new tasks can be picked up without manual UI input.
4. As a developer, I want Autopilot to respect my mesh configuration and automatically create branched worktrees for auto-spawned tasks, so that each task is isolated and ready for a pull request.
5. As a developer, I want Autopilot to restrict the number of concurrently running automated agent nodes to a set limit (e.g., max 2), so that my local CPU/RAM and LLM token budget are not exhausted.
6. As a developer, I want Autopilot to poll for new issues only when spare concurrency capacity is available, so that the local system database is not polluted with unstarted or stale queued nodes.
7. As a developer, I want Buildmesh to automatically bypass a `drifted root` or `unpushed commits on root` state on the parent Mesh when spawning an Autopilot node, so that local branch drift doesn't block background automation.
8. As a developer, I want Buildmesh to parse and reconcile remote issue states before spawning, so that issues that were closed or untagged while the app was offline are ignored.
9. As a developer, I want Buildmesh to run a lightweight LLM State Evaluator (e.g. via `cwrap`) on every PTY yield, so that it can autonomously classify if the agent is blocked (needs human help/keys) or finished.
10. As a developer, I want Buildmesh to automatically execute the `/finish` slash command prompt (expanded from my local `finish.md` recipe) in the agent's PTY when the task is classified as finished, so that the agent performs its own self-review and tests.
11. As a developer, I want the agent to automatically commit and push its changes and create a draft pull request on GitHub once it completes the `/finish` instructions, so that I don't have to run git commands manually.
12. As a developer, I want Buildmesh to detect if the agent failed the verification step during the `/finish` execution, so that it can feed the error logs back into the PTY stdin and let the agent self-correct.
13. As a developer, I want Buildmesh to cap the self-correction feedback loop at a configurable retry limit (e.g. 3 attempts), so that the agent doesn't get stuck in an infinite loops of failing tests.
14. As a developer, I want Buildmesh to notify me via system notifications and UI badges when an Autopilot task successfully creates a PR or fails its self-correction limit, so that I know exactly when my review is needed.
15. As a developer, I want to manually trigger the `/finish` command on any active node via the UI toolbar, so that I can automate the code-review, testing, and PR pipeline for tasks I started manually.

## Implementation Decisions

A list of implementation decisions that were made:

- **Mesh Config Schema Updates**: Add columns to the `meshes` table: `autopilot_enabled` (INTEGER), `autopilot_trigger_label` (TEXT), `autopilot_concurrency_limit` (INTEGER), `autopilot_provider` (TEXT), and `autopilot_action_on_success` (TEXT). Derive the Rust/TS struct mappings.
- **Autopilot Polling Daemon**: A background service `src-tauri/src/services/autopilot.rs` that polls GitHub issue labels every 2 minutes using the mesh's remote credentials.
- **Enforced Branched Worktree**: Spawning an autopilot node overrides `worktree_mode` to `branched` and `use_worktree` to `true`.
- **State Evaluator & Prompt Injection**: On PTY yield (`publish` turn), clean the buffer and spawn `cwrap --minimax` with a prompt to classify if the agent has finished or is blocked.
- **PTY `/finish` Command Injection**: If finished, write the expanded user-defined `finish.md` wrap-up prompt to the PTY's stdin.
- **Self-Correction & PR Completion**: Verify branch pushes and test completions in the background. Feed errors back to PTY up to 3 times before failing the node. Run `gh pr create` via PTY/CLI on successful clean pushes.

## Testing Decisions

A list of testing decisions that were made:

- **Unit and Integration Tests**:
  - Test `StateEvaluator` output classification with mock PTY transcript chunks.
  - Test `Autopilot` scheduler concurrency, verifying that it correctly respects limits and pulls the next issue from GitHub when space is cleared.
- **Prior Art**:
  - LLM-driven async operations: `src-tauri/src/session_naming.rs` and its tests (`DbSessionNamingRepository`).
  - Git worktree lifecycle: `src-tauri/src/git/worktree.rs` and its tests.

## Out of Scope

- Detailed interactive UI dialogs to edit PR descriptions before submission.
- Direct inbound webhook server support (polling or loopback Coordinator API only).
- Non-GitHub issue trackers (MVP targets GitHub exclusively).

## Further Notes

- The `/finish` prompt text will be loaded from a system configuration file, allowing developers to customize their wrap-up criteria.
