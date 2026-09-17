# Snap Layouts in a frameless Tauri window — what actually works

Learned making the bespoke title bar's caption buttons behave like real Windows
11 ones (ADR-0035, `src-tauri/src/windowing/`). This is a wall that costs days
if you attack it the way the documentation suggests, so the useful content is
mostly "what cannot work, and how to recognise that you are in it".

## Symptom → cause → fix

### 1. Hovering the maximise button shows no Snap Layouts flyout

**Cause.** Windows 11 only offers the flyout to a window whose window procedure
answers `WM_NCHITTEST` with `HTMAXBUTTON` for the maximise button's rectangle.
That is a Win32 return value: there is no DOM API, CSS property, or Tauri config
key that produces it, because the frontend is not a participant in that
conversation.

**The trap.** Microsoft documents the fix as "handle `WM_NCHITTEST` in your
window procedure". In Tauri that cannot work, and it fails *invisibly*. The page
lives in a WebView2 child HWND (`Chrome_RenderWidgetHostHWND`) that covers the
entire client area, so when the cursor is over the button, Windows asks **that**
window what is underneath. The Tauri window's procedure is never consulted.
Subclassing does not rescue it either: Chromium routes input through its own
descendants, so `SetWindowSubclass` lands on the wrong window, and forcing its
`WndProc` breaks rendering.

**How to tell you are in this trap:** your button still shows its CSS `:hover`
state and its HTML `title` tooltip. Both of those require the *webview* to have
received the mouse, which means your window did not.

**Fix.** Create your own small native child HWND parked exactly over the
maximise button, whose procedure returns `HTMAXBUTTON` unconditionally. It never
paints, so the design shows through. See `windowing::snap_overlay` for the
working implementation and the exact window style set.

### 2. "But tauri#4531 says `status: upstream`"

