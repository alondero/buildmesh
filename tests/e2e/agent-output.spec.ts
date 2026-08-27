/**
 * E2E Agent Output Tests
 *
 * Tests that agent processes spawn and produce terminal output.
 *
 * Issue: Backend logs show "process spawned successfully" but terminal shows nothing.
 * This test reproduces the issue: backend thinks agent is running, but no output.
 *
 * Run with: npx playwright test tests/e2e/agent-output.spec.ts --config playwright.config.ts
 */
import { test, expect } from '@playwright/test';
import { spawn } from 'child_process';
import { exec } from 'child_process';
import fs from 'fs';
import util from 'util';
import { invokeViaHttp, waitForPort, waitForPortClosed } from './utils/tauri-http';

const execPromise = util.promisify(exec);

const EXE_PATH = 'X:/src/buildmesh/src-tauri/target/release/buildmesh.exe';
const LOG_PATH = 'C:/Users/alond/AppData/Roaming/com.alond.buildmesh/logs/buildmesh.log';

async function killAllBuildmeshProcesses() {
  try {
    await execPromise('taskkill /IM buildmesh.exe /F');
  } catch {
    // Ignore
  }
}

async function readNewLogLines(fromByte: number): Promise<string[]> {
  let stat: fs.Stats;
  try {
    stat = await fs.promises.stat(LOG_PATH);
  } catch {
    return [];
  }
  if (fromByte >= stat.size) return [];
  const fd = await fs.promises.open(LOG_PATH, 'r');
  try {
    const length = stat.size - fromByte;
    const buf = Buffer.alloc(length);
    await fd.read(buf, 0, length, fromByte);
    return buf.toString('utf-8').split('\n').filter(l => l.trim().length > 0);
  } finally {
    await fd.close();
  }
}

async function logSize(): Promise<number> {
  try {
    return (await fs.promises.stat(LOG_PATH)).size;
  } catch {
    return 0;
  }
}

// Captures full RFC3339 incl. fractional seconds + timezone so time-diff
// math isn't truncated to the second.
function getLogTimestamp(line: string): string | null {
  const match = line.match(/^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2}))/);
  return match ? match[1] : null;
}

