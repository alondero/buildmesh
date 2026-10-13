# Grok terminal paste

Native Windows Grok can treat a multiline terminal paste as incremental input
even when Buildmesh sends the entire payload in one bracketed write. Desktop
clipboard actions now send Ctrl+V (`0x16`) to Grok so its native clipboard reader
inserts the content together. This does not press Enter, modify the clipboard,
or change programmatic prompt injection.

## Investigation (2026-09-29)

The installed Grok CLI initially reported 1.0.41 and automatically updated to
1.0.44 during investigation. The final comparison used 1.0.44
(`5b807183dd79`) with its updater disabled, and the same Microsoft ConPTY
1.24.260710001 runtime bundled by Buildmesh, with flags `0x2 | 0x4`.

A scratch Win32 probe launched the interactive CLI with a 140-by-35 console,
waited for startup, and staged synthetic diagnostic text without submitting it.
One `WriteFile` containing bracketed text still populated the composer
incrementally. CR versus LF did not restore a single paste. Encoding delimiters
as Win32 input events also failed and exposed literal delimiter text. The live
probe reproduced loss of an atomic paste, but not the reported hundreds of
separate paste chips on this CLI version.

Sending Ctrl+V with 100 lines (17 KB) on the desktop clipboard produced one
`[Pasted: 17 KB]` block with a preview of the first and last lines and
`(94 more lines)`. The probe preserved and restored the clipboard formats and
terminated its own CLI process. No diagnostic prompt was submitted.

Microsoft documents the underlying console limitation in
[terminal issue 18094](https://github.com/microsoft/terminal/issues/18094):
bracketed-paste delimiters are filtered when virtual-terminal input is disabled.
This is consistent with the observed Windows input behavior; the probe did not
inspect Grok's private console-reader implementation. Grok's installed keyboard
guide documents its native clipboard command and the Windows terminal's normal
interception of Ctrl+V.

## Scope and regression coverage

Only local Windows Grok clipboard gestures use native paste. WSL and non-Windows
hosts retain xterm paste; mobile retains its websocket text path. File-drop and
programmatic pastes retain their supplied text instead of reading unrelated
desktop clipboard content. Failed native delivery reports the error without
replaying text through another route.

Unit coverage exercises the registry's actual shortcut callback, shared
clipboard method, platform/harness selection, and clipboard fallback. Browser
coverage exercises real xterm keyboard, context-menu, and DOM paste handling
against mock IPC; it does not prove backend or CLI behavior.

The before/after screenshots under `docs/pr-screenshots/grok-paste/` replay the
captured live Grok console output in xterm at the original 140-by-35 dimensions.
They show the CLI comparison, not a running Buildmesh backend. The after replay
asserts the rendered buffer contains one complete paste marker and the
100-line preview. The before capture shows the stream still populating the
composer after 30 seconds.

## Programmatic pastes: line endings (2026-10-10)

"Handover to node" and other backend injection reach Grok as a bracketed paste,
not a clipboard gesture, so the Ctrl+V route above never applied to them. A
handover into Grok split into one message per line.

A scratch ConPTY probe (140-by-35, flags `0x2 | 0x4`, run on both the inbox host
and Buildmesh's bundled 1.24.260710001 runtime with identical results) wrote a
12-line synthetic paste inside `ESC[200~ … ESC[201~` to Grok 1.0.50 and read the
rendered composer. Only the line endings differed:

| Line endings in the paste | Result |
| --- | --- |
| LF | Every line lands in the composer, newlines dropped, lines glued together. |
| CRLF | Line 1 is submitted, Grok replies, the remaining lines queue as separate messages. |
| CR | One composer draft with every newline intact; a later Enter queues it as one message (`+11 lines`), also while Grok is mid-turn. |

xterm.js joins a selection with `\r\n` on Windows, and `handover_to_agent`
forwarded `getSelection()` unchanged, which is the CRLF row. Circuit prompts are
LF, the glued row. The frontend's own paste never hit this because xterm.js
rewrites every line break to CR before it emits a paste.

This differs from the 2026-09-29 note that CR versus LF did not restore a single
paste: that comparison watched the composer fill incrementally, this one compares
the finished composer and what was submitted.

The fix is the adapter capability `paste_requires_cr_newlines`
(`AgentProvider`), true for Grok and Codex, applied by
`circuit::delivery::PreparedPrompt::prepare`.
On Windows (and Windows-hosted WSL instances), ConPTY key-event translation causes
LF to drop newlines and CRLF to submit prematurely; across all platforms (Windows,
macOS, Linux), manual user pastes in xterm.js rewrite newlines to CR by default
(`replace(/\r?\n/g, "\r")`), so delivering CR mirrors xterm.js's standard paste behavior.
The Unix basis is inferred from this manual-paste behavior and has not yet been directly
probed on native Linux or macOS PTYs. Circuit prompt dispatches (turns, continuations, and nudges)
and terminal handovers go through this preparation. The coordinator's `AgentDriver::send_prompt`
is single-line by contract and bypasses staged delivery. Other harnesses keep their line endings
as given; any prompt containing CR or LF is treated as multiline, bracketed by
`injection_payload`, and subject to non-generic paste gates.

### Codex 0.162.1, same probe (fixed in #2199)

Running the same scratch ConPTY probe (140-by-35, flags `0x2 | 0x4`, bundled 1.24.260710001
runtime writing a 12-line bracketed paste inside `\x1b[200~ … \x1b[201~`) to Codex 0.162.1:

| Line endings in the paste | Result in Codex composer |
| --- | --- |
| LF | Every line lands in the composer, newlines dropped (lines glued together). |
| CRLF | Composer draft preserves multiline layout across all lines. |
| CR | Composer draft preserves multiline layout across all lines. |

Nothing was submitted in any run. LF dropped the newlines (lines glued together);
CRLF and CR both kept the lines. Before #2199 a Windows handover into a standalone
Codex node (outside any circuit) was the one preserved case: it bypassed the
split-marker gate and kept the CRLF a selection arrives with. `PreparedPrompt::prepare`
resolves the adapter from the database row for *every* node it can identify, not only
circuit-piloted ones, so the new policy normalizes those endings to CR as well —
preserved by the same evidence, but now by the same mechanism rather than by accident
of the path taken. Only a node absent from the database keeps its text as given (the
best-effort arm in `prepare`).

Codex now opts into `paste_requires_cr_newlines`, and the split-marker gate keeps
two texts apart instead of reusing one. `codex_sanitized_text` is the **payload**:
control-stripped, with the line endings `prepare` resolved, so the PTY receives CR.
`codex_composer_text` is the **proof**: the same text with CRLF and bare CR collapsed
to LF, which is exactly what Codex's `apply_paste` holds and counts. Because the
composer normalizes before counting, `[Pasted Content N chars]` is identical for
LF, CRLF and CR payloads, so the CR payload moves no byte of the gate's proof — this
answers the issue's question directly: Codex needs no custom burst-boundary handling,
only CR line endings on the wire.

Regressions pin both halves: `codex_circuit_multiline_delivery_stages_cr_and_never_bare_lf`
asserts the literal staged bytes for LF, CRLF and CR input, and
`codex_split_marker_count_is_identical_for_cr_crlf_and_lf_payloads` asserts one count
and one marker match across all three. Both run in CI; they pin the byte stream and the
proof, which is all the fix depends on.

The live ConPTY probe that produced the table above is a scratch harness and is not
checked in. Re-recording it — and the measurement problems that come with reading a
TUI transcript as text — is tracked in [#2290](https://github.com/alondero/buildmesh/issues/2290).
