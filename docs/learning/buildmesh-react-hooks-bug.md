---
name: buildmesh-react-hooks-bug
description: React #310/#321 hooks errors from duplicate component definitions and map-called hooks
metadata:
  type: reference
---

# React Hooks Error #310/#321 — Duplicate Component Definitions

## The Bug

When clicking the folder icon to open the mesh file explorer, the entire view would blank out. Console showed React error #310: "Cannot read properties of undefined (reading 'useMemo')".

## Root Causes

### Duplicate component definitions
During a large Edit operation to replace the `TreeNode` import with an inline definition, the old code was not fully removed. The result was two `FileTreeProps` interfaces and two `TreeNode` function declarations in the same file. TypeScript didn't catch this at build time. React's runtime hook dispatcher got confused because the second `TreeNode` declaration didn't have properly initialized hooks.

**Key insight**: A second function declaration of the same name shadows the first in JavaScript. React doesn't warn at build time — it just breaks at runtime when the second declaration (lacking proper hook initialization) gets used by the component tree.

### State variable used before declaration in console.log
`console.log` was placed BEFORE `useState` declarations, referencing variables (`loading`, `error`, `tree`) that don't exist yet because hooks haven't run. This was part of the debugging effort but was left behind.

## What We Learned

- **React hooks can't live in `.map()`**: But rendering `<Component />` from within `.map()` is fine — React handles per-item rendering correctly. The actual TreeNode was moved to its own file and is called properly via JSX.

- **State rename pattern**: When refactoring state variable names (e.g., `loading` → `loadingState`), always put any `console.log` AFTER the `useState` declarations, never before.

- **Early return pattern**: An unconditional `return` statement in a component skips ALL sibling JSX that follows it. The file explorer debug boxes were hidden by the `if (filteredNodes.length === 0) { return ...; }` early return.

- **useEffect with store-in-deps timing**: The effect closing the file explorer was firing when `fileExplorerContext` changed (including on first set from `toggleFileExplorer`). Fixed by adding an `initialized` flag (skip until after first render) and then removing it once we understood the early return was the primary issue.

- **No minification in dev saves debug time**: Sourcemap-enabled builds (`minify: false`) give readable stack traces that pinpoint exact component lines. Always keep this for development builds.

## Files Changed

- `src/components/FileTree/FileTree.tsx` — State variable rename (loading→loadingState, etc.), removed useMemo since gitStatusMap is recomputed cheaply on every render
- `src/components/FileTree/FileExplorerPanel.tsx` — Removed useCallback wrappers (not needed), removed debug console.log
- `src/components/SessionView/SessionView.tsx` — Removed debug red box placeholder, removed initialized flag workaround, simplified closeFileExplorer effect
- `src/components/Sidebar/Sidebar.tsx` — Removed clickCount debug state, removed console.log debug statements
- `src/components/FileTree/DiffView.tsx` — Added `data-hunk` attribute (debug aid)
- `vite.config.ts` — Reverted `minify: false, sourcemap: true` (was for debugging, not for production)

## Prevention Checklist

- [ ] Check for duplicate declarations when doing large refactors
- [ ] Ensure console.log comes AFTER useState calls, never before
- [ ] Verify early returns don't hide sibling components
- [ ] Test with sourcemaps enabled first — readable stack traces are worth the build time