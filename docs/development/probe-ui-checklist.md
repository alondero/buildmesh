# Probe Panel interaction & visual checklist

> **Audience:** anyone adding or reworking a tab in the Probe dock
> (`src/components/Probe/`). **Scope:** the shell-level contracts a tab must
> honour so the dock reads as one surface instead of eleven independently
> evolved panels.
>
> **Umbrella issue:** [#1464](https://github.com/alondero/buildmesh/issues/1464)
> (Standardize Probe shell scrolling, density, and action affordances).
> Seeded by [#1468](https://github.com/alondero/buildmesh/issues/1468), which
> fixed the Circuits tab against every item below.
>
> **Row-level reference:** `src/components/Probe/ProbeRow.tsx` (issue #463) and
> the GitHub/archive polish in #1140. Don't create a competing row pattern.

This is a checklist, not an architecture document. Work through it when you
touch a tab; each item names the defect it prevents so you can tell whether it
applies to you.

## 1. Scroll ownership

The panel shell is, from `ProbePanel.tsx`:

```
:220  flex flex-col h-full w-full overflow-hidden   <- clips the dock
:282  flex-1 overflow-y-auto                        <- INERT, see below
:283  animate-fade-in h-full flex flex-col          <- per-tab keyed wrapper
:284  <the tab>
```

- [ ] **Exactly one element scrolls.** The tab root is layout-only
      (`flex flex-col h-full min-h-0`) and one inner body owns
      `flex-1 min-h-0 overflow-y-auto`. Because the root is `h-full`, the
      panel's own `overflow-y-auto` at `:282` has content exactly its own
      height and never gains a scrollbar — so the inner body is the single
      *effective* scroll owner. Put `overflow-y-auto` on the tab root as
      well and you get two stacked scrollers, which is what #1468 fixed.
- [ ] **Prefer the shared primitive.** `<ProbeTabBody>` already provides that
      body region with the standard padding. Reach for it unless the tab needs
      a pinned toolbar as a sibling (Circuits, Agent Changes).
      *(Note: `ProbePanel.tsx` also declares a local `function ProbeTabBody`
      that is only a tab router — same name, different component. Don't
      confuse them.)*
- [ ] **Toolbars sit outside the scroller** as `shrink-0` siblings, so a
      filter or create-row doesn't scroll away from the list it controls.
- [ ] **`overflow-x` is stated explicitly.** `overflow-y-auto` alone computes
      `overflow-x: auto` — CSS forbids one axis being `visible` while the
      other scrolls — so a wide child can scroll the tab sideways with no
      visible cue. Add `overflow-x-hidden` and make the content wrap instead.
- [ ] **Nested scrollers are deliberate and bounded.** An inner
      `max-h-* overflow-y-auto` (an expanded row body, a log excerpt) is fine;
      an unbounded second `flex-1 overflow-y-auto` is not.

## 2. Narrow width

The dock resizes between **240px and 720px** (`PROBE_PANEL_BOUNDS` in
`useProbeResize.ts`); 240px is the case to design for, not the exception.

- [ ] **Wrap, don't truncate, anything unbounded.** Error text, trigger
      identities, branch names, prompts and node ids all exceed 240px. A
      `truncate` on them hides exactly the tail that carries the diagnosis.
- [ ] **The carve-out: a short single-line label in a control row may
      truncate**, provided it carries a `title` tooltip — a circuit or mesh
      name sitting beside its action buttons is the case, and `ProbeRow.tsx`
      treats issue/PR titles the same way. The test is whether the *tail*
      carries meaning: a name's tail rarely does, a stack trace's always does.
      If in doubt, wrap.
- [ ] **Pick the right break.** `break-words` for prose; `break-all` for
      unspaced identifiers (`issue:1468:buildmesh:run`) where a word-boundary
      break has nowhere to land and overflows instead.
- [ ] **`truncate` needs `min-w-0` on every flex ancestor**, or it silently
      does nothing and the text pushes the action buttons off-panel. See the
      `flexbox-truncate-trap` note in `ProbeRow.tsx`.
- [ ] **Never combine `truncate` with `flex-wrap` on the same row.** They are
      contradictory instructions; you get a clipped single line competing with
      the buttons for width.
- [ ] **Long lists cost height, not width.** Render sequences vertically so
      the body's scroll absorbs them.

## 3. Status language

- [ ] **No raw DB or scheduler tokens in the UI.** `pending_slot` is
      scheduler-internal shorthand for "eligible, every slot busy" — #1468 was
      filed because users could not decode it. Map to a display label
      (`stepStatusLabel` / `runStateLabel` in `circuitGraphModel.ts`) and pass
      unknown values through unchanged so a newer backend stays legible.
- [ ] **Keep the raw value on a data attribute** (`data-run-state`,
      `data-step-status`) so tests and CSS assert machine state without
      depending on prose.
- [ ] **Colour from one vocabulary.** `statusTextClass` is the single
      status→token map; don't start a second palette.
- [ ] **A waiting state says what it is waiting for.** "Queued" alone is not a
      diagnosis. If the reason can be derived honestly, show it; if it is an
      inference rather than a fact, say so in the code comment and name the
      issue that will make it authoritative.

## 4. Actions

- [ ] **Never nest a button inside a button.** Invalid HTML, and the inner
      control loses keyboard semantics. Row-level actions are siblings of the
      disclosure control, not children.
- [ ] **Icon-only buttons carry an `aria-label`** naming the object, not the
      icon (`Delete nightly-sweep`, not `Delete`).
- [ ] **Destructive and long-running actions respect a `busy` flag** with
      `disabled` + `disabled:opacity-40`, so a double-click can't fire twice.
- [ ] **Errors render in a `role="alert"` region** that is `shrink-0` and
      outside the scroller, so it can't scroll out of sight.

## 5. Disclosure & state

- [ ] **`aria-expanded` + `aria-controls`** on every expand/collapse control,
      with the panel carrying the matching `id`.
- [ ] **Default open state follows what the user came for** — live/failing
      items open, terminal ones collapsed.
- [ ] **Record only deliberate toggles.** Store user overrides keyed by id and
      fall back to the computed default. A live-refresh (`circuit-run-updated`,
      a poll, a refetch) must not snap shut a card the user just opened.
- [ ] **Derive the next toggle value from the current *effective* state**, not
      from the override map alone — otherwise the first click on a
      default-open item does nothing visible and needs a second.
- [ ] **A failure is never only behind a disclosure.** Surface a clamped
      excerpt while collapsed.

## 6. Tests

- [ ] Assert the scroll contract structurally: root has no `overflow-*`, the
      body has exactly one, and the count of scrollers under the root is 1.
- [ ] Assert wrapping by class (`break-words` / `break-all` present,
      `truncate` absent) on the elements that hold unbounded text.
- [ ] Cover the narrow-width assumptions with the *content* that breaks them:
      a long chain, a long identifier, a long error — not a short fixture.
- [ ] Assert humanised labels and the absence of the raw token
      (`not.toContain('pending_slot')`).
- [ ] If you wrote the component before the tests, stash the source and prove
      the new tests go red. A test that never failed hasn't been tested.
