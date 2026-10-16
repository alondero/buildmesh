// Run the production PTY decoder without building Tauri. Cargo caches the
// standalone harness under .tmp; pass --baseline <commit> for a comparison.
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
if (args.length !== 0 && (args.length !== 2 || args[0] !== '--baseline')) {
  throw new Error('Usage: node scripts/bench-session-capture.mjs [--baseline <commit>]');
}
const project = path.join(root, '.tmp', 'bench-session-capture');
fs.mkdirSync(path.join(project, 'src'), { recursive: true });
const modules = [['session_capture', 'src-tauri/src/session_capture.rs'], ['batch', 'src-tauri/src/pty/batch.rs']];
const declarations = modules.map(([name, relative]) => {
  let source = path.join(root, relative);
  if (args[1]) {
    const result = spawnSync('git', ['show', `${args[1]}:${relative}`], { cwd: root, encoding: 'utf8' });
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(result.stderr);
    source = path.join(project, `${name}-baseline.rs`);
    fs.writeFileSync(source, result.stdout);
  }
  // Forward slashes also work on Windows and avoid Rust string escapes.
  return `#[allow(dead_code)]\n#[path = ${JSON.stringify(source.replaceAll('\\', '/'))}]\nmod ${name};\n`;
}).join('');
fs.writeFileSync(path.join(project, 'Cargo.toml'), `[package]
name = "bench-session-capture"
version = "0.0.0"
edition = "2021"

[dependencies]
once_cell = "1"
regex = "1"
`);
const main = fs.readFileSync(path.join(root, 'tools', 'bench-session-capture.rs'), 'utf8');
fs.writeFileSync(path.join(project, 'src', 'main.rs'), declarations + main);
const result = spawnSync('cargo', ['run', '--offline', '--release', '--manifest-path', path.join(project, 'Cargo.toml')], {
  cwd: root, stdio: 'inherit', env: { ...process.env, CARGO_TARGET_DIR: path.join(project, 'target') },
});
if (result.error) throw result.error;
process.exitCode = result.status ?? 1;
