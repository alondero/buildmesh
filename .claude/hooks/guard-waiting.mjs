#!/usr/bin/env node
// PreToolUse(Bash|PowerShell): replace sleep-and-check waits with blocking waits.
// This is an early warning for common command shapes, not a shell parser or sandbox.
// Computed commands and scripts read from files are outside its scope. Fail open on
// malformed hook input; never execute the candidate command to classify it.

import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { stripHeredocs } from './guard-commit-staging.mjs';

// Hide prose, comments and quoted scripts before looking for command boundaries.
// Keep simple quoted paths so `tail "verify.log"` is still recognized as a log read.
function commandText(command) {
  return stripHeredocs(command).replace(
    /<#[\s\S]*?#>|(?:^|\s)#[^\n]*|"(?:[`\\][\s\S]|[^"\\`])*"|'(?:''|[^'])*'/g,
    (token) => {
      if (token.startsWith('"') || token.startsWith("'")) {
        const value = token.slice(1, -1);
        return /^[\w./:\\-]+$/.test(value) ? value : ' ARG ';
      }
      return token.replace(/[^\n]/g, ' ');
    },
  );
}

const BOUNDARY = String.raw`(?:^|[;|&\n{}(=])\s*(?:(?:while|until|if|do|then|else|!|&|command|exec|env|time)\s+)*`;
const SLEEP = new RegExp(String.raw`${BOUNDARY}(?:[^\s;|&{}()]*[\\/])?(?:Start-Sleep|sleep)(?:\.exe)?(?=\s|[;|&)}]|$)`, 'i');
const PROCESS_PROBE = new RegExp(String.raw`${BOUNDARY}(?:Get-Process|Get-Job|Receive-Job|task_output|tasklist|jobs|ps)(?:\.exe)?\b|${BOUNDARY}kill\s+-0\b`, 'i');
const EXIT_PROBE = /\$[\w:]+\.(?:HasExited|ExitCode)\b/i;
const HARNESS_PROBE = new RegExp(String.raw`${BOUNDARY}(?:npm(?:\.cmd)?\s+run\s+harness\s+--\s+status\b|node\s+\S*harness\.mjs\s+status\b)`, 'i');
const OUTPUT_PROBE = new RegExp(String.raw`${BOUNDARY}(?:Get-Content|Test-Path|cat|tail|head|test)\s+[^;\n|&}]*?(?:\.harness[\\/]|[^\s]+\.(?:log|out)(?=\s|[)}]|$))`, 'i');
const BLOCKING_PIPELINE = /\|\s*(?:Wait-Process|Wait-Job)\b/i;

function decide(command) {
  // A process/job piped into a completion wait is blocking, not a status poll.
  // Timed waits can return while work is running, so keep those in the analysis.
  const text = commandText(command).split(/([;\n{}])/).map(segment =>
    BLOCKING_PIPELINE.test(segment) && !/\s-Timeout\b/i.test(segment) ? '' : segment,
  ).join('');
  if (!SLEEP.test(text) || ![PROCESS_PROBE, EXIT_PROBE, HARNESS_PROBE, OUTPUT_PROBE].some(probe => probe.test(text))) return null;
  return {
    hookEventName: 'PreToolUse',
    permissionDecision: 'deny',
    permissionDecisionReason:
      'Sleep-and-poll waiting burns agent turns (#2620). For verification, block with ' +
      '`npm run harness -- wait` (optionally `--max-seconds 540`). For other background ' +
      'work, block on the task until it completes and read its output once; use the ' +
      'task tool\'s blocking completion/output option, shell `wait`, or PowerShell ' +
      '`Wait-Process` / `Wait-Job`. A bounded delay without a process/status/output poll is allowed.',
  };
}

function main() {
  let payload;
  try {
    payload = JSON.parse(readFileSync(0, 'utf8'));
  } catch {
    return;
  }
  if (payload?.tool_name !== 'Bash' && payload?.tool_name !== 'PowerShell') return;
  const command = payload?.tool_input?.command;
  if (typeof command !== 'string') return;
  const verdict = decide(command);
  if (verdict) process.stdout.write(JSON.stringify({ hookSpecificOutput: verdict }));
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) main();
