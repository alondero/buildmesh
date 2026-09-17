# 35. Native Windows caption-button affordances for the bespoke title bar

## Status

Accepted (2026-09-17). Scoped to the Windows caption buttons; macOS keeps the
hand-drawn traffic lights unchanged.

## Context

The window runs frameless (`"decorations": false`), so `TitleBar.tsx` draws its
own minimise / maximise-restore / close controls as ordinary HTML buttons
calling `getCurrentWindow().minimize() / toggleMaximize() / close()`. Those calls
work, but the buttons were missing most of the affordances a Windows 11 caption
button has — no hover backplates in the system's own vocabulary, no pressed
state, no inactive-window dimming, glyphs drawn with heavy stroke geometry, HTML
tooltips native buttons do not have, and most visibly **no Snap Layouts flyout**
on hover over maximise.

The last one is a hard wall rather than a styling gap. Windows 11 only offers the
flyout to a window whose `WM_NCHITTEST` answers `HTMAXBUTTON`, and the page lives
in a WebView2 child HWND covering the client area, so that child answers the hit
test and the Tauri window's procedure is never consulted. Neither
[tauri#4531](https://github.com/tauri-apps/tauri/issues/4531) nor
[winit#3884](https://github.com/rust-lang/winit/issues/3884) is going to change
that: both are about the framework shipping a first-class API.

## Decision

- **Fix the visuals in CSS, and the flyout natively.** The two are independent
  problems and only the second needs Win32.
- **Vendor the snap overlay into `src-tauri/src/windowing/`** rather than
  adopting a plugin. It is ~150 lines behind a Windows-only module with a no-op
  shim, reusing the `windows-sys` dependency the crate already declares.
- **Measure the button from the DOM; never hardcode its geometry in Rust.** The
  frontend reports the button's box in logical pixels once on mount and again on
  resize; the backend applies the window's DPI scale and repositions the overlay
  on `WM_SIZE` / `WM_DPICHANGED` from the stored right-inset. The inset is taken
  from the root element's *fractional* right edge rather than `window.innerWidth`,
  whose integer rounding biases it by up to a pixel at fractional display scaling
  — enough to drift the overlay onto the neighbouring close button.
- **Geometry and states follow VS Code's window controls**, which the title bar's
  design already takes as its reference: 46px full-bleed backplates forming the
  standard 138px cluster, translucent hover/pressed fills, the shell's fixed red
  for close, and the codicon `chrome-*` outlines at 16px. The glyphs also dim
  while the window is inactive (`useWindowFocused`). Native caption buttons carry
  no tooltip, so the `title` attributes are gone and `aria-label` is the sole
  accessible name.
- **Keep the title bar bespoke.** No `decorations: true`, no native frame, no
  plugin-owned DOM.

Invariants this creates:

- The overlay owns the mouse in its rectangle, so the maximise button's DOM
  `:hover`, `:active`, and `onClick` do not fire on Windows. Hover and press come
  from `titlebar-overlay:*` events; a mouse click routes through the same
  `toggleMaximize` the DOM handler calls. Keyboard activation is unaffected.
- The maximise **glyph** is still written only by the `onResized` re-query. The
  overlay adds a second *caller* of `toggleMaximize`, never a second writer of
  window state.
- The overlay is created lazily on the first metrics report, and is a no-op off
  Windows.
- **Its teardown hangs off destruction, not the close request.** Buildmesh vetoes
  the close request while the exit-confirmation modal is up (`WindowCloseGuard` →
  `cancel_window_close`), which makes `WM_CLOSE` advisory. Tearing down there
  would remove the overlay *and* its `WM_SIZE` subclass on a cancelled exit —
  silently killing Snap Layouts until something happened to trigger a
  re-measure. Teardown runs on `WM_NCDESTROY`, which only fires when the window
  really is going away.
- **A minimized window keeps the overlay hidden, never misplaced.** The client
  area is empty while minimized, so the positioning arithmetic would place the
  overlay at a negative x. `reposition` hides it instead and the restore's
  `WM_SIZE` re-shows it; the pure rect maths additionally floors x at the client's
  left edge, which is the half of that fix that can be unit-tested off Windows.

## Alternatives considered

- **`tauri-plugin-frame` (or decorum / window-controls / snap-layout).** The
  same technique, maintained by someone else — genuinely attractive. Rejected
  because `create_overlay_titlebar*` also evaluates its own `controls.js` /
  `titlebar.js`, which **inject the plugin's own `frame-tb-*` caption buttons**:
  it wants to own the buttons, not just the hit test. It also requires
  `withGlobalTauri: true` (currently unset, and a real surface change with
  `csp: null`) and pulls in `eyre` + `raw_window_handle`, neither of which is in
  the tree. Adopting it would have meant restyling its DOM and rewriting the
  pinned title-bar contracts.
- **Subclass the Tauri window and answer `WM_NCHITTEST`.** Microsoft's
  documented approach, and structurally impossible here — the WebView2 child
  answers first. See the failure signature in the learning note.
- **`decorations: true` + `DwmExtendFrameIntoClientArea`**, letting Windows draw
  the caption buttons. Fully native and zero custom Win32, but it reintroduces
  the native frame, puts the buttons in a 32px strip at `y = 0` with
  OS-controlled size and theme, and fights the bar's 45.5px height. Rejected as
  the thing the title bar exists to avoid.
- **Visuals only, deferring Snap Layouts.** Considered as a smaller first step;
  rejected because the flyout is the affordance users actually notice, and the
  visual work lands in the same component either way.

## Consequences

- **We own ~150 lines of `unsafe` Win32.** Consistent with the crate's existing
  Windows FFI (`sandbox::restricted_token`, `http::interface_rank`), but bug
  fixes in this area are ours. `windowing::snap_overlay` carries the style-set
  constraints inline because they are not discoverable from the code.
- **The DPI-scaled rect maths is unit-tested; the Win32 half is not.** It cannot
  be — and the flyout cannot be captured by a screenshot (it is a separate OS
  window), so `PrintWindow`-based verification produces false failures. Manual
  verification on Windows 11 is required for anything touching the overlay.
- **`minWidth: 900` caps snapping in practice.** The flyout appears, but zones
  narrower than 900px cannot accept the window; Microsoft's guidance is ≤500
  effective pixels, ideally ≤330. Deliberately left alone — it is a constraint of
  the bar's layout, not something to change as a side effect of a chrome fix.
- **The right-click system window menu is still missing.** Native Windows also
  shows Restore/Move/Size/Min/Max/Close on a right-click of the title bar. Same
  family of problem, separate Win32 work, out of scope here.
- **The inactive-window treatment is deliberately narrow.** Caption *glyphs* dim
  on focus loss, and only the glyphs — the backplate stays full-strength, as it
  does natively, so hovering an inactive window's button still reads as a live
  control. Two things were not taken: VS Code drops its whole title bar to 0.6
  opacity, which here would dim the wordmark and the toolbar with it, and macOS
  traffic lights would go grey rather than dim — but this ADR scopes itself to
  the Windows/Linux controls and leaves that branch alone.
- No capability or ACL change was needed: the command is an application command
  (not ACL-gated) and event listening is covered by `core:default`.

Verification evidence: `cargo test --lib windowing` (DPI/rect maths), the
`caption buttons` cases in `tests/unit/title-bar.test.tsx`, and
`tests/unit/use-window-control-overlay.test.tsx`; manual checks on Windows 11 for
the flyout itself, per the note above.

## References

- [Supporting notes and failure catalogue](../learning/windows-frameless-snap-layouts.md)
- `src-tauri/src/windowing/`, `src/hooks/useWindowControlOverlay.ts`
- [ADR-0030](0030-titlebar-navigation-on-demand-inspector.md) — the title-bar
  navigation model these controls sit alongside.
