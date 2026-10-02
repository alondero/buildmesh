#!/usr/bin/env node
// run-guarded.mjs — run one command under a hard deadline and kill its whole
// process tree when the deadline passes.
//
// CI's timeout guards used to be inline `timeout --kill-after` shell fragments
// duplicated per step. Two failure modes made that unreliable (issue #1961 and
// the lost-runner runs in issue #1520):
//
//   1. `| tee` pipelines never see EOF when a descendant escapes the process
//      group while holding the pipe, so the step outlives the guard meant to
//      end it. This script streams output itself and bounds every wait on the
//      pipes after the kill: the deadline path waits out the kill grace (so
//      the SIGKILL escalation always fires), then exits after a bounded
//      drain, whatever the descendants do with their inherited handles.
//   2. A job cancelled by its `timeout-minutes` cap flushes no log at all, so
//      the wedged step produces no evidence. The log here is written
//      incrementally as output arrives, and the `::error::` annotation is
//      emitted before the kill, so even a hard kill leaves both behind.
//
// Exit codes: the guarded command's own exit code on normal completion, 124
// when the deadline fired (GNU `timeout` convention), 127 when the command
// could not be started, 2 on a usage error.
//
// Usage:
//   node scripts/ci/run-guarded.mjs --minutes 30 --kill-grace-seconds 120 \
//     --log cargo-db.log --label "The db shard" -- cargo test --locked ...

import { spawn } from 'node:child_process';
import { closeSync, openSync, writeSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const USAGE = `Usage: node scripts/ci/run-guarded.mjs --minutes <n> [--kill-grace-seconds <n>]
         [--log <path>] [--label <text>] -- <command> [args...]

Runs <command> under a hard deadline of --minutes. On expiry the command's
whole process tree is terminated (SIGTERM, then SIGKILL after
--kill-grace-seconds on POSIX; taskkill /T /F immediately on Windows), a
GitHub Actions ::error:: annotation naming --label is emitted, and the script
exits 124. Combined output is streamed live and, when --log is given, written
to that file as it arrives (created or truncated when the guard starts).`;

export function parseArgs(argv) {
  let minutes = null;
  let killGraceSeconds = 120;
  let log = null;
  let label = null;
  let command = [];

  const readValue = (flag, inline, next) => {
    if (inline !== undefined) return { value: inline, consumed: 0 };
    if (next === undefined || next === '--') throw new Error(`Missing value for ${flag}.\n\n${USAGE}`);
    return { value: next, consumed: 1 };
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--') {
      command = argv.slice(i + 1);
      break;
    }
    const eq = arg.indexOf('=');
    const flag = eq === -1 ? arg : arg.slice(0, eq);
    const inline = eq === -1 ? undefined : arg.slice(eq + 1);
    if (flag === '--minutes' || flag === '--kill-grace-seconds' || flag === '--log' || flag === '--label') {
      const { value, consumed } = readValue(flag, inline, argv[i + 1]);
      i += consumed;
      if (flag === '--minutes') minutes = Number(value);
      else if (flag === '--kill-grace-seconds') killGraceSeconds = Number(value);
      else if (flag === '--log') log = value;
      else label = value;
    } else {
      throw new Error(`Unknown option: ${arg}\n\n${USAGE}`);
    }
  }

  if (!Number.isFinite(minutes) || minutes <= 0) throw new Error(`--minutes must be a positive number.\n\n${USAGE}`);
  if (!Number.isFinite(killGraceSeconds) || killGraceSeconds < 0) throw new Error(`--kill-grace-seconds must be a non-negative number.\n\n${USAGE}`);
  if (command.length === 0) throw new Error(`No command given after --.\n\n${USAGE}`);

  return { minutes, killGraceSeconds, log, label: label ?? 'The guarded command', command };
}

