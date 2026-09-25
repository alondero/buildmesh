#!/usr/bin/env node

import { DatabaseSync } from 'node:sqlite';
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, existsSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';
import { createCodexHookRelay } from './codex-hook-relay.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const issue = 1905;
const model = 'gpt-6-luna';
const appIdentifier = 'com.alond.buildmesh.issue1905.dev';
const cdpPort = Number(process.env.BUILDMESH_1905_CDP_PORT ?? 9224);
const appHttpPort = 2992;
const testBridgePort = 2991;
const appBinary = path.join(root, 'src-tauri', 'target', 'release', 'buildmesh-dev.exe');
const appDataRoot = process.env.APPDATA || path.join(process.env.USERPROFILE || os.homedir(), 'AppData', 'Roaming');
const appProfile = path.join(appDataRoot, appIdentifier);
const databasePath = path.join(appProfile, 'buildmesh.db');
const commit = runText('git', ['rev-parse', 'HEAD'], root);
const timestamp = new Date().toISOString().replaceAll(':', '-').replaceAll('.', '-');
const scratchRoot = path.join(root, '.tmp', `codex-hook-recovery-${timestamp}`);
const appData = path.join(scratchRoot, 'appdata');
const workspace = path.join(scratchRoot, 'workspace');
const reportPath = path.join(scratchRoot, 'evidence.json');
const report = {
  issue,
  startedAt: new Date().toISOString(),
  buildmeshCommit: commit,
  platform: { os: os.platform(), release: os.release(), version: os.version() },
  codexVersion: null,
  codexHome: process.env.CODEX_HOME ? 'CODEX_HOME environment override' : 'default user profile .codex directory',
  model,
  launch: {
    provider: 'codex',
    approval: 'never',
    sandbox: 'read-only (fixture relay mode)',
    projectHookTrust: 'Buildmesh provisions and bypasses project hook trust for its managed Codex process',
    externalEffects: 'synthetic text-only prompts; no repository writes or tools expected',
  },
  scenarios: [],
  cleanup: { cancelledRuns: [], meshDeleted: false, appShutdown: false },
  relayEvents: [],
};

let relay;
let app;
let appExited;
let browser;
let page;
let database;
let meshId;
let meshDeleted = false;
let circuitId;
const runIds = [];
const cancelledRuns = new Set();
let gracefulExitRequested = false;

function runText(command, args, cwd = root) {
  const windowsCodex = process.platform === 'win32' && command === 'codex';
  const executable = windowsCodex ? (process.env.ComSpec || 'cmd.exe') : command;
  const executableArgs = windowsCodex ? ['/d', '/s', '/c', `codex.cmd ${args.join(' ')}`] : args;
  const result = spawnSync(executable, executableArgs, {
    cwd,
    encoding: 'utf8',
    windowsHide: true,
  });
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(' ')} failed: ${result.stderr || result.stdout || result.error?.message || 'unknown process error'}`);
  }
  return result.stdout.trim();
}

async function assertPortAvailable(port) {
  const server = createServer();
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', resolve);
  }).catch((error) => {
    throw new Error(`Port ${port} is already in use; choose another CDP port with BUILDMESH_1905_CDP_PORT if needed (${error.message})`);
  });
  await new Promise((resolve) => server.close(resolve));
}

async function waitFor(description, probe, timeoutMs = 120_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const value = await probe();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  throw new Error(`Timed out waiting for ${description}${lastError ? `: ${lastError.message}` : ''}`);
}

async function testBridge(command, args = {}) {
  const response = await fetch(`http://127.0.0.1:${testBridgePort}/invoke`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ cmd: command, args }),
    signal: AbortSignal.timeout(15_000),
  });
  const result = await response.json();
  if (!result.ok) throw new Error(`test bridge ${command}: ${result.error}`);
  return result.data;
}

async function invoke(command, args = {}) {
  if (!page) throw new Error(`Cannot invoke ${command}: Buildmesh CDP is unavailable`);
  return page.evaluate(async ({ commandName, commandArgs }) => {
    const internal = window.__TAURI_INTERNALS__;
    if (!internal?.invoke) throw new Error('Tauri invoke bridge is unavailable in the real app window');
    return internal.invoke(commandName, commandArgs);
  }, { commandName: command, commandArgs: args });
}

