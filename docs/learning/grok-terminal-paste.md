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
