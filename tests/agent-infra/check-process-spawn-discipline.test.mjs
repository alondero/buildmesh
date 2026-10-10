import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const scriptPath = fileURLToPath(
  new URL('../../scripts/check-process-spawn-discipline.mjs', import.meta.url),
);

function runScanner(files) {
  const sourceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'process-spawn-discipline-'));
  try {
    for (const [relativePath, contents] of Object.entries(files)) {
      const filePath = path.join(sourceRoot, relativePath);
      fs.mkdirSync(path.dirname(filePath), { recursive: true });
      fs.writeFileSync(filePath, contents);
    }
    return spawnSync(process.execPath, [scriptPath, sourceRoot], { encoding: 'utf8' });
  } finally {
    fs.rmSync(sourceRoot, { recursive: true, force: true });
  }
}

test('reports production spawns while ignoring a trailing inline test module', () => {
  const result = runScanner({
    'lib.rs': [
      'fn run_git() {',
      '    std::process::Command::new("git");',
      '}',
      '',
      '#[cfg(test)]',
      'mod tests {',
      '    const JSON: &str = r#"{"ok": true}"#;',
      '    std::process::Command::new("git");',
      '    command.creation_flags(1);',
      '}',
    ].join('\n'),
  });

  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /lib\.rs:2/);
  assert.doesNotMatch(result.stderr, /lib\.rs:8|lib\.rs:9/);
});

test('honors the inline allow marker and existing test-file exclusions', () => {
  const result = runScanner({
    'lib.rs': 'std::process::Command::new("git"); // allow-inline-process-spawn: test fixture',
    'helpers_tests.rs': 'std::process::Command::new("git");',
    'tests.rs': 'std::process::Command::new("git");',
    'process_util.rs': 'command.creation_flags(1);',
    'tests/helper.rs': 'std::process::Command::new("git");',
  });

  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /Process-spawn discipline: clean\./);
});

test('a procps kill of a process group fails even with the spawn-allow marker', () => {
  const result = runScanner({
    'worker.rs': [
      'fn stop(pid: u32) {',
      '    let group = format!("-{}", pid);',
      '    crate::process_util::command_no_window("kill") // allow-inline-process-spawn: not a console flag',
      '        .args(["-KILL", &group]);',
      '}',
    ].join('\n'),
    'ok.rs': 'crate::process_util::kill_process_group(pid);',
  });

  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /worker\.rs:3/);
  assert.match(result.stderr, /issue #2103/);
  assert.doesNotMatch(result.stderr, /ok\.rs/);
});

test('the crate does not ask procps to signal a process group', () => {
  const result = spawnSync(process.execPath, [scriptPath], { encoding: 'utf8' });
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
});

test('scans the whole file when an inline test module cannot be balanced', () => {
  const result = runScanner({
    'lib.rs': [
      '#[cfg(test)]',
      'mod tests {',
      '    const JSON: &str = r#"{"open": true"#;',
      '    std::process::Command::new("git");',
    ].join('\n'),
  });

  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /lib\.rs:4/);
});
