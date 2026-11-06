import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { run } from '../../scripts/gh.mjs';

function item(overrides = {}) {
  return {
    number: 2599, title: 'Compact GitHub state', state: 'OPEN',
    url: 'https://github.com/alondero/buildmesh/pull/2599',
    headRefName: 'gh2623-snapshot', isDraft: false, reviewDecision: 'APPROVED',
    labels: { totalCount: 1, nodes: [{ name: 'ready-for-agent' }] },
    comments: { totalCount: 8, nodes: [{ author: { login: 'alice' }, body: 'Ship it\nwith tests' }] },
    reviews: { totalCount: 3, nodes: [{ author: null, state: 'APPROVED', body: 'Looks good' }] },
    commits: { nodes: [{ commit: { statusCheckRollup: {
      state: 'FAILURE', contexts: { totalCount: 4, nodes: [
        { __typename: 'CheckRun', name: 'unit', status: 'COMPLETED', conclusion: 'SUCCESS' },
        { __typename: 'CheckRun', name: 'lint', status: 'COMPLETED', conclusion: 'FAILURE' },
        { __typename: 'CheckRun', name: 'build', status: 'IN_PROGRESS', conclusion: null },
        { __typename: 'StatusContext', context: 'external', state: 'PENDING' },
      ] },
    } } }] },
    ...overrides,
  };
}

function invoke(args, response, failure) {
  const calls = [];
  let output = '', diagnostic = '';
  const status = run(args, {
    exec: (...call) => {
      calls.push(call);
      if (failure) throw failure;
      return typeof response === 'string' ? response : JSON.stringify(response);
    },
    stdout: { write: text => { output += text; } },
    stderr: { write: text => { diagnostic += text; } },
  });
  return { calls, status, output, diagnostic };
}

