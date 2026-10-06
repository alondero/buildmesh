---
name: muse-harness-capabilities
description: Meta Muse capability review against Buildmesh's harness contract — the persistent workspace-trust store and how the Muse adapter provisions it
metadata:
  type: reference
  harness: muse
  muse_version: 1.3.0 (1.3.0-R3401.1), native Windows + WSL (macOS unverified)
  date: 2026-09-22
  workspace_trust: implemented 2026-09-22 — pre-provisioned in Muse's config root (issue #1706); verified live with `muse skills list`
  launch_flags: verdicts recorded 2026-10-06 against 1.3.0 (issue #1710); no flag modeled
---

# Meta Muse harness capabilities vs Buildmesh

Review of what the `muse` binary exposes versus what Buildmesh's Muse adapter
advertises and uses. This page covers the harness contract; the
**workspace-trust** half landed with issue #1706 and is verified against the
installed 1.3.0 on **native Windows** and **WSL**. macOS is derived from the
same `canonicalize` / XDG rules but is **not yet verified** — see
[Verification scope](#verification-scope). Attention is still served by the
passive session-log watcher (issue #1709) — see
[Attention](#attention-still-watched-not-hooked).

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

## Launch flags: explicit verdicts (#1710)

`supports_extra_args = true`, so anything Muse accepts can reach a spawn
through `extra_args` — unvalidated, which is right for an escape hatch but a
poor place for a *default*. Every Muse launch flag Buildmesh does not bake
therefore carries an explicit verdict, so a mesh stops cargo-culting argv from
the internet into `extra_args` and re-creating the failure modes #1148 removed
(unsupported flags, harness-owned worktrees, swallowed positionals).

The vocabulary: **modeled** (Buildmesh emits it from configuration),
**passthrough-only** (reachable via `extra_args`, never baked), **never-pass**
(Buildmesh must not emit it — the escape hatch is documented, not filtered, per
#1358), **deferred**, or **cross-link** (a sibling issue owns the verdict and
wins on overlap).

Verified against the installed `Muse Code 1.3.0 (1.3.0-R3401.1)` — `muse --help`
and `muse exec --help`, re-run 2026-10-06 for this issue. The issue's opening
table was captured on **1.1.1**; 1.3.0 keeps every flag it lists and adds more
than it missed (`--agents`, `--no-session-log`, and the
`--sandbox-network` / `--disable-write` / `--disable-shell` /
`--enable-shell-tool` safety group), so the re-check was not a formality.

### Never-pass

| Flag | Why Buildmesh must not emit it |
|---|---|
| `-w, --worktree [off\|create\|existing]`, `--worktree-base`, `--worktree-existing` | **ADR-0003.** Buildmesh's worktree provisioner (`git/worktree/provision.rs`) has already created the node's checkout before the process starts; a harness-created one would leave the node running somewhere Buildmesh never provisioned, desyncing the node↔path mapping, the trust key (#1706) and the watcher's log path. Resume is deliberately a no-op on the worktree, so there is no resume-time reason to re-pass it either. Same verdict as Grok's `-w`. |
| `--workspace <PATH>` | A node's world is its spawn cwd, which is also what the trust store keys on (#1706). A second policy-gated tools root splits the agent from its own node directory, and nothing in Buildmesh would know which root won. |
| `--provider <echo\|meta>` | `echo` is a deterministic test double — its companion `--echo-delay-ms` exists only to make it deterministic. A node launched with it answers canned text, never touches the repo, and silently ignores the model override (the help scopes `--model` to "non-echo providers"). Never baked and never inherited from an app default; only a deliberate `extra_args` entry may select it. |
| `--base-url <URL>` | Endpoint routing is a credential decision, not a launch flag: it belongs with the proxied-provider story (`preferences/compatibility.rs` `resolve_provider_env`, the Codex-proxy precedent). A freeform base URL routes a node's traffic — and its bearer token — past that story. |
| `--subagent-worktree-isolation` | Documented as a compatibility no-op: "capability defaults on. Only an affirmative per-child request asks for isolation". Passing it asserts nothing, and reads as a policy Buildmesh holds when it does not. |
| `--no-session-log` | Buildmesh's attention signal *is* that durable log: `services::muse_watcher` tails the run boundaries in `~/.local/share/muse/sessions/…/session.jsonl` (#1709), and the transcript reader and session recovery read the same tree (#1708). Suppressing it does not make a node quieter — it makes the node unobservable. |

### Passthrough-only

| Flag | Why Buildmesh has no opinion |
|---|---|
| `--preset <native-basic\|miniswe>` | The help names the two values and nothing more — no documented tool, approval or sandbox semantics per preset, so there is no vocabulary to model and nothing stable to pin a test against. A preset would also override the launch policy Buildmesh already owns (baked approval/sandbox flags, pre-provisioned trust) rather than add to it. Revisit only if Muse publishes stable per-preset semantics. |
| `--parallel-tool-calls` / `--no-parallel-tool-calls` | Meta API concurrency, orthogonal to Buildmesh's contract. Also a shape argument: `ResolvedAgentConfig` forwards a *value*, so a negation flag has no slot — the capability-mask design cannot say "off" without inventing a tri-state for every harness. |
| `--agents <JSON>` | An ephemeral agent-definition overlay carried as a raw JSON blob. Buildmesh's per-node agent configuration already has its own source of truth, and a blob is not the closed string vocabulary `--agent <name>` is for OpenCode — see the slot note below. |

### Deferred

| Flag | Note |
|---|---|
| `--image <PATH>` | Multimodal prefill, explicitly out of scope here. One drift detail for whoever picks it up: the interactive TUI accepts a **single** `--image` while `muse exec` accepts it **repeatedly** — a per-surface cardinality difference, the same class of mismatch as `--effort` vs `--reasoning-effort`. |

### Owned by sibling issues

| Flag | Owner |
|---|---|
| `--reasoning-effort` (8-level closed vocabulary) | #1704 |
| `--approval-mode`, `--yolo`, `--permission-profile`, `--approval-judge`, `--disable-approval` | #1705 — `--permission-profile` and `--approval-judge` are recorded here only to note where they went |
| `--trust-workspace` | #1706 |
| `--disable-sandbox`, `--sandbox-network`, `--disable-write`, `--disable-shell`, `--enable-shell-tool` | #1707, #1788 |
| watcher consequences of the session log | #1709 |

### `exec`-only flags never reach the interactive recipe

Buildmesh spawns the **interactive TUI**, never `muse exec`. These flags exist
only on `muse exec` (verified live on 1.3.0), so forwarding one does not
degrade behaviour — it makes the CLI reject the whole launch instead of opening
a TUI:

`--json`, `--prompt-file`, `--api-key-stdin`, `--output-schema`,
`--max-model-steps`, `--max-tool-output-bytes`,
`--context-compaction-strategy`, `--context-compaction-soft-threshold`,
`--context-compaction-hard-threshold`, `--session-id`,
`--allow-workspace-switch`, `--user-input-auto-resolve`, `--disable-web-tools`,
`--no-foreign-personal-context`

`--max-model-steps` and `--max-tool-output-bytes` are the run-budget flags this
issue was opened for; `--context-compaction-*` is the third budget family. Two
notes worth keeping: `--session-id` being exec-only independently confirms that
Muse's interactive session ids cannot be assigned by the caller — which is why
the identity is captured from `session-index.db` instead (#1794) — and the
compaction thresholds take a fraction, so modeling them would have meant a
numeric type in `HarnessConfigValue`, not the string shape every current
harness option uses.

Conversely `--agents` and `--echo-delay-ms` are interactive-only.

### Nothing graduates to modeled in this pass

No Muse flag earned a `HarnessConfigValue` field, so `ResolvedAgentConfig` and
the capability table are unchanged. The slot is deliberately left open for a
future promotion: OpenCode's `--agent <name>` is the template — a value added to
`HarnessConfigValue` + `ResolvedAgentConfig`, behind a capability gate, rendered
by an adapter `*_args` helper, never a provider-name check at a call site. Of the
verdicts above, only two could eventually qualify: `--agents` (once Muse
publishes a stable, name-addressable overlay vocabulary to send instead of a
blob) and `--parallel-tool-calls` (once the harness-wide tri-state question is
answered for every adapter at once, not just Muse).

## Workspace trust (#1706)

Muse gates a workspace's **skills, rules and project-scoped plugins** behind an
explicit trust decision. Buildmesh spawns a PTY it cannot answer prompts in, so
before #1706 every Muse node ran untrusted: the workspace's `CLAUDE.md`/`AGENTS.md`
rules, `.claude/skills`, `.agents/skills`, and any `muse init` scaffolding were
silently skipped.

### The store is a persistent file, not a flag

`--trust-workspace` ("trust this workspace for this run … does not save trust")
and `--yolo` are **per-invocation only**. The durable store is Muse's config
root:

```
${XDG_CONFIG_HOME:-<home>/.config}/muse/trust.json
{
  "schema_version": 1,
  "projects": { "<canonical workspace root>": { "decision": "trusted" } }
}
```

Muse reads the XDG config root on every supported platform, including the
native Windows binary (`%USERPROFILE%\.config\muse\trust.json` — verified
present on the installed 1.3.0) and the WSL guest. Data lives separately under
`~/.local/share/muse`. The issue's evidence spells the same path
(`${XDG_CONFIG_HOME:-~/.config}/muse`) and determines the override Muse itself
would read, so Buildmesh honours `XDG_CONFIG_HOME` for a native launch — writing
to the default while a user has the override set would leave the entry
unread, which is the exact silent failure #1706 removes.

### Keys are canonicalized workspace roots

Muse files a workspace under the canonicalized path, so the key's *shape*
depends on which binary is running:

| Runtime | Example key | How Buildmesh derives it |
|---|---|---|
| Native Windows | `\\?\F:\src\buildmesh\.claude\worktrees\x` | `std::fs::canonicalize` on the workspace |
| Native POSIX host (Buildmesh itself on Linux / macOS) | `/home/u/proj` | `std::fs::canonicalize` on the workspace |
| WSL guest (guest `muse` spawned from a Windows host) | `/mnt/f/src/buildmesh/.claude/worktrees/x` | the guest path verbatim — a Windows host cannot canonicalize it |
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
- **never overriding** — a `decision` Muse already carries that is not
  `trusted` (a value such as `untrusted`), or that isn't even a string, is
  reported as a failure and left exactly as written. See
  [Decision vocabulary](#decision-vocabulary) for why that is a real decision
  and not a "seen" marker;
- **fail-closed** — a malformed file, a non-object `projects`, or a non-object
  entry for the workspace is refused with `Err` and left byte-identical, so the
  spawn path logs a warning and emits a `provider-error` naming the cause
  instead of clobbering user data (the mcode / Cursor provisioning precedent).
  The spawn is **not** aborted: the node still runs, just without the
  workspace's skills and rules — which is why the failure has to be reported
  rather than silently returned as success the way `mcode::provision_at` does
  for its own unresolvable case;
- **locked** — one process-wide `Mutex` guards the read/merge/write, because the
  store is machine-global and two concurrent spawns would otherwise lose each
  other's entry (atomic write only prevents torn reads).

`signal_health` is deliberately untouched by a trust failure: it records
whether the node can report its own turn completion, and Muse's passive watcher
is unaffected.

A write re-serializes the whole document through serde_json's sorted map, so
top-level key order can differ from Muse's own field order; values are
untouched, and Muse rewrites the file in its own order on its next write.

### Decision vocabulary

Two facts decide how Buildmesh treats an existing entry, and both are evidence,
not inference:

1. **There are exactly two decision values.** The binary's serde surface
   carries `ProjectTrustDecision { trusted, untrusted }` alongside
   `ProjectTrustStore { schema_version, projects }` — the store's own type.
2. **Absence, not `untrusted`, is the "not decided" state.** A workspace Muse
   has merely seen or run in is **missing** from the map: an untrusted
   `muse exec` in a fresh workspace wrote no entry at all, printed
   `project-skills-untrusted`, and left the store unchanged. Every observed
   store is likewise a set of `trusted` rows with nothing in between.

Together those mean a present `untrusted` row is a decision someone made, so
Buildmesh refuses to flip it and reports a provisioning failure instead. The
cost is deliberate: a workspace marked untrusted runs without its skills and
rules — visibly, with a named error, rather than by silently reversing a
consent decision.

### Known limits

- The write lock is **in-process**. Two Buildmesh instances pointed at the same
  Muse store can still lose an update — the atomic write is atomic, not
  additive across processes.
- Cross-runtime spawns resolve the runtime's **default** XDG root: the WSL
  branch goes through `cli_dir_for_spawn` (`wsl_home()`), so a guest
  `XDG_CONFIG_HOME` is not honoured and a node pinned to a non-default distro
  would write to the wrong guest home — issue #1697 (trust paths must be
  WSL-aware).
- Trust is the *only* launch-prerequisite step Muse takes; approval policy
  (#1705) and the OS sandbox (#1788) are separate, and `--yolo` is never baked.
- Only the PTY spawn path provisions trust. The MSP `muse serve` plane (#1681)
  has no in-repo launcher — only its telemetry slice landed under
  `agent::provider::muse` — so there is no second call site to keep in sync.

### Verification scope

| Claim | Verified how |
|---|---|
| Store location and JSON shape | Read from the real stores Muse wrote on this machine, native Windows and the WSL guest |
| A persisted entry loads project skills | Live `muse skills list --workspace <dir> --source project --json` against the installed 1.3.0 (table above) |
| `trust_key` reproduces Muse's own keys | `live_trust_key_reproduces_the_installed_stores_keys` — byte-exact against the real store's entries |
| Decision vocabulary is `{trusted, untrusted}` | `ProjectTrustDecision` / `ProjectTrustStore` type names and variant strings present in the installed binary |
| Absence is the "not decided" state | Live: an untrusted `muse exec` in a fresh workspace wrote no entry (`projects` count unchanged) while printing `project-skills-untrusted` |
| The native Windows binary honours `XDG_CONFIG_HOME` | Live A/B: `muse skills list` reports a workspace as trusted from the real store, and reports `project-skills-untrusted` for the same workspace once `XDG_CONFIG_HOME` points at an empty directory — its store moved with the variable |
| Key shape, merge, refusal, idempotency, relative-XDG rejection | Hermetic unit tests in `adapters::muse` (48 pass) |
| The POSIX (WSL guest / macOS) side of the XDG override | **Not verified.** The Windows arm was exercised; the POSIX arm follows the same code path and the XDG spec but no live POSIX A/B was run |
| macOS | **Not verified.** No macOS host was exercised; the macOS claims here are inferences from `canonicalize` semantics and the XDG convention |

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
