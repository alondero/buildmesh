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

### Fuzzing the parsers

The embedded server's request-read path (request head, `Content-Length`, body
read) has a checked-in fuzz target you can run locally — `cargo test --lib
http::fuzz` in `src-tauri/`, seeds in `src-tauri/fuzz/corpus/http_request/`,
campaign knobs and invariants in
[`docs/development/remote-access.md`](docs/development/remote-access.md#fuzzing-the-request-read-path).
A crash, hang, or invariant violation it reports is a bug worth filing; what it
finds is in scope, and fixing it is a separate issue from the harness.

## Where credentials are stored

On Windows, provider API keys (MiniMax, Kimi, OpenRouter, custom endpoints)
and the OpenCode and Antigravity sign-in tokens are held in **Windows
Credential Manager**, scoped to your Windows user. They are not written to
`preferences.json` or its `.bak` backup. A `preferences.json` written by an
older version is cleaned the first time the new version starts: its keys move
into Credential Manager and are blanked in the file and in the backup.

If Credential Manager cannot be reached (for example a session with no
interactive logon), Buildmesh keeps the key in `preferences.json` rather than
lose it, and moves it out on the next start or settings change once the store
is available. On macOS and Linux there is no credential store integration yet,
so keys stay in `preferences.json`, which is created readable by your user only.

The stable and dev builds keep separate entries, so running one never changes
the other's keys. You can inspect or delete the entries with
`cmdkey /list:buildmesh:*` or the Credential Manager control panel.

## Data export and backups

**Settings → Data & Diagnostics → Export a copy…** writes a single
`.bmsnap` file. With the default *Leave credentials out of the export*
setting ticked, it contains no provider API keys, no remote-access root
token, no coordinator tokens, no paired-device sessions, and **no LAN
HTTPS certificate or private key** — a copy of that key would let its
holder impersonate the HTTPS identity your paired devices already trust.
Terminal transcripts are never part of stored state, so they are never
included. The OpenCode and Antigravity sign-in tokens in Windows Credential
Manager are stored outside the data folder and are not exportable. Provider
API keys are also kept in Credential Manager, but an export with redaction
switched off puts them back into its copy of the settings so that it can
carry them to another machine.

A credential-free export is the one to attach to a bug report. If you turn
redaction off, the file contains your API keys and tokens — treat it as a
secret.

Snapshots Buildmesh takes automatically are full fidelity (they include
credentials) so that an upgrade or a restore is genuinely reversible, and
are written with owner-only permissions. The LAN CA private key is never
written to a snapshot or an export under any setting.

Note that a manual copy of the whole app-data directory is **not** a
backup that is safe to share: it includes `tls\ca.key.der` in the clear.
Use the in-app export instead.

## Recognition

Researchers who report valid, in-scope issues are credited in the
release notes and the GitHub Security Advisory unless they prefer
anonymity.