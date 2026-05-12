export type KeyAction = 'copy' | 'paste' | 'selectAll' | 'find' | 'clear' | 'passthrough';

export interface KeyEventState {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  isMac: boolean;
  hasSelection: boolean;
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
    if (k === 'k') return 'clear';
    return 'passthrough';
  }

  // Windows / Linux
  if (!ctrlKey) return 'passthrough';

  if (shiftKey) {
    if (k === 'c') return 'copy';
    if (k === 'v') return 'paste';
    if (k === 'a') return 'selectAll';
    if (k === 'f') return 'find';
    if (k === 'k') return 'clear';
    return 'passthrough';
  }

  // Ctrl without Shift
  if (k === 'c') return hasSelection ? 'copy' : 'passthrough';
  if (k === 'v') return 'paste';
  return 'passthrough';
}
