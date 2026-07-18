import { describe, it, expect } from 'vitest';
import { formatError } from '../../src/lib/errorUtils';

describe('formatError', () => {
  it('returns a native string verbatim (no prefix)', () => {
    expect(formatError('fatal: not a git repository')).toBe(
      'fatal: not a git repository',
    );
  });

  it('unwraps an Error instance to its message (strips the "Error: " prefix)', () => {
    // This is the Tauri `invoke` rejection shape — the whole point of #663.
    const e = new Error('mock: update_mesh_use_worktree failed');
    expect(formatError(e)).toBe('mock: update_mesh_use_worktree failed');
    expect(formatError(e)).not.toContain('Error:');
  });

  it('falls back to String(e) for an Error with an empty message', () => {
    // `String(new Error(''))` is the bare word "Error" — a last-resort
    // label, preferable to an empty banner.
    expect(formatError(new Error(''))).toBe('Error');
  });

  it('coerces non-string / non-Error values', () => {
    expect(formatError(42)).toBe('42');
    expect(formatError(null)).toBe('null');
    expect(formatError(undefined)).toBe('undefined');
    expect(formatError({ code: 1 })).toBe('[object Object]');
  });
});
