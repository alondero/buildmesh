# Attention system, hooks, and node lifecycle signals

Status: current

User-facing counterpart: [Attention hooks Buildmesh installs on
disk](../user-guide.md#attention-hooks-buildmesh-installs-on-disk) discloses the
per-harness files written on disk and the loopback POST they make. Keep both in
sync when a harness's hook target or payload changes.

## Attention System

### How It Works
Agents signal they need user input via Claude Code hooks configured in `.claude/settings.local.json` (written by `inject_attention_hook` in `agent/spawn.rs`): a catch-all `Notification` hook (permission prompts, idle prompts, elicitations) plus a `Stop` hook (turn ended). Both run the same curl command, which forwards the hook's **stdin JSON** as the POST body (issue #878):

```
curl -sf -X POST -H "Content-Type: application/json" --data-binary @- http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID || true
```

The hook reads `$BUILDMESH_PORT` (set per-agent in `spawn_environment`) at run time rather than baking a literal port, so it routes correctly across the 1992→1994 fallback and to the dev profile's 2992 when an agent is spawned by `buildmesh-dev`.

**Codex is a different contract.** Codex's hook runner launches the command in PowerShell (Windows, verified on 0.162, so cmd.exe-only syntax such as `& echo {}` is a parse error that surfaces as "Hook failed … exited with code 1") or `$SHELL -lc` (Unix), then `env_clear()`s down to a Core inherit snapshot. `BUILDMESH_PORT` / `BUILDMESH_SESSION_ID` are not in that snapshot, so the callback URL `http://localhost:<port>/api/attention/<node-id>` is baked in. The hooks are **launch arguments, not project files**: from a git worktree Codex loads the *main checkout's* `.codex/hooks.json` and ignores the worktree's own, so a file written beside the node is never read and a shared main-checkout file would be last-writer-wins across every worktree node. `AgentProvider::launch_hook_args` (called from `build_spawn_command_prepared`, which knows the node id) therefore emits `-c features.hooks=true` and one `-c hooks.<Event>=[…]` override per event: `SessionStart`, `Stop`, `PermissionRequest`, `UserPromptSubmit`, `PreToolUse` for `request_user_input`, catch-all `PostToolUse`, and `Interrupt`. Codex combines these with the user's own `hooks.json` hooks. The values are TOML literal strings, so the callback they wrap carries no quote characters of its own: a double quote is dropped by Windows PowerShell 5.1 on the way to a native program, and a single quote would force `toml_launch_string` to escape the value into a double-quoted string.

A Windows launch crosses **two** command-line budgets, and the tighter one belongs to the shell, not the process. Buildmesh hands `powershell.exe -NoLogo -NoProfile -EncodedCommand <script>`, which CreateProcess caps at 32,767 characters; PowerShell then runs an npm `codex.CMD` shim by delegating to `cmd.exe`, which parses only the first 8,191 characters of the line it is given and answers the rest with *The command line is too long.* on the node's own terminal. Moving the attention hooks onto the launch command line (#2184) left the launch within a few hundred characters of that 8,191 ceiling, so the PR reviewer's own review-policy prompt overflowed it and the node died mid-launch (the symptom that took node 5351 down). The callback is now PowerShell statement text (`env::windows_hook_callback_command`) rather than a nested `powershell.exe -EncodedCommand <base64>` payload — about 300 characters instead of 918, repeated once per event — which keeps a full 2,000-character prompt inside both budgets, and `spawn_environment::wrap` refuses a launch that cannot fit so an overflow surfaces as a spawn error instead of a mystery. The mandatory `command` field is a stub because Codex on Windows runs `commandWindows`; a WSL node keeps a POSIX `command`. `provision_attention_hooks` now only retires the handlers older versions wrote into `.codex/hooks.json` (in the spawn directory and the main checkout), never creating files and never touching the user's own entries. The callback is best-effort: errors are suppressed and the hook exits successfully if Buildmesh is unavailable or the node is stale. SessionStart is capture-only (`Decision::Ignore`): it persists `cli_session_id` from the hook payload but must not publish Ready or trigger Circuit observation or naming. Project trust (`ensure_codex_project_trusted`) is still required; `--dangerously-bypass-hook-trust` only skips hook-definition review. Resume argv is `codex resume [OPTIONS] <uuid>` — flags after the UUID are the optional prompt — modeled as `SpawnRecipe.base_args` (options) + `trailing_args` (session id, then prefill). The rollout poller (`services::codex_session`) remains the disk fallback and ignores `thread_source: subagent` files so a child thread cannot steal the stored id. Because each launch carries its own node id, Codex nodes can share a repository or worktree without redirecting each other's callbacks.

Windows-hosted Codex callbacks decode stdin as UTF-8 and post it as a JSON body
whose bytes must survive unaltered. PowerShell's default console and request
encodings can corrupt non-ASCII tool input or results into invalid JSON,
producing degraded signal health, so the callback pins
`[Console]::InputEncoding` to UTF-8 and sends the body as a UTF-8 byte array —
the body charset cannot be named in `Content-Type` because the `;` that
separates it would end the PowerShell statement. The Windows hook delivery
regression includes accents, CJK text, punctuation and emoji through both
PowerShell and cmd.exe.

`POST /api/attention/{session_id}` normalizes harness callbacks before publishing
through `node_turn` and `SessionLifecycle`. A clean completion lands in `Ready`,
a structured question or permission lands in `AwaitingInput`, and known pending
work stays `Running` with a `BackgroundRunning` observation. The lifecycle owner
commits status, observation timestamp and snapshot together before desktop/mobile
events or naming/autopilot consumers run. Node list reads restore that snapshot;
legacy attention-clear events do not carry authority to change client status.
See [Agent Node status observation](node-status-observation.md) for
ordering, health, recovery and harness capability limits.

### False-Yield Suppression (issue #878)

MiniMax Code Stop reconciliation reads the node's canonical `messages.jsonl`
and native `background-tasks` store, resolved through its own session ID and
spawn environment. It does not apply Claude's task scanner to MiniMax's
temporary compatible hook transcript. Explicit and timeout-promoted launches
remain pending until a native terminal tool receipt or terminal summary settles
them. Missing or partial canonical history remains unavailable evidence. An
unanswered foreground tool call also leaves Stop completion evidence unavailable;
its matching native result must arrive before Stop can report Ready. The
native lookup runs even when the compatible transcript path is missing or empty.
If the first Stop precedes identity capture, it uses the validated callback
session of the already resolved workspace owner; application still requires the
existing generation and conditional identity fences.

Claude Code ends its turn when it launches background work (`run_in_background` Bash, timeout-backgrounded commands) and re-invokes itself when the `<task-notification>` arrives — so a Stop (or 60s-idle Notification) is *not* always "the user is needed". The route reads `transcript_path` from the hook payload and asks `transcript_reader::count_pending_background_tasks` for launched-but-unnotified task IDs; finish = a `<task-id>` notification with a **terminal** status — `running`-status notifications don't count. A launch needs both halves of one exchange: a `tool_result` promising "You will be notified when it completes" **and** the `tool_use` it answers, named as a launch-capable tool (`Bash` — the only tool Claude Code launches background work from), with the receipt opening the result text. Wording alone is not evidence, since a tool result carries text too: an agent reading this repository's own source sees both launch phrasings quoted back in a `Read` result, and no notification can settle the task that wording alone would have registered. A receipt with no launch-capable call behind it owns no task. Pending work → the Node Turn is published via `node_turn::publish_hook` with `background_running` (naming/autopilot still fire; no attention mark). Permission-prompt Notifications always mark, even mid-background-wait. Any unknown (empty/garbage body, unreadable transcript) degrades to marking — never to silence.

Claude Code can stop a background command through `TaskStop` without queuing a terminal task notification. The transcript scan also accepts a successful `TaskStop` result bound to the matching tool call and task ID. A failed stop, a request without its result, or unrelated tool output keeps that task pending. Stop and idle Notification callbacks use this same scan.

**Safety net:** `attention_autoclear.rs` arms on every mark; if the PTY then produces ≥512 bytes of output more than 3s after the mark with no user keystroke, the node flips back to `running` and `attention-cleared` is broadcast. The 3s grace absorbs the Stop-hook-vs-final-redraw race; the burst threshold ignores idle control-sequence trickle. This self-heals the cases the transcript scan can't see (hook-less providers, format drift, lost notifications). Every path that clears attention or accepts user input must call `attention_autoclear::disarm` (see `write_to_agent_blocking`, `http::ws`, `coordinator::drive`, `circuit::delivery`).

### Cross-harness hook normalization (2026-09)

All structured attention callbacks enter `http::routes::attention` and are
normalized into the shared `agent-lifecycle` kinds. A clean `Stop` or native
idle event is `turn_completed`/`ready`; a permission or question callback is
`permission_requested`/`question_requested` and remains outstanding until its
matching resolution; input submission and permission resolution are
`work_resumed`/`running`. Errors, cancellation, malformed payloads, and
unreadable transcripts are degraded review checkpoints, never successful turn
completion. Background work is published as `background_running` without
attention until its terminal callback arrives.

Harness wire validation and mapping live in
`http::routes::attention::normalizers`, with one module per wired hook harness.
Dispatch uses the resolved node's harness, never a claimed payload provider or
another harness's classifier. Each module selects the fields it validates;
unrelated harness metadata cannot invalidate its callback. Unknown harnesses,
unsupported events and malformed payloads report `signal_unavailable` with
degraded health and cannot mutate turn/question/child correlation state.
Compatible hooks share envelope and tool mechanics, while event vocabulary and
observation strategy remain harness-owned. Explicit hooks are interpreted first;
only that harness's completion path may request transcript reconciliation.
Transcript reads run on the blocking pool after HTTP security checks, without
holding a database connection.

The route retains node/session ownership checks and delegates per-node callback
ordering to `attention::ordering`. Normalizers return observations and never
write node state. `node_turn` publishes the normalized lifecycle kind through
`SessionLifecycle`, then considers renaming only after an accepted lifecycle
commit. Replay fixtures and their evidence limits are documented in
[`src-tauri/tests/fixtures/attention`](../../src-tauri/tests/fixtures/attention/README.md).

The route keeps per-node ordering state and fences callbacks by provider turn
id/session id. Foreground activity is tracked separately from outstanding
questions, so a delayed background callback cannot publish `ready` in the
middle of a live turn. This matters for Kimi Code: a background
`AskUserQuestion` returns before its answer, so `PostToolUse` is correlation
only; a later `Notification` with `source_kind=background_task` and a terminal
`task.*` type resolves it after the foreground `Stop`. OpenCode question and
permission events are tracked by request id (with a conservative single-
request fallback), and child sessions cannot overwrite their parent. Codex has
no permission-result hook, so its tool result or identified terminal `Stop`
resolves the attention route's approval marker. Circuit evidence applies a
stricter request identity contract: a permission callback without a request ID
retains an unresolved permission wait, and neither a generic Stop nor an
unrelated tool result resolves it. Native hooks are
provisioned only where the installed harness contract is verified; Terminal,
Freebuff, and unvalidated DeepSeek profiles retain explicit capability gaps
rather than guessing from PTY output. MiniMax's Agent-Plugin attention hook is
live: `requires_attention_hook` is `true` after the issue #1797 validation
delivered `Stop` from the installed 0.4.12 TUI. SessionStart is provisioned
for identity capture, but delivery from that TUI is unvalidated; live Stop also
delivered the session id after a completed turn. mcode 0.4.0+ reads
`.claude-plugin/plugin.json` with `hooks` **inlined** (a separate
`hooks/hooks.json` document is ignored, and a directory with no manifest is
skipped silently), runs `command` + `args` with no shell interpretation, and
`env_clear()`s `BUILDMESH_*` — so the shared callback URL bakes the ports. It
bakes every Buildmesh instance listening on the machine, not just the one
that spawned the node: the manifest is one user-global file, so naming only the
spawning instance's port made the most recent MiniMax spawn steal the other
instances' callbacks, and those nodes stayed `Running` forever because the
callback landed on an instance that does not own the node (issue #2343). The
instance list comes from `http::instances`, a machine-global registry of bound
ports whose entries a TCP-connect probe filters to the ones that answer; curl
reads the stdin body once and POSTs it to each URL, and the node-independent
route lets the owning instance accept the callback while the others discard it
as unknown. See
[Remote Access](remote-access.md#multiple-instances-on-one-machine) for the
registry's location and lifetime. Native session/workspace payloads resolve the
node; the provisioned SessionStart handler is capture-only, but its delivery
has not been observed live.
Only `TurnCompleted` is advertised: the launch pins Full Access
(`permissionMode: bypassPermissions`, a surgical one-line edit to
`<dataDir>/config.yaml` — the TUI has no permission flag), so no permission
signal is claimed. Its
`messages.jsonl` transcript is wired too (`TranscriptFormat::Mcode`); see
`docs/learning/mcode-harness-capabilities.md`. Muse's interactive TUI exposes
no hook/event flag, so its turn signal comes from `services::muse_watcher`,
which tails the durable
`~/.local/share/muse/sessions/…/session.jsonl` run boundaries (`runtime.session`
records with `payload.kind == "run"` and `event.kind == "terminal"`) and
publishes each as a Node Turn — a passive watcher like Command Code's, with
`requires_attention_hook = false` and `attention_capability = None`. (Muse 1.3.0
does ship a claude-compatible plugin hook surface, but it is gated behind an
explicit `muse plugins approve` into a global plugin cache, and the node-local
`--scope project` install is refused until the workspace is trusted — issue
#1706 — so it is deliberately not provisioned; see
`docs/research/muse-attention-signals.md`.) Because
Buildmesh launches `muse --disable-approval`, a `PermissionRequested` signal is
impossible by construction and is not classified. Muse is additionally the only
harness shipped with its own always-on OS sandbox: Buildmesh bakes
`--disable-sandbox` next to the approval flag so the agent shell reaches the OS
credential store gh and git credential helpers resolve GitHub auth from (issue
#1788). See
`docs/learning/harness-attention-reliability.md` for the evidence matrix and
remaining limitations.

A turn the user cancelled is `turn_completed`/`ready` with the message "turn
interrupted by the user", not a failure: Codex `Interrupt`, Grok
`StopCancelled`, and an OpenCode `session.error` whose forwarded `error` names
`MessageAbortedError`. `StopFailure` and other OpenCode errors remain degraded
review checkpoints. Claude Code and MiniMax fire nothing for Esc, so
`process::watch_for_interrupt` handles a bare Esc or Ctrl+C written to a
`running` node: every `INTERRUPT_POLL` for up to `INTERRUPT_ATTEMPTS` looks
(MiniMax recorded an aborted tool three seconds after Esc) it asks the
harness's transcript reader (`interrupted_turn`), stopping as soon as the node
leaves that `running` revision. It settles that exact revision to Ready only
if the transcript was written after the key press and its tail records
the interruption (Claude's `[Request interrupted by user…]` record; MiniMax's
aborted tool result, or a prompt left unanswered by an abort that rewrote the
file).

Antigravity's question dialog blocks the turn without a Stop, so its hooks file
also carries `PreToolUse` and `PostToolUse` matcher groups scoped to
`ask_question`. A pre-tool hook is a gate whose answer is required — AGY 1.3.5
denied the question when the hook printed `{}` — so it prints
`{"decision":"allow"}`, built on Windows by PowerShell from `[char]34` because
AGY passes every `"` to cmd as `\"`. Every other tool is untouched. AGY's
payloads name no event, so these hooks put it in the URL (`?event=PreToolUse`)
and the route names the payload before any parser sees it; unnamed, the
pre-tool payload's `transcriptPath` would read as a Stop receipt. A WSL-hosted
Buildmesh keeps the Stop hook only.

### Native session continuation (issue #2263)
A hook session id that differs from `agent_nodes.cli_session_id` is normally a callback from a previous process generation, and the ordering token drops it. Claude Code can also carry a **running** process into a *new* native session, in which case every later callback legitimately carries an id Buildmesh has never stored: the run then looks permanently busy and its Circuit cannot observe its own report. The transition is recorded in the transcript of the session it came from — `{"type":"continued-in","sessionId":<old>,"continuedInSessionId":<new>}` — and that record is the only evidence separating the two cases.

`http/routes/attention/continuation.rs` owns the seam, split by what each phase touches:

- `prove` resolves the chain through the Claude Code transcript reader (`continued_session_id`, which accepts only a well-formed record naming its own session, bounded at 8 hops). It is file I/O, so the route runs it on the blocking pool *before* the apply pass with no database connection held. A callback that already matches the stored identity returns without touching a transcript, and any harness whose `TranscriptFormat` is not `ClaudeCode` is never granted lineage — Cursor reuses the parser over its own store and never writes the record.
- `admit` performs the identity write: `db::adopt_continued_cli_session_id`, conditional on `cli_session_id = <old>` **and** `session_started_at` (the process generation read with the request) **and** a live status **and** no other live node owning the successor. A node that respawned, restarted or whose identity was captured elsewhere keeps the newer identity, so a retired process's successor callback is dropped instead of resurrecting its conversation.

`/clear` also moves a running Claude Code process into a new session, but writes no `continued-in` record. The chain walk therefore ends with `cleared_successor`: the hook's own transcript proves it is the stored session's successor when its first real user record is the `/clear` command and that record is no older than the stored session's last record. A clear that predates the stored session's activity proves nothing. Both proofs read the transcripts under the directory the harness ran in (`coordinator::enrichment::transcript_dir`): a worktree node's transcripts are keyed under its worktree, so reading them under the mesh root finds nothing and drops every successor callback as stale.

Only the chain's terminal session is adoptable. Absent, malformed or non-UUID records prove nothing (the declared id becomes a transcript filename on the next hop and reaches the harness as `--resume <id>`), a reversed record is never lineage, and an id the chain has already passed is superseded. Because the stored identity moves first and only then does the callback publish, the lifecycle observation and the Circuit's report read (`coordinator::enrichment::circuit_report_snapshot` → `readiness::prepare`, which requires `owner.session_id == agent.cli_session_id`) follow the successor together.

### Auto-Spawn Behavior
`AgentTerminal` component auto-spawns the agent when mounting an agent node with `status === 'idle'` and a `provider`. It uses `fitAddon.proposeDimensions()` to get PTY size before calling `spawn_agent`. This couples terminal mount directly to agent spawn — debugging attention issues requires tracing this path.

## Agent Node ID Capture
Session IDs are **assigned, not captured**, for providers whose CLI accepts a caller-chosen id (Anthropic): the orchestrator mints a UUID up front, writes it to `agent_nodes.cli_session_id` *before* launch, and passes it via `--session-id <uuid>` (`agent/spawn.rs`, `SessionIdMode::Assign`; ADR 0024). The PTY reader thread's labeled-UUID sniff (`session_capture.rs`) runs **only** for self-assigning providers that print a UUID banner (Codex, Antigravity) — gated by `reader_should_capture_session_id` / `captures_session_id_from_pty` so there is exactly one writer per spawn (issue #651). Recent Codex TUIs often omit the banner: SessionStart (hook payload) and the rollout `session_meta` poller (`after_fresh_spawn`) are the load-bearing capture paths; PTY sniff is opportunistic. Antigravity also self-assigns UUIDs without printing them: `services::agy_session` scans transcripts past the mtime gate, reads matching launch `workspace_uris` from the sibling read-only `conversation_summaries.db`, and uses a single unambiguous decodable file root as an anchor when its path form matches the spawn directory; multi-root rows fall back to transcript Cwd. WSL guest paths and host UNC paths are not reconciled here; transcript Cwd follows the same limitation. Missing or unreadable summary data, blank rows, rows with no decodable root, and ambiguous multi-root rows fall back to transcript Cwd; malformed metadata for that conversation remains unverified. OpenCode also self-assigns, but its ids are `ses_…` (not UUIDs) and are not printed on the TUI: a fresh spawn uses `SessionIdMode::None` and `OpenCodeAdapter::after_fresh_spawn` reads the local `opencode.db` SQLite store (`services::opencode_session`) for a row created in the spawn time window whose `directory` matches the node; resume is `--session <id>`. MiniMax Code (`mcode`) also self-assigns: Buildmesh provisions SessionStart capture, but delivery from the installed TUI is unvalidated. Live Stop callbacks do deliver the session id and can capture identity after the first completed turn. Until a callback arrives, a new node has no stored session id; the removed time-window manifest poller has no verified replacement. The callback route binds a unique live node by conversation id and workspace (`services::mcode_session`), with a conditional write fenced by provider, workspace, process generation, and duplicate ownership. PTY sniff remains off. Don't replicate any of these paths — they are backend-only. `CLAUDE_CODE_SESSION_ID` is deliberately **not** used: Claude Code sets its `CLAUDE_CODE_*` vars *downward* into its own subprocesses, so a parent that spawns `claude` can't read it, and for Claude we already know the ID (we assigned it). See ADR 0024 and `docs/learning/opencode-harness-capabilities.md`.

## Turn Counting and Node Naming

**Background inference** is an adapter capability, independent of interactive
prefill, turn hooks, transcripts, and resume. `AgentProvider::background_recipe`
returns both the invocation and its `BackgroundInferenceCapability`; the
generated catalog and settings pickers derive eligibility from that recipe.
`agent::background` owns launch validation, prompt transport (stdin, argument,
or file), authentication environment, and final-answer extraction (stdout,
result file, or structured events). Naming and Circuit classifiers share it.
Unknown harness identifiers are rejected instead of using the legacy database
parser's Claude fallback. Both consumers own descendant cleanup through
`BackgroundProcessGuard`; callers supply isolated directories and execution bounds.
Claude command resolution preserves PATH lookup and Windows native/npm install
fallbacks for both binary spellings, unless an executable override is supplied.
Extra CLI arguments are rejected because they can change this protocol;
provider routes require explicit support from the background recipe. Adding a
runner belongs in its adapter, without an orchestration allow-list. Naming
preflight failures release in-flight ownership without consuming an inference
attempt, so repaired settings can retry on a later turn.

`session_naming.rs` names default-named nodes from initial user/assistant dialogue, through the harness-owned transcript readers. It resolves the saved launch harness and actual node working directory, reads on a blocking worker outside DB/naming locks, and sends at most four text messages (4,000-byte previews plus an ellipsis when truncated) with explicit JSON role labels to the configured naming backend. JSONL readers inspect at most the first 1 MiB with their normal message coalescing and ignore tool-only turns; Cline reads its document and OpenCode queries its first 50 message rows. Later tool traffic cannot evict the initial request. Tool arguments/results, reasoning, terminal redraws and status chrome are not naming input. Codex AGENTS.md injections and MiniMax's leading reminder wrappers are removed by their readers before text truncation.

The first accepted Node Turn can trigger naming, including short requests; there is no startup gate or terminal-byte threshold. A missing session, unreadable/unsupported transcript, or absent user request defers naming to a later turn without consuming an inference attempt. Unsupported harnesses retain the generated name. The configured naming backend remains independent of the node's harness. Existing non-default names are preserved. Rename reservations are generation-scoped so reset, restart and cleanup reject stale reads and inference completions. Set `BUILDMESH_DUMP_NAME_BUFFER=1` for an opt-in dump of the selected, secret-scrubbed dialogue and exact model prompt; normal logs contain only source and byte count.

**Slug length is clamped, not refused.** `slug_with_retry` prefers the first hyphenated line, then fits the candidate: empty tokens collapse, the tail is dropped past five words, and the remainder is cut at 50 characters on a whole-word boundary, so a hyphen-joined answer keeps its subject and loses only its tail. The clamp applies to responses that were already hyphen-joined, because that is what the prompt asks for. A response with no hyphen anywhere is ambiguous — `fix auth token flow` is a name, `please rename it to fix auth flow` is a sentence — and joining those words cannot tell them apart, so that path only normalises: a run of three to five words inside the character cap becomes a name, while a run past five words or past the cap stays retry-worthy rather than being trimmed into a name that describes nothing. Both limits matter there, because the character cut would drop a sentence's last word just as the word count would. A candidate is refused when it cannot name the task at all: fewer than three words, characters `SLUG_REGEX` forbids, or leading words that exceed the cap on their own. The regex enforces slug syntax — a lowercase alphanumeric start, then lowercase alphanumerics and hyphens inside the length bounds — and not meaning: it rejects punctuation, capitals and leading digits, but a short word run of ordinary words passes it, which is why the spaced path's own limits do the work of telling a name from a sentence.

**Name uniqueness is load-bearing for Worktree Nodes.** A Worktree Node's `name` is also its `worktree_name`, its worktree directory, and (branched mode) its local branch — see `services/agent_node.rs` (`worktree_db_name = session_name`). Issue/PR spawns and Circuit spawn steps derive that name deterministically (`issue_node_name` / `pr_node_name` / `circuit_step_node_name` → `gh{N}-{slug}` / `pr{N}-{slug}`, plus the review variants), so a second spawn for the same issue or PR derives the same worktree path as the first. `git::worktree::provision_for_spawn` cannot recover from that: its warm path refuses the adoption because the branch is already checked out there, and its cold path's path-exists short-circuit hands the same directory to both nodes. (The warm-failure cleanup used to read "path exists" as "our move created it" and delete the other node's live worktree; it now only removes a target it created itself.) Any new spawn source that derives a name deterministically must therefore make it unique per Mesh — `session_naming::disambiguate_node_name` is the helper, and the PR pill's reviewer spawn (`create_pr_node` with `reviewer`) is the worked example.

## auto_resume_agent_nodes
On app restart, the frontend calls the `auto_resume_agent_nodes` command (`src/lib/tauri.ts` exposes it as `autoResumeAgentNodes`) which iterates all `Suspended` agent nodes with a `cli_session_id` and calls `spawn_agent_inner` with `SessionIdMode::Resume`. Whether a harness participates is `AgentProvider::auto_resume_on_startup()` (true for Anthropic, Codex, Cursor, OpenCode, Kimi, and others that opt in). A harness that returns false is left `Suspended` (`decide_startup_resume` → `SkipAdapterDeclines`) so the user can Resume / Regenerate from the UI.

## Early-Exit Detection
The PTY reader thread records `spawned_at`. If the reader exits within 3 seconds, the agent node is marked `Error` and a `resume-failed` event is emitted. This catches failed `--resume` attempts where the agent CLI exits because the session has expired.

## Expired Session Recovery & Start Fresh (issue #1306)
When a node transitions to `Error` status after an expired or invalid session ID fails to resume, `session_lifecycle::on_resume_failed` marks the status as `Error` but intentionally leaves `cli_session_id` intact (since `on_resume_failed` is a status-only writer, preserving the ID for transient-failure retries).

To break unrecoverable restart loops where an expired session ID would otherwise be retried indefinitely:
- **Retry Resume (`↻` inline button):** Re-attempts spawning via `spawnAgent(node.id, node.provider)` with the existing `cli_session_id` (`SpawnIntent::Resume`) for transient failures (e.g. network blips or race conditions).
- **Start Fresh (Context Menu item):** Invokes `restartFreshAgent(node.id)` which calls `spawn_agent` with `resume = null` (`SpawnIntent::Fresh`). The backend `spawn_with_intent` pipeline detects `intent_replaces_conversation(&intent)` and executes `db::clear_cli_session_id(node_id)`, resetting `cli_session_id` to `NULL` in SQLite and launching the agent fresh in the existing worktree with correct terminal dimensions.