test.describe('agent output', () => {

  test.beforeEach(async () => {
    await killAllBuildmeshProcesses();
    const portReleased = await waitForPortClosed('127.0.0.1', 1991, 5000);
    expect(portReleased, 'port 1991 should be free before the next test spawns the exe').toBe(true);
  });

  test.afterEach(async () => {
    try {
      const projects = await invokeViaHttp('list_meshes') as Array<{ id: number; name: string }>;
      for (const project of projects) {
        if (project.name.includes('Test') || project.name.includes('Agent Output') || project.name.includes('Claude Code') || project.name.includes('Cwrap')) {
          await invokeViaHttp('delete_mesh', { meshId: project.id });
        }
      }
    } catch (e) {
      console.error('Cleanup failed:', e);
    }
    await killAllBuildmeshProcesses();
  });

  test('spawned agent produces terminal output', async () => {
    const appProcess = spawn(EXE_PATH, [], {
      stdio: 'ignore',
      windowsHide: true,
      detached: true,
    });
    appProcess.unref();

    const serverReady = await waitForPort('127.0.0.1', 1991, 15000);
    expect(serverReady, 'HTTP test server should be ready').toBe(true);

    const offsetBefore = await logSize();

    const project = await invokeViaHttp('create_test_mesh', { name: 'Agent Output Test' }) as { id: number };
    expect(project.id).toBeGreaterThan(0);

    const session = await invokeViaHttp('create_agent_node', {
      meshId: project.id,
      name: 'Test Session',
      path: 'X:\\src\\playbook',
      branch: 'main',
    }) as { id: number };
    expect(session.id).toBeGreaterThan(0);

    const spawnResult = await invokeViaHttp('spawn_agent', {
      nodeId: session.id,
      provider: 'anthropic',
    });
    expect(spawnResult).toBeTruthy();

    // Poll the log until BOTH the synchronous spawn-success line AND
    // the async reader-start line are present. The two come from
    // different threads; resolving on the first would race the second.
    let newLines: string[] = [];
    await expect
      .poll(
        async () => {
          const lines = await readNewLogLines(offsetBefore);
          const hasSpawn = lines.some(l => l.includes('process spawned successfully'));
          const hasReaderStart = lines.some(l => l.includes('starting reader thread'));
          if (hasSpawn && hasReaderStart) {
            newLines = lines;
            return true;
          }
          return null;
        },
        { timeout: 15000, intervals: [200, 500, 1000], message: 'log should record both spawn success and reader-thread start within 15s' },
      )
      .not.toBeNull();

    console.log('New log entries since spawn:');
    newLines.forEach(l => console.log(l));

    expect(newLines.filter(l => l.includes('process spawned successfully')).length).toBeGreaterThan(0);
    expect(newLines.filter(l => l.includes('starting reader thread')).length).toBeGreaterThan(0);

    // Reader thread lifetime check against the same snapshot — no
    // separate read needed.
    const readerStartMatch = newLines.filter(l => l.includes('starting reader thread'));
    const readerExitMatch = newLines.filter(l => l.includes('PTY reader thread exited'));

    if (readerStartMatch.length > 0 && readerExitMatch.length > 0) {
      const startMs = new Date(getLogTimestamp(readerStartMatch[0])!).getTime();
      const exitMs = new Date(getLogTimestamp(readerExitMatch[readerExitMatch.length - 1])!).getTime();
      const timeDiff = exitMs - startMs;
      console.log(`Reader thread lifetime: ${timeDiff}ms`);
      expect(timeDiff, `Reader thread should not exit immediately (was ${timeDiff}ms). Process likely crashed without output.`).toBeGreaterThan(1000);
    }
  });

  test('Claude Code agent process does not exit immediately when spawned', async () => {
    const appProcess = spawn(EXE_PATH, [], {
      stdio: 'ignore',
      windowsHide: true,
      detached: true,
    });
    appProcess.unref();

    const serverReady = await waitForPort('127.0.0.1', 1991, 15000);
    expect(serverReady).toBe(true);

    const offsetBefore = await logSize();

    const project = await invokeViaHttp('create_test_mesh', { name: 'Claude Code Exit Test' }) as { id: number };
    const session = await invokeViaHttp('create_agent_node', {
      meshId: project.id,
      name: 'Claude Code Test',
      path: 'X:\\src\\playbook',
      branch: 'main',
    }) as { id: number };

    const spawnResult = await invokeViaHttp('spawn_agent', { nodeId: session.id, provider: 'anthropic' });
    expect(spawnResult).toBeTruthy();

    // Bounded grace-window poll: a fast-crashing agent exits within
    // hundreds of ms; a healthy one keeps the reader alive. After 5 s
    // we know which class the agent is in:
    //   - Both lines present: assert timeDiff > 2000ms (bug detected).
    //   - Only start present:  healthy, test passes.
    //   - Neither present:     spawn failure, fail.
    //
    // We can't use `expect.poll(...).not.toBeNull()` here because the
    // test timeout *is* a valid outcome (the healthy path). A bounded
    // while loop with explicit deadline makes both outcomes explicit.
    const graceDeadline = Date.now() + 5000;
    let snapshot: { starts: string[]; exits: string[] } | null = null;
    while (Date.now() < graceDeadline) {
      const lines = await readNewLogLines(offsetBefore);
      const starts = lines.filter(l => l.includes('starting reader thread') && l.includes(`session ${session.id}`));
      const exits = lines.filter(l => l.includes('PTY reader thread exited') && l.includes(`session ${session.id}`));
      if (starts.length > 0 && exits.length > 0) {
        snapshot = { starts, exits };
        break;
      }
      await new Promise(r => setTimeout(r, 200));
    }

    if (snapshot === null) {
      const lines = await readNewLogLines(offsetBefore);
      const starts = lines.filter(l => l.includes('starting reader thread') && l.includes(`session ${session.id}`));
      expect(starts.length, 'Should have seen reader thread start').toBeGreaterThan(0);
      return;
    }

    const firstStart = snapshot.starts[0];
    const lastExit = snapshot.exits[snapshot.exits.length - 1];
    const startTime = getLogTimestamp(firstStart);
    const exitTime = getLogTimestamp(lastExit);
    expect(startTime, `first start line should have a parseable RFC3339 timestamp: ${firstStart}`).not.toBeNull();
    expect(exitTime, `last exit line should have a parseable RFC3339 timestamp: ${lastExit}`).not.toBeNull();
    const timeDiff = new Date(exitTime!).getTime() - new Date(startTime!).getTime();
    console.log(`Reader lifetime for session ${session.id}: ${timeDiff}ms`);
    expect(timeDiff, `Claude Code agent should keep running, not exit after ${timeDiff}ms`).toBeGreaterThan(2000);
  });
});
