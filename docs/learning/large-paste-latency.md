---
name: large-paste-latency
description: Where a large Windows terminal paste spends its time
metadata:
  type: reference
  date: 2026-09-24
---

# Large paste latency

A large clipboard paste can take seconds to show up in a provider prompt, one character at a time. Buildmesh delivers that paste as one write. The stall measured so far is in the provider's Windows console reader.

## What Buildmesh does

Desktop xterm emits one data event for a paste. `TerminalRegistry` sends that string in one `write_to_agent` call. The command enqueues those bytes as one message, and `pty_writer_thread` writes them with one `write_all`. A multi-line paste keeps its bracketed-paste markers (`ESC [ 200 ~` … `ESC [ 201 ~`) inside that same buffer. The mobile terminal sends the same xterm data event as one websocket message, which the server also enqueues with one `write_bytes` call.

The channel bound is 64 messages. A paste is one message no matter how large the text is. Splitting it into keystroke-sized writes would feed a one-record console reader more slowly and would break the rule that a multi-line buffer arrives as one write.

`write_to_agent_delivers_a_large_paste_as_one_pty_write` locks this at the production writer. It registers an agent through `register_agent` (the real `pty_writer_thread`, not a test-local channel loop) and writes a 17,508-byte multi-line paste, the size recorded for issue #1498. The recording `Write` must see one `write` of those exact bytes before `flush`.

## Where the time goes

Issue #1498 records a Windows ConPTY trace against Codex 0.152.1: that 17,508-byte paste was one write of about 15 ms, and Codex emitted its collapsed paste placeholder about 4.19 seconds later. This document does not repeat that live trace. It records the provider mechanism that matches it.

Codex on Windows reads console input through crossterm's Win32 event source. That source calls `GetNumberOfConsoleInputEvents` and then a one-record `ReadConsoleInputW` for every event. A large paste is thousands of those transitions before Codex's paste-burst timer can collapse the text into a placeholder. The batch-read proposal is [openai-oss-forks/crossterm#4](https://github.com/openai-oss-forks/crossterm/pull/4), linked from [openai/codex#14099](https://github.com/openai/codex/issues/14099). The pull request's isolated benchmark, on Windows 11 with 17,496 synthetic key records, measured a median of about 3.1 seconds on the unpatched async path Codex uses and about 4 milliseconds after one batched `ReadConsoleInputW`. That benchmark is a private console input queue, not a Windows Terminal clipboard paste.

## What to leave alone

Leave paste bytes intact on the way to the PTY. A fix for the multi-second stall belongs in the provider's Windows console reader, which has to drain the available input records in one call and then parse them. Pacing or slicing the paste in Buildmesh would still present one record at a time to that reader.
