# Traffic lights in a frameless Tauri window on macOS — what can and cannot be emulated

Learned making the bespoke title bar's macOS window controls behave like real
traffic lights (ADR-0036, `src/components/TitleBar/TitleBar.tsx`). The
expensive part is not the CSS; it is knowing how many states the system actually
has, and knowing which of them can never be reached from a webview.

## The state set is five states, not two

The original implementation had "colour, with a glyph on hover". The system has
four visual states plus the modifier-glyph variant, and the two that were
missing are the ones carrying information.

| State | What macOS does | How it is expressed here |
|---|---|---|
| Rest, window focused | System colour (red / yellow / green), no glyph | `bg-mac-close` / `-minimize` / `-zoom` |
| Pointer anywhere over the strip | **All three** glyphs appear at once; fills unchanged | `group-hover:opacity-100` on each glyph, `group` on the cluster |
| Pressed | The fill darkens; it does not lighten | `active:bg-mac-*-pressed` |
| Window unfocused | All three become **one flat grey**, no glyphs | `bg-mac-traffic-inactive` |
| Unfocused + pointer over the strip | The light under the pointer returns to its colour | `group-hover:bg-mac-<colour>` |

The unfocused state is the one users notice: it is how you tell at a glance which
window is taking your keystrokes, and without it a hand-drawn strip looks equally
"live" whichever window is in front.

## Symptom → cause → fix

### 1. Only the hovered light shows its glyph

**Cause.** `group` was on each `<button>`, so `group-hover` could only ever match
the one light the pointer was over. That is how a web tooltip behaves. macOS
reveals the × / − / + for the whole cluster the moment the pointer enters it —
the symbols are a hint about the strip, not about one button.

**Fix.** Put `group` on the cluster wrapper and leave the buttons ungrouped. One
class move; the glyph styles do not change.

### 2. "Why not just use the real traffic lights?"

Because it is a config-shaped problem, not a capability one. macOS does offer the
genuine article: `"decorations": true` with `"titleBarStyle": "Overlay"` and
`"hiddenTitle": true` hands the buttons back to AppKit, which is exactly what VS
Code does. Three things stop it here.

1. **`decorations` is a single cross-platform field**, and `app.windows` is an
   array. Tauri's config overlays *replace* arrays rather than merging them, so a
   `tauri.macos.conf.json` has to redeclare the entire window object — and then
   the same redeclaration has to survive wherever the dev and release overlays
   layer. This is the trap that already cost the dev profile its
   `decorations: false` once.
2. **Layout.** Native lights sit near the top of a 28px title-bar strip. This bar
   is 45.5px tall with its content centred, so the lights read roughly 9px high
   against the wordmark. Moving them means owning AppKit button-frame code for a
   cosmetic offset.
3. **It cannot be verified off a Mac**, and the failure mode is broken window
   chrome on every macOS build rather than a wrong shade on one control.

If macOS becomes a first-class target, revisit this: the native route removes the
entire state table above *and* supplies the behaviours in "what cannot be
emulated". The reason to decline it is cost and risk, not fidelity.

### 3. `w-3` is 9.75px, not 12px

**Cause.** This app sets `--font-size-base: 13px`, so every rem-based Tailwind
step is 3.25px: `w-3` / `h-3` compile to `0.75rem` = **9.75px**. The system
draws 12px circles, so the lights sat a size too small next to every other window
on the machine. This is the same trap that already forced the caption glyphs to
`h-[16px]` literals.

**Fix.** State the pixels: `h-[12px] w-[12px]`, with the glyph in a 12-unit
`viewBox` rendered at 12px so its coordinates are real pixels. Do not reach for a
rem step here — the arithmetic is not the 16px-root arithmetic you expect.

### 4. A hover brightening that no macOS light has

**Cause.** The implementation used `hover:brightness-[0.92]`. Real lights do not
change fill on hover; the revealed glyph is the whole cue. A brightness *filter*
on the button also tints the glyph with it, so the two effects compound.

