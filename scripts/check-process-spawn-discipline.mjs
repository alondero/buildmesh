#!/usr/bin/env node
// Enforce the process-spawn discipline from AGENTS.md: production code must
// spawn through `process_util` so CREATE_NO_WINDOW is set on Windows, rather
// than setting `creation_flags` inline or calling `Command::new("git")`
// directly. Issues #665 / #690.
//
// This was a shell `find | grep` in the Quality job. Restored CI ran it for the
// first time and it failed on five sites, all of them inside inline
// `#[cfg(test)] mod tests` blocks: the old file-level exclusions
// (`*_tests.rs`, `tests.rs`, `*/tests/*`) do not match a test module living
// inside a production file, so every test helper that shells out to git
// looked like a violation. Test code is exempt by intent — the rule is about
// windows popping up in front of users, which tests cannot do.
//
// Rather than paste `// allow-inline-process-spawn` at every test helper, skip
// inline test modules when their lexical structure is clear. If a trailing
// test module cannot be balanced, scan the whole file so a parsing problem
// makes this gate stricter rather than letting a real violation through.
//
// Exit codes: 0 clean, 1 violations found, 2 the tree could not be read.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const srcRoot = process.argv[2]
  ? path.resolve(process.argv[2])
  : path.join(repoRoot, 'src-tauri', 'src');

// The patterns the original gate looked for, and the files it never looked in.
const PATTERNS = [/\.creation_flags\(/, /std::process::Command::new\("git"\)/];
const MARKER = '// allow-inline-process-spawn';
const EXCLUDED_BASENAMES = new Set(['tests.rs', 'process_util.rs']);

function walk(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === 'tests') continue; // the old `*/tests/*` exclusion
      walk(full, out);
    } else if (entry.name.endsWith('.rs')) {
      if (entry.name.endsWith('_tests.rs')) continue; // the old `*_tests.rs` exclusion
      if (EXCLUDED_BASENAMES.has(entry.name)) continue;
      out.push(full);
    }
  }
  return out;
}

// Line indexes (0-based) to skip: the body of an inline `#[cfg(test)]` test
// module, but only when the module clearly runs to the end of the file.
function testModuleLines(lines) {
  const skip = new Set();
  for (let i = 0; i < lines.length; i++) {
    if (!/^\s*#\[cfg\(test\)\]/.test(lines[i])) continue;
    // The attribute must be immediately followed by `mod tests {`.
    let j = i + 1;
    while (j < lines.length && lines[j].trim() === '') j++;
    if (!/^\s*mod tests\s*\{\s*$/.test(lines[j] ?? '')) continue;

    const indent = lines[j].length - lines[j].trimStart().length;
    const closing = new RegExp(`^\\s{${indent}}\\}\\s*$`);
    const scan = new BraceScanner();
    let end = -1;
    for (let k = j; k < lines.length; k++) {
      scan.feed(lines[k]);
      if (k > j && scan.depth === 0 && closing.test(lines[k])) {
        end = k;
        break;
      }
    }
    // Only skip when the block is the file's last thing, i.e. the remainder is
    // blank. Otherwise leave it in scope and let the caller scan it.
    const remainderBlank = end !== -1 && lines.slice(end + 1).every((l) => l.trim() === '');
    if (!remainderBlank) continue;
    for (let k = j; k <= end; k++) skip.add(k);
    i = end;
  }
  return skip;
}

// Counts structural braces only. A raw `countBraces` desynchronises on the
// first string literal holding an unbalanced brace — and a test module full of
// JSON and shell snippets has plenty of those — which would leave the gate
// scanning the very block it meant to skip. Track enough lexical state to keep
// the count honest: line comments, block comments, normal strings with
// escapes, raw strings, and char/byte literals.
class BraceScanner {
  constructor() {
    this.depth = 0;
    this.inLineComment = false;
    this.inBlockComment = false;
    this.inString = false;
    this.escaped = false;
    this.rawHashes = null; // number of '#' in an open raw string
  }

  feed(line) {
    for (let i = 0; i < line.length; i++) {
      const ch = line[i];

      if (this.inLineComment) break;

      if (this.inBlockComment) {
        if (ch === '*' && line[i + 1] === '/') {
          this.inBlockComment = false;
          i++;
        }
        continue;
      }

      if (this.rawHashes !== null) {
        if (ch === '"') {
          let hashes = 0;
          while (line[i + 1 + hashes] === '#') hashes++;
          if (hashes >= this.rawHashes) {
            this.rawHashes = null;
            i += hashes;
          }
        }
        continue;
      }

      if (this.inString) {
        if (this.escaped) this.escaped = false;
        else if (ch === '\\') this.escaped = true;
        else if (ch === '"') this.inString = false;
        continue;
      }

      if (ch === '/' && line[i + 1] === '/') {
        this.inLineComment = true;
        break;
      }
      if (ch === '/' && line[i + 1] === '*') {
        this.inBlockComment = true;
        i++;
        continue;
      }
      if (ch === 'r') {
        // Raw string: r"…", r#"…"#, r##"…"##
        let hashes = 0;
        while (line[i + 1 + hashes] === '#') hashes++;
        if (line[i + 1 + hashes] === '"') {
          this.rawHashes = hashes;
          i += hashes + 1;
          continue;
        }
      }
      if (ch === '"') {
        this.inString = true;
        continue;
      }
      if (ch === "'") {
        // Char/byte literal; skip to the closing quote on the same line.
        const end = line.indexOf("'", i + 1);
        i = end === -1 ? line.length : end;
        continue;
      }
      if (ch === '{') this.depth++;
      else if (ch === '}') this.depth--;
    }
    this.inLineComment = false; // line comments end with the line
  }
}

let files;
try {
  files = walk(srcRoot);
} catch (err) {
  console.error(`::error::Cannot read ${srcRoot}: ${err.message}`);
  process.exit(2);
}

const findings = [];
let scanned = 0;
let skippedModules = 0;

for (const file of files) {
  const lines = fs.readFileSync(file, 'utf8').split(/\r?\n/);
  const skip = testModuleLines(lines);
  if (skip.size > 0) skippedModules++;
  lines.forEach((line, index) => {
    if (skip.has(index)) return;
    if (line.includes(MARKER)) return;
    if (!PATTERNS.some((pattern) => pattern.test(line))) return;
    findings.push({ file: path.relative(repoRoot, file).replaceAll('\\', '/'), line: index + 1, text: line.trim() });
  });
  scanned++;
}

console.log(`Scanned ${scanned} Rust file(s) for inline process-spawn patterns.`);
console.log(`Skipped an inline #[cfg(test)] module in ${skippedModules} file(s).`);

if (findings.length > 0) {
  console.error('');
  for (const finding of findings) {
    console.error(`  ${finding.file}:${finding.line}  ${finding.text}`);
  }
  console.error(
    `\n::error::${findings.length} inline process-spawn pattern(s) outside process_util ` +
      '(issue #665 / #690). Use crate::process_util::command_no_window("program") so ' +
      'CREATE_NO_WINDOW is set on Windows, or add `// allow-inline-process-spawn` with a reason.',
  );
  process.exit(1);
}

console.log('Process-spawn discipline: clean.');