function circuitGraph(prompt) {
  return {
    version: 3,
    blueprint: 'walking_skeleton',
    nodes: [
      { id: 'trigger', type: { type: 'manual' } },
      {
        id: 'spawn',
        type: {
          type: 'spawn_agent_node',
          prompt,
          name: 'Codex hook recovery smoke',
          provider: 'codex',
          model,
          effort: 'low',
          extra_args: null,
          timeout_seconds: 60,
        },
      },
    ],
    edges: [{ from: 'trigger', to: 'spawn', condition: 'always' }],
  };
}

async function createCircuit(prompt) {
  const circuit = await invoke('create_circuit', {
    meshId,
    name: 'Issue 1905 controlled Codex hook recovery',
    description: 'Disposable read-only live callback delivery fixture.',
    concurrencyLimit: 1,
    initialPrompt: prompt,
    triggerKind: 'manual',
    triggerLabel: null,
    intervalSeconds: null,
    blueprint: 'walking_skeleton',
  });
  circuitId = circuit.id;
  await invoke('update_circuit_graph', {
    circuitId,
    graphJson: JSON.stringify(circuitGraph(prompt)),
  });
  return circuit;
}

async function startRun() {
  const runId = await invoke('trigger_circuit_now', { circuitId });
  runIds.push(runId);
  return runId;
}

async function runDetail(runId) {
  const rows = await invoke('list_circuit_runs', { circuitId, limit: 100 });
  return rows.find((row) => row.run.id === runId) ?? null;
}

async function runEvidence(runId) {
  return invoke('circuit_run_history', { runId });
}

function readAgent(nodeId) {
  return database.prepare(`
    SELECT id, provider, status, cli_session_id, path, worktree_path, use_worktree, spawn_configuration
    FROM agent_nodes WHERE id = ?
  `).get(nodeId);
}

function readEffects(runId) {
  return database.prepare('SELECT node_id, attempt, kind, state FROM circuit_effects WHERE run_id = ? ORDER BY kind').all(runId);
}

async function waitForRunStep(runId, predicate, description, timeoutMs = 150_000) {
  return waitFor(description, async () => {
    const detail = await runDetail(runId);
    if (!detail) return null;
    const step = detail.steps.find((candidate) => candidate.node_id === 'spawn');
    const node = step?.agent_node_id ? readAgent(step.agent_node_id) : null;
    return predicate({ detail, step, node }) ? { detail, step, node } : null;
  }, timeoutMs);
}

async function waitForUnverifiedCheckpoint(runId) {
  const state = await waitForRunStep(
    runId,
    ({ step }) => step?.status === 'unverified',
    `Run ${runId} bounded Codex evidence recheck`,
  );
  const evidence = await runEvidence(runId);
  const checkpoint = evidence.checkpoints.find((item) => item.node_id === 'spawn' && item.attempt === 1);
  if (!checkpoint?.actions.includes('recheck')) {
    throw new Error(`Run ${runId} is Unverified without an actionable Recheck checkpoint`);
  }
  const rolloutRecheck = evidence.entries.some((entry) => entry.kind === 'observation'
    && JSON.parse(entry.detail).observation?.source === 'codex_rollout_task_complete');
  const boundedWindow = state.step.error_message?.includes('Evidence window ended after 1 minutes') ?? false;
  if (!rolloutRecheck && !boundedWindow) {
    throw new Error(`Run ${runId} reached Unverified without recording a Codex rollout recheck or its declared 60-second evidence window`);
  }
  return { ...state, evidence, checkpoint, boundedSource: rolloutRecheck ? 'codex_rollout_task_complete' : '60_second_evidence_window' };
}

function parseNativeReceipts(evidence) {
  return evidence.entries.flatMap((entry) => {
    if (entry.kind !== 'native_hook_received') return [];
    try {
      return [{ entry, receipt: JSON.parse(entry.detail) }];
    } catch {
      return [];
    }
  });
}

