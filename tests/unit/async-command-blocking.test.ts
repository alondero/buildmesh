import { describe, it, expect } from 'vitest';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, resolve } from 'node:path';

/**
 * Pattern guard for issue #1380: async Tauri commands must not call
 * blocking `db::*`, `std::fs::*`, or `preferences::load`/`save` on a
 * Tokio worker thread.
 *
 * A `#[command] pub async fn` (and `#[command(async)]` on a sync fn)
 * runs on Tauri's bounded tokio pool. SQLite, disk, and JSON
 * serialization park that worker for the duration and starve WebSocket
 * streaming / PTY output. The established offload is
 * `crate::commands::run_blocking` (or `spawn_blocking`).
 *
 * Per-line opt-out: `// allow-blocking-on-async: <reason>` on the
 * violation line, mirroring `// allow-webapi-on-this`.
 *
 * Issue #1389 extends the scope in two directions:
 *
 *   1. The session_lifecycle sink writes (`on_attention`,
 *      `on_attention_cleared`) are forbidden in **every** `#[command]`
 *      body, sync or async — they're an internal detail that callers
 *      must reach through the public `mark_attention` / `clear_attention`
 *      helpers. Sync `#[command] fn` already runs on Tauri's IPC thread
 *      pool (NOT Tokio), so it doesn't need `run_blocking` for offload
 *      purposes — but the helper indirection is still required for
 *      testability and so the guard stays in one place.
 *
 *   2. The same session_lifecycle sink-write tokens are forbidden inside
 *      `src-tauri/src/http/routes/` too — those handlers run on Tokio
 *      workers, so a direct sink call there would park the worker on
 *      `db::update_agent_node_status`. The previous guard didn't scan
 *      that directory at all, leaving the route changes for #1389
 *      without automated coverage (PR #1429 review feedback).
 */

const REPO_ROOT = resolve(__dirname, '..', '..');
const COMMANDS_DIR = join(REPO_ROOT, 'src-tauri', 'src', 'commands');
const HTTP_ROUTES_DIR = join(REPO_ROOT, 'src-tauri', 'src', 'http', 'routes');
const ESCAPE_HATCH = 'allow-blocking-on-async';

/** Tokens that may ONLY appear inside `run_blocking`/`spawn_blocking` closures
 *  in **async** `#[command]` bodies. Sync `#[command] fn` is allowed to call
 *  these directly because Tauri 2 dispatches sync commands to its IPC thread
 *  pool, not Tokio — the offload isn't needed. */
