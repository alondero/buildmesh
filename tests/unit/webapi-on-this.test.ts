import { describe, it, expect } from 'vitest';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, resolve } from 'node:path';

/**
 * Lint: forbid storing Web API methods on `this.x` without an arrow wrapper.
 *
 * Issue #156. Background (memory `buildmesh-webapi-receiver-binding`):
 * Chromium WebIDL bindings require methods like `requestAnimationFrame`,
 * `setTimeout`, `fetch`, etc. to be invoked with `window` as the receiver.
 * Storing one of those functions on an object property and then calling it
 * through that property silently throws `TypeError: Illegal invocation` in
 * production — the throw lands inside a Tauri listener that swallows it,
 * so the symptom looks like "events not arriving" rather than a crash.
 *
 * PR #149 fixed this for `TerminalWriter` + `requestAnimationFrame` (unit
 * test at `tests/unit/terminal-writer.test.ts:152`). The same trap recurs
 * for any Web API. This test is a pattern-guard analogous to
 * `tests/unit/ipc-contract.test.ts` — it walks `src/`, applies a regex,
 * and reports `file:line` for every match.
 *
 * Per-line opt-out: `// allow-webapi-on-this: <reason>` on the violation
 * line, mirroring the `// allow-dispose` / `// allow-wsl-path` convention
 * enforced by `.claude/hooks/guard-antipatterns.mjs`.
 */

const REPO_ROOT = resolve(__dirname, '..', '..');
const FRONTEND_SRC = join(REPO_ROOT, 'src');

// Keep this list aligned with the issue's "Fix" section. `crypto.subtle` is
// intentionally NOT here — `crypto.subtle.encrypt` is a method call on a
// property, not a bare global, so a `this.x = <bare>` regex can't catch it.
// (If we ever need it, the fix is an AST rule, not a regex one.)
const WEB_APIS = [
  'requestAnimationFrame',
  'cancelAnimationFrame',
  'queueMicrotask',
  'setTimeout',
  'setInterval',
  'clearTimeout',
  'clearInterval',
  'MutationObserver',
  'fetch',
];

// Match `this.<name> = <API>(;|EOL|,)`. The LHS is a single identifier so
// `this.foo.bar` (member-access on `this`) and `this.scheduler = window.x`
// (rhs isn't a bare API) don't false-positive. The `\b` after the API name
// avoids matching `this.x = requestAnimationFrameWrapper` (a parameter that
// happens to start with the API name). The negative lookahead `(?!\s*[.(])`
// excludes `this.x = requestAnimationFrame.bind(window)` (safe — .bind
// preserves the receiver) and `this.x = requestAnimationFrame(cb)` (also
// safe if you never call `this.x` with a different receiver, but in
// practice this form is what the arrow-wrap fix replaces).
const ESCAPE_HATCH = 'allow-webapi-on-this';
const PATTERN = new RegExp(
  String.raw`this\.\w+\s*=\s*(` +
    WEB_APIS.join('|') +
    String.raw`)\b(?!\s*[.(])`,
  'g',
);

interface Violation {
  api: string;
  file: string;
  line: number;
}

function walkTsFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    const st = statSync(full);
    if (st.isDirectory()) {
      walkTsFiles(full, out);
    } else if (/\.(ts|tsx)$/.test(entry)) {
      out.push(full);
    }
  }
  return out;
}

// Strip block + line comments. Same naive approach as ipc-contract.test.ts
// — an API name inside a comment is not a real assignment. Strings like
// `"// requestAnimationFrame"` would be over-stripped, but the API names
// are never meaningfully quoted in production code, so this is good
// enough. Returned shape is a list of (stripped-line, original-line) so
// the escape-hatch check can see the original line text.
function stripCommentsPreservingLineIndex(source: string): string[] {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .split('\n')
    .map(line => line.replace(/\/\/.*$/, ''));
}

// Production scan: walks files, applies the regex line-by-line, honours
// the per-line `// allow-webapi-on-this` escape hatch. The escape hatch
// is checked against the ORIGINAL (pre-strip) line so a comment is
// required on the violation line itself.
//
// Exported so synthetic tests can inject text without touching the
// filesystem (avoids temp-file teardown in unit tests).
export function findViolationsInSource(
  filePath: string,
  source: string,
): Violation[] {
  const violations: Violation[] = [];
  const stripped = stripCommentsPreservingLineIndex(source);
  const originalLines = source.split('\n');
  for (let i = 0; i < stripped.length; i++) {
    // Fresh non-global regex for line-by-line use to dodge lastIndex state.
    const lineRe = new RegExp(PATTERN.source);
    const m = lineRe.exec(stripped[i]);
    if (!m) continue;
    if (originalLines[i]?.includes(ESCAPE_HATCH)) continue;
    violations.push({ api: m[1], file: filePath, line: i + 1 });
  }
  return violations;
}

