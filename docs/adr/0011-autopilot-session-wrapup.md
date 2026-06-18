# 11. Autopilot Session Wrap-up — prompt-driven wrap-up and PR creation

Status: accepted

For Buildmesh's Autopilot mode, we decide to drive session wrap-up, testing, code review, and PR creation by injecting a structured template prompt (modeled on the user's custom `finish.md` command) directly into the agent's PTY, rather than implementing native git, test, and PR operations in the Tauri backend.

## Context

When an Autopilot-spawned agent finishes its work, the session needs to be verified (tests run, lints checked) and wrapped up (code committed, pushed, and a pull request created). Implementing this pipeline natively in the Tauri backend (using `git2` and direct GitHub API calls) introduces significant complexity:
- The backend would have to parse and run language-specific test runners, capture their output, and handle test failures.
- It would have to authenticate and interact with GitHub/GitLab to create PRs, duplicating tool access that the agent CLI already possesses.
- It bypasses the agent's own cognitive self-correction loop when resolving compilation or test failures.

Alternatively, the user already uses a highly refined, multi-step prompt sequence (stored in `finish.md`) to instruct the agent to run tests, do a code review, stage changes, push, and raise a PR.

## Decision

1. **PTY Prompt Injection**: Instead of executing backend scripts, Buildmesh triggers the `/finish` slash command by writing the full contents of the user's wrap-up prompt directly to the agent's PTY stdin.
2. **Agent-Driven Pipeline**: The agent CLI executes the tests, fixes linting/build issues, commits, pushes, and spawns the draft PR itself (using its own access to system tools like the GitHub `gh` CLI).
3. **LLM State Classification**: Buildmesh uses a cheap/fast LLM evaluator to scan PTY output on turns, determining when the agent has successfully finished the `/finish` sequence so the node can be marked `Completed` in the UI.

## Consequences

- **Extreme Simplicity**: The Tauri backend remains a "dumb" process supervisor and PTY driver. It doesn't need to know how to run tests or build PRs.
- **Robust Self-Correction**: If tests fail during the wrap-up, the agent automatically attempts to fix the code because it is driving the loop, eliminating complex retry/repair logic in Tauri.
- **Flexibility**: The `/finish` prompt template can be customized per project/mesh or overridden by the user without changing the compiled Rust application.
- **Dependency on Agent Access**: The agent CLI must have access to necessary system commands (like `npm test` and `gh`) in its environment (Windows/WSL) for the wrap-up to succeed.
