# Muse attention/turn signals (issue #1709)

Research date: 2026-09-12. Observed against **Muse Code 1.1.1 (1.1.1-R2514.1)**, build
`b934305d21`, installed at `/home/alond/.local/bin/muse` in WSL Ubuntu. All observations are
local and unpaid (no model calls were made to establish the surfaces below; the recorded
session logs are from pre-existing interactive runs).

Question: can Buildmesh obtain a **Node Turn** signal from the interactive Muse TUI, the
way it does from Claude/Codex/AGY/Grok hooks or the Command Code transcript watcher? Muse
declared `requires_attention_hook() == false` and no `attention_capability`, so Muse nodes
emitted no turn signal and Autopilot stayed blocked behind the compatibility gate.

## (a) MSP method index — `muse schema generate-json-schema`

`muse schema` describes itself as "export the MSP wire schema embedded in this binary …
the method index describes the served command plane (`session/*`, `turn/*`, `model/list`,
`view/*`, `approval/*`, `userInput/*`)". Running it:

```
$ muse schema generate-json-schema --out /tmp/muse-schema
wrote manifest.json, msp.schema.json to /tmp/muse-schema (stable surface)
$ cat /tmp/muse-schema/manifest.json
{"experimental":false,"fingerprint":"sha256:c669a30c…","schemaVersion":1}
```

The schema is precomputed at build time, so it is exact for this binary. It has five
top-level sections: `$defs`, `capabilities`, `description`, `errors`, `methods`,
`notifications`, `reserved`.

**Methods (client → host commands):**

| Group | Methods |
| --- | --- |
| `session/*` | `start`, `resume`, `read`, `list`, `fork`, `compact`, `setModel`, `setApprovalMode`, `userShell` |
| `turn/*` | `start`, `steer`, `cancel`, `interrupt`, `unqueue` |
| `approval/*` | `decide`, `listPending` |
| `userInput/*` | `answer`, `clarify`, `cancel` |
| `subagent/*` | `sendMessage`, `followupTask`, `interrupt`, `stop`, `resume`, `reopen`, `close`, `readResult` |
| `item/*`, `model/*`, `view/*` | `item/readOutput`; `model/list`; `view/page`, `view/subscribe`, `view/unsubscribe` |
| `initialize` | handshake |

**Notifications (host → client) — the lifecycle vocabulary:**

