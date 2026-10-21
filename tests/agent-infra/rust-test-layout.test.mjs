import { test } from 'node:test';
import assert from 'node:assert/strict';
import { rustTestLayoutViolations } from '../../scripts/check-rust-test-layout.mjs';

const check = (source, lines = null, file = 'src-tauri/src/owner.rs') =>
  rustTestLayoutViolations(file, source, lines ?? new Set(source.split('\n').map((_, i) => i + 1)));

test('new inline suites, tests and test-only fixtures belong in sibling files', () => {
  for (const source of [
    '#[cfg(test)]\nmod tests { #[test] fn example() {} }',
    '#[test] fn example() {}',
    '#[tokio::test(flavor = "current_thread")] async fn example() {}',
    '#[cfg(test)]\nuse std::cell::Cell;',
    '#[cfg(test)]\nstruct FakeHost { value: usize }',
    '#[cfg(test)]\nimpl FakeHost { fn new() -> Self { todo!() } }',
    '#[cfg(test)] const VALUE: &str = "test";',
    '#[cfg(all(test, unix))] fn unix_fixture() {}',
    '#[cfg(all(feature = "fixture", test))] fn feature_fixture() {}',
    '#[cfg(any(all(test, unix), all(windows, test)))] fn platform_fixture() {}',
  ]) assert.match(check(source).join('\n'), /rust-test-layout/, source);
});

test('sibling declarations, test files and mixed production configurations remain allowed', () => {
  for (const source of [
    '#[cfg(test)]\n#[path = "owner_tests.rs"]\nmod tests;',
    '#[path = "owner_tests.rs"]\n#[cfg(test)]\nmod tests;',
    '#[cfg(test)] mod tests;',
    '#[cfg(any(test, debug_assertions))] fn dev_tool() {}',
    '#[cfg(all(not(test), unix))] fn production_tool() {}',
  ]) assert.deepEqual(check(source), [], source);
  for (const file of ['src-tauri/src/owner_tests.rs', 'src-tauri/src/db/tests.rs', 'src-tauri/src/db/tests/helpers.rs', 'src-tauri/tests/integration.rs', 'tools/fixture/src/lib.rs']) {
    assert.deepEqual(check('#[test] fn example() {}', null, file), [], file);
  }
});

test('comments and Rust literals cannot create test attributes or hide a real item', () => {
  const source = [
    '// #[cfg(test)] struct Fake;',
    '/* nested /* #[test] */ #[cfg(test)] */',
    'const RAW: &str = r##"#[cfg(test)] mod tests { }"##;',
    'const TEXT: &str = "escaped \\" #[test] fn false_positive() {}";',
    "fn borrow<'a>(value: &'a str) { let brace = '}'; }",
    '#[cfg(test)]',
    'fn fixture() { let text = r#" } ; #[test] "#; }',
  ].join('\n');
  assert.deepEqual(check(source, new Set([1, 2, 3, 4, 5])), []);
  assert.equal(check(source, new Set([7])).length, 1);
});

test('legacy items are exempt only when untouched; a reasoned local exception covers its item', () => {
  const source = '#[cfg(test)]\nmod tests {\n    fn legacy() {}\n    fn added() {}\n}\nfn production() {}';
  assert.deepEqual(check(source, new Set([6])), []);
  assert.match(check(source, new Set([4])).join('\n'), /:4:/);
  assert.deepEqual(check('#[cfg(test)] // allow-test-in-production: shared by the sibling suites\nstruct Shared;'), []);
  assert.deepEqual(check('#[cfg(test)]\nstruct Shared; // allow-test-in-production: shared by sibling suites'), []);
  assert.equal(check('// allow-test-in-production: wrong item\n#[cfg(test)]\nstruct Shared;').length, 1);
  assert.equal(check('#[cfg(test)] // allow-test-in-production:\nstruct Shared;').length, 1);
  assert.equal(check('#[cfg(test)]\nconst TEXT: &str = "// allow-test-in-production: fixture data";').length, 1);
});

test('an array type semicolon does not end a fixture before its changed value', () => {
  const source = '#[cfg(test)]\nconst BYTES: [u8; 2] = [\n    1, 2,\n];\nfn production() {}';
  assert.equal(check(source, new Set([3])).length, 1);
  assert.deepEqual(check(source, new Set([5])), []);
});

test('visible and file-named sibling module declarations are allowed, but inline bodies are rejected', () => {
  for (const declaration of [
    'pub(super) mod tests',
    'pub(crate) mod tests',
    'pub(in crate::services) mod tests',
    'pub mod tests',
    'mod prune_tests',
    'pub(super) mod command_tests',
  ]) {
    const source = `#[cfg(test)]\n#[path = "owner_tests.rs"]\n${declaration}`;
    assert.deepEqual(check(`${source};\n`), [], declaration);
    assert.match(check(`${source} { fn fixture() {} }\n`).join('\n'), /rust-test-layout/, declaration);
  }
});

test('Windows separators preserve production enforcement and sibling test-file exclusions', () => {
  const source = '#[cfg(test)]\nstruct FakeHost;\n';
  const windows = 'src-tauri\\src\\services\\circuit_worker\\dispatch.rs';
  const unix = windows.replaceAll('\\', '/');
  const expected = check(source, null, unix);
  assert.equal(expected.length, 1);
  assert.deepEqual(check(source, null, windows), expected);
  for (const file of [
    'src-tauri\\src\\services\\circuit_worker\\dispatch_tests.rs',
    'src-tauri\\src\\db\\tests.rs',
    'src-tauri\\src\\db\\tests\\helpers.rs',
    'src-tauri\\tests\\integration.rs',
  ]) assert.deepEqual(check(source, null, file), [], file);
});
