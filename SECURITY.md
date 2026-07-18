# Security Policy

## Supported Versions

Buildmesh is in active single-maintainer development. Security fixes are
made against the latest commit on `main` only — older releases and the
currently-packaged Tauri bundle are **not** patched retroactively. If you
need a fix backported, mention it in your report.

## Reporting a Vulnerability

**Please do not file public issues for security problems.** Public issues
are visible to everyone and give attackers a free roadmap before a fix
ships.

Use GitHub's private vulnerability-reporting channel — the
"Report a vulnerability" button that GitHub auto-shows on the
**Security** tab when this file exists — so the report stays between
you and the maintainer until disclosure. The direct URL is
<https://github.com/alondero/buildmesh/security/advisories/new>.
GitHub will notify the maintainer; you do not need to know the
maintainer's email address to report.

### What to include

Help us triage quickly:

- The affected Buildmesh version (commit SHA, release tag, or installer
  filename + date) and your OS.
- A minimal reproduction or proof-of-concept — terminal commands, a
  recorded session, or a screenshot of `panic.log` / `panic_early.log`.
- Whether the issue is reachable from a sandboxed agent node, the host
  shell, or both. Buildmesh runs user-spawned agents with wide
  filesystem access, so "sandbox bypass" and "agent prompt → host shell"
  are distinct categories — say which you found.
- For dependency issues (Cargo / npm), the offending package and version
  range.

## Response Timeline

Buildmesh has a single maintainer and no formal SLA. Expect:

| Stage | Target |
|---|---|
| Acknowledgement | within 7 days of the report |
| Initial triage & severity call | within 14 days |
| Fix or documented decision | best-effort; severity-dependent |

Critical-severity findings (RCE, credential exposure, silent data loss)
take priority. Low-severity findings may be folded into a regular release.

## Disclosure Policy

We follow **coordinated disclosure**:

1. Reporter and maintainer agree on a fix timeline.
2. Maintainer prepares a fix and a release.
3. Maintainer publishes the GitHub Security Advisory (CVE requested if
   appropriate) **at or after** the fix release ships.
4. Reporter is credited in the advisory unless they ask to remain
   anonymous.

Please give a reasonable window (typically 90 days) before any public
disclosure so users can update.

## Scope

In scope:

- Sandbox or process isolation bypasses — a spawned agent reaching
  resources outside its declared scope.
- Credential exposure via logs, transcripts, the HTTP debug server, or
  the Tauri webview.
- RCE / arbitrary command execution through crafted agent output, file
  paths, or terminal escape sequences (the app embeds xterm.js — ANSI
  injection is on the table).
- Path-traversal or symlink-escape bugs in the WSL/host bridge
  (`src-tauri/src/env/`).
- Supply-chain issues in **direct** dependencies declared in `Cargo.toml`
  or `package.json`. Transitive-only issues should go to upstream first.

Out of scope:

- The behaviour of third-party AI agents you choose to spawn — they run
  on the host with your user's full filesystem access by design. Do not
  file "Claude / Codex did X" as a Buildmesh vulnerability.
- Denial-of-service against your own machine by running an infinite loop
  in a spawned agent.
- Issues only reproducible against an already-compromised host.

## Recognition

Researchers who report valid, in-scope issues are credited in the
release notes and the GitHub Security Advisory unless they prefer
anonymity.