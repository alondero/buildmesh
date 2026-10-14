import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  BRIDGE_ENABLE_ENV,
  BRIDGE_ORIGINS_ENV,
  BRIDGE_TOKEN_ENV,
  MIN_TOKEN_LENGTH,
  PROFILE_DIR_ENV,
  bridgeHeaders,
  bridgeToken,
} from '../../scripts/test-bridge-auth.mjs';

const rust = readFileSync(
  path.resolve(__dirname, '..', '..', 'src-tauri', 'src', 'commands', 'test.rs'),
  'utf8',
);
const startup = readFileSync(
  path.resolve(__dirname, '..', '..', 'src-tauri', 'src', 'startup', 'mod.rs'),
  'utf8',
);

function rustConst(name: string): string {
  const match = rust.match(new RegExp(`const ${name}: &str = "([^"]+)";`));
  expect(match, name).not.toBeNull();
  return match![1];
}

const TOKEN = 'a'.repeat(MIN_TOKEN_LENGTH);

describe('test bridge auth helper', () => {
  it('uses the same env names and minimum length as the Rust bridge', () => {
    expect(BRIDGE_ENABLE_ENV).toBe(rustConst('BRIDGE_ENABLE_ENV'));
    expect(BRIDGE_TOKEN_ENV).toBe(rustConst('BRIDGE_TOKEN_ENV'));
    expect(BRIDGE_ORIGINS_ENV).toBe(rustConst('BRIDGE_ORIGINS_ENV'));
    const length = rust.match(/const MIN_BRIDGE_TOKEN_LEN: usize = (\d+);/);
    expect(length).not.toBeNull();
    expect(MIN_TOKEN_LENGTH).toBe(Number(length![1]));
    const profile = startup.match(/const PROFILE_DIR_ENV: &str = "([^"]+)";/);
    expect(profile, 'PROFILE_DIR_ENV').not.toBeNull();
    expect(PROFILE_DIR_ENV).toBe(profile![1]);
  });

  it('rejects a missing, short, or whitespace token', () => {
    expect(() => bridgeToken({}, null)).toThrow(/BUILDMESH_TEST_BRIDGE=1/);
    expect(() => bridgeToken({}, null)).toThrow(/release entry points do not compile/);
    expect(() => bridgeToken({ [BRIDGE_TOKEN_ENV]: 'short' }, null)).toThrow(/shorter than 32/);
    expect(() => bridgeToken({ [BRIDGE_TOKEN_ENV]: `${TOKEN.slice(0, -1)} ` }, null)).toThrow(/whitespace/);
  });

  it('prefers the environment and can ignore the token file', () => {
    const dir = mkdtempSync(path.join(tmpdir(), 'bridge-token-'));
    const file = path.join(dir, 'token');
    writeFileSync(file, 'b'.repeat(MIN_TOKEN_LENGTH));
    expect(bridgeToken({ [BRIDGE_TOKEN_ENV]: TOKEN }, file)).toBe(TOKEN);
    expect(() => bridgeToken({}, null)).toThrow();
    expect(bridgeToken({}, file)).toBe('b'.repeat(MIN_TOKEN_LENGTH));
  });

  it('puts Authorization last so extra headers cannot replace it', () => {
    const headers = bridgeHeaders(
      { Authorization: 'Bearer nope', 'Content-Type': 'application/json' },
      { [BRIDGE_TOKEN_ENV]: TOKEN },
      null,
    );
    expect(headers.Authorization).toBe(`Bearer ${TOKEN}`);
    expect(headers['Content-Type']).toBe('application/json');
    expect(Object.keys(headers).at(-1)).toBe('Authorization');
  });
});
