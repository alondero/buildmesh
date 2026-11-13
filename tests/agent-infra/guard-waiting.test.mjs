import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const hookPath = fileURLToPath(new URL('../../.claude/hooks/guard-waiting.mjs', import.meta.url));
function runHook(command, tool = 'Bash') {
  return runInput(JSON.stringify({ tool_name: tool, tool_input: { command } }));
}
function runInput(input) {
  const result = spawnSync(process.execPath, [hookPath], { input, encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout;
}

for (const command of [
  'while kill -0 "$pid" 2>/dev/null; do sleep 10; done',
  'until ! ps -p 123; do /bin/sleep 2; done',
  'sleep 30; ps -ef',
  'while ps aux | grep "[n]pm run verify"; do sleep 10; done',
  'sleep 5; ps',
  'sleep 5 && jobs -r',
  'sleep 10; task_output 123',
  'Start-Sleep -Seconds 30; Get-Process -Id $proc.Id',
  'while (-not $proc.HasExited) { Start-Sleep -Milliseconds 100 }',
  'do { Start-Sleep 10; $job = Get-Job -Id 1 } while ($job.State -eq "Running")',
  'Start-Sleep 10; Receive-Job -Id 1 -Keep',
  'Get-Job | Wait-Job -Timeout 1 | Receive-Job; Start-Sleep 1',
  'Start-Sleep 30; npm run harness -- status',
  'sleep 30; node scripts/harness.mjs status',
  'Start-Sleep 30; Get-Content .harness/verification.json',
  'sleep 30; tail -n 20 "verify.log"',
  'Start-Sleep 30; Get-Content "verify.log" -Tail 10',
  'while (!(Test-Path .harness/receipt.json)) { sleep 5 }',
]) {
  test(`refuses sleep polling: ${command}`, () => {
    for (const tool of ['Bash', 'PowerShell']) {
      const verdict = JSON.parse(runHook(command, tool)).hookSpecificOutput;
      assert.equal(verdict.hookEventName, 'PreToolUse');
      assert.equal(verdict.permissionDecision, 'deny');
      assert.match(verdict.permissionDecisionReason, /npm run harness -- wait/);
      assert.match(verdict.permissionDecisionReason, /completes and read its output once/);
    }
  });
}

for (const command of [
  'Start-Sleep -Seconds 1',
  'sleep 0.1; echo ready',
  'for i in 1 2 3; do echo "$i"; sleep 1; done',
  'sleep 1; cat README.md',
  'npm run harness -- wait --max-seconds 540',
  'wait "$pid"; cat verify.log',
  'Wait-Process -Id $proc.Id; Start-Sleep 1',
  'Get-Job | Wait-Job | Receive-Job',
  'Get-Job | Wait-Job | Receive-Job; Start-Sleep -Milliseconds 100',
  'Get-Process -Id 42 | Wait-Process; Start-Sleep -Milliseconds 100',
  'Get-Process -Id 123',
  'echo "sleep 30; Get-Process -Id 123"',
  'git commit -m "fix: Start-Sleep 30; Get-Job"',
  "cat <<'EOF'\nsleep 30; ps -p 123\nEOF",
  "Set-Content notes.txt @'\nStart-Sleep 30; Get-Job\n'@",
  'sleep 1 # Get-Process -Id 123',
  '# sleep 30\nGet-Process -Id 123',
  'sleep 1; <# Get-Process #> echo ready',
  'echo "sleep 1"; Get-Process',
  'Start-Sleep 1; Write-Output "$proc.HasExited"',
  'sleep 1; printf "%s" file.HasExited',
  "Start-Sleep 1; rg '\\.HasExited' src-tauri",
]) {
  test(`allows non-polling command: ${command}`, () => {
    assert.equal(runHook(command), '');
  });
}

test('ignores other tools and malformed hook input', () => {
  assert.equal(runHook('sleep 30; ps -p 123', 'Read'), '');
  for (const input of ['', '{', 'null', '{}', '{"tool_name":"Bash","tool_input":{"command":42}}']) {
    assert.equal(runInput(input), '');
  }
});

test('guard is wired to both shell tools and included in the agent suite', () => {
  const settings = JSON.parse(readFileSync(new URL('../../.claude/settings.json', import.meta.url), 'utf8'));
  for (const tool of ['Bash', 'PowerShell']) {
    assert.ok(settings.hooks.PreToolUse.some(entry =>
      new RegExp(`^(?:${entry.matcher})$`).test(tool) &&
      entry.hooks.some(hook => hook.command.includes('/.claude/hooks/guard-waiting.mjs'))));
  }
  const pkg = JSON.parse(readFileSync(new URL('../../package.json', import.meta.url), 'utf8'));
  assert.ok(pkg.scripts['test:agent'].includes('tests/agent-infra/guard-waiting.test.mjs'));
});
