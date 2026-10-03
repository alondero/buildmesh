import { test } from 'node:test';
import assert from 'node:assert/strict';
import { executedAndroidTests } from '../../scripts/check-android.mjs';
import { liveOptions, liveResult } from '../../scripts/check-android-live.mjs';
import { executedTests, planGates } from '../../scripts/harness-plan.mjs';
import { classifyPaths } from '../../scripts/ci/changed-scope.mjs';

test('native source and its runner select the Android gate without changing Rust', () => {
  for (const path of ['android/app/src/main/java/App.kt', 'scripts/check-android.mjs', 'scripts/check-android-live.mjs', '.github/workflows/android.yml']) {
    const gates = planGates([path]);
    assert.ok(gates.some(gate => gate.id === 'android' && gate.tests === 'android'));
    assert.ok(!gates.some(gate => gate.id === 'rust-tests'));
    assert.deepEqual(classifyPaths([path]), { rust: false, frontend: true });
  }
  assert.ok(planGates(['src/types/generated/Node.ts']).some(gate => gate.id === 'rust-tests'));
  assert.ok(!planGates(['docs/development/android.md']).some(gate => gate.id === 'android'));
});

test('live acceptance rejects insecure and ambiguous fixture destinations', () => {
  assert.deepEqual(liveOptions(['--device', 'phone-1', '--origin', 'https://192.168.1.10:2992']), { device: 'phone-1', origin: 'https://192.168.1.10:2992', bridge: 'http://127.0.0.1:2991', http: 'http://127.0.0.1:2992' });
  for (const args of [[], ['--device', 'phone'], ['--device', 'phone', '--origin', 'http://192.168.1.10:2992'], ['--device', 'phone', '--origin', 'https://user:secret@localhost'], ['--device', 'phone', '--origin', 'https://localhost/#pair=secret'], ['--device', 'phone', '--origin', 'https://localhost', '--http', 'http://192.168.1.10:2992'], ['--device', 'phone;cmd', '--origin', 'https://localhost']]) {
    assert.throws(() => liveOptions(args));
  }
});

test('live acceptance requires an executed test and reports only a confirmed cleanup device', () => {
  assert.deepEqual(liveResult('INSTRUMENTATION_STATUS: buildmeshDeviceId=27\nOK (1 test)'), { deviceId: 27, passed: true });
  assert.deepEqual(liveResult('BUILD SUCCESSFUL'), { deviceId: null, passed: false });
  assert.equal(liveResult('FAILURES!!!\nOK (1 test)').passed, false);
  assert.equal(liveResult('OK (0 tests)').passed, false);
});

test('native gate needs executed, unskipped passing XML rather than compilation alone', () => {
  const passed = '<testsuite tests="8" skipped="0" failures="0" errors="0">';
  assert.equal(executedAndroidTests([passed, '<testsuite tests="2" skipped="0" failures="0" errors="0">']), 10);
  assert.equal(executedTests('android', 'Android tests: 10 passed\n'), 10);
  assert.equal(executedTests('android', 'BUILD SUCCESSFUL'), 0);
  for (const reports of [[], ['no suite'], ['<testsuite tests="0">'], ['<testsuite tests="8" skipped="1">'], ['<testsuite tests="8" failures="1">'], ['<testsuite tests="8" errors="1">']]) {
    assert.throws(() => executedAndroidTests(reports));
  }
});
