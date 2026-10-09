import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

/**
 * The Linux Rust jobs share one Cargo cache entry so `rust-build`'s single
 * compile is reused by the seven shards and the non-shard pass instead of being
 * paid nine times (issue #2046).
 *
 * `shared-key: linux-rust-target` is necessary but not sufficient, and the gap
 * between those two words is a silent failure rather than a loud one. rust-cache
 * also folds a hash of every environment variable matching `CARGO`, `CC`,
 * `CFLAGS`, `CXX`, `CMAKE`, or `RUST` into the cache key — see its `config.ts`,
 * which walks `process.env` and hashes every match. So two jobs share an entry
 * only when `shared-key` matches *and* those variables match.
 *
 * `rust-build` set `CARGO_BUILD_JOBS: "4"` and the shards did not. That is not a
 * cargo fingerprint input, so nothing failed and the job still reported a cache
 * hit — it simply saved `...-65c564bd-47ff76ef` while all eight downstream jobs
 * read `...-ecb08f1d-47ff76ef` (run 38000250072). The compile nobody read, and
 * every shard recompiled the crate anyway: ~59s each against a ~39s compile.
 *
 * A test is the only thing that catches this. The divergence produces no error,
 * no annotation, and no red job; it just quietly spends minutes per pull
 * request, which is exactly the class of regression that survives review because
 * the workflow still looks correct.
 */
const workflow = fs.readFileSync(path.join(process.cwd(), '.github', 'workflows', 'verify.yml'), 'utf8');

/** The body of one top-level `job:` block, so one job's env is not read as another's. */
function jobBlock(jobId) {
  const marker = `\n  ${jobId}:\n`;
  const start = workflow.indexOf(marker);
  assert.notEqual(start, -1, `verify.yml has no \`${jobId}\` job`);
  const rest = workflow.slice(start + marker.length);
  const end = rest.search(/\n {2}[a-z][\w-]*:\n/);
  return end === -1 ? workflow.slice(start) : workflow.slice(start, start + marker.length + end);
}

/** The `KEY: "value"` pairs in a job's job-level `env:` block (indent 6). */
function envVars(jobId) {
  const block = jobBlock(jobId);
  const envStart = block.search(/^ {4}env:$/m);
  assert.notEqual(envStart, -1, `the \`${jobId}\` job has no job-level env block`);
  // Start *after* the `env:` line itself: searching from there would let the
  // terminator below match the four spaces of `env:` at offset 0 and yield an
  // empty body, silently reporting every job as declaring no variables.
  const bodyStart = block.indexOf('\n', envStart) + 1;
  const rest = block.slice(bodyStart);
  const end = rest.search(/^ {4}\S/m);
  const body = end === -1 ? rest : rest.slice(0, end);
  const found = new Map();
  for (const line of body.split('\n')) {
    // Indent 6 is the level of a variable inside `env:`; anything deeper belongs
    // to a step's own `env:`, which does not affect this job's cache key.
    const match = line.match(/^ {6}([A-Z_][A-Z0-9_]*):\s*(.*)$/);
    if (match) found.set(match[1], match[2].trim().replace(/^"(.*)"$/, '$1'));
  }
  return found;
}

/** Does rust-cache hash this variable name into its key? */
function rustCacheHashes(name) {
  return ['CARGO', 'CC', 'CFLAGS', 'CXX', 'CMAKE', 'RUST'].some((prefix) => name.startsWith(prefix));
}

const JOBS = ['rust-build', 'rust-tests', 'rust-nonshard'];

test('every Linux Rust job shares one rust-cache entry', () => {
  const keys = new Map();
  for (const job of JOBS) {
    const match = jobBlock(job).match(/shared-key:\s*(\S+)/);
    assert.ok(match, `the \`${job}\` job has no \`shared-key\`; it would save a private ~590 MB copy`);
    keys.set(job, match[1]);
  }
  assert.equal(new Set(keys.values()).size, 1, `the Rust jobs use different shared-keys: ${[...keys].map(([j, k]) => `${j}=${k}`).join(', ')}`);
});

