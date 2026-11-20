import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { checkComments, sourceComments } from '../../scripts/check-comments.mjs';

test('recognizes comments in code and preserves comment-like literals', async () => {
  const cases = [
    ['ui.tsx', 'const node = <div>https://example.test{/* JSX */}</div>;\nconst pattern = /[/*]/;\nconst x = `😀 ${1 /* interpolation */}`; // TS',
      ['/* JSX */', '/* interpolation */', '// TS']],
    ['main.rs', 'const PATH: &str = r#"// literal"#;\n/// Documentation\nfn f() { /* outer /* inner */ block */ }',
      ['/// Documentation', '/* outer /* inner */ block */']],
    ['Main.kt', 'val raw = """// literal"""\n// Kotlin', ['// Kotlin']],
    ['start.sh', '#!/bin/bash\ncat <<EOF\n# data\nEOF\necho okay # shell',
      ['# shell']],
    ['start.ps1', '#requires -Version 5.1\nWrite-Host "# literal" # PowerShell\n<# block #>',
      ['#requires -Version 5.1', '# PowerShell', '<# block #>']],
    ['workflow.yml', 'name: "a # b" # YAML\ntext: |\n  # data\n', ['# YAML']],
    ['.github/workflows/check.yml', 'steps:\n  - run: |\n      cat <<EOF\n      # data\n      EOF\n      # Bash\n  - shell: pwsh\n    run: |\n      # PowerShell\n',
      ['# Bash', '# PowerShell']],
    ['theme.toml', 'name = "a # b" # TOML', ['# TOML']],
    ['index.html', '<!-- HTML --><script>const s = "// literal"; // script\n</script><style>/* style */</style>',
      ['<!-- HTML -->', '// script', '/* style */']],
    ['icon.xml', '<x><![CDATA[<!-- literal -->]]><?instruction <!-- literal -->?><!-- XML --></x>', ['<!-- XML -->']],
    ['schema.sql', "select '-- literal'; -- SQL", ['-- SQL']],
    ['start.cmd', '@echo off\n@REM batch\nset "LABEL=& REM text"\necho ^& REM literal\necho okay & REM inline',
      ['@REM batch', 'REM inline']],
  ];
  for (const [path, source, expected] of cases) {
    assert.deepEqual((await sourceComments(path, source)).map(comment => comment.text), expected, path);
  }
});

test('keeps the allowance per file and rejects reintroductions after the base advances', async t => {
  const root = mkdtempSync(join(tmpdir(), 'buildmesh-comment-lint-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim();
  const write = (name, contents) => writeFileSync(join(root, name), contents);
  git('init', '-q');
  git('config', 'user.name', 'Comment lint test');
  git('config', 'user.email', 'comment-lint@example.test');
  git('config', 'commit.gpgsign', 'false');
  write('old.rs', '// legacy\n// legacy\nfn old() {}\n');
  git('add', '.');
  git('commit', '-qm', 'Base');
  const base = git('rev-parse', 'HEAD');

  write('old.rs', '// staged\nfn old() {}\n');
  git('add', 'old.rs');
  write('old.rs', '// legacy\n// legacy\nfn old() {}\n');
  assert.deepEqual(await checkComments(root, base), ['old.rs:1: new source comment (staged)']);

  write('old.rs', '\nfn old() {}\n// legacy\n');
  git('add', 'old.rs');
  assert.deepEqual(await checkComments(root, base), []);
  git('commit', '-qm', 'Remove one occurrence');
  write('old.rs', '// legacy\n// legacy\nfn old() {}\n');
  assert.deepEqual(await checkComments(root), ['old.rs:2: new source comment']);

  write('old.rs', 'fn old() {}\n// replacement\n');
  write('new.ts', '// legacy\nexport const n = 1;\n');
  mkdirSync(join(root, 'docs'));
  write('docs/sample.ts', '// example\n');
  const violations = ['new.ts:1: new source comment', 'old.rs:2: new source comment'];
  assert.deepEqual(await checkComments(root, base), violations);
  git('add', 'new.ts');
  git('commit', '-qm', 'Add a commented file');
  assert.deepEqual(await checkComments(root, base), violations);
});
