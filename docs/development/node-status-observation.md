# Agent Node status observation

Status: current

## Contract

Node status describes the last observed session state. It is not proof that the
assigned task succeeded. A persistent terminal is not proof that its model is
working, and terminal silence is not proof that it finished.

| State | Meaning | Automation implication |
| --- | --- | --- |
| Starting | Provisioning or the process early-exit window | Wait for session evidence |
| Running | Work resumed or a live process was launched | Launch alone does not prove model activity |
| Waiting for background work | The foreground yielded with observed unfinished work | Do not infer task completion |
| Ready | A clean foreground turn ended with no known outstanding work | Another instruction can be sent; task correctness is not established |
| Needs an answer / Needs permission | A structured human request is outstanding | A matching reply is needed |
| Needs attention | A generic yield, failure, or degraded observation needs review | Inspect its reason and signal health |
| Completed | Automation recorded its completion outcome | Distinct from ordinary turn completion |
| Idle / Suspended / Lost / Error | No live process, interrupted process, lost work, or a failure | A late hook cannot resurrect the node |

Signal health is separate: `unverified` means provisioning has not yet been
confirmed by an observation; `ok` means an accepted observation was received;
`degraded` means its meaning could not be established; `unavailable` means
provisioning failed or no supported observer exists. None of these is a delivery
SLA. The observation timestamp is the last accepted observation, not a heartbeat.

Health is a property of the harness integration, not of the process, so two rules
bound it. Only a payload the harness actually produced may move the column: a
local process observation (exit, idle, resume) carries no delivery evidence and
must neither repair nor degrade health, even though its stored snapshot reports
the absence of evidence as `unverified`. And installation is an expectation, not
evidence, so a successful install may only replace another expectation — a
health that is still unknown, or the `unavailable` a *failed* previous install
recorded, which a later success supersedes. A health earned by a delivered
callback (`ok`, `degraded`) is evidence and is never downgraded, and an
uninterpretable callback is evidence even when it carries no provider event
name, so it still records `degraded`.

