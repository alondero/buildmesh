# Codex Circuit paste confirmation

Evidence captured on 2026-10-08 with Codex CLI 0.160.1 on Windows, using
native ConPTY and one bracketed-paste write followed by a separate Enter.

## Rendering contract

Codex can display a pasted prompt in full, collapse the whole prompt, or leave
an inline prefix and collapse only its suffix. Its marker counts the collapsed
text, so that count need not equal the complete prompt length.

The [Codex composer source](https://github.com/openai/codex/blob/rust-v0.160.1/codex-rs/tui/src/bottom_pane/chat_composer.rs)
describes Windows input arriving as character events and being grouped into
paste bursts. Each large burst gets a placeholder whose count describes that
burst. This explains the split observed in the native trace.

## Reproduction and regression

A 5,385-character multiline prompt at 79 columns displayed its first 119
characters inline, followed by `[Pasted Content 5266 chars]`. At 22 columns,
the same prompt displayed a wrapped `[Pasted Content 5385 chars]` marker.

The previous Circuit gate accepted a whole-prompt marker or the prompt's visible
ending. Neither was present in the split rendering, so it withheld Enter until
the readiness budget expired. Replaying the captured composer text through
production staging and readiness reproduced the failure before the fix.

`circuit::delivery` now confirms the complete normalized inline prefix directly
before a suffix marker, using its count to infer the split point. Existing
whole-marker and fully inline checks remain available. Fresh-output, quiet
redraw, and input-ownership fences still apply. Regression tests cover one
separate Enter, incorrect counts, incomplete prefixes, repeated fragments,
CRLF, Unicode, stale output, and intervening user input.