function parseObservations(evidence) {
  return evidence.entries.flatMap((entry) => {
    if (entry.kind !== 'observation') return [];
    try {
      return [{ entry, recorded: JSON.parse(entry.detail) }];
    } catch {
      return [];
    }
  });
}

function snapshotRelayEvents(events) {
  return events.map((event) => ({
    id: event.id,
    nodeId: event.nodeId,
    event: event.event,
    occurrence: event.occurrence,
    sessionId: event.sessionId,
    turnId: event.turnId,
    toolName: event.toolName,
    action: event.action,
    receivedAt: event.receivedAt,
    forwardCount: event.forwardCount,
    forwardStatuses: event.forwardStatuses,
    forwardedAt: event.forwardedAt,
  }));
}

function uniqueTurnIds(events, nodeId) {
  return [...new Set(events
    .filter((event) => event.nodeId === nodeId && event.event === 'UserPromptSubmit' && event.turnId)
    .map((event) => event.turnId))];
}

function assertNoToolEvents(events, nodeId) {
  const toolEvents = events.filter((event) => event.nodeId === nodeId
    && ['PreToolUse', 'PostToolUse', 'PostToolUseFailure', 'PermissionRequest', 'SubagentStart', 'SubagentStop'].includes(event.event));
  if (toolEvents.length) throw new Error(`Codex emitted tool or permission callbacks during no-tools smoke: ${toolEvents.map((event) => event.event).join(', ')}`);
}

async function createWorkspaceAndMesh() {
  mkdirSync(workspace, { recursive: true });
  runText('git', ['init', '--initial-branch', 'main', workspace], root);
  const mesh = await testBridge('create_test_mesh', { name: 'Issue 1905 disposable Codex smoke' });
  meshId = mesh.id;
  const available = new Set(database.prepare('PRAGMA table_info(meshes)').all().map((column) => column.name));
  const values = { path: workspace };
  if (available.has('pre_spawn_pool_size')) values.pre_spawn_pool_size = 0;
  if (available.has('use_worktree')) values.use_worktree = 0;
  if (available.has('default_provider')) values.default_provider = 'codex';
  if (available.has('autopilot_provider')) values.autopilot_provider = 'codex';
  const assignments = Object.keys(values).map((column) => `${column} = ?`).join(', ');
  database.prepare(`UPDATE meshes SET ${assignments} WHERE id = ?`)
    .run(...Object.values(values), meshId);
  return meshId;
}