test('the Rust jobs declare identical cache-keyed environment variables', () => {
  // The precise defect: rust-build hashed `CARGO_BUILD_JOBS` into a key the
  // shards never read. Comparing the hashed *sets* catches that, and catches a
  // future variable added to one job only, which is the same bug wearing a
  // different name.
  const hashed = new Map(JOBS.map((job) => [job, new Set([...envVars(job).keys()].filter(rustCacheHashes))]));
  const reference = [...hashed.get('rust-tests')].sort();
  for (const job of JOBS) {
    assert.deepEqual(
      [...hashed.get(job)].sort(),
      reference,
      `\`${job}\` sets cache-keyed env vars ${[...hashed.get(job)].sort().join(', ')}, but \`rust-tests\` sets ${reference.join(', ') || '(none)'}. ` +
        'rust-cache hashes every CARGO*/RUST* variable into its key, so one job setting a variable the others do not gives it a private cache entry that the others never restore.',
    );
  }
});

test('the fingerprint inputs the shared cache depends on stay identical', () => {
  // RUSTFLAGS and the profile-debug values are genuine cargo fingerprint
  // inputs: if these drift, every downstream job recompiles the crate the
  // previous job just built, whether or not the cache key matches. Asserted by
  // value, not just by name, so a flipped debug level cannot slip through.
  const reference = envVars('rust-tests');
  for (const job of JOBS) {
    for (const name of ['CARGO_PROFILE_DEV_DEBUG', 'CARGO_PROFILE_TEST_DEBUG', 'CARGO_INCREMENTAL', 'RUSTFLAGS']) {
      assert.equal(
        envVars(job).get(name),
        reference.get(name),
        `\`${job}\` sets ${name}=${envVars(job).get(name)} but \`rust-tests\` sets ${reference.get(name)}; a mismatch forces a recompile in every job downstream`,
      );
    }
  }
});

test('no Linux Rust job sets CARGO_BUILD_JOBS', () => {
  // Named explicitly because it is the one that regressed, and because it reads
  // like a harmless tuning knob: it caps build parallelism, but it is invisible
  // to cargo's fingerprint, so setting it in one job only splits the cache key
  // and silently discards that job's compile. The runner has four cores and
  // cargo already parallelises across them; if a memory cap is ever genuinely
  // needed, it belongs on every job at once or on none.
  for (const job of JOBS) {
    assert.equal(
      envVars(job).get('CARGO_BUILD_JOBS'),
      undefined,
      `\`${job}\` sets CARGO_BUILD_JOBS, which rust-cache hashes into its key. Give every Rust job the same value or none; one job alone gets a cache entry nobody reads.`,
    );
  }
});

test('the shard-coverage gate runs where the test binary is already built', () => {
  // The gate lists tests with `cargo test --lib -- --list`, which needs the
  // compiled binary. It used to sit in `rust-nonshard` ahead of the fuzz smoke
  // and paid a 56.6s compile there, which the fuzz smoke then repeated (48.8s);
  // run 38000250072 spent 105s of a 185s job to reach tests that take 0.3s. It
  // belongs immediately after `cargo test --no-run`, where the binary exists.
  const build = jobBlock('rust-build');
  const compile = build.indexOf('cargo test --locked --no-run');
  const gate = build.indexOf('scripts/check-rust-shard-coverage.mjs');
  assert.notEqual(compile, -1, 'the `rust-build` job no longer compiles the test binaries');
  assert.notEqual(gate, -1, 'the shard-coverage gate is no longer in `rust-build`');
  assert.ok(gate > compile, 'the shard-coverage gate must run after the test binaries are compiled, or it pays the build itself');
  assert.equal(
    jobBlock('rust-nonshard').includes('check-rust-shard-coverage.mjs'),
    false,
    'the shard-coverage gate is back in `rust-nonshard`, where listing the tests forces a second compile of the same binary',
  );
});

test('the shard-coverage gate is still on the merge path', () => {
  // Moving the gate must not move it off the gate. `rust-bindings` is the
  // required check and it already requires `rust-build`, so a coverage failure
  // there still turns the required check red — but only while that `needs`
  // entry stays.
  assert.match(
    jobBlock('rust-bindings').match(/needs: \[[^\]]*\]/)?.[0] ?? '',
    /rust-build/,
    '`rust-bindings` no longer requires `rust-build`, so the shard-coverage gate would run off the merge path',
  );
});