`unverified` earns no title-bar badge and no problem-bucket placement. It is the
normal state of a healthy session between turns, and badging it trained users to
ignore the amber that does mean something; it rides in the status tooltip
instead. See [DESIGN.md principle 6](../../DESIGN.md#principles) and
`isSignalHealthProblem` in `src/lib/status.ts`.

## Owners and seams

1. The harness adapter provisions its native integration or advertises a passive
   observer. Model-provider profiles inherit the selected harness contract.
2. The attention route normalizes callbacks and fences session and turn identity.
   It holds one node's hook state while correlating questions and owned children.
   Passive watchers enter the same lifecycle publisher after their own identity
   checks. The [harness audit](../learning/harness-attention-reliability.md)
   records the supported contracts and their evidence levels.
3. `SessionLifecycle` commits the normalized observation and resulting status in
   one SQLite update before publishing it. `agent_nodes.lifecycle_snapshot`
   contains the same envelope clients receive over Tauri and WebSocket events.
   The snapshot's timestamp must equal the row's `status_changed_at`, and its
   status must match. Process-only transitions invalidate old snapshots through
   that revision check, including startup recovery and explicit stopping.
4. Desktop and mobile derive labels from that envelope through the shared status
   module. Node-list reads restore background state and human request text after
   a reconnect. A legacy attention-clear notification only clears presentation;
   it cannot invent `running`. In-flight list reads cannot overwrite a newer
   lifecycle patch; desktop queues a fresh read and mobile rejects older refreshes.
   The spawn-completed list read happens before the early-exit window elapses, so
   it can still observe `spawning`. When that window promotes the row, the same
   write stores `process_running` and `running` with one timestamp. Clients adopt
   that status and snapshot. They do not copy the snapshot's signal health onto
   the node, so an unknown health column does not become the unverified tooltip
   and an unverified column is not repaired to ok. The badge cannot stay on
   Starting after the process has survived startup. A submitted prompt publishes the
   `work_resumed` envelope that was stored for that keystroke; the legacy clear
   still does not carry a status.
5. Circuits retain their separate, stronger
   [session observation contract](circuit-session-observation.md). The display
   snapshot is not new permission to advance a circuit. Native receipts, request
   identities, submission fences, reports, and owned-work evidence still apply.

All new harness integrations must name their observer and unsupported signals.
They must not gain lifecycle capability merely by installing a binary, writing a
configuration file, producing terminal output, or sharing another CLI's tool names.

## Findings addressed

- Codex question-tool results were classified through an approval-only branch.
  Answering `request_user_input` removed the tracked question but did not resume
  the node. Question results now follow their own existing request resolution.
- `Lost` was absent from the hook transition fence. Generic attention clearing
  also wrote `running` unconditionally. Both paths now preserve stopped states,
  and accepted input produces a normalized `work_resumed` observation.
- Claude child-agent hooks were already installed, but ordinary node status
  ignored them while circuits consumed them. Node tracking now retains child
  identities across foreground turns, waits for all known children, remembers
  completion-before-start delivery, and keeps a child completion from ending a
  newer foreground turn. A transcript-reported background wait remains a blocker
  even when the last known child finishes.
- Background and request reasons existed only in event delivery. The durable
  observation closes that reconnect gap without introducing a second status
  machine. Status and snapshot are one write; rejected transitions publish neither.
- Hook installation success was presented as confirmed signal delivery, including
  after failed trust setup. Provisioning now distinguishes unverified installation,
  failure, and unsupported observation. Local input does not repair delivery health.
- List refreshes could regress live state. Client request ownership now protects
  lifecycle updates, and the legacy desktop clear cannot overwrite a ready/idle
  lifecycle transition.

## Validation boundaries

The current adapter declarations have different coverage. This inventory describes
the code contract, not a fresh validation of every installed CLI version. The
`AgentProvider::attention_capability` and `supports_passive_turn_watcher` methods
in `src-tauri/src/agent/provider/adapters/` remain the source of truth.

| Harness | Observation path | Declared lifecycle coverage / limit |
| --- | --- | --- |
| Claude Code | Native hooks, plus transcript background evidence | Turn completion, input, questions, permissions, background work; native child identities now participate in node status |
| Codex | Native hooks | Turn completion, input, questions, permissions, background work; question replies now resume the node |
| OpenCode | Project plugin events | Turn completion, questions and permissions |
| Kimi Code / Grok Code | Native hooks | Turn completion, input, questions and permissions; no general background capability advertised |
| Antigravity / Cursor | Native hooks with background evidence | Turn completion and background work; launch policy suppresses ordinary permission prompts |
| MiniMax Code / Cline | Plugin or native completion hook | Turn completion only is advertised; do not extrapolate request support |
| Command Code / Meta Muse | Passive session-log watchers | Supported terminal-turn evidence; watcher activation is not proof that a record arrived |
| DeepSeek Harness / Freebuff / Terminal | No supported lifecycle observer declared | Process lifecycle only; signal health remains unavailable |

Regression tests exercise route normalization and request/child ordering, lifecycle
transition effects, production SQLite writes and reads, schema upgrades, client
refresh ordering, and mobile cold-load rendering. The original Codex answer and
terminal-state regressions were observed failing before their fixes.

This work does not establish measured delivery reliability for every installed
CLI/version/platform. Existing adapters vary: Claude and Codex have native hooks;
OpenCode has a plugin stream; Muse and Command Code use passive logs; several
others expose only completion. Terminal, Freebuff, and unvalidated DeepSeek
profiles cannot report all four AI states. Their capability gaps must remain
visible. Older callbacks without optional identity fields remain best-effort;
an explicit mismatch is rejected, but missing identity is not fabricated.

Further work should validate real callback delivery across the supported fleet,
including restart and shared-workspace races, and finish extracting harness-owned
normalizers from the HTTP route. These are evidence and maintenance improvements,
not reasons to weaken circuit completion gates or label uncertain observations
as successful work.

Tracked follow-ups: [real-runner delivery validation #1964](https://github.com/alondero/buildmesh/issues/1964),
[durable observation revisions and incarnation fencing #1965](https://github.com/alondero/buildmesh/issues/1965),
and [harness-owned normalizers #1879](https://github.com/alondero/buildmesh/issues/1879).

## Reply controls follow the request

An `awaiting_input` status says the user is needed, not what the harness is asking
for. Mobile reply controls are chosen from the normalized kind, not from the bare
status (issue [#1966](https://github.com/alondero/buildmesh/issues/1966)):

| Observation | Reply control |
| --- | --- |
| `permission_requested` | Approve / Reject, which send `y\r` / `n\r` |
| `question_requested` with an enumerated answer list | The harness's answers as text, plus one open-to-answer action |
| `question_requested` with no answer list | One open-to-answer action |
| Unclassified, or no observation at all | One open-to-answer action |

`y` and `n` are a guess about a harness's own approval prompt, so they ship only
against a permission observation. An unclassified request has less evidence than a
question and gets the same refusal to guess. Answers the harness enumerated travel on
the observation as `request.choices`, parsed only from the structured
`questions[].options[].label` list the question text already comes from; an open
question, a permission decision, and an unparseable shape all carry no request at
all. Choices are displayed as the harness's own wording rather than as one-tap
actions, because a keystroke delivered to the PTY is not evidence that the harness
accepted an answer.

Delivered-action state is keyed to the request, not the node. A node stays
`awaiting_input` across request boundaries, so a harness that answers one question
and asks another never fires an intermediate transition; without a request key the
replacement would inherit the previous request's disabled controls after a
reconnect. The key is the observation's kind and timestamp, so a replaced request
clears stale state on its own.

The real dev-backend driver `tests/integration/ui-shot-node-status.steps.mjs`
sends synthetic native callbacks through HTTP, reads the committed observations,
and checks desktop/mobile rendering, per-request reply controls, a replaced request,
mobile reload and final-child completion.
It uses a short-lived pairing ticket for the mobile browser. This verifies the
production delivery/read path; it does not launch a real model or prove a CLI's
hook configuration. Synthetic nodes have no PTY, so opening their terminals can
produce expected resize errors and adds no lifecycle evidence.

Because that driver needs a running dev backend, nothing in `npm test` would
otherwise prove its selectors resolve — a gap that once shipped a driver whose
positive assertions could only time out and whose `toHaveCount(0)` negatives
passed vacuously, because Playwright's `getByTestId` matches the whole attribute
and the card renders node-scoped ids like `attn-approve-2`. The guard
`keeps the real-SPA driver's selectors in sync with the rendered cards` in
`tests/unit/mobile-node-list.test.tsx` reads the driver, renders each request
kind, and checks every selector against the `data-testid` values actually in the
DOM: a positive assertion must name an element that exists, a `toHaveCount(0)`
must name one that does not, and a selector naming a node the fixtures do not
define fails rather than passing. Renaming a testid without updating the driver
fails the unit suite.