test('PR snapshot makes exactly one gh request containing state, checks, comments and reviews', () => {
  const result = invoke(['pr', '2599'], { data: { repository: { pullRequest: item() } } });
  assert.equal(result.status, 0);
  assert.equal(result.calls.length, 1);
  const [command, args, options] = result.calls[0];
  assert.equal(command, 'gh');
  assert.deepEqual(args.slice(0, 4), ['api', 'graphql', '--hostname', 'github.com']);
  assert.ok(args.includes('owner={owner}'));
  assert.ok(args.includes('name={repo}'));
  assert.ok(args.includes('number=2599'));
  const query = args.at(-1);
  for (const field of ['pullRequest', 'reviewDecision', 'headRefName', 'labels(first: 20)',
    'comments(last: 3)', 'reviews(last: 2)', 'commits(last: 1)', 'contexts(first: 100)',
    'StatusContext', 'CheckRun']) assert.ok(query.includes(field), field);
  assert.equal(options.windowsHide, true);
  assert.equal(options.timeout, 60_000);
  assert.match(result.output, /pr #2599: Compact GitHub state/);
  assert.match(result.output, /state: OPEN; draft: false; review: APPROVED/);
  assert.match(result.output, /head: gh2623-snapshot/);
  assert.match(result.output, /labels: 1\/1 ready-for-agent/);
  assert.match(result.output, /checks: FAILURE; 4\/4 sampled/);
  assert.match(result.output, /SUCCESS=1, FAILURE=1, IN_PROGRESS=1, PENDING=1/);
  assert.match(result.output, /FAILURE: lint/);
  assert.match(result.output, /PENDING: external/);
  assert.match(result.output, /comments: 1\/8 \(latest\)\n@alice: Ship it with tests/);
  assert.match(result.output, /@deleted APPROVED: Looks good/);
  assert.equal(result.diagnostic, '');
});

test('issue snapshot reads body and discussion in one request without PR-only fields', () => {
  const issue = item({ number: 2623, body: 'Reduce\nround trips', labels: { totalCount: 0, nodes: [] } });
  const result = invoke(['issue', '2623'], { data: { repository: { issue } } });
  assert.equal(result.status, 0);
  assert.equal(result.calls.length, 1);
  assert.match(result.calls[0][1].at(-1), /issue\(number: \$number\)/);
  assert.doesNotMatch(result.calls[0][1].at(-1), /pullRequest|statusCheckRollup|reviewDecision/);
  assert.match(result.output, /issue #2623/);
  assert.match(result.output, /body: Reduce round trips/);
  assert.match(result.output, /labels: 0\/0 \(none\)/);
  assert.match(result.output, /@alice: Ship it with tests/);
  assert.doesNotMatch(result.output, /checks:|reviews:|head:/);
});

test('missing status is not green and an absent review decision is explicit', () => {
  for (const commits of [{ nodes: [] }, { nodes: [{ commit: { statusCheckRollup: null } }] }]) {
    const result = invoke(['pr', '1'], { data: { repository: { pullRequest: item({ commits, reviewDecision: null }) } } });
    assert.equal(result.status, 0);
    assert.match(result.output, /checks: NONE \(no status reported\)/);
    assert.match(result.output, /review: NONE/);
  }
});

test('large snapshots stay within 24 lines of 120 characters and expose truncation', () => {
  const pr = item();
  pr.title = 'x'.repeat(1000);
  pr.headRefName = 'x'.repeat(1000);
  pr.labels = { totalCount: 21, nodes: Array.from({ length: 20 }, () => ({ name: 'x'.repeat(100) })) };
  pr.comments = { totalCount: 10, nodes: Array.from({ length: 3 }, () => ({ author: null, body: '\u001b[31mhello\u001b[0m\n' + 'x'.repeat(10000) })) };
  pr.reviews = { totalCount: 20, nodes: Array.from({ length: 2 }, () => ({ state: 'CHANGES_REQUESTED', body: 'x'.repeat(10000) })) };
  pr.commits.nodes[0].commit.statusCheckRollup.contexts = {
    totalCount: 150,
    nodes: Array.from({ length: 100 }, () => ({ __typename: 'CheckRun', name: 'x'.repeat(1000), status: 'COMPLETED', conclusion: 'FAILURE' })),
  };
  const { output, status } = invoke(['pr', '1'], { data: { repository: { pullRequest: pr } } });
  assert.equal(status, 0);
  const lines = output.trimEnd().split('\n');
  assert.ok(lines.length <= 24);
  assert.ok(lines.every(line => line.length <= 120));
  assert.match(output, /labels: 20\/21/);
  assert.match(output, /checks: FAILURE; 100\/150 sampled/);
  assert.match(output, /96 more non-success checks in sample/);
  assert.match(output, /comments: 3\/10/);
  assert.match(output, /reviews: 2\/20/);
  assert.match(output, /\.\.\./);
  assert.equal(output.includes('\u001b'), false);
});

test('missing CLI, gh failures, invalid JSON, API errors and inaccessible items fail without a success snapshot', () => {
  const cases = [
    [null, Object.assign(new Error('spawn failed'), { code: 'ENOENT' }), /not installed/],
    [null, Object.assign(new Error('exit 1'), { stderr: Buffer.from('authentication failed') }), /authentication failed/],
    [null, new Error('timed out'), /timed out/],
    ['not json', null, /gh snapshot failed/],
    [{ errors: [{ message: 'rate limited' }], data: { repository: { pullRequest: item() } } }, null, /rate limited/],
    [{ data: { repository: null } }, null, /not found or is inaccessible/],
    [{ data: { repository: { pullRequest: null } } }, null, /not found or is inaccessible/],
    [{ data: { repository: { pullRequest: { number: 1 } } } }, null, /gh snapshot failed/],
  ];
  for (const [response, failure, expected] of cases) {
    const result = invoke(['pr', '1'], response, failure);
    assert.equal(result.status, 1);
    assert.equal(result.calls.length, 1);
    assert.equal(result.output, '');
    assert.match(result.diagnostic, expected);
  }
});

test('help and invalid arguments do not launch gh', () => {
  for (const args of [[], ['pr'], ['pr', '0'], ['issue', '-1'], ['pr', '1.5'], ['pr', '2147483648'],
    ['pr', 'x'], ['pr', '1', '--extra'], ['other', '1']]) {
    const result = invoke(args);
    assert.equal(result.status, 2);
    assert.equal(result.calls.length, 0);
    assert.match(result.diagnostic, /Usage:/);
  }
  const result = invoke(['--help']);
  assert.equal(result.status, 0);
  assert.equal(result.calls.length, 0);
  assert.match(result.output, /Usage:/);
});

test('package scripts wire the command and regression into the agent suite; CLI preserves exit status', () => {
  const pkg = JSON.parse(readFileSync(new URL('../../package.json', import.meta.url), 'utf8'));
  assert.equal(pkg.scripts.gh, 'node scripts/gh.mjs');
  assert.ok(pkg.scripts['test:agent'].includes('tests/agent-infra/gh.test.mjs'));
  const script = fileURLToPath(new URL('../../scripts/gh.mjs', import.meta.url));
  const result = spawnSync(process.execPath, [script, 'pr', 'bad'], { encoding: 'utf8' });
  assert.equal(result.status, 2);
  assert.match(result.stderr, /Usage:/);
});
