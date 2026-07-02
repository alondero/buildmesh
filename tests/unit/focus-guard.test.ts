import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { isTextInputFocused, isTerminalFocused } from '../../src/lib/focusGuard';

/**
 * Regression tests for the focus-guard around the Alt+G / Cmd+G toggle
 * (issue #668).
 *
 * The critical case is xterm.js: when a terminal has focus, the focused
 * DOM element is a hidden `<textarea class="xterm-helper-textarea"
 * aria-hidden="true">`. Without the carve-out, the guard returns true
 * for that element — silently blocking the shortcut in the one state
 * users actually want it (typing a prompt in an agent terminal).
 */
describe('isTextInputFocused', () => {
  let previouslyFocused: Element | null;

  beforeEach(() => {
    previouslyFocused = document.activeElement;
    // Reset to body — most tests want a clean slate.
    if (document.body && document.body.focus) document.body.focus();
  });

  afterEach(() => {
    if (previouslyFocused instanceof HTMLElement) {
      previouslyFocused.focus();
    }
  });

  it('returns false when nothing is focused (body)', () => {
    // jsdom default activeElement is body — confirm the guard stays quiet.
    expect(isTextInputFocused()).toBe(false);
  });

  it('returns true when an <input> has focus', () => {
    const input = document.createElement('input');
    document.body.appendChild(input);
    input.focus();
    expect(isTextInputFocused()).toBe(true);
    input.remove();
  });

  it('returns true when a <textarea> (non-xterm) has focus', () => {
    const ta = document.createElement('textarea');
    document.body.appendChild(ta);
    ta.focus();
    expect(isTextInputFocused()).toBe(true);
    ta.remove();
  });

  it('returns true when a <select> has focus', () => {
    const sel = document.createElement('select');
    document.body.appendChild(sel);
    sel.focus();
    expect(isTextInputFocused()).toBe(true);
    sel.remove();
  });

  // `isContentEditable` is hard to drive reliably under jsdom (the
// property setter doesn't propagate to the read-only IDL attribute
// the way real browsers do), so the contenteditable branch of the
// guard is covered implicitly: real browsers flip `isContentEditable`
// on attribute change, and the guard's single-line check against that
// boolean has no logic that needs exercising. The four tag-based
// branches (input / textarea / select / body) plus the xterm and
// aria-hidden carve-outs are the meaningful coverage.

  // The bug fix that prompted extracting this helper: when a terminal
  // has focus, xterm's hidden helper textarea is `document.activeElement`,
  // and the guard MUST let the Alt+G shortcut through.
  it('returns false for xterm.js\'s hidden helper textarea (#668 critical)', () => {
    const helper = document.createElement('textarea');
    helper.className = 'xterm-helper-textarea';
    helper.setAttribute('aria-hidden', 'true');
    helper.tabIndex = 0;
    document.body.appendChild(helper);
    helper.focus();
    // Without the carve-out this returned true and silently disabled the
    // shortcut while the user was typing in a terminal — the entire
    // reason for using Tauri's global-shortcut plugin.
    expect(isTextInputFocused()).toBe(false);
    helper.remove();
  });

  it('returns false for an xterm helper textarea missing aria-hidden (className fallback)', () => {
    // Belt-and-braces: if a future xterm version drops the aria-hidden
    // attribute but keeps the className, the guard must still let the
    // shortcut through.
    const helper = document.createElement('textarea');
    helper.className = 'xterm-helper-textarea';
    helper.tabIndex = 0;
    document.body.appendChild(helper);
    helper.focus();
    expect(isTextInputFocused()).toBe(false);
    helper.remove();
  });

  it('returns false for any aria-hidden element (e.g. invisible capture helpers)', () => {
    const capture = document.createElement('input');
    capture.setAttribute('aria-hidden', 'true');
    document.body.appendChild(capture);
    capture.focus();
    expect(isTextInputFocused()).toBe(false);
    capture.remove();
  });
});

/**
 * Regression tests for the inverse-direction guard added for issue #731:
 * the `?`-key cheatsheet toggle wants to NOT fire when the user is typing
 * in an agent terminal (the `?` is content for the agent, not a help
 * request). This is the opposite policy from Alt+G, which DOES want to
 * fire from a terminal prompt.
 */
describe('isTerminalFocused', () => {
  let previouslyFocused: Element | null;

  beforeEach(() => {
    previouslyFocused = document.activeElement;
    if (document.body && document.body.focus) document.body.focus();
  });

  afterEach(() => {
    if (previouslyFocused instanceof HTMLElement) {
      previouslyFocused.focus();
    }
  });

  it('returns true for xterm.js\'s hidden helper textarea (the policy case)', () => {
    const helper = document.createElement('textarea');
    helper.className = 'xterm-helper-textarea';
    helper.setAttribute('aria-hidden', 'true');
    document.body.appendChild(helper);
    helper.focus();
    expect(isTerminalFocused()).toBe(true);
    helper.remove();
  });

  it('returns true for an xterm helper even without aria-hidden (className fallback)', () => {
    const helper = document.createElement('textarea');
    helper.className = 'xterm-helper-textarea';
    document.body.appendChild(helper);
    helper.focus();
    expect(isTerminalFocused()).toBe(true);
    helper.remove();
  });

  it('returns false for a plain textarea (NOT an xterm helper)', () => {
    const ta = document.createElement('textarea');
    document.body.appendChild(ta);
    ta.focus();
    expect(isTerminalFocused()).toBe(false);
    ta.remove();
  });

  it('returns false for an input element', () => {
    const input = document.createElement('input');
    document.body.appendChild(input);
    input.focus();
    expect(isTerminalFocused()).toBe(false);
    input.remove();
  });

  it('returns false when nothing is focused (body)', () => {
    expect(isTerminalFocused()).toBe(false);
  });
});