async function scenarioMissingStopAndDuplicateStart() {
  relay.setPlan([
    { event: 'UserPromptSubmit', occurrence: 1, action: 'duplicate' },
    { event: 'Stop', occurrence: 1, action: 'drop' },
  ]);
  const prompt = 'Reply with exactly BUILDMESH_HOOK_SMOKE_ONE. Do not use tools, run commands, modify files, follow links, or cause any external effect.';
  await createCircuit(prompt);
  const runId = await startRun();
  const running = await waitForRunStep(runId, ({ step }) => step?.agent_node_id != null, `Run ${runId} Codex session attachment`);
  const nodeId = running.step.agent_node_id;
  console.log(`Run ${runId} attached Agent Node ${nodeId}; waiting for the omitted Stop recheck.`);

  const start = await relay.waitFor((event) => event.nodeId === nodeId && event.event === 'UserPromptSubmit', 150_000);
  await waitFor('both duplicate UserPromptSubmit forwards', () => start.forwardCount === 2, 15_000);
  const duplicateCallbackForwardCount = start.forwardCount;
  if (duplicateCallbackForwardCount !== 2) throw new Error('Expected to record both duplicate UserPromptSubmit forwards');
  const stopped = await relay.waitFor((event) => event.nodeId === nodeId && event.event === 'Stop', 150_000);
  if (stopped.action !== 'drop' || stopped.forwardCount !== 0) throw new Error('The selected Stop callback was not omitted by the relay');

  const recovered = await waitForUnverifiedCheckpoint(runId);
  const receipts = parseNativeReceipts(recovered.evidence);
  const starts = receipts.filter(({ receipt }) => receipt.hook?.event === 'UserPromptSubmit');
  if (starts.length !== 1) throw new Error(`Expected one durable receipt after duplicate delivery, got ${starts.length}`);
  if (starts[0].receipt.hook.turn_id !== start.turnId) throw new Error('The deduplicated receipt does not match the live Codex turn');
  const effects = readEffects(runId);
  if (effects.filter((effect) => effect.kind === 'spawn').length !== 1) throw new Error(`Expected one spawn effect, got ${effects.length}`);
  if (effects.some((effect) => effect.kind !== 'spawn')) throw new Error('The controlled graph recorded an unexpected external effect');
  assertNoToolEvents(relay.events, nodeId);

  const scenario = {
    name: 'omitted Stop with duplicate UserPromptSubmit',
    runId,
    circuitId,
    nodeId,
    sessionId: start.sessionId,
    turnId: start.turnId,
    promptSubmissions: uniqueTurnIds(relay.events, nodeId).length,
    duplicateCallbackForwardCount,
    durableUserPromptSubmitReceipts: starts.length,
    omittedStop: { event: stopped.event, action: stopped.action, forwarded: stopped.forwardCount === 0 },
    rolloutRecheck: recovered.evidence.entries.some((entry) => entry.kind === 'observation'
      && JSON.parse(entry.detail).observation?.source === 'codex_rollout_task_complete'),
    boundedReconciliation: {
      source: recovered.boundedSource,
      disposition: 'unverified_actionable',
      reason: recovered.step.error_message,
      checkpointActions: recovered.checkpoint.actions,
    },
    state: recovered.detail.run.state,
    stepStatus: recovered.step.status,
    attempt: recovered.step.attempt,
    checkpointActions: recovered.checkpoint.actions,
    effects,
    toolCallbacks: relay.events.filter((event) => event.nodeId === nodeId
      && ['PreToolUse', 'PostToolUse', 'PostToolUseFailure', 'PermissionRequest', 'SubagentStart', 'SubagentStop'].includes(event.event)).length,
  };
  if (scenario.stepStatus !== 'unverified' || !scenario.checkpointActions.includes('recheck')) {
    throw new Error('Missing-hook recheck did not leave an actionable Unverified checkpoint');
  }
  report.scenarios.push(scenario);

  await invoke('cancel_circuit_run', { runId });
  cancelledRuns.add(runId);
  return scenario;
}

