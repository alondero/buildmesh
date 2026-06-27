import { describe, it, expect } from 'vitest';
import {
  resolveKeyAction,
  resolveZoomKeyAction,
  KeyEventState,
} from '../../src/components/Terminal/terminalKeyAction';

function makeState(overrides: Partial<KeyEventState>): KeyEventState {
  return {
    key: '',
    ctrlKey: false,
    shiftKey: false,
    metaKey: false,
    isMac: false,
    hasSelection: false,
    ...overrides,
  };
}

describe('resolveKeyAction', () => {
  describe('macOS (isMac: true)', () => {
    it('Cmd+C with selection → copy', () => {
      expect(resolveKeyAction(makeState({ isMac: true, metaKey: true, key: 'c', hasSelection: true }))).toBe('copy');
    });

    it('Cmd+C without selection → passthrough', () => {
      expect(resolveKeyAction(makeState({ isMac: true, metaKey: true, key: 'c', hasSelection: false }))).toBe('passthrough');
    });

    it('Cmd+V → paste', () => {
      expect(resolveKeyAction(makeState({ isMac: true, metaKey: true, key: 'v' }))).toBe('paste');
    });

    it('Cmd+A → selectAll', () => {
      expect(resolveKeyAction(makeState({ isMac: true, metaKey: true, key: 'a' }))).toBe('selectAll');
    });

    it('Cmd+F → find', () => {
      expect(resolveKeyAction(makeState({ isMac: true, metaKey: true, key: 'f' }))).toBe('find');
    });

    it('Cmd+K → clear', () => {
      expect(resolveKeyAction(makeState({ isMac: true, metaKey: true, key: 'k' }))).toBe('clear');
    });

    it('Ctrl+C (no Cmd) → passthrough (SIGINT)', () => {
      expect(resolveKeyAction(makeState({ isMac: true, ctrlKey: true, key: 'c' }))).toBe('passthrough');
    });

    it('Cmd+Shift+C with selection → copy', () => {
      expect(resolveKeyAction(makeState({ isMac: true, metaKey: true, shiftKey: true, key: 'c', hasSelection: true }))).toBe('copy');
    });

    it('Cmd+Shift+C without selection → passthrough', () => {
      expect(resolveKeyAction(makeState({ isMac: true, metaKey: true, shiftKey: true, key: 'c', hasSelection: false }))).toBe('passthrough');
    });

    it('plain "a" (no modifiers) → passthrough', () => {
      expect(resolveKeyAction(makeState({ isMac: true, key: 'a' }))).toBe('passthrough');
    });

    it('Cmd+unhandled key → passthrough', () => {
      expect(resolveKeyAction(makeState({ isMac: true, metaKey: true, key: 'z' }))).toBe('passthrough');
    });
  });

  describe('Linux/Windows (isMac: false)', () => {
    it('Ctrl+C with selection → copy', () => {
      expect(resolveKeyAction(makeState({ ctrlKey: true, key: 'c', hasSelection: true }))).toBe('copy');
    });

    it('Ctrl+C without selection → passthrough (SIGINT)', () => {
      expect(resolveKeyAction(makeState({ ctrlKey: true, key: 'c', hasSelection: false }))).toBe('passthrough');
    });

    it('Ctrl+Shift+C with selection → copy', () => {
      expect(resolveKeyAction(makeState({ ctrlKey: true, shiftKey: true, key: 'c', hasSelection: true }))).toBe('copy');
    });

    it('Ctrl+Shift+C without selection → copy (convention)', () => {
      expect(resolveKeyAction(makeState({ ctrlKey: true, shiftKey: true, key: 'c', hasSelection: false }))).toBe('copy');
    });

    it('Ctrl+V → paste', () => {
      expect(resolveKeyAction(makeState({ ctrlKey: true, key: 'v' }))).toBe('paste');
    });

    it('Ctrl+Shift+V → paste', () => {
      expect(resolveKeyAction(makeState({ ctrlKey: true, shiftKey: true, key: 'v' }))).toBe('paste');
    });

    it('Ctrl+Shift+A → selectAll', () => {
      expect(resolveKeyAction(makeState({ ctrlKey: true, shiftKey: true, key: 'a' }))).toBe('selectAll');
    });

    it('Ctrl+A (no Shift) → passthrough (readline)', () => {
      expect(resolveKeyAction(makeState({ ctrlKey: true, key: 'a' }))).toBe('passthrough');
    });

    it('Ctrl+Shift+F → find', () => {
      expect(resolveKeyAction(makeState({ ctrlKey: true, shiftKey: true, key: 'f' }))).toBe('find');
    });

    it('Ctrl+F (no Shift) → passthrough', () => {
      expect(resolveKeyAction(makeState({ ctrlKey: true, key: 'f' }))).toBe('passthrough');
    });

    it('Ctrl+Shift+K → clear', () => {
      expect(resolveKeyAction(makeState({ ctrlKey: true, shiftKey: true, key: 'k' }))).toBe('clear');
    });

    it('Ctrl+K (no Shift) → passthrough (readline kill-to-end)', () => {
      expect(resolveKeyAction(makeState({ ctrlKey: true, key: 'k' }))).toBe('passthrough');
    });

    it('plain "c" (no modifiers) → passthrough', () => {
      expect(resolveKeyAction(makeState({ key: 'c' }))).toBe('passthrough');
    });

    it('Ctrl+unhandled key → passthrough', () => {
      expect(resolveKeyAction(makeState({ ctrlKey: true, key: 'z' }))).toBe('passthrough');
    });

    it('Ctrl+Shift+unhandled key → passthrough', () => {
      expect(resolveKeyAction(makeState({ ctrlKey: true, shiftKey: true, key: 'z' }))).toBe('passthrough');
    });
  });

  describe('case insensitivity', () => {
    it('Ctrl+C uppercase key → copy with selection', () => {
      expect(resolveKeyAction(makeState({ ctrlKey: true, key: 'C', hasSelection: true }))).toBe('copy');
    });

    it('Cmd+V uppercase key → paste on macOS', () => {
      expect(resolveKeyAction(makeState({ isMac: true, metaKey: true, key: 'V' }))).toBe('paste');
    });
  });
});