| Notification | Params | Meaning |
| --- | --- | --- |
| `turn/started` | `TurnStartedParams` (`sessionId`, `turnId`, `commandId`, `viewCursor`) | A foreground turn began running (fresh submits immediately; queued at launch; never steered). |
| `turn/completed` | `TurnCompletedParams` (`turnId`, `terminal`, `error?`, `reason?`, `usage?`, `durationMs?`) | Turn terminal: `completed \| failed \| cancelled`. `error` present iff `terminal == "failed"`. |
| `turn/retracted`, `turn/retryScheduled`, `turn/unqueued` | — | Interrupt/retry/queue bookkeeping. |
| `approval/requested`, `approval/resolved`, `approval/updated` | `Approval*Params` (`approvalId`, `itemId`, `turnId`, `availableChoices`, …) | Tool-approval open/decide/update. |
| `userInput/requested`, `userInput/settled` | `UserInput*Params` | Agent question open/settled. |
| `session/tokenUsage`, `session/contextUsage` | — | Already ingested for Observed Session Telemetry (#1680). |
| `session/{approvalModeChanged,modelChanged,goalChanged,todoListChanged,branchChanged,…}` | — | Session facts. |
| `item/{started,delta,updated,completed}`, `view/gap` | — | Transcript items and delivery-gap marker. |

Conclusion: the **agent loop has exactly the events attention needs** (`turn/*`,
`approval/*`, `userInput/*`). The open question was whether the interactive TUI path
exposes them — it does not; they are served on the MSP plane (see (c)).

## (b) Interactive TUI on-disk surfaces

`muse --help` has no hook/event flag. The TUI writes several artefacts under
`~/.local/share/muse`:

| Artefact | Shape | Usable as a turn signal? |
| --- | --- | --- |
| `sessions/YYYY/MM/DD/<uuid>/session.jsonl` | Append-only durable event log (NDJSON) | **Yes** — see below. |
| `sessions/.msp-view-v1/<uuid>/` | `HEAD.json` + binary `journal-*.bin` / `snapshot-*.json` | No — binary MSP view fold, not a text stream. |
| `tui-history.jsonl` | TUI input history | No — no turn boundaries. |
| `local-tracing/bootstrap/cli-<uuid>.log` | Human-readable tracing | No — debug spans, not a structured lifecycle stream. |
| `session-index.db` (`sessions` table) | Session metadata index (`session_id`, `session_log_path`, workspace, name, …) | Path resolver for the log above. |
| `sessions/**.peer-history.sqlite3` | Per-session peer history | No. |

The durable session log is the signal. Every run appends a `runtime.session` record whose
`payload.kind == "run"`:

```json
{"payload_type":"runtime.session","payload":{"kind":"run","run_id":"<uuid>",
  "event":{"kind":"started","prompt":"…"}}}
{"payload_type":"runtime.session","payload":{"kind":"run","run_id":"<uuid>",
  "event":{"kind":"terminal","terminal":"completed","reason":null,"turn_duration_ms":1027}}}
```

`event.kind == "terminal"` is the durable `turn/completed` fact; `terminal` is
`completed | failed | cancelled`. Task-level records (`payload.kind == "task"`) also carry
`completed`/`failed`/`cancelled` event kinds and must not be mistaken for run boundaries.

**Observed across 20 retained top-level session logs** (subagent logs excluded):

- `run/started` = **35**, `run/terminal` = **35** — exactly one terminal per run.
- Terminal values: `completed` 32, `cancelled` 1, `failed` 2.
- `approval/requested` records exist, but **only** in sessions whose
  `security_mode == "normal"`. Sessions launched with `--disable-approval`
  (`security_mode == "approval_disabled"`, `source: "explicit-cli-flag"`) carried
  **zero** `approval/requested` records.

The log is written live: in one retained session, records span the full ~11-minute run.

## (c) Could `muse serve` back attention for PTY nodes?

`muse serve` is "Serve an MSP session host over stdio": "The client owns this process's
stdin and stdout and is its only connection. Sandbox posture and session durability are
constructed here … Approval mode is … selected on the wire." It is a **headless JSON-RPC
host** — it does not render the interactive TUI. Adopting it would replace the PTY+TUI
execution model with a Buildmesh-owned MSP client and renderer. That is the separate
serve-path architecture, not a drop-in attention source for the existing PTY node. This
research deliberately does **not** duplicate it (see out-of-scope in #1709; MSP transport
is the #1681 surface).

## (d) Would untrusted-workspace hook files load?

Moot: **no hook files exist**. `muse --help` exposes no hook registration; a recursive
search of `~/.config/muse`, the feature-config cache, and the runtime directory found no
hook configuration, and `~/.config/muse` holds only `auth.json`, `trust.json`, and
`settings.json`. Muse's `--trust-workspace` loads a workspace's *skills and rules*, not
hooks. There is therefore no trust prerequisite to satisfy and no trust issue to inherit
for an attention mechanism.

## Verdict and wiring

**Passive watcher** (option 2). A native hook is impossible (no registration surface), and
the MSP serve plane is a different architecture. The interactive TUI's durable session log
already carries the turn boundary, so `services::muse_watcher` tails it (mirroring
`commandcode_watcher`) and publishes a Node Turn on each `run/terminal` record. Muse keeps
`requires_attention_hook == false` and `attention_capability == None`; it now sets
`supports_passive_turn_watcher == true`.

**Launch mode is `SkipPermissions`.** Under Buildmesh's `--disable-approval`,
`PermissionRequested` is impossible by construction (zero `approval/requested` records in
every `approval_disabled` session), so the watcher classifies only turn terminals — never a
permission. `failed`/`cancelled` terminals still yield the node back to the user and are
published as turns with the terminal value carried in `completion_reason`.

The watcher resolves the log through the same `session-index.db` the adapter already uses
for session recovery, converting the guest path with `env::to_host_path` for WSL hosts. It
defers unterminated trailing lines (Muse may be mid-write), suppresses an earlier terminal
when a newer run has already started, and baselines resumed sessions at their pre-spawn EOF
so a prior completed turn is never replayed as the resumed node's signal.
