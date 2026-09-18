# 36. Emulated macOS traffic-light affordances for the bespoke title bar

## Status

Accepted (2026-09-18). Scoped to the macOS traffic lights; the Windows/Linux
caption controls keep the treatment [ADR-0035](0035-native-windows-caption-button-affordances.md)
gave them.

## Context

The window runs frameless (`"decorations": false`), so `TitleBar.tsx` draws its
own window controls. On macOS that means three hand-drawn circles standing in
for the traffic lights AppKit would otherwise draw. They had the right three
colours and revealed their glyph on hover, but the system's state vocabulary is
wider than that, and the missing states are the ones that carry meaning:

- **The glyph reveal is per *cluster*, not per button.** macOS shows the × / − /
  + together as soon as the pointer enters the strip. Ours revealed exactly one,
  under the pointer — a web tooltip behaviour, not a platform one.
- **There is no pressed state.** The fill darkens while a light is held.
- **An unfocused window paints all three lights one flat grey**, with no glyphs.
  This is the cue for which window is taking keystrokes, and it was absent
  entirely; the strip looked equally "live" whichever window was in front.
- **There is no hover brightening on macOS.** Ours darkened the fill on hover
  (`hover:brightness-[0.92]`), which no system light does — the revealed glyph is
  the hover cue.
- **The lights carried an HTML tooltip** (`title`), which native lights do not;
  ADR-0035 dropped the same attribute from the caption buttons for the same
  reason.
- **They were 9.75px, not 12px.** `w-3` / `h-3` compile to `0.75rem`, and this
  app's root font is 13px — the same rem trap that already forced the caption
  glyphs to `h-[16px]` literals. Every other window on the machine draws 12px
  circles, so the strip read as slightly undersized next to them.

macOS is the one platform where a genuinely native answer is available:
`decorations: true` with `titleBarStyle: "Overlay"` and `hiddenTitle: true`
returns the lights to AppKit with every one of the behaviours above, and VS Code
ships exactly that. It is declined here for layout and cost reasons rather than
capability ones — see **Alternatives considered**.

## Decision

- **Emulate the system's state set on the hand-drawn lights** rather than
  reintroducing a native frame. macOS draws the same red / yellow / green in both
  appearances, so the fills, pressed fills and glyph tints are theme-independent
  tokens in `App.css`; only the inactive grey is themed.
- **Move the hover `group` from each light to the cluster.** One group is what
  makes the pointer entering the strip reveal all three glyphs, which is the
  native behaviour and the reason a per-button group looked wrong.
- **Grey the whole strip while the window is unfocused**, and restore a light's
  colour under the pointer. Gating on the cluster rather than a listener means
  the grey is a render state, not an event.
- **Draw the circles at the native 12px, as pixel literals** (`h-[12px]
  w-[12px]`), with the glyph rescaled into a 12-unit box so its coordinates are
  real pixels. Any rem-based step here is 3.25px-multiplied and wrong.
- **Drop the tooltip.** `aria-label` is the only accessible name, matching
  ADR-0035's caption buttons.

Invariants this creates:

- **One focus read, two consumers.** `useWindowFocused` now drives both families
  — the caption glyphs' dimming and the traffic lights' grey — so the two
  branches cannot disagree about whether the window has focus. It needs no new
  capability: `isFocused` / `onFocusChanged` are already granted for ADR-0035.
- **The macOS branch still owns nothing about window state.** The zoom light
  keeps calling the same `toggleMaximize` the caption button calls, so
  `isMaximized` keeps its single writer (the `onResized` re-query).
- **The state classes must stay literal strings.** Tailwind v4's source scanner
  compiles what it finds in the source; a template-built `group-hover:bg-…` would
  never emit a rule, and the light would silently keep one state forever.
- **The restore-on-hover fill is deliberately only present while inactive.** A
  focused light already paints its own fill, so adding the same colour twice is
  noise in a class contract the tests read by token name.

## Alternatives considered

- **`decorations: true` + `titleBarStyle: "Overlay"`** (VS Code's approach). The
  real lights, zero emulation, and it would also supply the behaviours noted
  below. Rejected on three counts:
  - `decorations` is a single cross-platform field. Enabling it for macOS needs a
    `tauri.macos.conf.json` overlay that redeclares the entire `app.windows`
    array, because config arrays **replace** rather than merge — the same trap
    that once cost the dev profile its `decorations: false`. It would then need
    the same redeclaration wherever the dev/release overlays are layered, plus a
    new guard test to keep them in step.
  - Native traffic lights sit near the top of a 28px title-bar strip. This bar is
    45.5px tall with its content centred, so the lights would read roughly 9px
    high against the wordmark and the utility pills. Moving them means owning
    AppKit button-frame code for a cosmetic offset.
  - It cannot be verified in the environment this work was done in, and the
    failure mode is broken window chrome for every macOS user rather than a wrong
    shade on one button.
- **Vendor a plugin for traffic-light insets** (`tauri-plugin-decorum` and
  friends). Rejected for the reason ADR-0035 gave: these plugins want to own the
  buttons and the title bar, not just one property, and adopting one would mean
  restyling its DOM and rewriting the pinned title-bar contracts.
- **Fix only the inactive dim.** Considered as the smallest change that matches
  the reported symptom. Rejected because the cluster glyph reveal, the pressed
  fill and the 9.75px diameter are the same defect — a light that is almost
  right — and they all land in the same component, so splitting them buys
  nothing but three review rounds.

## Consequences

- **The emulation covers the visual state set, not the behaviours.** Missing, and
  deliberately so:
  - **The green button's meaning.** Natively it enters full screen, and
    Option-click zooms; ours calls `toggleMaximize()` (zoom) for both. Changing
    it would move the window to a macOS space and hide the title bar, which is a
    behavioural change to the whole shell rather than a chrome fix.
  - **The press-and-hold tiling menu on green** and the Option-modified zoom
    glyph. The menu is `NSWindow._windowTilingMenu`, a private AppKit menu
    rendered out of process, so it is unreachable without private API.
  - **VoiceOver and the unsaved-document dot on close**, and `⌘W` / `⌘M`, which
    are app-level shortcuts living in a different surface.
- **The restore-on-hover fill is best-effort.** An unfocused macOS window may not
  deliver hover events to the webview at all, in which case the grey simply
  stays — which is the native look for the common case (a window you are not
  looking at) regardless.
- **The lights remain real `<button>`s and therefore tabbable**, which native
  traffic lights are not. Kept on purpose: they are hand-drawn, so a keyboard
  user has no other route to close or minimise the window.
- **Tokens are the contract.** The tests assert token names, never hexes, so a
  changed fill cannot leave its pressed or inactive variant behind.
- **No capability or ACL change.** The command surface is untouched; the focus
  read was already granted for ADR-0035.

Verification evidence: `tests/unit/title-bar.macos.test.tsx` (native geometry,
cluster glyph reveal, pressed fill, the inactive grey from both a focus event and
the mount-time query, the dropped tooltip, and the glyph box). The states were
not verified against a running macOS build — this work was done on Windows, and
that gap is recorded here rather than implied away.

## References

- [Supporting notes and state table](../learning/macos-frameless-traffic-lights.md)
- [ADR-0035](0035-native-windows-caption-button-affordances.md) — the Windows
  caption buttons these controls sit opposite
- `src/components/TitleBar/TitleBar.tsx`, `src/hooks/useWindowFocused.ts`,
  `src/App.css`
- [NSWindow.ButtonType](https://developer.apple.com/documentation/appkit/nswindow/buttontype) —
  the system's own names and roles for the three lights
