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

## Common change checklists

### Adding or changing a harness/provider

- Update the Rust adapter and its capability inventory.
- Update the frontend mirror only where the existing static contract requires
  it; keep the Rust/TS capability tests green.
- Regenerate committed wire types with `cargo test` when a wire struct changes.
- Add fresh and resume coverage, plus attention/transcript behavior where the
  harness supports it.
- Update [the user guide](../user-guide.md), the README's harness summary when
  needed, and [troubleshooting](../troubleshooting.md) for runtime caveats.
- Capture externally verified CLI behavior in `docs/learning/` or `docs/research/`
  with a source link instead of copying uncertain assumptions into the user
  guide.

### Adding a Tauri command or HTTP route

- Keep the external boundary thin and register new Tauri commands in
  `src-tauri/src/lib.rs`.
- Derive and regenerate Rust↔TypeScript wire types; never hand-edit generated
  files.
- Test malformed input, unavailable dependencies, auth/error status, and
  acknowledged success at the real boundary where practical.
- Document the user-visible behavior or the developer/API contract and record
  security or lifecycle decisions in an ADR.

### Changing a user-visible feature

- Update the task procedure, defaults, limitations, and recovery path.
- Add or update `CHANGELOG.md` for behavior changes.
- Add screenshots or browser evidence for layout, accessibility, or interaction
  changes when the engineering contract calls for it.
- Run `npm run check:docs` and the product checks appropriate to the boundary.

## Architecture documents

- [Knowledge primer](../knowledge-primer.md) is the detailed AI architecture
  reference; read only the sections relevant to the code you will touch.
- [ADR index](../adr/README.md) explains which decisions are current,
  proposed, or superseded.
- [Specs index](../specs/README.md) explains how to treat implementation PRDs.
- [Release procedure](releasing.md) is authoritative for the updater and tag
  workflow.