const FORBIDDEN_ASYNC = [
  { kind: 'db', re: /\b(?:crate::)?db::/g },
  { kind: 'std::fs', re: /\bstd::fs::/g },
  { kind: 'preferences::load', re: /\b(?:crate::)?preferences::load\s*\(/g },
  { kind: 'preferences::save', re: /\b(?:crate::)?preferences::save\s*\(/g },
  // Evidence #1 in issue #1380: create/delete do SQLite + git worktree
  // work without a `db::` token in the command body, so token-level
  // matching against `db::*` alone would miss them. After the
  // trampoline refactor, `regenerate` is the async orchestrator whose
  // body chains three sync helpers — the command boundary must wrap
  // each in `run_blocking`, so an async command calling
  // `services::agent_node::regenerate(...)` is the same class of
  // violation as create/delete.
  //
  // PR #1388 round-2 review feedback 1: the matching token was
  // `regenerate_blocking` (a name that does not exist; the
  // refactor kept the orchestrator as `regenerate`). The fix below
  // matches the real orchestrator. Regression fixture at
  // async-command-blocking.test.ts:481 pins this case.
  {
    kind: 'services::agent_node::create',
    re: /\bservices::agent_node::create\s*\(/g,
  },
  {
    kind: 'services::agent_node::delete',
    re: /\bservices::agent_node::delete\s*\(/g,
  },
  {
    kind: 'services::agent_node::regenerate',
    re: /\bservices::agent_node::regenerate\s*\(/g,
  },
] as const;

/** Tokens forbidden in **every** `#[command]` body (sync or async) AND in
 *  every `pub async fn` body inside `src-tauri/src/http/routes/`. These are
 *  internal lifecycle sinks — the public surface is `mark_attention` /
 *  `clear_attention`. A direct call from a command or HTTP route bypasses
 *  the helper indirection and is a structural defect, not just a perf bug.
 *
 *  The regex makes `agent::` optional so a `use crate::agent::session_lifecycle;`
 *  at the top of the file is covered too (PR #1429 review feedback). */
const FORBIDDEN_ALL = [
  {
    kind: 'session_lifecycle::on_attention',
    re: /\b(?:(?:crate::)?agent::)?session_lifecycle::on_attention\s*\(/g,
  },
  {
    kind: 'session_lifecycle::on_attention_cleared',
    re: /\b(?:(?:crate::)?agent::)?session_lifecycle::on_attention_cleared\s*\(/g,
  },
] as const;

export interface BlockingViolation {
  kind: string;
  command: string;
  file: string;
  line: number;
}

function walkRsFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    const st = statSync(full);
    if (st.isDirectory()) {
      walkRsFiles(full, out);
    } else if (entry.endsWith('.rs')) {
      out.push(full);
    }
  }
  return out;
}

/** Replace comments with spaces so line numbers stay aligned. */
function stripCommentsPreserveLines(source: string): string {
  let out = '';
  let i = 0;
  while (i < source.length) {
    if (source.startsWith('//', i)) {
      while (i < source.length && source[i] !== '\n') {
        out += ' ';
        i++;
      }
      continue;
    }
    if (source.startsWith('/*', i)) {
      out += '  ';
      i += 2;
      while (i < source.length && !source.startsWith('*/', i)) {
        out += source[i] === '\n' ? '\n' : ' ';
        i++;
      }
      if (i < source.length) {
        out += '  ';
        i += 2;
      }
      continue;
    }
    out += source[i];
    i++;
  }
  return out;
}

function matchPair(
  source: string,
  openIndex: number,
  openChar: string,
  closeChar: string,
): number {
  let depth = 0;
  let inString = false;
  let stringChar = '';
  let escaped = false;
  let rawHashes = 0;
  for (let i = openIndex; i < source.length; i++) {
    const c = source[i];

    // Inside a string literal, advance to the matching close. Skip the
    // next character on `\\` (Rust escapes `\\`, `\"`, `\'`, `\n`, ...).
    if (inString) {
      if (rawHashes > 0) {
        // Raw string `r"..."` / `r#"..."#`: only `"` followed by the
        // exact same number of `#`s closes it. No escapes inside.
        if (c === '"') {
          let ok = true;
          for (let k = 0; k < rawHashes; k++) {
            if (source[i + 1 + k] !== '#') {
              ok = false;
              break;
            }
          }
          if (ok) {
            rawHashes = 0;
            inString = false;
            i += rawHashes;
          }
        }
        continue;
      }
      if (escaped) {
        escaped = false;
        continue;
      }
      if (c === '\\') {
        escaped = true;
        continue;
      }
      if (c === stringChar) inString = false;
      continue;
    }

    // Detect raw string opener: `r"`, `r#"`, `r##"`, ... The number of
    // `#`s before `"` is the count that must follow the closing `"`.
    if (c === 'r' && (source[i + 1] === '"' || source[i + 1] === '#')) {
      let j = i + 1;
      let hashes = 0;
      while (source[j] === '#') {
        hashes++;
        j++;
      }
      if (source[j] === '"') {
        inString = true;
        stringChar = '"';
        rawHashes = hashes;
        i = j;
        continue;
      }
    }

    // Detect byte literal opener: `b'x'` — exactly 1 ASCII char or
    // single escape between `'` quotes, preceded by `b`.
    if (c === 'b' && source[i + 1] === "'") {
      const after1 = source[i + 2];
      if (
        after1 !== undefined &&
        source[i + 3] === "'" &&
        (after1 === '\\' || after1 === "'" || /[^'\\\n]/.test(after1))
      ) {
        i = i + 3; // consume `b'?'` whole
        continue;
      }
    }

    // Detect Rust lifetime: `'` preceded by `&`, `<`, `,`, or an
    // identifier char (so `&'a`, `Foo<'a>`, `<'a, 'b>`, and turbofish
    // `bar::<'a>(...)` are all lifetimes, not char literals).
    // Optional whitespace between the prefix and the `'` is allowed
    // (`Foo< 'a>` parses). Lifetimes are one identifier char — skip
    // past the next char to consume the lifetime entirely.
    //
    // PR #1388 round-2 review feedback 2: the original fix only
    // recognised `&` and identifier-char prefixes, which silently
    // dropped body-internal lifetimes like `Foo<'a>` from the audit.
    // The regression fixture at async-command-blocking.test.ts:419
    // pins this case so the bug cannot regress.
    if (c === "'" && i > 0) {
      let j = i - 1;
      while (j >= 0 && /\s/.test(source[j])) j--;
      const prev = j >= 0 ? source[j] : '';
      if (
        prev === '&' ||
        prev === '<' ||
        prev === ',' ||
        /[A-Za-z0-9_]/.test(prev)
      ) {
        if (/[A-Za-z_]/.test(source[i + 1] ?? '')) {
          i += 1; // consume `'a`
          continue;
        }
      }
    }

    if (c === '"' || c === "'") {
      inString = true;
      stringChar = c;
      continue;
    }
    if (c === openChar) depth++;
    else if (c === closeChar) {
      depth--;
      if (depth === 0) return i;
    }
  }
  return -1;
}

const OFFLOAD_RE = /\b(?:run_blocking|spawn_blocking)\s*\(/g;

/** Blank out `run_blocking(...)` / `spawn_blocking(...)` invocations. */
export function stripOffloadedCalls(body: string): string {
  const chars = body.split('');
  OFFLOAD_RE.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = OFFLOAD_RE.exec(body)) !== null) {
    const open = m.index + m[0].length - 1;
    const close = matchPair(body, open, '(', ')');
    if (close < 0) break;
    for (let i = m.index; i <= close; i++) {
      if (chars[i] !== '\n') chars[i] = ' ';
    }
    OFFLOAD_RE.lastIndex = close + 1;
  }
  return chars.join('');
}

interface AsyncCommand {
  name: string;
  body: string;
  bodyStart: number;
}

const COMMAND_ATTR = /#\[(?:tauri::)?command(?:\(([^)]*)\))?\]/g;
// PR #1388 review feedback 1B: the leading group captures *every*
// intervening attribute / doc-comment run between `#[command]` and
// `pub ... fn`, so any amount of decoration is allowed (no magic
// window). `pub` is matched anywhere after the attribute run; the
// greedy `(?:#\[[^\]]*\]\s*)*` consumes all `#[...]` blocks first.
const FN_AFTER_ATTR =
  /(?:#\[[^\]]*\][\s\S]*?)*pub(?:\s*\(\s*crate\s*\))?\s+(async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)/;

export function extractAsyncCommandBodies(
  source: string,
  requireAsync: boolean = true,
): AsyncCommand[] {
  const stripped = stripCommentsPreserveLines(source);
  const found: AsyncCommand[] = [];
  COMMAND_ATTR.lastIndex = 0;
  let attr: RegExpExecArray | null;
  while ((attr = COMMAND_ATTR.exec(stripped)) !== null) {
    const attrAsync = /\basync\b/.test(attr[1] ?? '');
    const after = stripped.slice(attr.index + attr[0].length);
    const fnMatch = FN_AFTER_ATTR.exec(after);
    if (!fnMatch) continue;
    const isAsyncFn = Boolean(fnMatch[1]);
    if (requireAsync && !attrAsync && !isAsyncFn) continue;
    const name = fnMatch[2];
    // PR #1388 review point 3 — when an async command is annotated
    // with `#[blocking_command]` (the proc-macro that wraps the body
    // in `run_blocking(label, move || { ... }).await`), the source
    // body still LOOKS unguarded to this regex scanner (the macro
    // rewrites at compile time). Skip the body so the macro user
    // isn't penalised for the very feature that removes the
    // boilerplate. `fnMatch[0]` is the substring the regex consumed
    // between `#[command]` and the fn name, so any
    // `#[blocking_command]` between them is in there.
    if (/#\[(?:\w+::)?blocking_command\b/.test(fnMatch[0])) {
      continue;
    }
    const fnStart = attr.index + attr[0].length + fnMatch.index;
    const sigStart = stripped.indexOf('(', fnStart);
    if (sigStart < 0) continue;
    const sigEnd = matchPair(stripped, sigStart, '(', ')');
    if (sigEnd < 0) continue;
    const brace = stripped.indexOf('{', sigEnd);
    if (brace < 0) continue;
    const braceEnd = matchPair(stripped, brace, '{', '}');
    if (braceEnd < 0) continue;
    found.push({
      name,
      body: stripped.slice(brace, braceEnd + 1),
      bodyStart: brace,
    });
    COMMAND_ATTR.lastIndex = braceEnd + 1;
  }
  return found;
}

function lineNumberAt(source: string, index: number): number {
  let line = 1;
  for (let i = 0; i < index && i < source.length; i++) {
    if (source[i] === '\n') line++;
  }
  return line;
}

export function findBlockingInAsyncCommands(
  filePath: string,
  source: string,
): BlockingViolation[] {
  const originalLines = source.split('\n');
  const commands = extractAsyncCommandBodies(source, /* requireAsync */ true);
  return scanCommandsForTokens(filePath, source, originalLines, commands, FORBIDDEN_ASYNC);
}

/** Scan ALL `#[command]` bodies (sync or async) for the
 *  `session_lifecycle::on_attention[_cleared]` sink writes. The sink is an
 *  internal detail — the public surface is `mark_attention` /
 *  `clear_attention` in `commands::attention`. Sync commands on Tauri's IPC
 *  thread pool don't need `run_blocking` for offload, but the helper
 *  indirection is still mandatory so this single guard can pin the rule. */
export function findSinkWritesInAllCommands(
  filePath: string,
  source: string,
): BlockingViolation[] {
  const originalLines = source.split('\n');
  const commands = extractAsyncCommandBodies(source, /* requireAsync */ false);
  return scanCommandsForTokens(filePath, source, originalLines, commands, FORBIDDEN_ALL);
}

/** Scan every `pub async fn` (and `pub fn`) body inside
 *  `src-tauri/src/http/routes/` for `session_lifecycle::on_attention[_cleared]`.
 *  Route handlers run on Tokio workers, so a direct sink call would park
 *  the worker on `db::update_agent_node_status`; route code must reach
 *  through `crate::commands::attention::{mark,clear}_attention` instead.
 *  PR #1429 review feedback: the previous guard never scanned this
 *  directory, leaving the #1389 HTTP route changes without automated
 *  coverage. */
export function findSinkWritesInHttpRoutes(
  filePath: string,
  source: string,
): BlockingViolation[] {
  const originalLines = source.split('\n');
  const bodies = extractRouteHandlerBodies(source);
  return scanCommandsForTokens(
    filePath,
    source,
    originalLines,
    bodies,
    FORBIDDEN_ALL,
  );
}

function scanCommandsForTokens(
  filePath: string,
  source: string,
  originalLines: string[],
  commands: AsyncCommand[],
  tokens: readonly { kind: string; re: RegExp }[],
): BlockingViolation[] {
  const violations: BlockingViolation[] = [];
  for (const cmd of commands) {
    const remaining = stripOffloadedCalls(cmd.body);
    for (const { kind, re } of tokens) {
      re.lastIndex = 0;
      let m: RegExpExecArray | null;
      while ((m = re.exec(remaining)) !== null) {
        const absIndex = cmd.bodyStart + m.index;
        const line = lineNumberAt(source, absIndex);
        const original = originalLines[line - 1] ?? '';
        if (original.includes(ESCAPE_HATCH)) continue;
        violations.push({ kind, command: cmd.name, file: filePath, line });
      }
    }
  }
  return violations;
}

/** Walk every top-level `pub fn` / `pub async fn` body in a source file.
 *  Used for `src-tauri/src/http/routes/` where handlers are plain
 *  `pub async fn`, not `#[command]`-annotated. Mirrors the body-extraction
 *  logic in `extractAsyncCommandBodies` but skips the `#[command]` attribute
 *  search entirely. */
const FN_DEF =
  /pub(?:\s*\(\s*crate\s*\))?\s+(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)/g;

function extractRouteHandlerBodies(source: string): AsyncCommand[] {
  const stripped = stripCommentsPreserveLines(source);
  const found: AsyncCommand[] = [];
  FN_DEF.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = FN_DEF.exec(stripped)) !== null) {
    const name = m[1];
    const sigStart = stripped.indexOf('(', m.index + m[0].length);
    if (sigStart < 0) continue;
    const sigEnd = matchPair(stripped, sigStart, '(', ')');
    if (sigEnd < 0) continue;
    const brace = stripped.indexOf('{', sigEnd);
    if (brace < 0) continue;
    const braceEnd = matchPair(stripped, brace, '{', '}');
    if (braceEnd < 0) continue;
    found.push({
      name,
      body: stripped.slice(brace, braceEnd + 1),
      bodyStart: brace,
    });
    FN_DEF.lastIndex = braceEnd + 1;
  }
  return found;
}

function relPath(absFile: string): string {
  return absFile.slice(REPO_ROOT.length + 1).replace(/\\/g, '/');
}

describe('async Tauri commands must offload blocking db/fs/prefs (#1380)', () => {
  const files = walkRsFiles(COMMANDS_DIR);
  const violations = files.flatMap((file) =>
    findBlockingInAsyncCommands(file, readFileSync(file, 'utf8')),
  );

  // Issue #1389 second pass: session_lifecycle sink writes are forbidden
  // in every #[command] body (sync or async) and in every HTTP route
  // handler. Pre-#1429 the guard only checked async command bodies, so the
  // #1389 HTTP route changes had zero automated coverage.
  const sinkWriteViolations = [
    ...files.flatMap((file) =>
      findSinkWritesInAllCommands(file, readFileSync(file, 'utf8')),
    ),
    ...walkRsFiles(HTTP_ROUTES_DIR).flatMap((file) =>
      findSinkWritesInHttpRoutes(file, readFileSync(file, 'utf8')),
    ),
  ];

  it('walks src-tauri/src/commands and finds Rust files', () => {
    expect(files.length).toBeGreaterThan(10);
  });

  it('walks src-tauri/src/http/routes and finds Rust files', () => {
    expect(walkRsFiles(HTTP_ROUTES_DIR).length).toBeGreaterThan(5);
  });

  it('no async #[command] body calls db::*, std::fs::*, or preferences::load/save outside run_blocking', () => {
    if (violations.length === 0) return;
    const report = violations
      .map(
        (v) =>
          `  ${v.command}  ${v.kind}  (${relPath(v.file)}:${v.line})`,
      )
      .join('\n');
    throw new Error(
      `Found ${violations.length} blocking call(s) on a Tokio async command worker:\n` +
        report +
        `\n\nA #[command] async fn runs on Tauri's bounded tokio pool. SQLite,` +
        `\ndisk I/O, and preferences.json serialization must go through` +
        `\ncrate::commands::run_blocking (or spawn_blocking) so WebSocket` +
        `\nstreaming and PTY output keep being polled. See issue #1380.` +
        `\n\nFix: wrap the blocking work:` +
        `\n  crate::commands::run_blocking("command_name", move || { ... }).await` +
        `\n\nIf the call is genuinely non-blocking, add on the same line:` +
        `\n  db::foo(); // allow-blocking-on-async: <reason>`,
    );
  });

  // Issue #1389 — sink writes are forbidden in every #[command] body
  // (sync or async) and in every HTTP route handler. Callers must reach
  // through the public `mark_attention` / `clear_attention` helpers in
  // `commands::attention` — direct sink calls are an internal detail.
  it('no #[command] body or HTTP route handler calls session_lifecycle::on_attention[_cleared] directly (#1389)', () => {
    if (sinkWriteViolations.length === 0) return;
    const report = sinkWriteViolations
      .map(
        (v) =>
          `  ${v.command}  ${v.kind}  (${relPath(v.file)}:${v.line})`,
      )
      .join('\n');
    throw new Error(
      `Found ${sinkWriteViolations.length} direct session_lifecycle sink write(s):\n` +
        report +
        `\n\nThe sink writes (session_lifecycle::on_attention / on_attention_cleared)` +
        `\nare an internal detail. Callers (Tauri commands, mobile HTTP routes)` +
        `\nmust reach through the public helpers:` +
        `\n  crate::commands::attention::mark_attention(node_id, &app)` +
        `\n  crate::commands::attention::clear_attention(node_id, &app)` +
        `\nSync commands don't need run_blocking for offload (Tauri 2 dispatches` +
        `\nsync #[command] fn to its IPC thread pool, not Tokio), but the helper` +
        `\nindirection is still mandatory so the guard stays in one place.`,
    );
  });

  it('flags a db:: call in an async command body (positive)', () => {
    const src = `
#[command]
pub async fn list_agent_nodes() -> Result<Vec<AgentNode>, String> {
    db::list_agent_nodes().map_err(|e| e.to_string())
}
`;
    const found = findBlockingInAsyncCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual(['db']);
    expect(found[0].command).toBe('list_agent_nodes');
  });

  it('flags preferences::load in an async command body (positive)', () => {
    const src = `
#[command]
pub async fn get_app_preferences() -> Result<AppPreferences, String> {
    preferences::load()
}
`;
    const found = findBlockingInAsyncCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual(['preferences::load']);
  });

  it('flags std::fs:: in an async command body (positive)', () => {
    const src = `
#[tauri::command]
pub async fn write_settings() -> Result<(), String> {
    std::fs::write("x", "y").map_err(|e| e.to_string())
}
`;
    const found = findBlockingInAsyncCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual(['std::fs']);
  });

  it('flags an unwrapped services::agent_node::create (positive, issue evidence)', () => {
    const src = `
#[command]
pub async fn create_agent_node() -> Result<AgentNode, String> {
    services::agent_node::create(1, &path, &branch, None, None, None, None, None, None)
        .map_err(|e| e.to_string())
}
`;
    const found = findBlockingInAsyncCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual([
      'services::agent_node::create',
    ]);
  });

  // PR #1388 round-2 review feedback 2 — lifetime detection was
  // passing only by accident. The PREVIOUS fixture
  //   `pub async fn re<'a>(s: &'a str)`
  // had the lifetime in the SIGNATURE — matchPair for `{...}`
  // starts AT `{`, so it never sees the signature. The CORRECT
  // regression fixture has the lifetime INSIDE the body (a generic
  // type like `Foo<'a>` or a turbofish `bar::<'a, T>(...)`), which
  // IS processed by matchPair and trips the `prev === '<'` case
  // the reviewer flagged.
  it('flags a db:: call inside an async command with a single <\'a> lifetime in the body (regression #1388 r2)', () => {
    const src = `
#[command]
pub async fn re() -> Result<(), String> {
    let _ : Foo<'a> = db::record().map_err(|e| e.to_string())?;
    Ok(())
}
`;
    const found = findBlockingInAsyncCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual(['db']);
    expect(found[0].command).toBe('re');
  });

  it('does not break on multiple comma-separated generic lifetimes in the body <\'a, \'b>', () => {
    const src = `
#[command]
pub async fn two() -> Result<(), String> {
    let _ : Foo<'a, 'b, T> = db::combine().map_err(|e| e.to_string())?;
    Ok(())
}
`;
    const found = findBlockingInAsyncCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual(['db']);
    expect(found[0].command).toBe('two');
  });

  it('does not break on a turbofish call with a lifetime: bar::<\'a>(...)', () => {
    // turbofish `bar::<'a, T>(x)` is a common Rust idiom. The
    // `<'a>` after `::` is what the original lexer dropped because
    // prev = '<' is neither '&' nor an identifier char.
    const src = `
#[command]
pub async fn turbo() -> Result<(), String> {
    let n = bar::<'a, u32>(7);
    db::store(n).map_err(|e| e.to_string())
}
`;
    const found = findBlockingInAsyncCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual(['db']);
    expect(found[0].command).toBe('turbo');
  });

  it('does not break on a lifetime preceded by whitespace inside <>', () => {
    // Whitespace between `<` and `'a` is allowed in Rust (rare but legal).
    // `Foo< 'a>` parses. The lexer must accept it.
    const src = `
#[command]
pub async fn spaced() -> Result<(), String> {
    let _ : Foo< 'a> = db::count().map_err(|e| e.to_string())?;
    Ok(())
}
`;
    const found = findBlockingInAsyncCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual(['db']);
    expect(found[0].command).toBe('spaced');
  });

  // PR #1388 round-2 review feedback 1 — the FORBIDDEN regex was
  // matching `services::agent_node::regenerate_blocking` (a name
  // that does NOT exist; the refactor extracted three helpers with
  // `_blocking` suffixes but the ASYNC orchestrator kept the
  // original name `regenerate`). An async command body that calls
  // `services::agent_node::regenerate(...)` must be flagged.
  it('flags an async command body calling services::agent_node::regenerate', () => {
    const src = `
#[command]
pub async fn regenerate_agent_node(node_id: i64) -> Result<(), String> {
    services::agent_node::regenerate(node_id, &"claude", &app).await
}
`;
    const found = findBlockingInAsyncCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual([
      'services::agent_node::regenerate',
    ]);
    expect(found[0].command).toBe('regenerate_agent_node');
  });

  it('does not flag services::agent_node::create inside run_blocking (negative)', () => {
    const src = `
#[command]
pub async fn create_agent_node() -> Result<AgentNode, String> {
    crate::commands::run_blocking("create_agent_node", move || {
        services::agent_node::create(1, &path, &branch, None, None, None, None, None, None)
            .map_err(|e| e.to_string())
    })
    .await
}
`;
    expect(findBlockingInAsyncCommands('synth.rs', src)).toEqual([]);
  });

  it('does not flag a db:: call already inside run_blocking (negative)', () => {
    const src = `
#[command]
pub async fn list_agent_nodes() -> Result<Vec<AgentNode>, String> {
    crate::commands::run_blocking("list_agent_nodes", || {
        db::list_agent_nodes().map_err(|e| e.to_string())
    })
    .await
}
`;
    expect(findBlockingInAsyncCommands('synth.rs', src)).toEqual([]);
  });

  it('does not flag a sync #[command] fn (negative)', () => {
    const src = `
#[command]
pub fn list_circuits(mesh_id: i64) -> Result<Vec<AutopilotCircuit>, String> {
    crate::db::list_autopilot_circuits(mesh_id).map_err(|e| e.to_string())
}
`;
    expect(findBlockingInAsyncCommands('synth.rs', src)).toEqual([]);
  });

  it('honours the per-line escape hatch', () => {
    const src = `
#[command]
pub async fn weird() -> Result<(), String> {
    db::list_meshes().map_err(|e| e.to_string())?; // allow-blocking-on-async: test fixture
    Ok(())
}
`;
    expect(findBlockingInAsyncCommands('synth.rs', src)).toEqual([]);
  });

  it('does not honour an escape hatch on the wrong line', () => {
    const src = `
#[command]
pub async fn weird() -> Result<(), String> {
    // allow-blocking-on-async: not on the call
    db::list_meshes().map_err(|e| e.to_string())?;
    Ok(())
}
`;
    expect(findBlockingInAsyncCommands('synth.rs', src).map((v) => v.kind)).toEqual([
      'db',
    ]);
  });

  it('still flags a db:: call that sits beside an offloaded closure', () => {
    const src = `
#[command]
pub async fn mixed(mesh_id: i64) -> Result<String, String> {
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    crate::commands::run_blocking("git_sync", move || git_sync_blocking(mesh.path)).await
}
`;
    const found = findBlockingInAsyncCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual(['db']);
    expect(found[0].command).toBe('mixed');
  });

  // PR #1388 review feedback 1A — matchPair must distinguish Rust
  // lifetimes (`&'a str`) and byte literals (`b'x'`) from string
  // delimiters. The previous implementation treated every `'` as a
  // string opener, so any async command using a lifetime was silently
  // dropped from the audit.
  it('flags a db:: call inside an async command body that uses a lifetime', () => {
    const src = `
#[command]
pub async fn re<'a>(s: &'a str) -> Result<(), String>
where
    &'a str: Sized,
{
    db::record(s).map_err(|e| e.to_string())
}
`;
    const found = findBlockingInAsyncCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual(['db']);
    expect(found[0].command).toBe('re');
  });

  it('does not break on byte literals like b\'x\'', () => {
    // Byte literal `b'\\n'` after the same `db::` call must not split
    // the body — the test scans for `db::` and must still find one.
    const src = `
#[command]
pub async fn scan() -> Result<u8, String> {
    let n: u8 = b'\\n';
    db::get_count().map(|c| c + n as u64).map_err(|e| e.to_string())
}
`;
    const found = findBlockingInAsyncCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual(['db']);
    expect(found[0].command).toBe('scan');
  });

  it('does not break on raw strings like r#"..."#', () => {
    // Raw strings contain unbalanced " and # sequences; the lexer
    // must close on r#"..."# not on a stray " inside.
    const src = `
#[command]
pub async fn query() -> Result<String, String> {
    let pattern = r#"SELECT * FROM "meshes" WHERE name = 'core'"#;
    db::run_raw(pattern).map_err(|e| e.to_string())
}
`;
    const found = findBlockingInAsyncCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual(['db']);
    expect(found[0].command).toBe('query');
  });

  // PR #1388 review feedback 1B — the magic 80-char window between
  // #[command] and pub async fn silently dropped any command with
  // attributes or doc comments longer than ~80 chars total. Engineered
  // against a real example from services/agent_node.rs::regenerate
  // (which has 13+ lines of doc comment block).
  it('flags a db:: call in an async command with a long attribute/doc-comment run between #[command] and pub async fn', () => {
    // The doc-comment + 3 attribute lines between #[command] and
    // `pub async fn` push the FN_AFTER_ATTR match index well past 80
    // chars — the historical "magic window" the reviewer flagged.
    const src = `
#[command]
/// Long doc comment line one describing the function in detail.
/// Long doc comment line two describing the function in detail.
/// Long doc comment line three describing the function in detail.
#[tracing::instrument(skip(app))]
#[deprecated(note = "use regenerate_v2")]
#[allow(dead_code)]
pub async fn regenerate_agent_node(node_id: i64, app: tauri::AppHandle) -> Result<(), String> {
    let _ = app;
    db::set_provider(node_id, "anthropic").map_err(|e| e.to_string())
}
`;
    const found = findBlockingInAsyncCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual(['db']);
    expect(found[0].command).toBe('regenerate_agent_node');
  });

  it('flags a db:: call in an async command with stacked attributes that exceed 80 chars', () => {
    // Three attributes with realistic argument strings push fnMatch.index
    // over the 80-char guard rail.
    const src = `
#[command]
#[tracing::instrument(skip_all, fields(mesh_id = %mesh_id, name = %name))]
#[allow(clippy::needless_pass_by_value)]
#[deprecated(since = "1.3.0", note = "regenerate v3 supersedes; remove after 1.5.0")]
pub async fn legacy_regenerate(mesh_id: i64, name: String) -> Result<(), String> {
    db::set_provider(mesh_id, &name).map_err(|e| e.to_string())
}
`;
    const found = findBlockingInAsyncCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual(['db']);
    expect(found[0].command).toBe('legacy_regenerate');
  });

  // PR #1388 review point 3 — `#[blocking_command]` is the proc-macro
  // that wraps the body in `run_blocking(label, move || { ... }).await`.
  // The source body still LOOKS unguarded to this scanner; the guard
  // must recognise the macro and skip the body.
  it('does not flag a db:: call inside an async command annotated with #[blocking_command]', () => {
    const src = `
#[command]
#[blocking_command]
pub async fn read_count() -> Result<i64, String> {
    db::count_nodes().map_err(|e| e.to_string())
}
`;
    expect(findBlockingInAsyncCommands('synth.rs', src)).toEqual([]);
  });

  it('does not flag an async command annotated with #[buildmesh_macros::blocking_command]', () => {
    const src = `
#[command]
#[buildmesh_macros::blocking_command]
pub async fn read_count() -> Result<i64, String> {
    db::count_nodes().map_err(|e| e.to_string())
}
`;
    expect(findBlockingInAsyncCommands('synth.rs', src)).toEqual([]);
  });

  // Issue #1389 — the lifecycle sink writes the DB through
  // `db::update_agent_node_status` with no `db::` token in the
  // command body. Guard must flag the helper name so a future
  // async command that calls it gets caught.
  it('flags session_lifecycle::on_attention in an async command body (issue #1389)', () => {
    const src = `
#[command]
pub async fn register_attention_node(app: tauri::AppHandle, node_id: i64) -> Result<(), String> {
    let sink = crate::agent::session_lifecycle::AppSessionLifecycleSink { app: &app };
    let _ = crate::agent::session_lifecycle::on_attention(&sink, node_id);
    Ok(())
}
`;
    const found = findSinkWritesInAllCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual(['session_lifecycle::on_attention']);
    expect(found[0].command).toBe('register_attention_node');
  });

  it('flags session_lifecycle::on_attention_cleared in an async command body (issue #1389)', () => {
    const src = `
#[command]
pub async fn clear_attention_node(app: tauri::AppHandle, node_id: i64) -> Result<(), String> {
    let sink = crate::agent::session_lifecycle::AppSessionLifecycleSink { app: &app };
    crate::agent::session_lifecycle::on_attention_cleared(&sink, node_id)
}
`;
    const found = findSinkWritesInAllCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual([
      'session_lifecycle::on_attention_cleared',
    ]);
    expect(found[0].command).toBe('clear_attention_node');
  });

  // Issue #1389 / PR #1429 review feedback: sync `#[command] fn` runs on
  // Tauri's IPC thread pool, NOT Tokio, so it doesn't need run_blocking
  // for offload purposes. But the helper indirection is still mandatory —
  // direct sink calls in a sync command body are a structural defect
  // because they bypass `mark_attention` / `clear_attention`.
  it('flags session_lifecycle::on_attention in a SYNC command body (issue #1389, PR #1429 feedback)', () => {
    const src = `
#[command]
pub fn register_attention_node(app: tauri::AppHandle, node_id: i64) -> Result<(), String> {
    let sink = crate::agent::session_lifecycle::AppSessionLifecycleSink { app: &app };
    let _ = crate::agent::session_lifecycle::on_attention(&sink, node_id);
    Ok(())
}
`;
    const found = findSinkWritesInAllCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual(['session_lifecycle::on_attention']);
    expect(found[0].command).toBe('register_attention_node');
  });

  it('flags session_lifecycle::on_attention_cleared in a SYNC command body (issue #1389, PR #1429 feedback)', () => {
    const src = `
#[command]
pub fn clear_attention_node(app: tauri::AppHandle, node_id: i64) -> Result<(), String> {
    let sink = crate::agent::session_lifecycle::AppSessionLifecycleSink { app: &app };
    crate::agent::session_lifecycle::on_attention_cleared(&sink, node_id)
}
`;
    const found = findSinkWritesInAllCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual([
      'session_lifecycle::on_attention_cleared',
    ]);
    expect(found[0].command).toBe('clear_attention_node');
  });

  // Issue #1389 / PR #1429 review feedback: the regex used to require
  // `agent::session_lifecycle::` exactly. A `use
  // crate::agent::session_lifecycle;` at the top of the file would let
  // the bare `session_lifecycle::on_attention(...)` slip through. The
  // FORBIDDEN_ALL regex now makes `agent::` optional.
  it('flags session_lifecycle::on_attention via use-import (PR #1429 feedback)', () => {
    const src = `
use crate::agent::session_lifecycle;

#[command]
pub async fn mark(app: tauri::AppHandle, node_id: i64) -> Result<(), String> {
    let sink = session_lifecycle::AppSessionLifecycleSink { app: &app };
    session_lifecycle::on_attention(&sink, node_id);
    Ok(())
}
`;
    const found = findSinkWritesInAllCommands('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual(['session_lifecycle::on_attention']);
  });

  it('does not flag session_lifecycle::on_attention inside run_blocking (issue #1389 negative)', () => {
    // The closure inside `run_blocking(...)` is blanked by the guard's
    // OFFLOAD_RE pass, so the helper call inside the closure is invisible
    // to FORBIDDEN_ALL.
    const src = `
#[command]
pub async fn register_attention_node(app: tauri::AppHandle, node_id: i64) -> Result<(), String> {
    crate::commands::run_blocking("register_attention_node", move || {
        let sink = crate::agent::session_lifecycle::AppSessionLifecycleSink { app: &app };
        let _ = crate::agent::session_lifecycle::on_attention(&sink, node_id);
        Ok(())
    })
    .await
}
`;
    expect(findSinkWritesInAllCommands('synth.rs', src)).toEqual([]);
  });

  it('does not flag session_lifecycle::on_attention when reached through mark_attention (issue #1389 negative)', () => {
    // The public surface — `mark_attention` is the only sanctioned entry
    // point. Direct sink calls live inside the helper, which the guard
    // does not scan (it's not a #[command] body).
    const src = `
#[command]
pub fn register_attention_node(app: tauri::AppHandle, node_id: i64) -> Result<(), String> {
    crate::commands::attention::mark_attention(node_id, &app);
    Ok(())
}
`;
    expect(findSinkWritesInAllCommands('synth.rs', src)).toEqual([]);
  });

  it('does not flag session_lifecycle::on_attention_cleared when reached through clear_attention (issue #1389 negative)', () => {
    const src = `
#[command]
pub fn clear_attention_node(app: tauri::AppHandle, node_id: i64) -> Result<(), String> {
    crate::commands::attention::clear_attention(node_id, &app);
    Ok(())
}
`;
    expect(findSinkWritesInAllCommands('synth.rs', src)).toEqual([]);
  });

  // Issue #1389 / PR #1429 review feedback: the previous guard didn't
  // scan src-tauri/src/http/routes/ at all. The session_lifecycle sink
  // writes are forbidden in HTTP route handlers too — those run on Tokio
  // workers, so a direct sink call would park the worker on
  // db::update_agent_node_status.
  it('flags session_lifecycle::on_attention in an HTTP route handler (issue #1389, PR #1429 feedback)', () => {
    const src = `
pub async fn discover(lines: &mut tokio::io::BufStream<MaybeTls>, mesh_id: i64) {
    let sink = crate::agent::session_lifecycle::AppSessionLifecycleSink { app: &app };
    crate::agent::session_lifecycle::on_attention(&sink, 7);
}
`;
    const found = findSinkWritesInHttpRoutes('synth.rs', src);
    expect(found.map((v) => v.kind)).toEqual(['session_lifecycle::on_attention']);
    expect(found[0].command).toBe('discover');
  });

  it('does not flag a session_lifecycle::on_attention call inside run_blocking inside an HTTP route (issue #1389 negative)', () => {
    const src = `
pub async fn discover(lines: &mut tokio::io::BufStream<MaybeTls>) {
    let app = crate::http::app_handle().unwrap();
    crate::commands::run_blocking("http_discover_attention", move || {
        let sink = crate::agent::session_lifecycle::AppSessionLifecycleSink { app: &app };
        crate::agent::session_lifecycle::on_attention(&sink, 7);
        Ok(())
    })
    .await;
}
`;
    expect(findSinkWritesInHttpRoutes('synth.rs', src)).toEqual([]);
  });
});