export function findViolations(files: string[]): Violation[] {
  const violations: Violation[] = [];
  for (const file of files) {
    const source = readFileSync(file, 'utf8');
    violations.push(...findViolationsInSource(file, source));
  }
  return violations;
}

// Same regex on an arbitrary string, used by the synthetic tests so we can
// prove the lint catches the regressions even when the production tree is
// (thankfully) clean. Strips all comments, no escape hatch (the synthetic
// tests assert each pattern in isolation). Returns the API name for each
// match.
export function findInText(source: string): string[] {
  const stripped = source
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/\/\/.*$/gm, '');
  const matches: string[] = [];
  // Fresh non-global regex for line-by-line use to dodge lastIndex state.
  const lineRe = new RegExp(PATTERN.source);
  for (const line of stripped.split('\n')) {
    lineRe.lastIndex = 0;
    const m = lineRe.exec(line);
    if (m) matches.push(m[1]);
  }
  return matches;
}

function relPath(absFile: string): string {
  return absFile.slice(REPO_ROOT.length + 1).replace(/\\/g, '/');
}

describe('Web API stored on this.x without arrow wrapper (#156)', () => {
  const files = walkTsFiles(FRONTEND_SRC);
  const violations = findViolations(files);

  it('walks src/ and finds .ts/.tsx files', () => {
    // Sanity gate — if this fails, the walkTsFiles path or include filter
    // is broken, and every other assertion in this file is meaningless.
    expect(files.length).toBeGreaterThan(20);
  });

  it('src/ has no this.x = <Web API> assignments', () => {
    const report = violations.map(v => `  ${v.api}  (${relPath(v.file)}:${v.line})`);

    if (violations.length > 0) {
      const distinctApis = [...new Set(violations.map(v => v.api))].sort();
      const message =
        `Found ${violations.length} assignment(s) of a raw Web API to this.x:\n` +
        report.join('\n') +
        `\n\nDistinct APIs: ${distinctApis.join(', ')}` +
        `\n\nIn Chromium/WebView2, storing requestAnimationFrame (and other Web APIs) on an` +
        `\nobject property and then calling it via this.x invokes the API with the wrong` +
        `\nreceiver (the object, not window), which throws "TypeError: Illegal invocation".` +
        `\nThe throw lands inside a Tauri listener that swallows it, so the symptom looks like` +
        `\n"events not arriving" rather than a crash. See issue #156 and memory` +
        `\nbuildmesh-webapi-receiver-binding.md.\n\n` +
        `\nFix: wrap the API in an arrow function so the bare-global call keeps window as the` +
        `\nreceiver:\n` +
        `\n  this.scheduler = (cb) => requestAnimationFrame(cb);\n` +
        `\nIf the assignment is genuinely safe (and you've checked the receiver), add the` +
        `\nper-line escape comment on the same line:\n` +
        `\n  this.scheduler = requestAnimationFrame; // allow-webapi-on-this: <reason>\n` +
        `\n(mirrors the // allow-dispose / // allow-wsl-path convention enforced by` +
        `\n.claude/hooks/guard-antipatterns.mjs).`;
      throw new Error(message);
    }
  });

  it('catches the canonical regression: this.scheduler = requestAnimationFrame', () => {
    // This is the exact pattern that bit TerminalWriter in #144 / PR #149.
    // If the regex ever regresses to miss this, the synthetic test fails
    // before the production scan becomes a silent no-op.
    const sample = `class T {
  constructor() {
    this.scheduler = requestAnimationFrame;
  }
}`;
    expect(findInText(sample)).toEqual(['requestAnimationFrame']);
  });

  it('every Web API in the list is caught when stored on this.x', () => {
    for (const api of WEB_APIS) {
      const sample = `this.x = ${api};`;
      const matches = findInText(sample);
      expect(matches, `expected ${api} to be flagged`).toEqual([api]);
    }
  });

  it('allows the arrow-wrapped safe pattern (TerminalWriter\'s default)', () => {
    // Mirrors src/components/Terminal/TerminalWriter.ts:60 — the canonical
    // safe form, written here as a regression guard so the regex can never
    // be tightened in a way that breaks the existing safe wrapper.
    const sample = `class TerminalWriter {
  private scheduler: SchedulerFn;
  constructor(scheduler: SchedulerFn = (cb) => requestAnimationFrame(cb)) {
    this.scheduler = scheduler;
  }
}`;
    expect(findInText(sample)).toEqual([]);
  });

  it('allows storing a parameterised scheduler (not the bare API)', () => {
    // `this.scheduler = scheduler;` where the rhs is a parameter — the
    // parameter could itself be the bad form upstream, but that's the
    // upstream's responsibility. Storing a non-API identifier is safe.
    const sample = `class T {
  constructor(scheduler: SchedulerFn) {
    this.scheduler = scheduler;
  }
}`;
    expect(findInText(sample)).toEqual([]);
  });

  it('allows the explicit-bind safe pattern (window.requestAnimationFrame.bind(window))', () => {
    // .bind(window) preserves the receiver, so the stored function is
    // safe to call through any `this`. The bare-API regex doesn't match
    // because the rhs starts with `window.`, not the API name.
    const sample = `this.scheduler = window.requestAnimationFrame.bind(window);`;
    expect(findInText(sample)).toEqual([]);
  });

  it('allows this.x assigned to an arrow wrapper over a different API', () => {
    // Same arrow-wrap form as the canonical safe pattern, but with setTimeout.
    // Belt-and-braces: prove the safe form is recognised for all APIs in
    // the list, not just requestAnimationFrame.
    for (const api of WEB_APIS) {
      const sample = `this.x = (cb: () => void) => ${api}(cb);`;
      expect(findInText(sample), `arrow-wrapped ${api} should pass`).toEqual([]);
    }
  });

  it('honours the per-line // allow-webapi-on-this escape hatch', () => {
    // The escape hatch sits on the same line as the violation, mirroring
    // the // allow-dispose / // allow-wsl-path convention. Exercise the
    // production path (`findViolationsInSource`) so a regression in the
    // hatch check would surface here, not just in the main file scan.
    const withHatch = [
      'this.scheduler = requestAnimationFrame; // allow-webapi-on-this: legacy chromium test stub',
    ].join('\n');
    expect(findViolationsInSource('synthetic.ts', withHatch)).toEqual([]);

    // The SAME pattern without the hatch comment MUST still be flagged.
    const withoutHatch = ['this.scheduler = requestAnimationFrame;'].join('\n');
    expect(findViolationsInSource('synthetic.ts', withoutHatch)).toEqual([
      { api: 'requestAnimationFrame', file: 'synthetic.ts', line: 1 },
    ]);

    // The hatch comment on a DIFFERENT line is NOT honoured — it must be
    // on the violation line itself, same as the other buildmesh allow-
    // comments. Catches a future "let me add the hatch to a sibling line"
    // mistake.
    const hatchOnWrongLine = [
      'this.scheduler = requestAnimationFrame;',
      '// allow-webapi-on-this: misplaced',
    ].join('\n');
    expect(findViolationsInSource('synthetic.ts', hatchOnWrongLine)).toEqual([
      { api: 'requestAnimationFrame', file: 'synthetic.ts', line: 1 },
    ]);
  });

  it('does NOT match identifiers that merely start with an API name', () => {
    // E.g. a parameter named `requestAnimationFrameWrapper` must not be
    // confused for the Web API. The `\b` after the API name in the regex
    // is what prevents this.
    const sample = `class T {
  constructor(requestAnimationFrameWrapper: (cb: () => void) => void) {
    this.scheduler = requestAnimationFrameWrapper;
  }
}`;
    expect(findInText(sample)).toEqual([]);
  });

  it('does NOT match local const / ref / window-qualified assignments', () => {
    // `const id = setTimeout(...)`, `ref.current = setTimeout(...)`, and
    // `window.setTimeout(...)` are all safe — the receiver-binding trap
    // is specific to `this.x` (or any plain object property) where the
    // stored function is later invoked through that property.
    const sample = `
      const id = setTimeout(cb, 100);
      timerRef.current = setTimeout(cb, 100);
      window.setTimeout(cb, 100);
      globalThis.setTimeout(cb, 100);
    `;
    expect(findInText(sample)).toEqual([]);
  });
});
