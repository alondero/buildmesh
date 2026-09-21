# Buildmesh design system

Status: current. Audience: anyone changing UI code, on desktop or mobile.

This document is the design system contract for Buildmesh. It tells you which
colour, type, spacing, and component pattern to reach for so the desktop app
and the mobile remote UI read as one product.

The **source of truth lives in code**, not in this file:

- Desktop tokens: the `@theme` block in `src/App.css` (dark), re-pointed by
  the `[data-theme="light"]` override in the same file.
- Mobile tokens: the `:root` block in `src/mobile/styles.css`.
- Status vocabulary (labels, dot glyphs, colours): `src/lib/status.ts`.
- Mesh accent palette (user-pickable): `src/lib/meshColors.ts`.
- Terminal palettes: `src/components/Terminal/terminalConfig.ts` (xterm.js
  cannot read CSS variables, so it holds literal mirrors of the tokens).

When a value here and the code disagree, the code wins — fix this document in
the same change.

## Principles

1. **Dark-first, layered neutrals.** Surfaces are near-black zinc steps.
   Elevation is communicated by slightly lighter fills, not shadows or
   saturated tints. A light theme exists and re-points the same tokens.
2. **One brand accent.** Electric cyan (`#00d4ff`) is the single
   interactive/brand hue. Violet is reserved for AI/agent actions. Blue is
   the pressed/deep step of the accent, not a second brand colour.
3. **Colour means status.** Green/amber/red only ever mean
   success/warning/error (or added/modified/deleted in diffs). Never use them
   decoratively.
