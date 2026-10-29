# ADR 0048: Result-file turn completion for harnesses without a lifecycle

Status: accepted

## Context

Autopilot Circuits advance an agent step when they learn that the agent's turn
ended. That signal came from an attention hook, a passive transcript watcher or
a native pull. Freebuff, Perch and DeepSeek Harness have none of these, so the
Circuit compatibility gate and the reviewer gate refused them, and the user
could not run them as an implementer or a reviewer.

Circuits already ask an agent with a result contract to write its final report
to a per-step result file, and only once it has finished
([circuits](../development/circuits.md), handoff files). The question was
whether that file is a sound turn signal, or whether another mechanism would be
better.

## Decision

The settled result file is a turn signal of last resort, available to every
agent harness. Only the plain shell cannot be asked for one.

- `HarnessCapabilities::supports_result_file_handoff` is true for every agent
  harness, and the shared gate predicate counts it, so both gates admit a
  harness with no lifecycle of its own.
- Such a harness owes a result file for every non-empty Circuit prompt, not only
  for gates with a result-line contract.
- The lost-turn watchdog publishes Ready through the fenced turn-recovery path
  once the owed file holds text, has been unchanged for five seconds and
  postdates the turn's lifecycle stamp. The rest of the pipeline (report binding,
  contract interpretation, bounded reminders, continuations) is unchanged.
- A yielded agent with no readable report binds the result file as its report,
  with the same session, incarnation, input and content fences.
- For a harness that does report its lifecycle, the same recovery is a fallback
  for a lost signal, and also requires a terminal quiet for one minute and no
  transcript activity newer than the file.

The developer contract is in
[result-file turn completion](../development/circuit-session-observation.md#result-file-turn-completion).

## Alternatives

- **Terminal silence or a PTY classifier as the turn signal.** TUIs redraw while
  idle and long tool calls are silent while busy. The observation contract
  already forbids inventing completion from generic silence; the existing
  quiet-turn classifier is a recovery aid, not a signal.
- **A sentinel line printed to the terminal.** The prompt itself contains the
  sentinel, so its echo is a false positive, and wrapping, alternate screens and
  scrollback make the line unreliable to find.
- **A per-harness transcript reader or watcher.** The best fidelity where a
  documented transcript exists, and still preferred when one is added. Freebuff
  and Perch document none, and this approach does not reach every harness.
- **Process exit.** Interactive harnesses keep one process across turns.

## Consequences

- The file proves a finished report and nothing else. Questions, permissions,
  progress and owned background work from a file-only harness are never
  observed; the diagnostics keep them Unavailable, and the active wait budget
  still bounds a turn that never writes its file.
- An agent that writes the file early and keeps working can be read as finished
  after the settle window. The prompt forbids it and the Ready publication is
  fenced to unchanged input, but nothing native detects it.
- A file-only agent that owes no file for a step (the source of a title-bar
  review, whose current turn the Circuit did not prompt) reports
  `TurnCompletionUnobservable` at once, and a person moves the step on.
- No schema or IPC command changes; the capability is one additive generated
  field, and the blocker is one additive generated variant.

## Verification

Rust tests pin the gate admission, the settle and freshness rules, the prompt
request for a file-only target, and a file-only handoff that binds its result
file and completes an issue-driven gate through the stepper. Live runs of
Freebuff and Perch inside a Circuit have not been recorded.
