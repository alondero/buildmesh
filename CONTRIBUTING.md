# Contributing to Buildmesh

Thanks for your interest. Buildmesh is built primarily by
[@alondero](https://github.com/alondero), but issues and pull requests from
others are welcome. This document is the contract for contributing.

## Before you open a PR

1. **File an issue first.** Describe the problem, not just the fix. Use the
   [triage labels](docs/agents/triage-labels.md) so the maintainer can route it.
2. **Read the project docs in this order:**
   - [`CONTEXT.md`](CONTEXT.md) — domain language (what a *Mesh* and *Agent
     Node* are, and how they relate).
   - [`docs/knowledge-primer.md`](docs/knowledge-primer.md) — architecture,
     conventions, and **anti-patterns**. Required reading before touching
     backend, terminal, agent-spawn, or path code.
3. **Match existing patterns.** The codebase has a strong opinionated style.
   New abstractions, dependencies, or speculative generality aren't wanted.
   PRs that match the surrounding code land faster.
4. **Add or update tests** for any behaviour change. Unit tests live in
   `tests/unit`, integration in `tests/integration`, e2e in `tests/e2e`.

## Dev loop

**Prerequisites:** Node.js 20+ (LTS), Rust stable (1.74+), the platform deps
listed in the Tauri 2 prerequisites, Git CLI, and (optionally) the `gh` CLI
for the GitHub Issues / PR features.

```bash
npm install
npm run tauri dev        # launches the Tauri shell + Vite dev server
```

**Quality gate before pushing.** All three must be green:

```bash
scripts\check.ps1 all    # Windows/worktree wrapper (dist/mobile build, vitest, cargo test)
npm run test:ci          # vitest unit + integration + Playwright e2e (needs the app on :1991)
cargo test               # Rust unit tests (run inside src-tauri/)
```

The `/verify` skill is the project-blessed verification flow — run it before
requesting review. It calls `check.ps1`, launches the dev profile, and scans
the debug log.

> **Worktree tip.** Inside a worktree (path contains `.claude/worktrees/`),
> `check.ps1` already handles the Windows-specific gotchas (clears
> `BUILDMESH_PREFILL`, forces vitest `--pool=threads`, builds `dist/mobile`
> first). Prefer it over running the raw commands — the raw forms false-green
> in a worktree.

## Commit & PR conventions

- Commit messages use **Conventional Commits** (`feat(scope): …`,
  `fix(scope): …`, `refactor(scope): …`, `docs(scope): …`, `chore(scope): …`).
  Reference the issue number in the body when one exists.
- One logical change per PR; PRs should be squash-mergeable.
- Follow [the engineering contract](docs/agents/engineering.md) and `/verify`
  for scope-appropriate checks. Report actual pass/fail/not-run results, including
  whether UI evidence used mock IPC or the real backend.
- PR titles should follow the commit convention; the current build workflow does not gate titles.

## Harness-enforced rules

Claude Code hooks catch selected edit/commit mistakes. Shell writes and other
harnesses are covered by `npm run check:agent` and CI's content checks. See the
engineering contract for their scope and limits; none proves behavior on its own.

| Rule | Source | Why it matters |
|---|---|---|
| Never call `.dispose()` on an xterm.js terminal unless the agent node is deleted | `.claude/hooks/guard-antipatterns.mjs` | Causes permanent terminal blanking — `TerminalManager` is a singleton, instances survive React remounts |
| Never pass Linux/WSL paths to Windows-side APIs | `.claude/hooks/guard-antipatterns.mjs` | Use `env::to_host_path` (`src-tauri/src/env/host_path.rs`); build `\\wsl$\` paths only inside that module |
| Never hand-declare a TS interface for a Rust wire type | CI drift-gate on `src/types/generated/` | Use `#[derive(TS)]` and import the generated type instead. Annotate 64-bit ints with `#[ts(as = "i32")]`. See *Shared Rust↔TS Types* in `docs/knowledge-primer.md` |
| Inside a worktree, only edit paths under your worktree root | `.claude/hooks/guard-antipatterns.mjs` | Otherwise you silently edit the main checkout on a different branch |
| `git commit` with nothing staged | `.claude/hooks/guard-commit-staging.mjs` | Empty/aspirational commit trap (#491→#504). Stage your files first |
| New `#[command]` Tauri commands must be registered in `lib.rs` | (runtime — fails with "command not found") | Easy to forget; the handler list is the source of truth |

If you genuinely need to override a hook, use the per-rule env-var escape
hatch (`BUILDMESH_ALLOW_WORKTREE_ESCAPE=1`, etc.). **Never** use `--no-verify`.

## Triage & issue workflow

This repo uses a canonical 5-label triage vocabulary — see
[`docs/agents/triage-labels.md`](docs/agents/triage-labels.md) for definitions.

```
needs-triage → needs-info ↔ ready-for-agent ↔ ready-for-human → wontfix
```

If you want an autonomous agent to pick up your issue, work with the
maintainer to get it to **`ready-for-agent`** — that's the contract for
AFK-friendly implementation.

## Maintainer & SLA

This repo is maintained by **[@alondero](https://github.com/alondero)** on a
best-effort basis. Review SLA is "when I can," not a fixed turnaround. Be
patient, be persistent — feel free to ping after two or three weeks of
silence on an open PR or issue.

## Security

If you find a security issue (sandbox bypass, RCE via agent output, exposed
credential, etc.), **please do not file a public issue**. See
[`SECURITY.md`](SECURITY.md) for the private vulnerability-reporting flow
(GitHub Security Advisories, with email fallback).

## Code of Conduct

By participating, you agree to the
[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md) — Contributor Covenant v2.1.
Report CoC violations through the project's [GitHub Security Advisories](../../security/advisories/new)
channel (private until disclosure) — the same path used for security
reports.

## Filing issues & PRs

Issue and PR templates live under
[`.github/ISSUE_TEMPLATE/`](.github/ISSUE_TEMPLATE) and
[`.github/PULL_REQUEST_TEMPLATE.md`](.github/PULL_REQUEST_TEMPLATE.md).
GitHub will surface them automatically when you open a new issue or PR;
following them speeds up triage.

## License

By contributing, you agree that your contributions are licensed under the
project's [MIT License](LICENSE).
