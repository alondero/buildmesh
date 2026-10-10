# Development guide

This guide is the contributor-facing map for the Tauri desktop application. It
assumes a Windows checkout when it gives PowerShell commands; CI also runs
Linux and platform smoke builds.

## Start here

1. Read [CONTRIBUTING.md](../../CONTRIBUTING.md) for the contribution and PR
   contract.
2. Read [CONTEXT.md](../../CONTEXT.md) for domain vocabulary.
3. Read [the engineering contract](../agents/engineering.md) for seams,
   evidence, and scope-based checks.
4. Read [CLAUDE.md](../../CLAUDE.md) for always-on rules. `AGENTS.md` points to
   the same canonical file for other coding agents.
5. Run the smallest relevant check, then the full check before handoff.

## Repository map

| Area | Responsibility |
|---|---|
| `src/` | React UI, Zustand state, Tauri IPC wrapper, mobile SPA, and generated TS wire types |
| `android/` | Native Kotlin/Compose remote client; [build, pairing and verification](android.md) |
| `src-tauri/src/commands/` | Tauri command boundary and command-level tests |
| `src-tauri/src/agent/` | Harness adapters, detection, launch recipes, lifecycle, and provider routing |
| `src-tauri/src/db/` | SQLite schema, migrations, and persistence tests |
| `src-tauri/src/env/` | Host/WSL path and runtime translation |
| `src-tauri/src/http/` | Loopback/LAN server, auth, pairing, TLS, WebSockets, and remote routes |
| `tests/unit/` | Frontend unit/component tests |
| `tests/integration/` | Frontend integration tests with shared harnesses |
| `tests/e2e/` | Browser/runtime tests; read the Playwright config before running |
| `scripts/` | Windows check wrapper, CI drift gates, bundle budget, and test helpers |
| `docs/adr/` | Durable architectural decisions |
| `docs/specs/` | Product and technical design contracts |

For the Circuit side-effect inventory and restart/replay policy, see the
[Circuit effect recovery contract](circuit-effect-recovery.md).
For the Rust backend's seams — which module owns git access, why the stepper is
pure, where the large files still are — see the
[Rust module map](module-map.md).
For a Codex model-switch report checkpoint, see the
[run 293 investigation](../archive/2026-10/circuit-run-293-codex-context.md).
For Settings keyboard behavior, compact sidebar targets and the theme inventory,
see [desktop interaction and theme polish](../archive/2026-10/desktop-polish-2026-10.md).

## Local development

Prerequisites are Node.js 20+ with npm, Rust stable, Git, and the Tauri 2
platform dependencies. Optional WSL2 and `gh` are needed only for the features
that use them.

```powershell
npm install
npm run tauri dev
```

The dev profile is separate from the stable profile. Use the repository scripts
when possible because they build mobile assets, clear leaked test environment,
select the safe Vitest pool, and handle Windows worktree behavior.

## Verification matrix

Start with `npm run verify` for an active harness task, or pass
`-- --base <commit>` for standalone verification. The
[development harness](../agents/development-harness.md) documents selected
gates, current-tree receipts, prerequisites, completion, evaluations and
recovery. Focused commands below remain useful during iteration.

| Scope | Command | Evidence it provides |
|---|---|---|
| Documentation and agent infrastructure | `npm run test:docs` and `npm run check:docs` | Link/structure contract, source drift, and documentation-impact regression tests |
| Frontend | `scripts\check.ps1 all-ts` | Build, lint, fixtures, unit/integration tests, README/docs gates, bundle budget |
| Rust | `scripts\check.ps1 rust` | Rust tests and generated TypeScript binding refresh |
| Frontend + Rust | `scripts\check.ps1 all` | The default Windows green bar; see the engineering contract for exclusions |
| Browser smoke | `npx playwright test --project=verify-smoke` | Real browser events with mock IPC; not backend proof |
| Visible UI/runtime | Read `.claude/skills/verify-ui/SKILL.md` | Functional assertions and inspected screenshots against the dev profile |

