import { describe, it, expect } from 'vitest';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, resolve } from 'node:path';

/**
 * IPC contract test — guards against the "command not found" regression class.
 *
 * History (per memory): default_provider, get_repo_issues, list_providers,
 * to_host_path, and others have all shipped without being added to lib.rs's
 * `tauri::generate_handler!` list. Symptom is silent at compile time, surfaces
 * at runtime as a frontend `.catch` swallowing "command not found" and
 * falling back to a default.
 *
 * This test parses lib.rs to find the registered command set, then walks
 * src/ to find every `invoke('name', ...)` literal, and asserts subset.
 */

const REPO_ROOT = resolve(__dirname, '..', '..');
const LIB_RS = join(REPO_ROOT, 'src-tauri', 'src', 'lib.rs');
const FRONTEND_SRC = join(REPO_ROOT, 'src');

function extractRegisteredCommands(libRsSource: string): Set<string> {
  return new Set(extractHandlerEntries(libRsSource).map(entry => entry.command));
}

interface HandlerEntry {
  /** The JS-visible command name — the last path segment of the entry. */
  command: string;
  /** The `#[cfg(...)]` above the entry, or null when the entry carries none. */
  gate: string | null;
}

/** Walk `tauri::generate_handler![ ... ]` one entry per line, keeping any `#[cfg]` prefix. */
function extractHandlerEntries(libRsSource: string): HandlerEntry[] {
  const lines = libRsSource.split(/\r?\n/);
  const start = lines.findIndex(line => line.includes('generate_handler!['));
  if (start === -1) {
    throw new Error('Could not find generate_handler! in lib.rs');
  }

  const entries: HandlerEntry[] = [];
  let gate: string | null = null;
  let closed = false;
  for (const rawLine of lines.slice(start + 1)) {
    const line = rawLine.replace(/\/\/.*$/, '').trim();
    // The list closes on its own line (`])`). Bracket matching would stop at
    // the `]` of an `#[cfg(...)]` entry and silently scan a prefix of the list.
    if (line === '])' || line === ']') {
      closed = true;
      break;
    }
    if (line === '') continue;
    if (line.startsWith('#[cfg(')) {
      gate = line.slice(2, -1);
      continue;
    }
    const entry = line.match(/([a-z_][a-z0-9_]*)\s*,$/);
    if (!entry) continue;
    entries.push({ command: entry[1], gate });
    gate = null;
  }
  if (!closed) {
    throw new Error('Could not find the end of generate_handler! in lib.rs');
  }
  return entries;
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

interface InvokeCall {
  command: string;
  file: string;
  line: number;
}

function extractInvokeCalls(files: string[]): InvokeCall[] {
  // Matches: invoke('name'), invoke("name"), invoke<Type>('name'), invoke<T[]>("name")
  // and the wrapper's `_invoke<T>(...)` form (issue #386). The optional
  // leading underscore catches both — `_rawInvoke` is a low-level call
  // that also goes through the registered handler, so we want to lint it.
  const re = /\b_?invoke(?:<[^>]+>)?\(\s*['"]([a-zA-Z_][a-zA-Z0-9_]*)['"]/g;
  const calls: InvokeCall[] = [];

  for (const file of files) {
    const source = readFileSync(file, 'utf8');
    // Strip block comments and line comments for clean matching.
    // Naive but sufficient — invoke() inside comments isn't a real call.
    const cleaned = source
      .replace(/\/\*[\s\S]*?\*\//g, '')
      .replace(/\/\/.*$/gm, '');

    for (const m of cleaned.matchAll(re)) {
      const command = m[1];
      // Compute line number from match index in the cleaned source.
      // Close enough for diagnostic purposes.
      const upto = cleaned.slice(0, m.index ?? 0);
      const line = upto.split('\n').length;
      calls.push({ command, file, line });
    }
  }
  return calls;
}

describe('IPC contract: every invoke() target must be registered in lib.rs', () => {
  const libRsSource = readFileSync(LIB_RS, 'utf8');
  const registered = extractRegisteredCommands(libRsSource);
  const files = walkTsFiles(FRONTEND_SRC);
  const invokeCalls = extractInvokeCalls(files);

  it('lib.rs registration list is non-empty', () => {
    expect(registered.size).toBeGreaterThan(20);
  });

  it('finds invoke() calls in src/', () => {
    expect(invokeCalls.length).toBeGreaterThan(20);
  });

  it('every invoked command is registered in tauri::generate_handler!', () => {
    const unregistered = invokeCalls.filter(c => !registered.has(c.command));
    const report = unregistered.map(c => {
      const rel = c.file.slice(REPO_ROOT.length + 1).replace(/\\/g, '/');
      return `  ${c.command}  (${rel}:${c.line})`;
    });

    if (unregistered.length > 0) {
      const distinctCommands = [...new Set(unregistered.map(c => c.command))].sort();
      const message =
        `Found ${unregistered.length} invoke() call(s) targeting commands not registered in lib.rs:\n` +
        report.join('\n') +
        `\n\nDistinct missing commands: ${distinctCommands.join(', ')}` +
        `\n\nFix options:` +
        `\n  1. Add the command to tauri::generate_handler![ ... ] in src-tauri/src/lib.rs` +
        `\n  2. Remove the dead invoke() call from src/`;
      throw new Error(message);
    }
  });
});

/**
 * Issue #2153 — `create_test_mesh` writes a Mesh into the user's real
 * database, and the two `debug_*` commands expose process-registry state.
 * None has a product UI caller, so they are registered behind a dev-only
 * `#[cfg]` and a shipped release build answers "command not found" for all
 * three. The HTTP test bridge keeps its own `create_test_mesh` route for e2e
 * and ui-shot; that route is separate from this handler list.
 */
describe('IPC contract: dev-only commands stay out of the release window', () => {
  const libRsSource = readFileSync(LIB_RS, 'utf8');
  const entries = extractHandlerEntries(libRsSource);
  const devOnly = entries.filter(entry => entry.gate !== null);
  const devOnlyNames = devOnly.map(entry => entry.command).sort();

  it('registers exactly the documented dev-only commands behind a cfg gate', () => {
    expect(devOnlyNames).toEqual([
      'create_test_mesh',
      'debug_crash_snapshot',
      'debug_list_agents',
    ]);
  });

  it('gates each one on debug_assertions or the dev-tools feature', () => {
    // debug_assertions covers `cargo build` / `tauri dev`; the dev profile is
    // a release-mode `tauri build`, so it opts in through the `dev-tools`
    // feature that `npm run tauri:build:dev` passes. Losing either half
    // silently breaks /use's IPC probes or the dev-profile fixtures.
    for (const entry of devOnly) {
      expect(entry.gate, entry.command).toContain('debug_assertions');
      expect(entry.gate, entry.command).toContain('feature = "dev-tools"');
    }
  });

  it('has no src/ invoke() targeting a dev-only command', () => {
    // A wrapper left in the frontend for a dev-only command is a release-only
    // "command not found" the type checker cannot see.
    const devOnlySet = new Set(devOnlyNames);
    const leaked = extractInvokeCalls(walkTsFiles(FRONTEND_SRC))
      .filter(call => devOnlySet.has(call.command))
      .map(call => {
        const rel = call.file.slice(REPO_ROOT.length + 1).replace(/\\/g, '/');
        return `${call.command}  (${rel}:${call.line})`;
      });

    expect(
      leaked,
      `src/ still invokes dev-only command(s), which a shipped build does not register:\n${leaked.join('\n')}`,
    ).toEqual([]);
  });
});

/** Synthetic handler sources, so the parser is provably not a placebo. */
describe('extractHandlerEntries', () => {
  const HANDLER = [
    '.invoke_handler(tauri::generate_handler![',
    '            commands::mesh::list_meshes,',
    '            #[cfg(any(debug_assertions, feature = "dev-tools"))]',
    '            commands::mesh::create_test_mesh,',
    '            // a comment with a ] bracket, and a trailing one',
    '            commands::agent::spawn_agent,',
    '        ])',
    '        .build(tauri::generate_context!())',
  ].join('\n');

  it('reads every entry, including the ones after a cfg-gated entry', () => {
    // The bracket-matching version of this parser stopped at the `]` closing
    // `#[cfg(...)]` and scanned only a prefix of the list, which would make
    // both dev-only guards above pass on an empty set.
    expect(extractHandlerEntries(HANDLER)).toEqual([
      { command: 'list_meshes', gate: null },
      { command: 'create_test_mesh', gate: 'cfg(any(debug_assertions, feature = "dev-tools"))' },
      { command: 'spawn_agent', gate: null },
    ]);
  });

  it('reads a CRLF source the same way', () => {
    expect(extractHandlerEntries(HANDLER.replace(/\n/g, '\r\n'))).toEqual(
      extractHandlerEntries(HANDLER),
    );
  });

  it('throws when the handler list never closes', () => {
    const unterminated = '.invoke_handler(tauri::generate_handler![\n  commands::mesh::list_meshes,\n';
    expect(() => extractHandlerEntries(unterminated)).toThrow(/end of generate_handler/);
  });

  it('throws when there is no handler at all', () => {
    expect(() => extractHandlerEntries('fn main() {}')).toThrow(/Could not find generate_handler/);
  });
});
