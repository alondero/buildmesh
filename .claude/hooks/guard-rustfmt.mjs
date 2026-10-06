#!/usr/bin/env node
// PreToolUse(Bash|PowerShell) guard: deny `rustfmt` / `cargo fmt` runs that would write files.
// `rustfmt <lib.rs|mod.rs>` also rewrites every child module and `cargo fmt` rewrites the
// whole crate (#2022). `--check` and the informational flags are read-only and stay allowed;
// `node scripts/rustfmt-touched.mjs <file>` is the supported way to format. `--emit stdout`
// is denied too: it dumps every child module, and redirecting it over a file is how a
// one-file format became a 256,223-line diff.
//
// Early warning, not a sandbox. A command is judged at the START of each shell segment,
// after skipping common launchers (`env`, `time`, `rtk`, `xargs`, `bash -c`, `cargo +toolchain`,
// `NAME=value`, `do`, `{`, `(`, ...). Quoted strings containing a separator and heredoc bodies
// are treated as prose, so a commit message that mentions rustfmt is never mistaken for a run.
// Anything cleverer (a runner built at run time, a nested quoted script) is not caught.
// Fails open on any parse error so it can never wedge a session.

import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { SEGMENT_SEP, stripHeredocs } from './guard-commit-staging.mjs';

const LAUNCHERS = String.raw`(?:[({&]\s*|\$\(\s*|(?:do|then|else)\s+|\w+=\S*\s+|(?:time|env|sudo|npx|xargs|rtk|command|exec|nohup|call)(?:\.exe)?\s+(?:-\S+\s+)*|ForEach-Object\s*\{\s*|timeout\s+\S+\s+|rustup\s+run\s+\S+\s+|(?:bash|sh|zsh|pwsh|powershell|cmd)(?:\.exe)?\s+(?:(?:-\w+|/\w)\s+)*)*`;
// A command may be a path (`~/.cargo/bin/rustfmt.exe`); Windows is case-insensitive.
const RUSTFMT = new RegExp(String.raw`^\s*${LAUNCHERS}(?:\S*[\\/])?rustfmt(?:\.exe)?(?=\s|$)`, 'i');
const CARGO_FMT = new RegExp(String.raw`^\s*${LAUNCHERS}(?:\S*[\\/])?cargo(?:\.exe)?\s+(?:\+\S+\s+|--?[\w-]+(?:=\S+)?\s+(?:(?:never|always|auto)\s+)?)*fmt(?=\s|$)`, 'i');
const FIND_EXEC = /\s-exec\s+(?:\S*[\\/])?rustfmt(?:\.exe)?(?=\s|$)/i;
const READ_ONLY = /(?:^|\s)(?:--check|--help|-h|--version|-V)(?=\s|$)/;
// Quoted text that holds a shell separator is prose or a nested script; it must not start new segments.
const SEPARATOR_IN_QUOTES = /"[^"]*[&|;\n][^"]*"|'[^']*[&|;\n][^']*'/g;

export function decide(cmd) {
  const flat = stripHeredocs(cmd).replace(SEPARATOR_IN_QUOTES, ' ').replace(/["']/g, '');
  const writes = flat.split(SEGMENT_SEP).some(raw => {
    const segment = raw.replace(/(?:^|\s)#.*$/, '');
    return (RUSTFMT.test(segment) || CARGO_FMT.test(segment) || FIND_EXEC.test(segment)) && !READ_ONLY.test(segment);
  });
  if (!writes) return null;
  return {
    hookEventName: 'PreToolUse',
    permissionDecision: 'deny',
    permissionDecisionReason:
      'rustfmt on a module root (lib.rs, mod.rs) also rewrites its child modules, and `cargo fmt` rewrites the whole crate (#2022). ' +
      'To format files use `node scripts/rustfmt-touched.mjs <file.rs>...`, which restores every other Rust file. ' +
      'To only look at diffs add `--check`.',
  };
}

function main() {
  let payload;
  try {
    payload = JSON.parse(readFileSync(0, 'utf8'));
  } catch {
    return;
  }
  if (payload?.tool_name !== 'Bash' && payload?.tool_name !== 'PowerShell') return;
  const verdict = decide(payload?.tool_input?.command ?? '');
  if (verdict) process.stdout.write(JSON.stringify({ hookSpecificOutput: verdict }));
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) main();