4. **Readable by contract.** Body text meets WCAG AA (4.5:1) on every surface
   it renders on. This is enforced by regression tests, not by eyeballing —
   see [Accessibility](#accessibility).
5. **Borders are hairlines.** 1px white-alpha (dark) / black-alpha (light)
   lines that composite cleanly on any elevation. No pre-mixed border hexes.

## Colour

### Surfaces

| Token | Dark | Light | Use |
|---|---|---|---|
| `bg-base` | `#0a0a0e` | `#fafafa` | App background, terminal background |
| `bg-surface` | `#111116` | `#ffffff` | Panels, sidebar, app bar |
| `bg-overlay` | `#15151c` | `#ffffff` | Floating layers (menus, modals) |
| `bg-card` | `#16161d` | `#f5f5f7` | Cards, selected rows |
| `bg-card-hover` | `#1b1b23` | `#ebebed` | Hover/pressed state on cards |
| `bg-input` | `#0e0e13` | `#ffffff` | Text fields |
| `bg-selection` | `#1a2a3a` | `#cce8ff` | Active selection fill (omnibar row, lists) |
| `bg-highlight` | `rgba(0,212,255,0.06)` | `rgba(8,145,178,0.06)` | Cyan wash for highlighted regions |

Tailwind utilities are generated from these: `bg-bg-card`,
`text-text-primary`, `border-border-default`, etc.

### Text

| Token | Dark | Light | Use |
|---|---|---|---|
| `text-primary` | `#e2e8f0` | `#0f172a` | Headings, names, primary content |
| `text-secondary` | `#94a3b8` | `#475569` | Paths, branch names, dates, counts — load-bearing metadata |
| `text-muted` | `#7a8492` | `#5b6471` | Placeholders, disabled labels, decorative captions |
| `text-inverse` | `#0a0a0e` | `#ffffff` | Text drawn on accent fills |

The hierarchy is monotonic (primary > secondary > muted) and every tier
clears WCAG AA on every surface. **Do not dim `text-muted` further**, and do
not use `text-muted` for information the user needs to act on — that is what
`text-secondary` is for.

### Accents

| Token | Dark | Light | Use |
|---|---|---|---|
| `accent-cyan` | `#00d4ff` | `#0891b2` | Brand, focus rings, active states, interactive links |
| `accent-cyan-dim` / `-glow` | 15% / 8% alpha | same hue | Selected fills, hover washes |
| `accent-blue` | `#0ea5e9` | `#0284c7` | Pressed/deep step of the accent |
| `accent-violet` | `#8b5cf6` | `#7c3aed` | AI/agent actions only (spawn, autopilot, rename suggestion) |
| `accent-green` | `#22c55e` | `#16a34a` | Success / diff-added |
| `accent-amber` | `#f59e0b` | `#d97706` | Warning / diff-modified / needs attention |
| `accent-red` | `#ef4444` | `#dc2626` | Error / diff-deleted / destructive |

On light backgrounds the accents darken one step (cyan-500 → cyan-600 etc.)
because the dark-theme hues fall below 3:1 on near-white. Always consume the
token — never hardcode the dark hex into a component.

### Status

Agent-node status is one vocabulary shared by desktop and mobile:
`STATUS_CONFIG` in `src/lib/status.ts` carries the Tailwind classes, the dot
glyph, the label, and the literal `hex` (mobile renders inline styles).

| Status | Colour | Dot | Label |
|---|---|---|---|
| `pending`, `spawning` | muted (pulsing) | ◌ | Starting… |
| `running` | cyan | ● | Running |
| `idle` | cyan | ○ | Idle |
| `awaiting_input` | amber (pulsing) | ● | Needs attention |
| `error` | red | ✗ | Error |
| `suspended` | violet | ⏸ | Suspended |
| `completed` | green | ✓ | PR opened |
| `ready` | green | ✓ | Ready |
| `archived` | muted | ◌ | Archived |

Idle and running intentionally share cyan; the dot glyph and label
disambiguate. Every status colour also has a `status-*-bg` 10%-alpha token
for chip/badge fills.

File-diff status letters (A/M/D/R/?) map to green/amber/red/violet/muted via
`fileDiffStatusMeta` in the same file — the same mapping applies on mobile.

### Borders

| Token | Dark | Light |
|---|---|---|
| `border-subtle` | white 5% | black 8% |
| `border-default` | white 7% | black 17% |
| `border-strong` | white 14% | black 37% |
| `border-active` | `#00d4ff` | `#0891b2` |

## Typography

Fonts load in a single preconnected Google Fonts request from `index.html`
and `mobile/index.html`; keep the weights there in sync with the stacks.

| Role | Stack |
|---|---|
| Sans (desktop UI) | `'Geist', 'Inter', -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif` |
| Sans (mobile UI) | `'Geist', -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif` |
| Mono (terminal, code, paths, diffs) | `'JetBrains Mono', 'Fira Code', 'Cascadia Code', 'Consolas', monospace` |

The mobile stack skips `Inter` (a desktop-only fallback) and adds `Roboto`
for Android; otherwise identical.

Weight 800 exists for the wordmark only. UI text runs 400/500/600; 700 for
small solid badges.

**Desktop scale.** Base body is 13px (`--font-size-base`). Use the Tailwind
steps: `text-xs` (12px) for secondary content, `text-sm` (14px) for emphasis,
`text-base`/`text-lg` for headings. `--text-2xs` (10px) is the **absolute
floor** — badges, gutter labels, micro-captions. Nothing renders below 10px.

**Mobile scale.** Slightly larger for touch: 13px body, 15px card titles,
16px on text fields (prevents iOS focus auto-zoom), 11–12px metadata, 10px
floor for micro-labels.

## Spacing, radius, shadow, motion

- **Spacing:** 4px base unit (`--spacing-1` = 4px … `--spacing-8` = 32px);
  Tailwind's numeric utilities match it.
- **Radius (desktop):** `--radius-sm` 2px, `--radius-md` 4px, `--radius-lg`
  6px, `--radius-pill` 9999px. Small dense UI; when in doubt pick the smaller
  radius. **Radius (mobile):** 8px controls, 10px cards/fields, 12px sheets —
  the touch adaptation of the same scale.
- **Shadows:** `--shadow-sm` / `--shadow-md` for elevation,
  `--shadow-glow-cyan` for accent glow, `--shadow-focus` for the focus ring.
- **Motion:** `--ease-snappy` (cubic-bezier(0.16,1,0.3,1)),
  `--duration-micro` 120ms, `--duration-fast` 200ms; Tailwind transitions
  default to 150ms snappy. Entrance animations: `animate-fade-in`,
  `animate-scale-in`, `animate-slide-in-right`. Everything collapses under
  `prefers-reduced-motion`.


## Component patterns

### Buttons

- **Primary (desktop):** accent-cyan fill (or `accent-cyan/10` tinted variant
  for in-panel actions), `text-inverse` text on solid fills, `rounded-md`,
  `hover:brightness-125` for tint variants. Disabled: muted text, no pointer
  affordance.
- **Ghost/secondary:** transparent fill, `border-border-strong` hairline,
  `text-text-secondary`; hover raises to `bg-bg-card-hover` +
  `text-text-primary`.
- **Icon buttons:** square hit area, `text-text-secondary` glyph, hover fill
  `bg-bg-card-hover`; never a bare glyph without an `aria-label`.
- **Mobile:** `.btn-primary` (accent fill, `--on-accent` text), `.btn-ghost`,
  `.chip-btn` (hairline chip). Touch targets are ≥ 44×44px — flex-center the
  glyph inside an explicit box, don't pad your way to the size.

### Pills, badges, chips

Status/PR pills are **translucent-fill chips**: 10% status-colour background,
status-colour text, 30% ring (`bg-accent-green/10 text-accent-green ring-1
ring-inset ring-accent-green/30`). Compact variant: `rounded-md` square chip.
Micro-badges (2xs status letters) use `px-1 py-px rounded text-2xs`. A pill
never invents a colour outside the status/accent tables.

### Menus and list rows

- Rows: `w-full text-left px-3 py-1 text-xs`, hover `bg-bg-card-hover`,
  keyboard/active selection `bg-bg-selection` with a `border-l-accent-cyan`
  caret edge (omnibar). Hover paint is CSS-only and must not fight the
  keyboard caret.
- Section labels inside menus: 2xs, uppercase, `text-text-muted`.

### Modals, sheets, overlays

Desktop: shared `Modal.tsx` — `bg-bg-overlay`, `border-border-subtle`,
`rounded-lg`, `--shadow-md`, `animate-scale-in` entrance. Mobile: bottom
`Sheet` anchored to `#root` so it survives the soft keyboard. Destructive
confirmations go through `ConfirmDialog`, not ad-hoc prompts.

### Inputs

`bg-bg-input`, `border-border-default`; focus moves the border to
`border-active` (an outline on top would double up). Placeholder:
`text-text-muted`. Mobile `.field` additionally uses 16px text (iOS
auto-zoom) and a 10px radius.

### Banners and toasts

Banner = inline strip at the top of a region; toast = floating card above
content. Both take their colours from the status tokens: a 15% status-colour
wash, status-colour text, ~25–35% status-colour border. No bespoke warning
orange or success green hexes. The one carve-out: a **toast floats over
scrolling content**, so it composites the wash onto a solid `bg-card` base
rather than onto transparency (`--surface-2` + status text/border on
mobile).

### Terminal

xterm.js takes literal values from `terminalConfig.ts`, which mirrors the
tokens: dark `#0a0a0e` / `#e2e8f0` / cyan cursor; light `#fafafa` /
`#0f172a` / cyan-600 cursor. The mobile terminal imports the same
`DARK_TERMINAL_THEME` object rather than keeping its own copy.
Font: JetBrains Mono at 500 weight; desktop size is user-adjustable 8–18px
(default 10), mobile fixed 13px.

## Accessibility

- WCAG AA (4.5:1) is the floor for body text; 3:1 for non-text UI
  (focus rings, borders that carry meaning).
- The contract is executable: `tests/unit/theme-tokens-contrast.test.ts` and
  `tests/unit/theme-tokens-light-contrast.test.ts` recompute contrast from
  the CSS and fail the build if a token regresses; a static list in the same
  test pins load-bearing text to `text-secondary`. The mobile mirror is
  pinned the same way by `tests/unit/mobile-tokens-contrast.test.ts` (AA on
  every mobile surface + value parity with the desktop tokens).
- `tests/unit/radii-audit.test.ts` pins the radius conventions on audited
  components.
- Keyboard focus is always visible: 1.5px accent-cyan outline on button-like
  elements, focus border on inputs.
- Colour is never the only signal — statuses pair colour with a dot glyph
  and a text label.

## Mobile token mapping

Mobile CSS predates the desktop token names, so `src/mobile/styles.css` keeps
its shorthand names but holds the **same values** as the desktop dark theme:

| Mobile var | Desktop token |
|---|---|
| `--bg` | `bg-base` |
| `--surface` / `--surface-2` / `--surface-3` | `bg-surface` / `bg-card` / `bg-card-hover` |
| `--border` / `--border-strong` | `border-default` / `border-strong` |
| `--text` / `--text-dim` / `--text-faint` | `text-primary` / `text-secondary` / `text-muted` |
| `--accent` / `--accent-dim` / `--on-accent` | `accent-cyan` / `accent-blue` / `text-inverse` |
| `--green` / `--amber` / `--red` / `--violet` | `accent-green` / `accent-amber` / `accent-red` / `accent-violet` |
| `--green-dim` / `--amber-dim` / `--red-dim` / `--violet-dim` / `--accent-glow` | 15% alpha variants of the same hues (the desktop diff-fill / `accent-*-dim` convention) |
| `--amber-border` / `--red-border` / `--attention-ring` | 25% status borders; 40% needs-attention ring |

Retune a colour in both files in the same change.

## Rules for contributors

1. Use a token or a shared utility. If none fits, add a token in the same PR
   and document it here — don't inline a one-off hex.
2. No hardcoded hex/rgba colours in components. Exceptions are exactly the
   mirror files listed at the top of this document (xterm palette,
   `STATUS_CONFIG.hex`, mesh palette, OS-caption palettes), each of which
   comments what it mirrors.
3. No text below 10px, no new font families, no new border colours.
4. Status and diff colours come from `src/lib/status.ts` — never re-derive
   them in a component.
5. If you change a token value, run the contrast and radii tests
   (`npm run test:unit`) and update this document.

