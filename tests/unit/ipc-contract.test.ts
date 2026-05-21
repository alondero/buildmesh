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
  // Match the body of `tauri::generate_handler![ ... ]`
  const handlerMatch = libRsSource.match(/generate_handler!\s*\[([\s\S]*?)\]/);
  if (!handlerMatch) {
    throw new Error('Could not find generate_handler! in lib.rs');
  }
  const body = handlerMatch[1];

  // Strip line comments
  const stripped = body.replace(/\/\/.*$/gm, '');

  // Each entry looks like `commands::module::function_name,`
  // We capture the last segment (the function name = JS command name).
  const commands = new Set<string>();
  const entryRe = /([a-z_][a-z0-9_]*)\s*,/g;
  for (const m of stripped.matchAll(entryRe)) {
    commands.add(m[1]);
  }
  return commands;
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
  const re = /\binvoke(?:<[^>]+>)?\(\s*['"]([a-zA-Z_][a-zA-Z0-9_]*)['"]/g;
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
