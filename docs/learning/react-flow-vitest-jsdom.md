# React Flow under vitest/jsdom — the polyfill recipe

Learned while building the circuit canvas editor tests (#1209,
`tests/unit/circuit-flow-editor.test.tsx`). React Flow v12 (`@xyflow/react`)
renders *nothing useful* in jsdom until three things are satisfied. Symptoms
first, fix second, so future sessions can jump straight to the cure.

## Symptom → cause → fix

### 1. Nodes render but stay `visibility: hidden`; the edges layer is empty

React Flow measures nodes through `ResizeObserver` and derives dimensions from
**`offsetWidth`/`offsetHeight`**, which are always 0 in jsdom. A no-op
ResizeObserver stub is NOT enough — nodes never become "measured", so they
stay hidden and edges (which need measured handle bounds) are skipped
entirely.

Fix: an observer that (a) defines non-zero `offsetWidth`/`offsetHeight`
getters on every observed element, (b) reports a matching `contentRect`, and
(c) **defers its callback with `setTimeout(0)`** — a synchronous callback
fires before React Flow finishes wiring its measurement handlers.

```ts
class ResizeObserverMock {
  callback: ResizeObserverCallback;
  constructor(callback: ResizeObserverCallback) { this.callback = callback; }
  observe(target: Element): void {
    const el = target as HTMLElement & { offsetWidth: number; offsetHeight: number };
    try {
      Object.defineProperty(el, 'offsetWidth', { configurable: true, get: () => 220 });
      Object.defineProperty(el, 'offsetHeight', { configurable: true, get: () => 64 });
    } catch { /* non-elements keep zero dims */ }
    const entry = { target, contentRect: { width: 220, height: 64, /* … */ },
                    borderBoxSize: [{ inlineSize: 220, blockSize: 64 }], /* … */ };
    setTimeout(() => this.callback([entry] as unknown as ResizeObserverEntry[],
                                   this as unknown as ResizeObserver), 0);
  }
  unobserve(): void {} disconnect(): void {}
}
```

Gotchas inside the mock:
- The callback receives an **array** of entries — passing a single entry dies
  with `entries.forEach is not a function`.
- Once nodes measure, fit-view math needs **`DOMMatrixReadOnly`** (jsdom has
  none): a stub whose constructor parses `scale(...)` into `m22` suffices.
  Assign it on both `globalThis` and `window`.

### 2. Clicking nodes/badges throws `Cannot read properties of null (reading 'document')`

That's d3-drag's `nodrag.js` dereferencing `event.view.document`.
`userEvent.click` dispatches MouseEvents with `view: null`. Use plain
`fireEvent.click` for anything inside the React Flow pane; reserve user-event
for text inputs outside it.

### 3. Typing literal braces loses characters

user-event treats `{...}` as special-key syntax and `{{` as an escaped literal
`{`. To type two literal braces (e.g. the Mustache `{{` trigger), pass FOUR:
`await user.type(area, 'Fix {{{{')`.

## Controlled-flow reminder

In a fully controlled React Flow (nodes/edges passed as props),
`useReactFlow().setEdges()` writes to the internal store, which is overwritten
from props on the next render — the change silently vanishes. Mutate through
the owning component's state instead (see ADR-0027).
