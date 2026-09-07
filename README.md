# Buildmesh

![Buildmesh Wordmark](./src/assets/wordmark.png)

Buildmesh is a Tauri desktop app for orchestrating multiple AI coding agents — **Claude Code, Codex, Antigravity, OpenCode, Cursor, Grok Code, Kimi Code, MiniMax Code, DeepSeek Harness, Command Code, Freebuff**, and a plain **Terminal** harness — across multiple meshes at the same time. It runs each agent as a durable process in a persistent xterm.js terminal, isolates work via Git worktrees, and exposes a tiled grid view so you can watch all of them at once.

If you use Claude Code / Antigravity / OpenCode from your shell and find yourself `tmux`-ing, copy-pasting between tabs, or losing context when a long-running agent restarts — Buildmesh is what that should look like.

## Install Buildmesh

Buildmesh ships as a Windows installer through GitHub Releases and is upgraded in place by the in-app updater. Each release publishes an updater feed, and every update is verified with the app's own minisign signature before it installs. OS code-signing is **not** in place yet, so first launch shows a SmartScreen / Gatekeeper warning — see [SmartScreen & Gatekeeper warnings](#smartscreen--gatekeeper-warnings-unsigned-installers).

### Download

Grab the latest installer from the [GitHub Releases page](https://github.com/alondero/buildmesh/releases/latest). Each Windows release publishes **two** installers (pick whichever you prefer):

| File | Format | Notes |
|---|---|---|
| `Buildmesh_<version>_x64-setup.exe` | NSIS `.exe` | Smallest download; per-user install. |
| `Buildmesh_<version>_x64_en-US.msi` | Windows Installer `.msi` | MSI deployment; per-machine install. |

Each installer ships with its matching `.sig` (the **auto-updater** signature — see [Upgrade](#upgrade)) and a `latest.json` updater feed; existing installs read that feed to detect new releases.

### Supported platforms

| Platform | Status | How to get it |
|---|---|---|
| **Windows 10/11** (x64) | **Stable** — installer shipped on every tag. | Download from the Releases page above. |
| **WSL2** (hybrid Windows/WSL mode) | **Stable** on Windows 10/11 hosts — Linux agents run inside WSL2, the Windows-side backend drives the UI. | Install Windows first, then enable WSL2 from Mesh settings. |
| macOS (Apple Silicon + Intel) | **Preview** — the macOS bundles compile on every weekly CI smoke run, but no signed `.dmg` is published yet. | [Build from source](#build-from-source). |
| Linux (Ubuntu) | **Preview** — same caveat as macOS. | [Build from source](#build-from-source). |

### First-run prerequisites

Buildmesh itself has no host dependencies — it bundles WebView2 on Win 11 and ships the React UI as a Tauri webview. The **agents** you spawn are a separate question:

- **`Terminal` harness** — works out of the box (PowerShell on Windows; routed through `wsl.exe` on WSL meshes).
- **Every other harness** — install the agent CLI on the host (or inside WSL for a WSL mesh), then sign in or add an API key from **App Settings → Providers**. Buildmesh detects the CLI binary on `PATH` and surfaces enabled harnesses in the Spawn Menu.
- **(Optional) `gh` CLI** — only required for the GitHub Issues / PR features.
- **(Optional) WSL2** — only for hybrid Windows/WSL meshes.

### SmartScreen & Gatekeeper warnings (unsigned installers)

Buildmesh installers are **not** OS-code-signed yet. This is **expected** for now and is unrelated to the in-app updater — every update is verified with the app's own minisign signature regardless. To get past the OS trust prompt:

- **Windows (SmartScreen)** — *"Windows protected your PC … unknown publisher."* Click **More info → Run anyway**.
- **macOS (Gatekeeper)** — *"cannot be opened because the developer cannot be verified."* Right-click the app → **Open** → **Open** again (or System Settings → Privacy & Security → **Open Anyway**).

Verify the installer against its `.sig` with [minisign](https://jedisct1.github.io/minisign/) before bypassing the prompt on a machine you don't fully control. The full verification command lives in [`docs/development/releasing.md`](docs/development/releasing.md).

### Data location, logs, backup, uninstall

Buildmesh stores everything under a single per-profile directory. The stable and dev profiles are independent and use different bundle identifiers (`com.alond.buildmesh` vs `com.alond.buildmesh.dev`), so they can be installed side-by-side without colliding.

- **Windows stable** — `%APPDATA%\com.alond.buildmesh\`
- **Windows dev profile** — `%APPDATA%\com.alond.buildmesh.dev\`

Inside:

| Path | Contents |
|---|---|
| `buildmesh.db` | SQLite database (meshes, Agent Nodes, settings). |
| `logs\buildmesh.log` | Rotating Rust + frontend log, size-bounded and name-stable. The `/use`, `/verify`, and `scripts\tail-dev-log.ps1` helpers tail this exact file. |
| `logs\panic.log` | External crash-watchdog dump (Windows only). |
| `autopilot\finish.md` | Per-mesh autopilot wrap-up template. |

OAuth secrets for each provider are stored in the **Windows Credential Manager** (catch-all `CRED_TYPE_GENERIC` entries, *not* in this directory).

To **uninstall** Buildmesh and **remove all user data**: uninstall from *Windows Settings → Apps → Installed apps → Buildmesh → Uninstall*, **then** delete the profile directory above. The MSI/NSIS uninstaller removes the binary and registry entries but does **not** delete the data dir, so the two-step matters if you want a clean slate.

There is **no automatic cloud backup** — your meshes, prompts, and Provider configuration live only in the local directory above. Back it up the way you would any other project folder.

### Upgrade

Existing installs check the updater feed at `https://github.com/alondero/buildmesh/releases/latest/download/latest.json` on launch and surface an *"Update available"* prompt. Install the update from inside the app — no separate download needed. Locally-built dev builds disable the updater via their `.dev` bundle identifier, so they never nag you about a release you already have.

To upgrade manually, install the new `.msi` or `-setup.exe` over the existing install — both installers support in-place upgrade.

### Get help

- **Bug or unexpected behaviour?** Open an issue using the [bug template](.github/ISSUE_TEMPLATE/bug.md). Include the contents of `logs\buildmesh.log` for the relevant session and your version (from *Help → About*, or the installer filename).
- **Feature request?** Use the [feature template](.github/ISSUE_TEMPLATE/feature.md).
- **Question / "how do I…"** — open a [GitHub Discussion](https://github.com/alondero/buildmesh/discussions) (or an issue with no template).
- **Security vulnerability** — **do not** file a public issue; report privately through [GitHub Security Advisories](https://github.com/alondero/buildmesh/security/advisories/new). See [`SECURITY.md`](SECURITY.md) for the supported-versions policy and response timeline.

## Features

### Multi-agent orchestration
- **Twelve harnesses, one workflow**: Claude Code, Codex, Antigravity, OpenCode, Grok Code, Cursor, Kimi Code, MiniMax Code, DeepSeek Harness, Command Code, Freebuff, and a plain `Terminal` harness — switch harnesses per Agent Node. Some harnesses pair with a *Model Provider* (Anthropic, MiniMax, Kimi) so each Agent Node can carry live quota / balance widgets; custom Claude-compatible endpoints attach as proxied providers.
- **Multi-mesh workspaces**: open several meshes side by side. Each mesh is its own grid of agent terminals.
- **Tiled grid view**: split each mesh into a 1–6 pane grid. Layouts are saved per mesh.
- **Persistent terminals**: agents run as durable background processes and their PTY state survives mesh and pane switches. Quitting the app prompts you to confirm when there are non-resumable sessions; on relaunch anything still running is restored automatically and anything suspended shows up in the Resume menu. The plain `Terminal` harness and any harness that hasn't yet captured a session id are **non-resumable** — exiting loses their progress.
- **Git worktree isolation**: every agent node gets its own worktree branch, so concurrent agents on the same repo never collide.

### Productivity
- **Attention hook**: agents raise a notification when they need your input. Buildmesh listens for `idle_prompt` events on the Claude Code side and surfaces a "needs input" badge in the sidebar — no polling, no timers.
- **Auto-named nodes**: agent nodes are auto-named from their first turn (LLM-generated slug) so a 10-agent swarm stays readable.
- **Session resume**: surviving processes and stored `cli_session_id`s mean a restart resumes where you left off. Crash recovery marks `Running` nodes as `Suspended` on startup.
- **Build & run**: per-mesh build/run commands with auto-detection for Rust, Node, Tauri, JVM, Go, and Python projects.

### Visibility & review
- **File explorer with inline diff**: per-mesh and per-node file trees with line-addition/deletion counts and a side-by-side diff viewer.
- **Changed Files panel**: aggregated view of modified files across the whole mesh.
- **Mesh-level PR creation**: uncommitted-changes indicator + `gh pr create` from Mesh Properties.
- **GitHub Issues integration**: triage, view, and spawn an agent on an issue without leaving the app.
- **Accounts & Usage panel**: per-provider quota tracking for the providers that expose a usage endpoint.

### Power features
- **Remote access**: expose the mesh over the local network via a root token. WebSocket PTY relay streams any terminal to a phone or another machine. Port fallback (1992→1993→1994) means a port conflict never blocks a session.
- **AI context portability**: share `CLAUDE.md`, `.claude/skills`, and friends with Codex, OpenCode, and Antigravity via `AGENTS.md` + `.agents/skills` git symlinks — no per-provider duplication.
- **Dev / Stable side-by-side profiles**: run an in-development build (`buildmesh-dev`) without interrupting the stable hub you orchestrate agents from.

## Agent sandboxing (security)

A prompt-injected or runaway agent runs real shell commands on your machine. Each
**Mesh** has an opt-in **Sandbox** toggle (off by default) that confines every
agent node spawned from it to its own Git worktree, using the host OS's native
confinement — **Seatbelt** on macOS, a **restricted token** on Windows. The toggle
is read at spawn time, so flipping it never disturbs already-running agents, and on
a host with no backend it's a safe no-op.

### What it protects against

| Capability | macOS (Seatbelt) | Windows (restricted token) |
|---|---|---|
| Agent can spawn child processes (`bash`, `git`, `ripgrep`, hooks) | ✅ | ✅ |
| Agent reaches the network (Anthropic API, `git push`) | ✅ | ✅ |
| Agent reaches the hub on loopback (attention hook → `127.0.0.1`) | ✅ | ✅ |
| **Writes** confined to the worktree (rest of disk read-only/denied) | ✅ | ⏳ not yet |
| **Reads** of home credentials (`~/.ssh`, `~/.aws`, registry) denied | ✅ | ⏳ not yet |

The Windows backend was pivoted off a per-node AppContainer: the AppContainer's private object namespace hung `claude.exe` at libuv's named-pipe creation and blocked loopback. The restricted token fixes both. Deny-by-default **read/write confinement** on Windows is deferred — a same-user restricted token can't deny home reads while MSYS `bash` runs (both are secured by the same user SID), so the surviving path is a separate low-privilege user principal (or WSL). Until then the Windows sandbox fixes the hang and loopback but does **not** yet restrict file access.

### What it is *not*

- **Not a container or VM.** It's an OS access-control boundary on a single process tree, not virtualization or namespacing.
- **Not network egress control.** A sandboxed agent still reaches the internet (it has to, for the model API) — the sandbox limits *filesystem and host* access, not where data can be sent.
- **Not a guarantee the agent binary is trustworthy.** It confines what the agent process can touch; it doesn't vet the agent or its dependencies.

## Keyboard shortcuts

- `Ctrl + T` / `Cmd + T` — new agent node
- `Ctrl + Alt + ←/→/↑/↓` / `Cmd + Option + ←/→/↑/↓` — traverse the on-screen node grid (wrap within row; Up/Down no-op when there's only one row; from a maximized solo view, the first press restores the grid). Uses two modifiers so the readline `backward-word` / `forward-word` gesture in any focused agent terminal (bash, zsh, fish, PSReadLine, REPLs) still works.
- `Ctrl + 0` / `Cmd + 0` — reset terminal font size
- `Ctrl + +` / `Cmd + +` — increase terminal font size
- `Ctrl + -` / `Cmd + -` — decrease terminal font size
- `Alt + G` / `Cmd + G` — toggle grid ↔ single view (maximize the active node or restore the split; no-op when there is no active node)

The `Ctrl` modifier shown above is `Cmd` on macOS — Tauri normalises
`CommandOrControl+…` registrations and JSX labels use `isMac ? '⌘' : 'Ctrl'`,
so every binding works the same way on Windows, Linux, and macOS.

## Tech stack

- **Frontend** — React 19, Zustand 5, xterm.js 6, Tailwind 4, TypeScript ~5.8, Vite 7
- **Backend** — Tauri 2, Rust, `portable-pty`, `rusqlite` (SQLite), `git2`
- **Persistence** — local SQLite (`app_settings`, `meshes`, `agent_nodes`)
- **Runtime** — Windows 10/11, with optional WSL2 support
- **Testing** — Vitest (unit + integration) + Playwright (e2e)

## Build from source

This section is for contributors and for users on macOS/Linux (which aren't published as releases yet — see [Supported platforms](#supported-platforms)).

### Prerequisites

- **Node.js 20+ (LTS)** and **npm**
- **Rust stable** (1.74+, the minimum Tauri 2 supports)
- **Platform deps** — see [Tauri 2 prerequisites](https://tauri.app/start/prerequisites/) for your OS
  - Windows: WebView2 runtime (preinstalled on Win 11), Microsoft C++ Build Tools
  - macOS: Xcode Command Line Tools
  - Linux: `webkit2gtk`, `libssl-dev`, `libgtk-3-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`
- **Git CLI** — used for worktree ops, branch handling, and the optional GitHub Issues / PR features
- **(Optional) WSL2** — required only if you want the hybrid Windows/WSL runtime
- **(Optional) `gh` CLI** — only if you want the GitHub Issues and PR features authenticated

### Install

```bash
npm install
```

### Develop

```bash
npm run tauri dev
```

This launches the Tauri shell + Vite dev server. First run takes a few minutes while the Rust backend compiles.

### Build a release binary

```bash
# Stable profile (com.alond.buildmesh, port 1991)
npm run tauri build

# Dev profile (com.alond.buildmesh.dev, port 2991) — runs side-by-side with stable
npm run tauri:build:dev
```

Binaries land in `src-tauri/target/release/`:

| Profile | Binary | Port |
|---|---|---|
| Stable | `buildmesh.exe` | 1991 (HTTP fallback 1992→1993→1994) |
| Dev | `buildmesh-dev.exe` | 2991 (HTTP fallback 2992→2993→2994) |

The dev profile is what `/use` and `/verify` use — it never touches the stable hub.

## Test

```bash
npm test                 # unit + integration
npm run test:e2e         # Playwright e2e (requires the app running on :1991)
npm run test:ci          # all three in one go
cargo test               # Rust unit tests (run inside src-tauri/)
```

## Bundle size budget

The desktop entry chunk (`dist/assets/index-*.js`) is the single largest
byte-cost on first paint. After the lazy-xterm + lazy-Probe-tab
split, it sits at roughly 894 kB minified / 274 kB gzip — with xterm and
its five addons (≈430 kB / ≈100 kB gzip) only fetched the first time a
terminal pane is opened, and each Probe tab (≈2–24 kB / ≈1–7 kB gzip)
only fetched when the user actually opens that destination.

The build is gated by [`scripts/check-bundle-size.mjs`](scripts/check-bundle-size.mjs)
against the **budget limits** in [`scripts/bundle-budget.json`](scripts/bundle-budget.json):

| Asset | Budget limit (raw) | Budget limit (gzip) |
|---|---|---|
| `dist/assets/index-*.js` | 1.05 MB | 360 KB |
| `dist/assets/index-*.css` | 117 KB | 20 KB |

The numbers above are **upper bounds**, not the current size — the
table deliberately leaves headroom (~20% raw, ~30% gzip) so a typical
regression surfaces as a visible budget breach instead of a cliff-edge
green/red flip. Run `npm run check:bundle` after `npm run build` to see
the current numbers and the headroom remaining.

```bash
npm run build           # produces dist/
npm run check:bundle    # reads dist/ and compares against scripts/bundle-budget.json
```

`scripts/check.ps1 all-ts` (and `all`) runs the budget check as part of
the green bar — `unit` / `integration` / `rust` skip it because they
don't produce a build. The quality job on CI runs it after
`npm run build` so a regression on the entry chunk fails the build.
Bump the budget values intentionally — every visible CI failure costs a
hot path; don't paper over a real growth by raising the ceiling.

## Project structure

```
buildmesh/
├── src/                       # React frontend
│   ├── components/            # Terminal, Sidebar, FileTree, Probe, …
│   ├── stores/                # Zustand stores
│   └── lib/                   # shared utilities
├── src-tauri/
│   ├── src/
│   │   ├── commands/          # Tauri command handlers (#[command])
│   │   ├── agent/             # spawn logic + provider adapters
│   │   │   └── provider/adapters/   # one file per provider (anthropic, minimax, kimi, …)
│   │   ├── db/                # SQLite schema + migrations
│   │   ├── env/               # Windows/WSL path translation
│   │   ├── http/              # attention webhook + remote-access routes
│   │   └── session_naming.rs  # PTY-side session-id capture + LLM rename
│   ├── tauri.conf.json        # stable profile config
│   └── tauri.dev.conf.json    # dev-profile overlay (identifier *.dev, port +1000)
├── tests/
│   ├── unit/                  # Vitest unit tests
│   ├── integration/           # Vitest integration tests
│   └── e2e/                   # Playwright e2e tests
├── docs/
│   ├── knowledge-primer.md    # architecture, conventions, anti-patterns (read first)
│   ├── adr/                   # Architecture Decision Records
│   └── specs/                 # feature specs
├── CONTEXT.md                 # domain language (read alongside knowledge-primer.md)
├── AGENTS.md                  # portable AI-context for non-Claude agents
└── CLAUDE.md                  # AI-agent rules (token-efficient, do not bloat)
```

## Documentation

- **[`docs/knowledge-primer.md`](docs/knowledge-primer.md)** — architecture, conventions, anti-patterns. Read this before touching backend, terminal, agent-spawn, or path code.
- **[`CONTEXT.md`](CONTEXT.md)** — domain language and mental model (what a "Mesh" is, what an "Agent Node" is, how they relate).
- **[`docs/adr/`](docs/adr/)** — Architecture Decision Records.
- **[`docs/specs/`](docs/specs/)** — feature specs and PRDs.
- **[`CLAUDE.md`](CLAUDE.md)** — AI-agent rules (token-efficient; not a human onboarding doc).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the dev loop, commit conventions,
harness-enforced rules, and the triage workflow.

## License

[MIT](LICENSE) — Copyright (c) 2026 Adam Londero.