// The child is spawned detached on POSIX, so it leads its own process group
// and `kill(-pid)` reaches every descendant that has not left the group.
// Windows has no SIGTERM semantics worth waiting for on a console process, so
// taskkill /T /F does the tree in one step.
export function killTree(pid, killGraceMs) {
  if (pid == null) return { escalated: Promise.resolve() };
  if (process.platform === 'win32') {
    const killer = spawn('taskkill', ['/pid', String(pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
    killer.on('error', () => {});
    return { escalated: Promise.resolve() };
  }
  const signalGroup = (signal) => {
    try { process.kill(-pid, signal); } catch { /* already gone */ }
  };
  signalGroup('SIGTERM');
  // Deliberately NOT unref'd: the timer keeps the event loop alive until
  // SIGKILL has actually been sent, so a direct child that dies on SIGTERM
  // (cargo, bash) cannot let this script exit first and strand a descendant
  // that ignored SIGTERM — the escalation is the whole point of the grace
  // period (review of PR #1991).
  const escalated = new Promise((resolve) => {
    setTimeout(() => {
      signalGroup('SIGKILL');
      resolve();
    }, killGraceMs);
  });
  return { escalated };
}

export function runGuarded({ minutes, killGraceSeconds, log, label, command }) {
  const deadlineMs = minutes * 60_000;
  const killGraceMs = killGraceSeconds * 1000;
  // After the kill the script must exit whatever the descendants do with the
  // inherited pipes (issue #1961), so every wait path is bounded by this cap.
  const postKillCapMs = (process.platform === 'win32' ? 0 : killGraceMs) + 10_000;
  const drainCapMs = 3_000;

  let logFd = null;
  if (log) logFd = openSync(log, 'w');

  const emit = (chunk) => {
    if (logFd != null) writeSync(logFd, chunk);
    process.stdout.write(chunk);
  };

  const child = spawn(command[0], command.slice(1), {
    stdio: ['ignore', 'pipe', 'pipe'],
    detached: process.platform !== 'win32',
    windowsHide: true,
  });

  return new Promise((resolve) => {
    let timedOut = false;
    let settled = false;
    // Set when the deadline fires; the exit handler awaits it before
    // finishing so SIGKILL always gets its chance to clear the tree.
    let escalated = Promise.resolve();

    const finish = (code) => {
      if (settled) return;
      settled = true;
      clearTimeout(deadline);
      clearTimeout(overallCap);
      if (logFd != null) {
        try { closeSync(logFd); } catch { /* already closed */ }
        logFd = null;
      }
      // Closing our read ends releases the pipe handles so the process can
      // exit through the normal path, which flushes queued stdout/stderr —
      // process.exit() would truncate them on a pipe.
      if (!child.stdout.destroyed) child.stdout.destroy();
      if (!child.stderr.destroyed) child.stderr.destroy();
      resolve(code);
    };

    // Bounded drain: wait for the streams to end so buffered output reaches
    // the log, but never longer than drainCapMs — a descendant that inherited
    // the pipe and outlived the guard would otherwise hold this open forever.
    // The `readableEnded` guard matters because a stream can reach EOF and
    // emit `end` before the child's `exit` event is processed, in which case
    // a freshly attached listener would never fire.
    const drainAndFinish = (code) => {
      let pending = 0;
      for (const stream of [child.stdout, child.stderr]) {
        if (stream.readableEnded || stream.destroyed) continue;
        pending += 1;
        stream.once('end', () => { if (--pending === 0) finish(code); });
      }
      if (pending === 0) {
        finish(code);
        return;
      }
      setTimeout(() => finish(code), drainCapMs).unref();
    };

    child.stdout.on('data', emit);
    child.stderr.on('data', emit);

    const deadline = setTimeout(() => {
      timedOut = true;
      const message = `${label} exceeded ${minutes} minutes and was killed - a test in it is hanging.`;
      // Emitted before the kill so a hard stop still leaves the reason behind.
      emit(`::error::${message}\n`);
      escalated = killTree(child.pid, killGraceMs).escalated;
      setTimeout(() => finish(124), postKillCapMs).unref();
    }, deadlineMs);

    const overallCap = setTimeout(() => finish(timedOut ? 124 : 1), Math.max(deadlineMs, 0) + postKillCapMs + 5_000);

    child.on('error', (err) => {
      emit(`run-guarded: failed to start ${command[0]}: ${err.message}\n`);
      finish(err.code === 'ENOENT' ? 127 : 1);
    });

    child.on('exit', (code, signal) => {
      if (timedOut) {
        // The direct child can die on SIGTERM while a descendant ignores it;
        // the SIGKILL escalation is the only thing that clears that
        // descendant, so hold the guard open until it has fired rather than
        // exiting out from under the timer (review of PR #1991).
        escalated.then(() => drainAndFinish(124));
        return;
      }
      if (code != null) {
        drainAndFinish(code);
        return;
      }
      // Exited by a signal we did not originate: fail rather than report 0.
      emit(`run-guarded: ${command[0]} terminated by ${signal ?? 'an unknown signal'}.\n`);
      drainAndFinish(1);
    });
  });
}

async function main() {
  let options;
  try {
    options = parseArgs(process.argv.slice(2));
  } catch (err) {
    process.stderr.write(`${err.message}\n`);
    process.exitCode = 2;
    return;
  }
  process.exitCode = await runGuarded(options);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}
