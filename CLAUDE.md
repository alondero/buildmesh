# Buildmesh

Tauri 2 desktop app (React 19, Rust) orchestrating coding agents across
repositories, with persistent xterm.js terminals and Windows/WSL support.

## Start every task

1. Check the worktree root, branch and `git status --short`; record the base
   commit. Write under this worktree root and confirm edits with `git diff`.
2. Read [the engineering contract](docs/agents/engineering.md) for the shared
   workflow and conditional references. Start or resume the development harness.
3. Use [the architecture index](docs/knowledge-primer.md) to find the relevant
   owner document; read that section, then its owning code. The index and owner
   docs are references, not whole-file reading assignments. Domain language:
   [CONTEXT.md](CONTEXT.md). Decisions: `docs/adr/`. Dated investigations in
   `docs/archive/` are history.
4. Iterate with `npm run harness -- check`; complete with `npm run verify` and
   the harness's current acceptance and review evidence.

## Read when the scope needs it

| Scope | Reference |
|---|---|
| Windows/worktree dependencies, Cargo targets, test environment | [Worktree setup](docs/agents/development-harness.md#worktree-setup) |
| Rust, DB connections/tests, generated Rust?TS wire types | [Rust conventions](docs/development/rust-conventions.md); pass one connection through helpers and keep external I/O outside DB locks |
| Terminal lifecycle or streaming | [Terminals](docs/development/terminals.md); retain singleton terminals across React remounts and dispose only when a node is deleted |
| Windows/WSL paths | [Windows](docs/development/windows.md); convert through `env::to_host_path` |
| Agent launch, naming or shell choice | [Agent Nodes](docs/development/agent-nodes.md), [providers](docs/development/providers.md); naming is backend-owned and launch shells are adapter-owned |
| New provider, Tauri command, HTTP route or user workflow | [Common-change checklists](docs/development/README.md#common-change-checklists) |
| UI, theme or Probe tabs | [DESIGN.md](DESIGN.md), [Probe checklist](docs/development/probe-ui-checklist.md), [UI verification](.claude/skills/verify-ui/SKILL.md) |
| Review or review repairs | [Review skill](.claude/skills/review/SKILL.md), [self-review](docs/agents/self-review.md) |
| Instructions, hooks, CI or documentation | [Enforcement](docs/agents/engineering-reference.md#enforcement-and-maintenance), [documentation standards](docs/documentation-standards.md) |
| GitHub issues or triage | [Issue tracker](docs/agents/issue-tracker.md), [labels](docs/agents/triage-labels.md) |

## Shared entrypoints

`CLAUDE.md` is canonical. `AGENTS.md` points here and `.agents/skills` points to
`.claude/skills`. When Windows materializes a link as a pointer file, read its
target explicitly and preserve the link when editing.