Both [tauri#4531](https://github.com/tauri-apps/tauri/issues/4531) and
[winit#3884](https://github.com/rust-windowing/winit/issues/3884) are real, and
both are about the *framework* shipping a first-class API. Neither says anything
about what your application can do — nothing stops an app creating its own Win32
window alongside Tauri's. Reading `status: upstream` as "impossible until winit
changes" is the expensive misreading here; it is why people conclude they should
switch to Electron.

Five independent Tauri plugins converged on the identical overlay technique,
which is reasonable evidence there is no other route.

### 3. The flyout appears but the window will not snap into a zone

Microsoft's own guidance: a window whose **minimum width exceeds ~500 effective
pixels** can invoke the menu but cannot fit the narrower zones. The ideal is
330 epx or less. Buildmesh's window is `minWidth: 900`, so the flyout appears and
zones narrower than 900px reject the window. That is an accepted trade-off from
the bar's layout, not a bug in the overlay — don't chase it.

### 4. The button goes visually dead once the overlay is installed

Expected. The overlay **owns the mouse** in its rectangle, so the button's DOM
`:hover` and `:active` never fire, and neither does `onClick`. The overlay has to
report hover, press, and click back to the page as events
(`titlebar-overlay:*`), and the frontend drives the button state from them.
Keyboard activation still goes through the DOM, so both paths must stay wired.

This is unavoidable with any overlay approach, plugin or hand-rolled.

### 5. Verify with the OS, never with a screenshot

`PrintWindow` cannot capture the flyout: it is a separate OS window owned by the
shell, so screenshot-based verification produces confident false failures. The
same applies to rounded corners, which DWM composites rather than paints. Assert
against the live window (hit-test results, window rects) or verify by hand.

### 6. Snap Layouts stops working after the user cancels an exit

**Cause.** The overlay's teardown was hung off `WM_CLOSE`. In Buildmesh that
message is *advisory*: `WindowCloseGuard` vetoes the close request whenever the
exit-confirmation modal is up (`event.preventDefault()` → `cancel_window_close`),
so the window survives while the overlay **and** its `WM_SIZE` subclass are
already gone. Snap Layouts then silently stops working — and unlike most of the
failures here it can look like it recovered, because any later resize re-runs the
frontend's metrics report and reinstalls the overlay.

**Fix.** Hang teardown off `WM_NCDESTROY`, which only fires when the window is
genuinely going away. More generally: never tie a native child's lifetime to an
advisory message. Note that the *confirmed*-exit path never sends `WM_CLOSE` at
all — `exit_application` calls `AppHandle::exit(0)` — so on `WM_CLOSE` the veto is
the only thing that ever happens. This is worth knowing because it means the bug
fires on essentially every cancelled exit, not on some rare race.

### 7. A minimized window places the overlay at a negative x

**Cause.** A minimized window reports an empty client area, so
`x = client_width − inset − width` goes negative. It is clipped out of sight
rather than visibly broken, which is why it survives casual testing — but it is a
position the parent cannot contain, and it leaves a live hit-test target pointing
at nothing.

**Fix.** Check `IsIconic(parent)` (and a non-positive client rect) and hide the
overlay instead; the restore's `WM_SIZE` re-shows it, because the normal path
passes `SWP_SHOWWINDOW`. Also floor `x` at 0 in the rect arithmetic — that half is
pure, so it can be unit-tested off Windows.

## Anti-patterns to avoid

- **`WS_EX_LAYERED`** costs the hit test, and **`WS_EX_TRANSPARENT`** makes the
  window hit-test-transparent — either one defeats the entire purpose.
  Invisibility has to come from *never painting* (`NULL_BRUSH`, no `WM_PAINT`),
  not from alpha.
- **`WS_CLIPSIBLINGS` is load-bearing**, not decorative: it keeps the overlay
  from being painted over by its sibling, the WebView2 host HWND.
- **Don't hang the overlay's lifetime off `WM_CLOSE`.** It is advisory while a
  close can still be vetoed, so the overlay silently dies on a cancelled exit.
  Use `WM_NCDESTROY`.
- **Don't compute a position from an empty client rect.** A minimized window has
  none; hide the overlay instead of moving it somewhere the parent cannot
  contain.
- **Don't re-arm `TrackMouseEvent` on every `WM_NCMOUSEMOVE`.** Tracking stays
  armed until `WM_NCMOUSELEAVE`, so the enter edge is the only place it belongs —
  otherwise it is a syscall per pixel.
- **Don't hardcode the button geometry in Rust.** The overlay is positioned by
  arithmetic, so a constant that drifts from a Tailwind class stops the flyout
  appearing and breaks nothing else — a failure with no other symptom. Measure
  the button from the DOM and report it (ADR-0035), or at minimum pin the
  constants with a test.
- **Don't measure the inset with `window.innerWidth`.** It is an integer while
  `getBoundingClientRect()` is fractional, so mixing the two biases the inset by
  up to a pixel at fractional display scaling — and because the bias flips as the
  viewport width changes, the overlay drifts about a pixel and overlaps the
  neighbouring close button. Use the root element's fractional right edge.

## Sources

- [Support snap layouts for desktop apps on Windows 11](https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/ui/apply-snap-layout-menu)
  — the `HTMAXBUTTON` requirement and the minimum-width guidance.
- [Title bar design](https://learn.microsoft.com/en-us/windows/apps/design/basics/titlebar-design)
  — caption-control glyphs (Segoe Fluent Icons E921–E923, E8BB) and states.
- [tauri-snap-layouts](https://github.com/Zbrooklyn/tauri-snap-layouts) (MIT) —
  the verified overlay recipe, its window-style experiments, and its failure
  catalogue. The `windowing::snap_overlay` style set is copied from it.
- [VS Code `titlebarpart.css`](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/browser/parts/titlebar/media/titlebarpart.css)
  — the 46px backplate, hover alphas, and close-red values our controls match.
- [vscode-codicons](https://github.com/microsoft/vscode-codicons) — the
  `chrome-minimize` / `chrome-maximize` / `chrome-restore` / `chrome-close`
  outlines inlined in `TitleBar.tsx`.
