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
import util from 'util';
import { invokeViaHttp } from './utils/tauri-http';

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

async function waitForPort(port: number, timeoutMs: number = 10000): Promise<boolean> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/health`);
      if (response.ok) return true;
    } catch {
      // Not ready yet
    }
    await new Promise(r => setTimeout(r, 200));
  }
  return false;
}

function getLogTimestamp(line: string): string | null {
  const match = line.match(/^(\d{4}-\d{2}-\d{2}T[\d:]+)/);
  return match ? match[1] : null;
}

test.describe('agent output', () => {

  test.beforeEach(async () => {
    await killAllBuildmeshProcesses();
    await new Promise(r => setTimeout(r, 1000));
  });

  test.afterEach(async () => {
    // Clean up test project and sessions
    try {
      // List projects and delete ones with test-related names
      const projects = await invokeViaHttp('list_meshes') as Array<{id: number; name: string}>;
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
    // Start the built exe directly
    const appProcess = spawn(EXE_PATH, [], {
      stdio: 'ignore',
      windowsHide: true,
      detached: true
    });
    appProcess.unref();

    // Give the app time to start and bind to port
    await new Promise(r => setTimeout(r, 3000));

    // Wait for HTTP test server to be ready
    const serverReady = await waitForPort(1991, 15000);
    expect(serverReady, 'HTTP test server should be ready').toBe(true);

    // Clear or note the current log position
    const logContentBefore = await fs.promises.readFile(LOG_PATH, 'utf-8').catch(() => '');
    const lastLineBefore = logContentBefore.split('\n').filter(l => l.trim()).slice(-1)[0] || '';
    const lastTimeBefore = getLogTimestamp(lastLineBefore);

    // Create a test project
    const project = await invokeViaHttp('create_test_mesh', { name: 'Agent Output Test' }) as { id: number };
    expect(project.id).toBeGreaterThan(0);

    // Create a session
    const session = await invokeViaHttp('create_agent_node', {
      meshId: project.id,
      name: 'Test Session',
      path: 'X:\\src\\playbook',
      branch: 'main',
    }) as { id: number };

    expect(session.id).toBeGreaterThan(0);

    // Spawn an agent
    const spawnResult = await invokeViaHttp('spawn_agent', {
      nodeId: session.id,
      provider: 'anthropic',
    });

    expect(spawnResult).toBeTruthy();

    // Wait for agent to initialize and produce output
    await new Promise(r => setTimeout(r, 5000));

    // Check log file for evidence
    const logContent = await fs.promises.readFile(LOG_PATH, 'utf-8');
    const newLines = logContent.split('\n').filter(l => {
      if (!l.trim()) return false;
      const ts = getLogTimestamp(l);
      if (!ts) return false;
      if (lastTimeBefore && ts <= lastTimeBefore) return false;
      return true;
    });

    console.log('New log entries since spawn:');
    newLines.forEach(l => console.log(l));

    // Backend should have spawned successfully
    const spawnLogs = newLines.filter(l => l.includes('process spawned successfully'));
    expect(spawnLogs.length, 'Backend should log "process spawned successfully"').toBeGreaterThan(0);

    // Check for "starting reader thread"
    const readerStartLogs = newLines.filter(l => l.includes('starting reader thread'));
    expect(readerStartLogs.length, 'Backend should log "starting reader thread"').toBeGreaterThan(0);

    // KEY CHECK: Did the reader thread exit quickly (EOF) without emitting output?
    // If we see "PTY reader thread exited" quickly after "starting reader thread",
    // it means the spawned process exited immediately without output
    const readerStartMatch = newLines.filter(l => l.includes('starting reader thread'));
    const readerExitMatch = newLines.filter(l => l.includes('PTY reader thread exited'));

    if (readerStartMatch.length > 0 && readerExitMatch.length > 0) {
      // Extract timestamps
      const firstStartLine = readerStartMatch[0];
      const lastExitLine = readerExitMatch[readerExitMatch.length - 1];

      const startTime = getLogTimestamp(firstStartLine);
      const exitTime = getLogTimestamp(lastExitLine);

      if (startTime && exitTime) {
        const startMs = new Date(startTime).getTime();
        const exitMs = new Date(exitTime).getTime();
        const timeDiff = exitMs - startMs;

        console.log(`Reader thread lifetime: ${timeDiff}ms`);

        // If reader exits in less than 1000ms, it likely got EOF immediately
        // This would indicate the spawned process exited without producing output
        expect(timeDiff, `Reader thread should not exit immediately (was ${timeDiff}ms). Process likely crashed without output.`).toBeGreaterThan(1000);
      }
    }
  });

  test('Claude Code agent process does not exit immediately when spawned', async () => {
    // Start the built exe directly
    const appProcess = spawn(EXE_PATH, [], {
      stdio: 'ignore',
      windowsHide: true,
      detached: true
    });
    appProcess.unref();

    // Give the app time to start and bind to port
    await new Promise(r => setTimeout(r, 3000));

    // Wait for server
    const serverReady = await waitForPort(1991, 15000);
    expect(serverReady).toBe(true);

    // Create project and session
    const project = await invokeViaHttp('create_test_mesh', { name: 'Claude Code Exit Test' }) as { id: number };
    const session = await invokeViaHttp('create_agent_node', {
      meshId: project.id,
      name: 'Claude Code Test',
      path: 'X:\\src\\playbook',
      branch: 'main',
    }) as { id: number };

    // Spawn agent
    const spawnResult = await invokeViaHttp('spawn_agent', { sessionId: session.id, provider: 'anthropic' });
    expect(spawnResult).toBeTruthy();

    // Wait 5 seconds for any potential output
    await new Promise(r => setTimeout(r, 5000));

    // Check logs for reader thread behavior
    const logContent = await fs.promises.readFile(LOG_PATH, 'utf-8');
    const lines = logContent.split('\n');

    // Find reader start and exit for our session
    const sessionReaderStarts = lines.filter(l => l.includes(`starting reader thread`) && l.includes(`session ${session.id}`));
    const sessionReaderExits = lines.filter(l => l.includes(`PTY reader thread exited`) && l.includes(`session ${session.id}`));

    console.log(`Session ${session.id} reader starts: ${sessionReaderStarts.length}`);
    console.log(`Session ${session.id} reader exits: ${sessionReaderExits.length}`);

    if (sessionReaderStarts.length > 0 && sessionReaderExits.length > 0) {
      // Check how quickly it exited
      const firstStart = sessionReaderStarts[0];
      const lastExit = sessionReaderExits[sessionReaderExits.length - 1];

      const startTime = getLogTimestamp(firstStart);
      const exitTime = getLogTimestamp(lastExit);

      if (startTime && exitTime) {
        const timeDiff = new Date(exitTime).getTime() - new Date(startTime).getTime();
        console.log(`Reader lifetime for session ${session.id}: ${timeDiff}ms`);

        // This assertion will fail if the Claude Code agent exits immediately (the bug we're testing for)
        expect(timeDiff, `Claude Code agent should keep running, not exit after ${timeDiff}ms`).toBeGreaterThan(2000);
      }
    } else {
      // If we don't see both, something is wrong with the spawn
      expect(sessionReaderStarts.length, 'Should have seen reader thread start').toBeGreaterThan(0);
    }
  });
});