#!/usr/bin/env node
// One bounded GraphQL request through the existing gh login; no cached state.
import { execFileSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { stripVTControlCharacters } from 'node:util';

const USAGE = 'Usage: npm run gh -- <pr|issue> <positive-number>';
const COMMON = `
  number title state url
  labels(first: 20) { totalCount nodes { name } }
  comments(last: 3) { totalCount nodes { author { login } body } }
`;
const PR = `
  headRefName isDraft reviewDecision
  reviews(last: 2) { totalCount nodes { author { login } state body } }
  commits(last: 1) { nodes { commit {
    statusCheckRollup {
      state
      contexts(first: 100) {
        totalCount
        nodes {
          __typename
          ... on CheckRun { name status conclusion }
          ... on StatusContext { context state }
        }
      }
    }
  } } }
`;

// Keep discussion and user-controlled fields on one terminal line. An ellipsis
// always signals shortened text rather than silently presenting it as complete.
function compact(value, limit = 100) {
  const text = stripVTControlCharacters(String(value ?? ''))
    .replace(/[\s\p{Cc}\p{Cf}]+/gu, ' ').trim();
  return text.length > limit ? `${text.slice(0, limit - 3)}...` : text;
}

function discussion(label, connection, review = false) {
  const nodes = connection.nodes;
  return [
    `${label}: ${nodes.length}/${connection.totalCount} (latest)`,
    ...nodes.map(node => compact(
      `  @${node.author?.login ?? 'deleted'}${review ? ` ${node.state}` : ''}: ${node.body || '(no body)'}`,
      120,
    )),
  ];
}

function checkLines(pr) {
  const rollup = pr.commits.nodes[0]?.commit.statusCheckRollup;
  if (!rollup) return ['checks: NONE (no status reported)'];
  const { nodes, totalCount } = rollup.contexts;
  const checks = nodes.map(node => ({
    name: node.__typename === 'CheckRun' ? node.name : node.context,
    state: node.__typename === 'CheckRun'
      ? (node.status === 'COMPLETED' ? node.conclusion || 'UNKNOWN' : node.status)
      : node.state,
  }));
  const counts = new Map();
  for (const { state } of checks) counts.set(state, (counts.get(state) ?? 0) + 1);
  const detail = checks.filter(check => !['SUCCESS', 'NEUTRAL', 'SKIPPED'].includes(check.state));
  return [
    `checks: ${rollup.state}; ${nodes.length}/${totalCount} sampled`,
    compact(`conclusions: ${[...counts].map(([state, count]) => `${state}=${count}`).join(', ')}`, 120),
    ...detail.slice(0, 4).map(check => compact(`  ${check.state}: ${check.name}`, 120)),
    ...(detail.length > 4 ? [`  ... ${detail.length - 4} more non-success checks in sample`] : []),
  ];
}

function render(kind, item) {
  const labels = item.labels.nodes.map(node => node.name).join(', ') || '(none)';
  return [
    compact(`${kind} #${item.number}: ${item.title}`, 120),
    compact(item.url, 120),
    `state: ${item.state}${kind === 'pr' ? `; draft: ${item.isDraft}; review: ${item.reviewDecision ?? 'NONE'}` : ''}`,
    ...(kind === 'pr' ? [`head: ${compact(item.headRefName)}`] : [`body: ${compact(item.body)}`]),
    compact(`labels: ${item.labels.nodes.length}/${item.labels.totalCount} ${labels}`, 120),
    ...(kind === 'pr' ? checkLines(item) : []),
    ...discussion('comments', item.comments),
    ...(kind === 'pr' ? discussion('reviews', item.reviews, true) : []),
  ].join('\n') + '\n';
}

// Inject only the process boundary so offline tests exercise the actual request,
// response handling and terminal output together.
export function run(args, { exec = execFileSync, stdout = process.stdout, stderr = process.stderr } = {}) {
  if (args.length === 1 && ['--help', '-h'].includes(args[0])) {
    stdout.write(`${USAGE}\nUses origin (or GH_REPO) and the existing gh authentication.\n`);
    return 0;
  }
  const [kind, number] = args;
  if (args.length !== 2 || !['pr', 'issue'].includes(kind)
    || !/^[1-9]\d*$/.test(number ?? '') || Number(number) > 2147483647) {
    stderr.write(`${USAGE}\n`);
    return 2;
  }
  const field = kind === 'pr' ? 'pullRequest' : 'issue';
  const query = `query($owner: String!, $name: String!, $number: Int!) {
    repository(owner: $owner, name: $name) {
      ${field}(number: $number) { ${COMMON} ${kind === 'pr' ? PR : 'body'} }
    }
  }`;
  try {
    const response = JSON.parse(exec('gh', [
      'api', 'graphql', '--hostname', 'github.com',
      '-F', 'owner={owner}', '-F', 'name={repo}', '-F', `number=${number}`,
      '-f', `query=${query}`,
    ], { encoding: 'utf8', windowsHide: true, timeout: 60_000, maxBuffer: 8 * 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'] }));
    if (response.errors?.length) throw new Error(response.errors.map(error => error.message).join('; '));
    const item = response.data?.repository?.[field];
    if (!item) throw new Error(`${kind} #${number} was not found or is inaccessible`);
    stdout.write(render(kind, item));
    return 0;
  } catch (error) {
    const reason = error.code === 'ENOENT' ? 'gh is not installed or is not on PATH'
      : error.stderr?.toString().trim() || error.message;
    stderr.write(`gh snapshot failed: ${compact(reason, 400)}\nCheck gh authentication and origin (or GH_REPO); retry with gh directly for details.\n`);
    return 1;
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = run(process.argv.slice(2));
}
