#!/usr/bin/env node
// Format specific Rust files without the cascade: `rustfmt <lib.rs|mod.rs>` also rewrites
// every child module it declares (a bare `rustfmt src-tauri/src/lib.rs` once produced a
// 256,223-line diff), and `cargo fmt` rewrites the whole crate (#2022). This formats the
// requested files, then puts every other .rs file (tracked, or untracked but not ignored)
// back to the bytes it had before, including uncommitted edits and mixed CRLF/LF endings.
//
// Usage: node scripts/rustfmt-touched.mjs <file.rs>...   (run from the repository root)

import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const slash = path => path.replaceAll('\\', '/');
// Git reports repo-relative paths with their on-disk case; Windows accepts any spelling.
const caseKey = process.platform === 'win32' ? path => path.toLowerCase() : path => path;

// rustfmt has no way to discover the edition, and a wrong one misparses the
// file. Read it from the crate that owns the code instead of pinning a literal
// that silently drifts from Cargo.toml.
export function rustfmtEdition(root) {
  const manifest = resolve(root, 'src-tauri', 'Cargo.toml');
  const contents = existsSync(manifest) ? readFileSync(manifest, 'utf8') : '';
  // The section ends at the next table header or end of input; `[^[]*` would stop at an array value such as `authors = [...]`.
  const edition = /^edition\s*=\s*"([^"]+)"/m.exec(/^\[package\][\s\S]*?(?=^\[|(?![\s\S]))/m.exec(contents)?.[0] ?? '')?.[1];
  if (!edition) throw new Error(`Cannot read edition = from the [package] section of ${manifest}.`);
  return edition;
}

export function formatTouched(root, files, run) {
  const wanted = new Map(files.map(path => {
    const relativePath = slash(relative(root, resolve(root, path)));
    return [caseKey(relativePath), relativePath];
  }));
  const listed = execFileSync('git', ['ls-files', '-z', '--cached', '--others', '--exclude-standard', '--', '*.rs'], { cwd: root, encoding: 'utf8' }).split('\0').filter(Boolean);
  // A tracked file deleted in the working tree has no bytes to protect and no content to restore.
  const others = listed.filter(path => !wanted.has(caseKey(path)) && existsSync(resolve(root, path)));
  const snapshot = new Map(others.map(path => [path, readFileSync(resolve(root, path))]));
  const formatted = [...wanted.values()];
  const restored = [];
  try {
    run(formatted.map(path => resolve(root, path)));
  } finally {
    // A failed rustfmt may already have rewritten children, so restore on every exit path.
    for (const [path, bytes] of snapshot) {
      if (!existsSync(resolve(root, path)) || !readFileSync(resolve(root, path)).equals(bytes)) {
        writeFileSync(resolve(root, path), bytes);
        restored.push(path);
      }
    }
  }
  return { formatted, restored: restored.sort() };
}

function main(args) {
  const root = process.cwd();
  const bad = args.filter(path => !path.endsWith('.rs') || !existsSync(resolve(root, path)));
  if (!args.length || bad.length) {
    console.error(`Usage: node scripts/rustfmt-touched.mjs <existing file.rs>...${bad.length ? ` (not an existing .rs file: ${bad.join(', ')})` : ''}`);
    process.exit(2);
  }
  const run = absolute => {
    const result = spawnSync('rustfmt', ['--edition', rustfmtEdition(root), ...absolute], { cwd: root, stdio: 'inherit' });
    if (result.error || result.status !== 0) throw new Error(`rustfmt failed (${result.error?.message ?? `exit ${result.status}`}); other files were restored.`);
  };
  try {
    const { formatted, restored } = formatTouched(root, args, run);
    console.log(`Formatted: ${formatted.join(', ')}`);
    if (restored.length) console.log(`Restored ${restored.length} file(s) rustfmt rewrote as child modules: ${restored.join(', ')}`);
  } catch (error) {
    console.error(error.message);
    process.exit(1);
  }
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) main(process.argv.slice(2));
