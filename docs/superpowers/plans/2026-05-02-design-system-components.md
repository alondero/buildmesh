# Design System — Component Integration

> **For agentic workers:** Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Historical plan — not authoritative.** This document records the
> tokens chosen on 2026-05-02. Current authoritative values live in
> `src/App.css` and are regression-pinned by
> `tests/unit/theme-tokens-contrast.test.ts`. Amendments:
> - Issue #732 (2026-07-02): `--color-text-muted` bumped from `#4a5568`
>   to `#7a8492` (was 2.6:1 on `bg-base` / `bg-surface` — fails WCAG AA
>   body text; now 5.2:1 / 5.1:1). The `text-[#4a5568]` recipe lines
>   below (Task 1 status.ts, Task 2 Sidebar.tsx) describe the original
>   migration, not the current state — they were updated in place during
>   the modernization pass.

**Goal:** Apply the design system's visual language to existing React components without changing data model or architecture. Colors, typography, borders, and spacing updated to match the design system token values.

**Architecture:** Changes are purely cosmetic — updating inline color values, CSS classes, and Tailwind utilities to match design system tokens. The existing data model (projects/sessions) is preserved. No component structure changes.

**Tech Stack:** React 19, Tailwind CSS v4 (`@theme` tokens), existing Zustand stores

---

## Files to Modify

| File | Changes |
|---|---|
| `src/lib/status.ts` | Update STATUS_CONFIG colors to match design system (cyan for running, violet for suspended, etc.) |
| `src/components/Sidebar/Sidebar.tsx` | Background, border, text colors, font, session item styling |
| `src/components/SessionView/SessionView.tsx` | Tab bar, grid container, session card backgrounds |
| `src/components/Terminal/Terminal.tsx` | xterm.js theme colors to match design system |
| `src/App.tsx` | Background, loading indicator, debug overlay colors |

---

## New Token Values to Apply

```css
/* Backgrounds */
--color-bg-base:     #09090f  /* body */
--color-bg-surface:  #0d0d16  /* sidebar */
--color-bg-overlay: #13131e  /* elevated panels */
--color-bg-card:    #18182a  /* session cards */
--color-bg-input:   #111120  /* inputs */

/* Text */
--color-text-primary:   #e2e8f0
--color-text-secondary: #94a3b8
--color-text-muted:     #4a5568

/* Accent */
--color-accent-cyan:  #00d4ff
--color-accent-violet: #8b5cf6

/* Borders */
--color-border-subtle:  #1a1a28
--color-border-default: #22223a

/* Status */
--color-status-running:  #00d4ff  (was green)
--color-status-success:  #22c55e
--color-status-warning:  #f59e0b
--color-status-error:    #ef4444
--color-status-idle:     #4a5568
```

---

### Task 1: Update status.ts

**Files:**
- Modify: `src/lib/status.ts` — update STATUS_CONFIG color values

- [ ] **Step 1: Update status config colors**

Replace STATUS_CONFIG with:
```ts
export const STATUS_CONFIG = {
  running: {
    color: 'text-[#00d4ff]',
    dot: '●',
  },
  idle: {
    color: 'text-[#4a5568]',
    dot: '○',
  },
  awaiting_input: {
    color: 'text-[#f59e0b]',
    dot: '◐',
  },
  error: {
    color: 'text-[#ef4444]',
    dot: '✗',
  },
  suspended: {
    color: 'text-[#8b5cf6]',
    dot: '⏸',
  },
  archived: {
    color: 'text-[#4a5568]',
    dot: '○',
  },
} as const;
```

---

### Task 2: Update Sidebar.tsx

**Files:**
- Modify: `src/components/Sidebar/Sidebar.tsx` — update all color values

Changes:
- Line 79: `bg-[#111]` → `bg-[#0d0d16]`, `border-[#2a2a2a]` → `border-[#1a1a28]`
- Line 82: header text `text-[#e0e0e0]` → design system text color (use `text-[#e2e8f0]`)
- Line 89: label `text-[#888]` → `text-[#4a5568]`
- Line 92: add button `text-[#00d4ff]`
- Lines 118, 123: dropdown colors to match dark surface palette
- Lines 134-136: active project text `text-[#3b82f6]` → `text-[#00d4ff]`
- Line 161: footer text `text-[#666]` → `text-[#4a5568]`
- Lines 185-186: active session border to use cyan glow style
- Session item text `text-[#aaa]` → `text-[#94a3b8]`
- Env badge `text-[#666]` → `text-[#4a5568]`

- [ ] **Step 1: Update Sidebar component colors**

Apply the color changes listed above.

---

### Task 3: Update App.tsx

**Files:**
- Modify: `src/App.tsx` — update background, loading indicator, toast colors

Changes:
- Line 145: loading screen background `bg-[#0f0f0f]` → `bg-[#09090f]`, dot color to cyan
- Line 152: app container background `bg-[#0a0a0a]` → `bg-[#09090f]`
- Lines 157-179: debug overlay — border, text, background to match design system palette
- Lines 185-188: toast — border and text colors updated

- [ ] **Step 1: Update App.tsx colors**

Apply the color changes listed above.

---

### Task 4: Update SessionView.tsx

**Files:**
- Modify: `src/components/SessionView/SessionView.tsx` — update tab bar, grid, session card colors

Changes:
- Line 69: empty state background
- Line 81: main container background
- Lines 83-84: tab bar background and border
- Lines 94-97: active/inactive tab styles → cyan accent for active
- Line 101: session name color
- Line 135: grid background → `#09090f`
- Lines 138-139: session card → `bg-[#18182a]` with subtle border
- Lines 146-148: session card header text → muted palette
- Line 151: env badge

- [ ] **Step 1: Update SessionView.tsx colors**

Apply the color changes listed above.

---

### Task 5: Update Terminal.tsx xterm theme

**Files:**
- Modify: `src/components/Terminal/Terminal.tsx` — update xterm theme

Changes in `new Terminal({ theme: {...} })`:
- `background: '#0f0f0f'` → `'#09090f'`
- `foreground: '#e0e0e0'` → `'#e2e8f0'`
- `cursor: '#3b82f6'` → `'#00d4ff'`
- `selectionBackground: 'rgba(59, 130, 246, 0.3)'` → `'rgba(0, 212, 255, 0.15)'`
- `fontFamily: 'Cascadia Code, Consolas, monospace'` → `'JetBrains Mono', 'Fira Code', 'Cascadia Code', 'Consolas', monospace`
- Add `fontWeight: 500`

- [ ] **Step 1: Update Terminal.tsx xterm theme**

Apply the theme changes listed above.

---

### Task 6: Verify build and test

- [ ] **Step 1: Run npm run build**

Run: `cd X:/src/buildmesh && npm run build`
Expected: Build completes with exit code 0

- [ ] **Step 2: Run unit tests**

Run: `npm run test:unit` (or `npx vitest run`)
Expected: All tests pass

---

### Task 7: Commit

- [ ] **Step 1: Stage and commit**

```bash
git add src/lib/status.ts src/components/Sidebar/Sidebar.tsx src/components/SessionView/SessionView.tsx src/components/Terminal/Terminal.tsx src/App.css
git commit -m "$(cat <<'EOF'
feat: apply design system colors to components

- Sidebar, App, SessionView, Terminal updated to design system palette
- xterm.js theme colors updated: background #09090f, cursor #00d4ff
- STATUS_CONFIG updated: running=cyan, suspended=violet
- All hardcoded hex values replaced with design system values

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```