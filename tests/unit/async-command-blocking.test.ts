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
 */

const REPO_ROOT = resolve(__dirname, '..', '..');
const COMMANDS_DIR = join(REPO_ROOT, 'src-tauri', 'src', 'commands');
const ESCAPE_HATCH = 'allow-blocking-on-async';

const FORBIDDEN = [
  { kind: 'db', re: /\b(?:crate::)?db::/g },
  { kind: 'std::fs', re: /\bstd::fs::/g },
  { kind: 'preferences::load', re: /\b(?:crate::)?preferences::load\s*\(/g },
  { kind: 'preferences::save', re: /\b(?:crate::)?preferences::save\s*\(/g },
  // Evidence #1 in issue #1380: create/delete do SQLite + git worktree
  // work without a `db::` token in the command body.
  {
    kind: 'services::agent_node::create',
    re: /\bservices::agent_node::create\s*\(/g,
  },
  {
    kind: 'services::agent_node::delete',
    re: /\bservices::agent_node::delete\s*\(/g,
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
  for (let i = openIndex; i < source.length; i++) {
    const c = source[i];
    if (inString) {
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
const FN_AFTER_ATTR =
  /(?:#\[[^\]]*\]\s*)*pub(?:\s*\(\s*crate\s*\))?\s+(async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)/;

export function extractAsyncCommandBodies(source: string): AsyncCommand[] {
  const stripped = stripCommentsPreserveLines(source);
  const found: AsyncCommand[] = [];
  COMMAND_ATTR.lastIndex = 0;
  let attr: RegExpExecArray | null;
  while ((attr = COMMAND_ATTR.exec(stripped)) !== null) {
    const attrAsync = /\basync\b/.test(attr[1] ?? '');
    const after = stripped.slice(attr.index + attr[0].length);
    const fnMatch = FN_AFTER_ATTR.exec(after);
    if (!fnMatch || fnMatch.index > 80) continue;
    const isAsyncFn = Boolean(fnMatch[1]);
    if (!attrAsync && !isAsyncFn) continue;
    const name = fnMatch[2];
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
  const commands = extractAsyncCommandBodies(source);
  const violations: BlockingViolation[] = [];
  for (const cmd of commands) {
    const remaining = stripOffloadedCalls(cmd.body);
    for (const { kind, re } of FORBIDDEN) {
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

function relPath(absFile: string): string {
  return absFile.slice(REPO_ROOT.length + 1).replace(/\\/g, '/');
}

describe('async Tauri commands must offload blocking db/fs/prefs (#1380)', () => {
  const files = walkRsFiles(COMMANDS_DIR);
  const violations = files.flatMap((file) =>
    findBlockingInAsyncCommands(file, readFileSync(file, 'utf8')),
  );

  it('walks src-tauri/src/commands and finds Rust files', () => {
    expect(files.length).toBeGreaterThan(10);
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
    expect(found.map((v) => v.kind)).toEqual(['services::agent_node::create']);
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
});