async function scenarioDelayedOldTurn() {
  relay.setPlan([{ event: 'Stop', occurrence: 1, action: 'delay' }]);
  const prompt = 'Reply with exactly BUILDMESH_HOOK_SMOKE_OLD_TURN. Do not use tools, run commands, modify files, follow links, or cause any external effect.';
  await invoke('update_circuit_graph', {
    circuitId,
    graphJson: JSON.stringify(circuitGraph(prompt)),
  });
  const runId = await startRun();
  const running = await waitForRunStep(runId, ({ step }) => step?.agent_node_id != null, `Run ${runId} Codex session attachment`);
  const nodeId = running.step.agent_node_id;
  const firstStart = await relay.waitFor((event) => event.nodeId === nodeId && event.event === 'UserPromptSubmit', 150_000);
  const oldStop = await relay.waitFor((event) => event.nodeId === nodeId && event.event === 'Stop', 150_000);
  if (oldStop.action !== 'delay' || relay.heldEvents().length !== 1) throw new Error('The old-turn Stop was not delayed in the fixture relay');
  const firstCheckpoint = await waitForUnverifiedCheckpoint(runId);
  const availableNode = await waitFor(`Agent Node ${nodeId} retains its live Codex process`, () => {
    const node = readAgent(nodeId);
    return node && ['ready', 'running'].includes(node.status) ? node : null;
  }, 10_000);

  const secondPrompt = 'Reply with exactly BUILDMESH_HOOK_SMOKE_NEW_TURN. Do not use tools, run commands, modify files, follow links, or cause any external effect.';
  await invoke('send_to_agent', { sessionId: nodeId, input: secondPrompt });
  const currentStart = await relay.waitFor((event) => event.nodeId === nodeId
    && event.event === 'UserPromptSubmit' && event.turnId && event.turnId !== firstStart.turnId, 30_000);
  const [released] = await relay.releaseHeld((event) => event.id === oldStop.id);
  if (!released || released.turnId !== firstStart.turnId || released.forwardCount !== 1) {
    throw new Error('The delayed old-turn callback was not forwarded with its original turn identity');
  }

  await waitFor('delayed old-turn observation recorded as rejected', async () => {
    const evidence = await runEvidence(runId);
    return parseObservations(evidence).find(({ recorded }) => recorded.disposition === 'rejected'
      && recorded.observation?.source === 'codex_native_hook'
      && recorded.observation?.identity?.turn_id === firstStart.turnId) ?? null;
  }, 30_000);
  const finalState = await waitForRunStep(runId, ({ step }) => step?.status === 'unverified', `Run ${runId} remains Unverified after stale callback`);
  const evidence = await runEvidence(runId);
  const checkpoint = evidence.checkpoints.find((item) => item.node_id === 'spawn' && item.attempt === 1);
  const observations = parseObservations(evidence);
  const stale = observations.find(({ recorded }) => recorded.disposition === 'rejected'
    && recorded.observation?.source === 'codex_native_hook'
    && recorded.observation?.identity?.turn_id === firstStart.turnId);
  if (!stale) throw new Error('The released old-turn callback did not persist a rejected Circuit observation');
  if (finalState.step.attempt !== 1 || finalState.step.agent_node_id !== nodeId) {
    throw new Error('Delayed callback changed the current Circuit attempt or its owned Agent Node');
  }
  if (!checkpoint?.actions.includes('recheck')) throw new Error('The run lost its actionable Unverified checkpoint after the delayed callback');
  const effects = readEffects(runId);
  if (effects.filter((effect) => effect.kind === 'spawn').length !== 1 || effects.some((effect) => effect.kind !== 'spawn')) {
    throw new Error(`The delayed callback caused a duplicate spawn or unexpected effect: ${JSON.stringify(effects)}`);
  }
  assertNoToolEvents(relay.events, nodeId);
  const turns = uniqueTurnIds(relay.events, nodeId);
  if (turns.length !== 2 || !turns.includes(currentStart.turnId)) {
    throw new Error(`Expected the two deliberate Codex turns and no replay, got ${turns.length}`);
  }

  const scenario = {
    name: 'delayed old-turn Stop after a new turn starts',
    runId,
    circuitId,
    nodeId,
    sessionId: firstStart.sessionId,
    oldTurnId: firstStart.turnId,
    currentTurnId: currentStart.turnId,
    oldStopReleasedAfterCurrentSubmit: true,
    oldTurnDisposition: stale.recorded.disposition,
    promptSubmissions: turns.length,
    state: finalState.detail.run.state,
    stepStatus: finalState.step.status,
    attempt: finalState.step.attempt,
    checkpointActions: checkpoint.actions,
    effects,
    toolCallbacks: relay.events.filter((event) => event.nodeId === nodeId
      && ['PreToolUse', 'PostToolUse', 'PostToolUseFailure', 'PermissionRequest', 'SubagentStart', 'SubagentStop'].includes(event.event)).length,
    firstTurnCheckpointStatus: firstCheckpoint.step.status,
    nodeStatusBeforeSecondPrompt: availableNode.status,
  };
  report.scenarios.push(scenario);
  await invoke('cancel_circuit_run', { runId });
  cancelledRuns.add(runId);
  return scenario;
}

