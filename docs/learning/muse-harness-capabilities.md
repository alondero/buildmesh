---
name: muse-harness-capabilities
description: Meta Muse capability review against Buildmesh's harness contract — the persistent workspace-trust store and how the Muse adapter provisions it
metadata:
  type: reference
  harness: muse
  muse_version: 1.3.0 (1.3.0-R3401.1), native Windows + WSL
  date: 2026-09-22
  workspace_trust: implemented 2026-09-22 — pre-provisioned in ~/.config/muse/trust.json (issue #1706); verified live with `muse skills list`
---

# Meta Muse harness capabilities vs Buildmesh

Review of what the `muse` binary exposes versus what Buildmesh's Muse adapter
advertises and uses. This page covers the harness contract; the
**workspace-trust** half landed with issue #1706 and is verified against the
installed 1.3.0. Attention is still served by the passive session-log watcher
(issue #1709) — see [Attention](#attention-still-watched-not-hooked).

## Sources (primary only)

| Source | What it is |
|---|---|
| Installed `Muse Code 1.3.0 (1.3.0-R3401.1)` — `%LOCALAPPDATA%\Programs\muse\muse.exe`, plus the WSL/Linux install | The shipped CLI: `--help`, `muse skills list`, `muse plugins install`, and the on-disk config/data stores |
| `%USERPROFILE%\.config\muse\trust.json` and the guest `~/.config/muse/trust.json` | The real trust stores, as Muse itself wrote them |
| `src-tauri/src/agent/provider/adapters/muse.rs` | Current Buildmesh adapter (`ensure_workspace_trusted`, `trust_key`, `resolve_config_dir`) |
| `src-tauri/src/services/muse_sessions.rs` | Session-index/on-disk discovery (`data_root`) |
| `src-tauri/src/agent/capabilities.rs`, `autopilot/compatibility.rs` | Harness contract and Autopilot gate |

## What the Muse adapter advertises

| Flag | Adapter value | Notes |
|---|---|---|
| `supports_resume` | `true` | `resume_args` → `resume <session-uuid>` |
| `auto_resume_on_startup` | `true` | |
| `self_assigns_session_id` | `true` | Muse mints its own ids; captured post-spawn from `session-index.db` / the on-disk session tree, never from the PTY |
| `captures_session_id_from_pty` | `false` | No verified banner |
| `supports_prefill` | `true` | Trailing positional prompt; `prefill_requires_pty` is `true` (resume documents no positional prompt) |
| `supports_model_override` | `true` | `--model <id>` |
| `effort_control` | `None` | No effort flag is baked |
| `supports_extra_args` | `true` | |
| `requires_attention_hook` | `false` | No native hook wired yet (#1709) |
| `attention_capability` | `None` | Same |
| `supports_passive_turn_watcher` | `true` | `services::muse_watcher` tails the durable `session.jsonl` run boundaries |
| `produces_readable_transcript` | `true` | `TranscriptFormat::Muse` (#1708) |
| `available_on` | Linux, macOS, Windows | Native on Windows since Muse 1.3.0 |
| Base recipe | `--disable-approval --disable-sandbox` | Approval policy #1705 (`--yolo` explicitly rejected); sandbox off so the agent shell reaches the OS credential store (#1788) |
| Shell | `WindowsShell::Direct` | `muse` / `muse.exe` is a real PE/ELF/Mach-O binary on every platform |

## Workspace trust (#1706)

Muse gates a workspace's **skills, rules and project-scoped plugins** behind an
explicit trust decision. Buildmesh spawns a PTY it cannot answer prompts in, so
before #1706 every Muse node ran untrusted: the workspace's `CLAUDE.md`/`AGENTS.md`
rules, `.claude/skills`, `.agents/skills`, and any `muse init` scaffolding were
silently skipped.

### The store is a persistent file, not a flag

`--trust-workspace` ("trust this workspace for this run … does not save trust")
and `--yolo` are **per-invocation only**. The durable store is

```
${XDG_CONFIG_HOME:-~/.config}/muse/trust.json
{
  "schema_version": 1,
  "projects": { "<canonical workspace root>": { "decision": "trusted" } }
}
```

Muse uses the XDG config root on **every** platform, including the native
Windows binary (`%USERPROFILE%\.config\muse\trust.json` — verified present on
the installed 1.3.0). Data lives separately under `~/.local/share/muse`.

### Keys are canonicalized workspace roots

Muse files a workspace under the canonicalized path, so the key's *shape*
depends on which binary is running:

| Runtime | Example key | How Buildmesh derives it |
|---|---|---|
| Native (Windows / macOS / Linux) | `\\?\F:\src\buildmesh\.claude\worktrees\x` / `/home/u/proj` | `std::fs::canonicalize` on the workspace |
| WSL guest | `/mnt/f/src/buildmesh/.claude/worktrees/x` | the guest path verbatim (a Windows host cannot canonicalize it) |
| Windows binary reached by interop | `\\?\C:\work\proj` | verbatim form: separators normalized, `\\?\` prefixed |

Canonicalizing is attempted only when the path kind matches the host
(`is_windows_path(path) == cfg!(windows)`) — a Windows host resolving a guest
`/mnt/…` would otherwise land on its own current drive's `\mnt\…` and trust the
wrong directory.

### Reproduction (installed 1.3.0, native Windows)

A scratch workspace holding `.claude/skills/probe-skill/SKILL.md`:

| Probe | Observed result |
|---|---|
| `muse skills list --workspace <dir> --source project --json` | `skills: []`, with `diagnostics: [{ code: "project-skills-untrusted", message: "project skills skipped because workspace is untrusted", scope: "project", path: ".claude/skills" }]` |
| Same command with `--trust-workspace` | `probe-skill` listed, no diagnostics, `scope: "project"` |
| Same command with **no** flag, after Buildmesh's `ensure_workspace_trusted` wrote the entry | `probe-skill` listed, no diagnostics |

The third row is the #1706 fix: a *persisted* entry — not a launch flag —
unblocks project context. It also proves the entry's key matches what Muse
looks up, and that the JSON shape Buildmesh writes is accepted by Muse (not
just re-read by Buildmesh).

`muse plugins install <path> --scope user|project` accepts **no** trust flag
(its help is `install <path> [--scope user|project] [--json]`), so a persisted
entry is also the only lever that can unblock a project-scoped plugin install —
which is why issue #1709's attention bundle needs this landed first, and why
Buildmesh pre-provisions rather than baking `--trust-workspace` into
`spawn_recipe`.

### What Buildmesh writes

`MuseAdapter::ensure_workspace_trusted` runs on the spawn path just before
attention-hook provisioning (`agent/spawn/provision.rs`), against the config
root of the runtime that will execute the child. The merge is:

- **additive** — sibling project entries and unknown top-level keys round-trip;
  an existing entry keeps any fields Muse carried beside `decision`;
- **idempotent** — an entry already reading `{"decision":"trusted"}` writes
  nothing (no mtime bump, no re-serialization);
- **fail-closed** — a malformed file, a non-object `projects`, or a non-object
  entry for the workspace is refused with `Err` and left byte-identical, so the
  spawn path surfaces `workspace trust unavailable` instead of clobbering user
  data (the mcode / Cursor provisioning precedent);
- **locked** — one process-wide `Mutex` guards the read/merge/write, because the
  store is machine-global and two concurrent spawns would otherwise lose each
  other's entry (atomic write only prevents torn reads).

A write re-serializes the whole document through serde_json's sorted map, so
top-level key order can differ from Muse's own field order; values are
untouched, and Muse rewrites the file in its own order on its next write.

### Known limits

- The WSL branch resolves the **default** distro's home (`cli_dir_for_spawn`
  uses `wsl_home()`), so a node pinned to a non-default distro would write to
  the wrong guest home — issue #1697 (trust paths must be WSL-aware).
- Trust is the *only* launch-prerequisite step Muse takes; approval policy
  (#1705) and the OS sandbox (#1788) are separate, and `--yolo` is never baked.
- Only the PTY spawn path provisions trust. The MSP `muse serve` plane (#1681)
  has no in-repo launcher — only its telemetry slice landed under
  `agent::provider::muse` — so there is no second call site to keep in sync.

## Attention: still watched, not hooked

Muse 1.3.0 does ship a plugin system with a real hook surface (`Stop`,
`PreToolUse`, …), but the attention hook is **not wired**: `requires_attention_hook`
stays `false` and the turn signal comes from the passive watcher over the
durable session log (`~/.local/share/muse/sessions/…/session.jsonl`). Muse is
additionally the only Buildmesh harness that ships its own always-on OS sandbox.

This section is deliberately thin: the hook wiring is issue #1709's deliverable,
and the 1.1.1-era claim that "Muse exposes no interactive hook surface" is stale
against 1.3.0. See `docs/research/muse-attention-signals.md` for the original
(a)–(d) research.
