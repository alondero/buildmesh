# Codex Circuit paste confirmation

Evidence captured on 2026-10-08 with Codex CLI 0.160.1 on Windows, using
native ConPTY and one bracketed-paste write followed by a separate Enter.

## Rendering contract

Codex can display a pasted prompt in full or as a sequence of inline text and
collapsed bursts. A whole-prompt marker and an inline prefix followed by one
suffix marker are two instances of that sequence. Burst boundaries depend on
input timing, so confirmation cannot assume a single collapsed burst.

The [Codex composer source](https://github.com/openai/codex/blob/rust-v0.160.1/codex-rs/tui/src/bottom_pane/chat_composer.rs)
describes Windows input arriving as character events. The
[burst detector](https://github.com/openai/codex/blob/rust-v0.160.1/codex-rs/tui/src/bottom_pane/paste_burst.rs#L162-L170)
uses a 60-millisecond idle timeout on Windows; each flush is integrated
independently. This permits several markers from one PTY write.

The count convention comes from
[`apply_paste`](https://github.com/openai/codex/blob/rust-v0.160.1/codex-rs/tui/src/bottom_pane/chat_composer/paste_input.rs#L118-L146):
it converts CRLF and bare CR to LF, sanitizes control text, then counts Rust
`chars()` (Unicode scalar values, not UTF-8 bytes or display columns). Counts
above 1,000 produce a placeholder. The
[placeholder allocator](https://github.com/openai/codex/blob/rust-v0.160.1/codex-rs/tui/src/bottom_pane/chat_composer.rs#L1931-L1954)
puts duplicate-size ordinals after the closing bracket, for example
`[Pasted Content 1500 chars] #2`.

The pinned
[`sanitize_user_text`](https://github.com/openai/codex/blob/rust-v0.160.1/codex-rs/tui/src/history_cell/messages.rs#L24-L50)
removes `ESC [` through the first ASCII character in `@` through `~`. An
unfinished sequence consumes the remaining paste. Other control characters,
including NUL, DEL and C1 controls, are removed; tabs and newlines remain.
An ESC followed by something other than `[` removes only the ESC, retaining
the printable text that follows. Buildmesh applies these rules to the Codex
prompt body before the PTY write, then counts that same text, builds its
visible-text proof and precomputes segment boundaries.

Sanitizing only the expected text is insufficient: Codex sanitizes each burst
independently. If a trailing `ESC [31m` is cut after `ESC [3`, the first burst
discards that fragment and the next retains printable `1m`. The first marker
could then appear to count the entire globally sanitized prompt while input
still remained. Transforming the body before adding the bracketed-paste
protocol removes CSI/control fragments before burst boundaries can affect them.

## Reproduction and regression

A 5,385-character multiline prompt at 79 columns displayed its first 119
characters inline, followed by `[Pasted Content 5266 chars]`. At 22 columns,
the same prompt displayed a wrapped `[Pasted Content 5385 chars]` marker.
These captures establish the single-burst rendering shapes; they do not
establish how often a busy machine splits the input into several bursts.

The previous Circuit gate accepted a whole-prompt marker or the prompt's visible
ending. Neither was present in the split rendering, so it withheld Enter until
the readiness budget expired. Replaying the captured composer text through
production staging and readiness reproduced the failure before the fix.

PR #2132's initial fix still rejected two collapsed bursts with inline text
between them. A production staging/readiness regression with two synthetic
500-character markers and ten intervening characters reproduced its timeout
without Enter. Those small markers minimize the matcher failure; Codex's
actual threshold is higher. The production-path regression uses two
1,001-character bursts and confirms one separate Enter. Several-burst frames
are source-backed synthetic fixtures, not live captures.

`circuit::delivery` walks the sequence backward, accounting for each marker's
count and each expected inline segment. A marker's count and ordinal identify
one burst; same-size ordinals must increase in prompt order, so repeated
redraws or a reversed interpretation of literal `#2` cannot contribute its
count twice. Ordinal
parsing retains the alternative that a following `#42` is literal inline
prompt text until expected-text matching resolves it. Digit boundaries are
also explored: `] #242` can mean ordinal 2 followed by literal `42`.
Normalized prompt text and Unicode
boundary offsets are computed at staging. Existing whole-marker and fully
inline checks remain available. Fresh-output, quiet-redraw, and input-ownership
fences still apply. Regressions cover two and three bursts, duplicate-size
ordinals, repeated redraws, literal inline `#42` and `#2`, digits after an
ordinal, adjacent markers,
an inline ending, wrong counts, missing or changed
segments, repeated fragments, hidden CRLF, Unicode, stale output and user input.
Equivalent normalized boundary states are deduplicated; within an ignored
whitespace span, the largest valid boundary admits the earlier boundaries too.
A rejected frame with five markers and long ignored text pins this failure path.
Ordinal history retains only the smallest later ordinal for counts that can
still recur earlier. A rejected 30-marker frame with ambiguous numeric labels
pins that independent source of duplicate search paths.
Recurrence can still leave many independent numeric interpretations, so each
snapshot also has a finite state budget and checks the enclosing readiness
deadline during reconstruction. Exhaustion leaves the paste unconfirmed;
Enter is withheld until a later snapshot proves it or the readiness budget
expires. A 60-marker rejected fixture asserts state-budget exhaustion.

## The omitted newline in the native submission

The separate native submission smoke used the same 5,385-character payload,
but its rollout contained 5,384 characters. Comparing the complete user text
locates exactly one deletion: the LF at zero-based character 34, after
`Please review this example change.`. It was in the inline prefix.

That run's composer displayed 142 inline characters and a 5,242-character
suffix marker. The corresponding expected prefix has 143 characters including
the omitted LF; the expected suffix still has exactly 5,242. Removing whitespace
gives identical inline-prefix proofs, so this deletion does not shift the
suffix boundary. The captured-frame regression pins those literal lengths
and that wrapped marker. The earlier 119/5,266 frame is a different run.

The active-burst key path appends Enter as LF before placeholder counting;
the observed missing LF was outside the collapsed suffix. This evidence does
not establish a rule permitting arbitrary newline loss inside hidden bursts.
The matcher applies counts to newline-normalized, sanitized character boundaries
without adding count tolerances. Boundaries differing only in ignored
whitespace or punctuation remain indistinguishable in rendered output.

## Control-character regression

The multi-burst revision still counted unsanitized input. A 1,028-character
prompt containing `Review this change`, LF, 1,000 `x` characters, `ESC [31m`
and `tail` is 1,023 characters after removing the five-character color
sequence. The production staging/readiness regression timed out on the reviewed
revision before the fix. It now pins the literal sanitized PTY body,
1,023-character marker and separate Enter. Trailing SGR and unfinished CSI
before Unicode also assert the entire canonical transmitted body, preventing
the premature first-burst proof. Control-bearing fixtures cover
sanitized inline text and burst suffixes, retained tabs/newlines, Unicode,
non-CSI escape text, unfinished CSI sequences and incorrect marker counts.
These are synthetic composer traces, not live control-character captures.

Rendered confirmation cannot verify the contents hidden behind a marker or
provide byte-exact delivery. The standalone native smoke retained the complete
normalized prompt and accepted separate Enter. No packaged Buildmesh runtime
smoke or live multi-burst capture is claimed.