async function main() {
  if (process.platform !== 'win32') throw new Error('The controlled live Codex hook smoke runs on Windows only');
  report.codexVersion = runText('codex', ['--version']);
  if (!existsSync(appBinary)) throw new Error('Build the isolated dev app first: npm run tauri:build:dev:codex-hook-smoke');
  for (const port of [testBridgePort, appHttpPort, cdpPort]) await assertPortAvailable(port);

  mkdirSync(appData, { recursive: true });
  mkdirSync(workspace, { recursive: true });
  relay = await createCodexHookRelay();
  app = spawn(appBinary, [], {
    cwd: root,
    windowsHide: true,
    stdio: 'ignore',
    env: {
      ...process.env,
      APPDATA: appData,
      BUILDMESH_CODEX_HOOK_RELAY_URL: relay.baseUrl,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${cdpPort}`,
      RUST_BACKTRACE: '1',
    },
  });
  appExited = new Promise((resolve) => {
    app.once('error', (error) => resolve({ error: error.message }));
    app.once('exit', (code, signal) => resolve({ code, signal }));
  });

  await waitFor('isolated dev app test bridge', async () => {
    const response = await fetch(`http://127.0.0.1:${testBridgePort}/health`, { signal: AbortSignal.timeout(1_000) });
    return response.ok;
  });
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${cdpPort}`);
  page = await waitFor('real Buildmesh WebView page', async () => {
    const pages = browser.contexts().flatMap((context) => context.pages());
    return pages.find((candidate) => !candidate.isClosed()) ?? null;
  });
  await waitFor('Tauri invoke bridge', async () => page.evaluate(() => Boolean(window.__TAURI_INTERNALS__?.invoke)));

  report.appIdentifier = await invoke('get_app_identifier');
  if (report.appIdentifier !== appIdentifier) {
    throw new Error(`Expected isolated app identifier ${appIdentifier}, got ${report.appIdentifier}`);
  }
  database = new DatabaseSync(databasePath);
  report.relayOrigin = relay.baseUrl;
  report.relayModeSandbox = 'read-only';
  report.testDatabase = `${appIdentifier}/buildmesh.db`;
  const leftoverFixture = database.prepare('SELECT id FROM meshes WHERE name = ? LIMIT 1')
    .get('Issue 1905 disposable Codex smoke');
  if (leftoverFixture) {
    await testBridge('delete_mesh', { meshId: leftoverFixture.id });
    if (database.prepare('SELECT 1 FROM meshes WHERE id = ?').get(leftoverFixture.id)) {
      throw new Error(`The isolated profile still contains the previous smoke Mesh ${leftoverFixture.id}`);
    }
  }
  console.log(`Isolated Buildmesh app ready (${appIdentifier}); starting real Codex callbacks.`);
  await createWorkspaceAndMesh();

  const missing = await scenarioMissingStopAndDuplicateStart();
  const delayed = await scenarioDelayedOldTurn();
  report.completedAt = new Date().toISOString();
  report.relayEvents = snapshotRelayEvents(relay.events);
  await writeReport();
  console.log(`Issue #${issue} live Codex hook recovery smoke passed.`);
  console.log(`Run ${missing.runId}: omitted Stop source ${missing.boundedReconciliation.source} ended as ${missing.boundedReconciliation.disposition}; actionable ${missing.stepStatus} checkpoint.`);
  console.log(`Run ${delayed.runId}: delayed ${delayed.oldTurnId} callback recorded ${delayed.oldTurnDisposition}; current attempt ${delayed.attempt} stayed ${delayed.stepStatus}.`);
  console.log(`Evidence: ${path.relative(root, reportPath)}`);
}

async function writeReport() {
  mkdirSync(scratchRoot, { recursive: true });
  writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`, 'utf8');
}

async function cleanup() {
  if (page && app) {
    for (const runId of runIds) {
      if (cancelledRuns.has(runId)) continue;
      try {
        await invoke('cancel_circuit_run', { runId });
        cancelledRuns.add(runId);
      } catch (error) {
        report.cleanup.runCancelErrors ??= [];
        report.cleanup.runCancelErrors.push({ runId, error: error.message });
      }
    }
  }
  if (meshId != null && !meshDeleted) {
    try {
      await testBridge('delete_mesh', { meshId });
      meshDeleted = true;
    } catch (error) {
      report.cleanup.meshDeleteError = error.message;
    }
  }
  if (page && app) {
    await invoke('exit_application').then(() => { gracefulExitRequested = true; }).catch(() => {});
  }
  let appExitResult = null;
  if (app) {
    appExitResult = await Promise.race([
      appExited,
      new Promise((resolve) => setTimeout(() => resolve(null), 15_000)),
    ]);
    if (!appExitResult) {
      const killed = spawnSync('taskkill.exe', ['/PID', String(app.pid), '/T', '/F'], {
        encoding: 'utf8',
        windowsHide: true,
      });
      report.cleanup.appForcedExit = true;
      if (killed.status !== 0) report.cleanup.appKillError = killed.stderr || killed.stdout || killed.error?.message || 'taskkill failed';
      appExitResult = await Promise.race([
        appExited,
        new Promise((resolve) => setTimeout(() => resolve(null), 10_000)),
      ]);
    } else if (appExitResult.error) {
      console.error(`Buildmesh app process error: ${appExitResult.error}`);
    }
  }
  if (relay) {
    await relay.releaseHeld().catch(() => {});
    await relay.close().catch(() => {});
  }
  database?.close();
  if (browser) await browser.close().catch(() => {});
  report.cleanup.meshDeleted = meshDeleted;
  report.cleanup.appShutdown = gracefulExitRequested && appExitResult != null
    && !appExitResult.error && appExitResult.code === 0;
  report.cleanup.cancelledRuns = [...cancelledRuns];
  report.cleanup.circuitId = circuitId;
  const profilePath = path.resolve(appProfile);
  const expectedProfilePath = path.resolve(appDataRoot, appIdentifier);
  const safeProfileTarget = profilePath === expectedProfilePath
    && path.dirname(profilePath) === path.resolve(appDataRoot)
    && path.basename(profilePath) === appIdentifier
    && report.appIdentifier === appIdentifier;
  if (!report.failure && report.scenarios.length === 2 && meshDeleted && report.cleanup.appShutdown && safeProfileTarget) {
    try {
      rmSync(appProfile, { recursive: true, force: false });
      report.cleanup.isolatedProfileRemoved = true;
    } catch (error) {
      report.cleanup.isolatedProfileRemovalError = error.message;
    }
  }
  report.relayEvents = relay ? snapshotRelayEvents(relay.events) : [];
  report.completedAt ??= new Date().toISOString();
  await writeReport().catch((error) => console.error(`Could not write smoke evidence: ${error.message}`));
}

async function capturePartialFailureEvidence() {
  if (!page || !database) return;
  report.partialRuns = [];
  for (const runId of runIds) {
    try {
      const detail = await runDetail(runId);
      const evidence = await runEvidence(runId);
      const observations = parseObservations(evidence).map(({ recorded }) => ({
        source: recorded.observation?.source ?? null,
        disposition: recorded.disposition ?? null,
        turnId: recorded.observation?.identity?.turn_id ?? null,
      }));
      const receipts = parseNativeReceipts(evidence).map(({ receipt }) => ({
        event: receipt.hook?.event ?? null,
        turnId: receipt.hook?.turn_id ?? null,
        turnFenced: receipt.turn_fenced ?? null,
        explicitTurnMismatch: receipt.explicit_turn_mismatch ?? null,
      }));
      const nodeIds = [...new Set(detail?.steps.map((step) => step.agent_node_id).filter(Boolean) ?? [])];
      const agents = nodeIds.map((nodeId) => {
        const row = readAgent(nodeId);
        return row ? { id: row.id, status: row.status, hasSessionId: Boolean(row.cli_session_id) } : { id: nodeId, missing: true };
      });
      report.partialRuns.push({
        runId,
        runState: detail?.run.state ?? null,
        steps: detail?.steps.map(({ node_id, status, attempt, agent_node_id }) => ({
          nodeId: node_id,
          status,
          attempt,
          agentNodeId: agent_node_id,
        })) ?? [],
        historyKinds: evidence.entries.map((entry) => entry.kind),
        observations,
        receipts,
        checkpoints: evidence.checkpoints.map((checkpoint) => ({
          nodeId: checkpoint.node_id,
          attempt: checkpoint.attempt,
          actions: checkpoint.actions,
        })),
        effects: readEffects(runId),
        agents,
      });
    } catch (error) {
      report.partialRunCaptureError = error.message;
    }
  }
}

try {
  await main();
} catch (error) {
  console.error(`Issue #${issue} live smoke failed: ${error.stack ?? error.message}`);
  report.failure = error.message;
  report.completedAt = new Date().toISOString();
  await capturePartialFailureEvidence();
} finally {
  await cleanup();
}

if (report.failure) process.exitCode = 1;
