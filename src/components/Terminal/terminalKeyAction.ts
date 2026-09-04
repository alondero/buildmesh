export type KeyAction = 'copy' | 'paste' | 'selectAll' | 'find' | 'clear' | 'passthrough';

export type ZoomKeyAction = 'reset' | 'in' | 'out';

export interface KeyEventState {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  isMac: boolean;
  // Only consulted by resolveKeyAction for the Ctrl/Cmd+C copy gate; optional
  // so handlers that don't gate on selection (e.g. resolveZoomKeyAction) don't
  // have to fabricate it.
  hasSelection?: boolean;
}

export function resolveKeyAction(state: KeyEventState): KeyAction {
  const { key, ctrlKey, shiftKey, metaKey, isMac, hasSelection } = state;
  const k = key.toLowerCase();

  if (isMac) {
    if (!metaKey) return 'passthrough';
    if (k === 'c') return hasSelection ? 'copy' : 'passthrough';
    if (k === 'v') return 'paste';
    if (k === 'a') return 'selectAll';
    if (k === 'f') return 'find';
    // Issue #1409 — ⌘+K is claimed by the Omnibar (global CommandOrControl+K).
    // Terminal clear moves to ⌘+Shift+K.
    if (shiftKey && k === 'k') return 'clear';
    return 'passthrough';
  }

  // Windows / Linux
  if (!ctrlKey) return 'passthrough';

  if (shiftKey) {
    if (k === 'c') return 'copy';
    if (k === 'v') return 'paste';
    if (k === 'a') return 'selectAll';
    if (k === 'f') return 'find';
    // Issue #1568 — Ctrl+Shift+K is owned by the Omnibar's Tauri global
    // shortcut (App.tsx:146). Remap terminal clear off that chord to
    // Ctrl+Shift+L, following the primer rule "remap the terminal side
    // or augment the modifier rather than stealing the user's keystroke"
    // (docs/knowledge-primer.md:133).
    if (k === 'l') return 'clear';
    return 'passthrough';
  }

  // Ctrl without Shift
  if (k === 'c') return hasSelection ? 'copy' : 'passthrough';
  if (k === 'v') return 'paste';
  return 'passthrough';
}

// Terminal font-zoom shortcuts (issue #667). Same Mac/Win split as
// resolveKeyAction above: meta is the modifier on macOS, ctrl elsewhere.
// The previous inline handler at Terminal.tsx only checked `e.ctrlKey`, so
// ⌘+0/+/- never fired on Mac — WebView's browser zoom captured ⌘ and the
// app never saw the gesture. Encoding the split here lets a unit test pin
// down the platform contract.
export function resolveZoomKeyAction(state: KeyEventState): ZoomKeyAction | null {
  const { key, ctrlKey, metaKey, isMac } = state;
  const modifier = isMac ? metaKey : ctrlKey;
  if (!modifier) return null;
  if (key === '0') return 'reset';
  if (key === '=' || key === '+') return 'in';
  if (key === '-' || key === '_') return 'out';
  return null;
}