`npm run lint` intentionally excludes Markdown. Documentation has its own
`check:docs` gate so link and information-architecture failures are visible
without pretending that a code linter can judge prose quality. CI passes
`--base <commit>` to the same script to require documentation or a reasoned
`docs: none — <reason>` exemption for behavior-sensitive diffs.

A change is not mergeable until the required status checks on `main` have run, so
CI gates every merge. Their names, what each one covers, the release gate, and
the emergency bypass are owned by
[the release procedure](releasing.md#required-checks-and-branch-protection),
which stays in step with `.github/workflows/verify.yml`; they are deliberately
not restated here, because a copied list of check names is a claim nothing
verifies. The Windows and macOS platform smoke builds are post-merge signals:
they run on pushes to `main`, release tags, and manual dispatches, not on pull
requests.

## Common change checklists

The file-by-file order for a cross-cutting change is a skill. This page links
to that skill instead of keeping a second copy. `npm run check:docs` fails
when a path named in a skill does not exist.

- [Add or change a harness adapter](../../.claude/skills/add-harness-adapter/SKILL.md)
- [Add a Tauri command or HTTP route](../../.claude/skills/add-tauri-command/SKILL.md)
- [Change the database schema](../../.claude/skills/db-migration/SKILL.md)
- [Add or change a setting](../../.claude/skills/add-setting/SKILL.md)

### Changing a user-visible feature

- Update the task procedure, defaults, limitations, and recovery path.
- Write a Conventional Commit message clear enough to seed the release note
  drafted at release time (`npm run release:notes`); do not edit `docs/releases/`
  in the pull request.
- Add screenshots or browser evidence for layout, accessibility, or interaction
  changes when the engineering contract calls for it.
- Run `npm run check:docs` and the product checks appropriate to the boundary.

## Architecture documents

The [October desktop UX audit](../archive/2026-10/desktop-ux-audit-2026-10.md) records ranked
findings, native evidence and the linked follow-up checklist.
The [October performance review](../archive/2026-10/performance-audit-2026-10.md) records request
and watcher fixes, memory/scaling findings, and the linked measurement backlog.
Both are historical records in the [archive](../archive/README.md), not current contracts.

Every document in *this* directory is a current contract and declares a `Status:`
line that `npm run check:docs` enforces. Durable architecture lives in one focused
owner document per subsystem, reachable from the
[knowledge primer index](../knowledge-primer.md):

| Owner document | Area |
|---|---|
| [providers.md](providers.md) | Model providers, usage meters, credentials, spawn recipes |
| [terminals.md](terminals.md) | Terminals, xterm lifecycle, PTY input and output streaming |
| [agent-nodes.md](agent-nodes.md) | Agent Node lifecycle, process registry, worktrees |
| [attention.md](attention.md) | Attention system, hooks, turn counting, node naming |
| [probe-ui.md](probe-ui.md) | Probe panel, view modes, context lenses |
| [git-query-cache.md](git-query-cache.md) | Git query cache: freshness and retention contract |
| [windows.md](windows.md) | Windows, WSL paths, frameless window, shortcuts |
| [startup-and-profiles.md](startup-and-profiles.md) | Startup, profiles, crash recovery, environment detection |
| [circuits.md](circuits.md) | Autopilot circuits |
| [remote-access.md](remote-access.md) | Coordinator API, LAN/VPN exposure |
| [state-recovery.md](state-recovery.md) | Snapshot, export, integrity check, restore |
| [rust-conventions.md](rust-conventions.md) | DB, threading, caches, pattern guards, wire types |
| [module-map.md](module-map.md) | Rust backend seams and which module owns which capability |
| [supply-chain.md](supply-chain.md) | Dependency and workflow supply-chain controls: action pinning, advisory policy |
| [mobile.md](mobile.md) | Mobile client |

Add new architecture to the owning document rather than growing the primer; the
primer is an index and has an enforced read-cost budget.

- [Knowledge primer](../knowledge-primer.md) is the **index** to that
  architecture, not the architecture itself; read the row for your area first.
- [ADR index](../adr/README.md) explains which decisions are current,
  proposed, or superseded.
- [Specs index](../specs/README.md) explains how to treat implementation PRDs.
- [Release procedure](releasing.md) is authoritative for the updater and tag
  workflow, and owns the required-check names.