// Terminal font-zoom shortcuts (issue #667). The previous inline handler in
// Terminal.tsx only checked `e.ctrlKey`, which meant ⌘+0/+/- on macOS never
// fired — the OS-level browser zoom captured ⌘ but the handler ignored it.
// The platform-canonical modifier is meta on Mac and ctrl elsewhere; this
// helper encodes that split so the test guards against regressions.
describe('resolveZoomKeyAction', () => {
  describe('macOS (isMac: true)', () => {
    it('Cmd+0 → reset', () => {
      expect(resolveZoomKeyAction(makeState({ isMac: true, metaKey: true, key: '0' }))).toBe('reset');
    });

    it('Cmd+= → in (unshifted US layout)', () => {
      expect(resolveZoomKeyAction(makeState({ isMac: true, metaKey: true, key: '=' }))).toBe('in');
    });

    it('Cmd++ → in (shifted US layout)', () => {
      expect(resolveZoomKeyAction(makeState({ isMac: true, metaKey: true, shiftKey: true, key: '+' }))).toBe('in');
    });

    it('Cmd+- → out', () => {
      expect(resolveZoomKeyAction(makeState({ isMac: true, metaKey: true, key: '-' }))).toBe('out');
    });

    it('Cmd+_ → out (shifted dash)', () => {
      expect(resolveZoomKeyAction(makeState({ isMac: true, metaKey: true, shiftKey: true, key: '_' }))).toBe('out');
    });

    it('Ctrl+0 (no Cmd) → null — Mac does not honor ctrl for zoom', () => {
      expect(resolveZoomKeyAction(makeState({ isMac: true, ctrlKey: true, key: '0' }))).toBeNull();
    });

    it('Cmd+unhandled key → null', () => {
      expect(resolveZoomKeyAction(makeState({ isMac: true, metaKey: true, key: 'a' }))).toBeNull();
    });

    it('plain "0" (no modifier) → null', () => {
      expect(resolveZoomKeyAction(makeState({ isMac: true, key: '0' }))).toBeNull();
    });
  });

  describe('Linux/Windows (isMac: false)', () => {
    it('Ctrl+0 → reset', () => {
      expect(resolveZoomKeyAction(makeState({ ctrlKey: true, key: '0' }))).toBe('reset');
    });

    it('Ctrl+= → in', () => {
      expect(resolveZoomKeyAction(makeState({ ctrlKey: true, key: '=' }))).toBe('in');
    });

    it('Ctrl++ → in', () => {
      expect(resolveZoomKeyAction(makeState({ ctrlKey: true, shiftKey: true, key: '+' }))).toBe('in');
    });

    it('Ctrl+- → out', () => {
      expect(resolveZoomKeyAction(makeState({ ctrlKey: true, key: '-' }))).toBe('out');
    });

    it('Ctrl+_ → out', () => {
      expect(resolveZoomKeyAction(makeState({ ctrlKey: true, shiftKey: true, key: '_' }))).toBe('out');
    });

    it('Cmd+0 (no Ctrl) → null — Win/Linux do not honor meta for zoom', () => {
      expect(resolveZoomKeyAction(makeState({ metaKey: true, key: '0' }))).toBeNull();
    });

    it('Ctrl+unhandled key → null', () => {
      expect(resolveZoomKeyAction(makeState({ ctrlKey: true, key: 'a' }))).toBeNull();
    });
  });
});