**Fix.** Delete it. Hover in the focused state changes nothing but glyph opacity.
The visible colour change belongs to the inactive state, where hover restores the
colour.

### 5. The restore-on-hover colour may never fire

**Cause.** An unfocused macOS window does not necessarily deliver hover events to
its webview. When it does not, the strip stays grey while the pointer is over it.

**Fix.** None needed — grey is the correct appearance for a window receiving no
interaction, so the degraded case is the native look for the common case. Worth
knowing so the behaviour does not get "fixed" by adding a listener that also
never fires.

## What cannot be emulated

Do not spend time re-deriving these; they need AppKit, not CSS.

- **The green button's meaning.** Natively it enters full screen by default, and
  Option-click zooms. Ours calls `toggleMaximize()` (zoom) for both. Changing it
  moves the window into a macOS space and hides the title bar — a shell
  behaviour, not a chrome style.
- **The press-and-hold tiling menu** on the green button, and the Option-modified
  zoom glyph. The menu is `NSWindow._windowTilingMenu`, a private AppKit menu
  rendered out of process.
- **VoiceOver integration**, the unsaved-document dot on close, and `⌘W` / `⌘M`,
  which are app-level shortcuts.
- **Not being in the tab order.** Native lights are not focusable; ours are
  `<button>`s. That is deliberately kept: they are hand-drawn, so a keyboard user
  has no other route to close or minimise the window.

## Anti-patterns to avoid

- **Don't put `group` on each light.** The glyph reveal is cluster-wide on macOS;
  a per-button group produces the "one symbol at a time" behaviour that reads as
  a web widget.
- **Don't build state class names with template literals.** Tailwind v4 compiles
  what it finds in the source, so a constructed `group-hover:bg-…` emits no rule
  and the light silently keeps one state forever. Keep the strings literal in the
  lookup table.
- **Don't assert hexes in the tests.** Assert token names. A hex assertion passes
  while the pressed and inactive variants drift away from the fill they belong
  to.
- **Don't add a `title`.** Native lights carry no tooltip; `aria-label` is the
  accessible name, exactly as with the caption buttons.
- **Don't express the geometry in rem steps.** At a 13px root they are all
  3.25px multiples, and the error is invisible until you compare against a real
  window.
- **Don't enable `decorations` globally** to get macOS lights. It is not a
  platform-conditional field, and turning it on for the other platforms
  reintroduces the native title bars the bespoke bar exists to replace.

## Sources

- [lwouis/macos-traffic-light-buttons-as-SVG](https://github.com/lwouis/macos-traffic-light-buttons-as-SVG)
  — the per-state colour reference this note's fills and pressed values follow
  (normal / pressed fills, unfocused grey, glyph tints).
- [aw3r1se/macOS-traffic-lights](https://github.com/aw3r1se/macOS-traffic-lights)
  — the same buttons packaged with explicit `default` / `hover` / `active` and
  `unfocused` variants.
- [merqurio's "Mac OS X Traffic Lights" gist](https://gist.github.com/merqurio/4e17987b8515d44141e5952c55591869)
  (and the [atdrago CodePen](https://codepen.io/atdrago/pen/yezrBR) it credits) —
  the canonical Electron-era CSS emulation for a frameless window. It is the
  clearest statement of the cluster rules: `.traffic-lights:hover` reveals all
  three glyphs, restores colour on an unfocused strip, and `:active:hover` is a
  darker fill.
- [NSWindow.ButtonType](https://developer.apple.com/documentation/appkit/nswindow/buttontype)
  and
  [`standardWindowButton(_:)`](https://developer.apple.com/documentation/appkit/nswindow/standardwindowbutton(_:))
  — Apple's own names and roles for close / minimise / zoom.
- [Traffic lights — UI Dictionary](https://devdocs.dev/ui-dictionary/macos/traffic-lights)
  — the placement and behaviour contract custom title bars are expected to keep.
