// Shared credential for the development test bridge.
//
// The bridge is compiled out of production builds. A debug or dev-tools
// build listens only when BUILDMESH_TEST_BRIDGE=1 and
// BUILDMESH_TEST_BRIDGE_TOKEN is set in that process. Launchers that are a
// different process from the app share the token through .tmp/test-bridge-token.
// The app itself reads the environment only, never the file.

import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

export const BRIDGE_ENABLE_ENV = 'BUILDMESH_TEST_BRIDGE';
export const BRIDGE_TOKEN_ENV = 'BUILDMESH_TEST_BRIDGE_TOKEN';
export const BRIDGE_ORIGINS_ENV = 'BUILDMESH_TEST_BRIDGE_ORIGINS';
export const MIN_TOKEN_LENGTH = 32;
export const DEFAULT_TOKEN_FILE = path.join(root, '.tmp', 'test-bridge-token');

function readTokenFile(tokenFile) {
  try {
    return readFileSync(tokenFile, 'utf8');
  } catch {
    return '';
  }
}

function tokenError() {
  return new Error(
    `${BRIDGE_TOKEN_ENV} is missing, shorter than ${MIN_TOKEN_LENGTH} characters, or contains whitespace. ` +
      `Start the app with ${BRIDGE_ENABLE_ENV}=1 and a per-launch token ` +
      '(scripts/run-dev.ps1 -TestBridge or scripts/run-dev.sh --test-bridge). ' +
      'A production build does not expose the test bridge.',
  );
}

/**
 * Resolve the bridge token. `tokenFile === null` skips the file, which is
 * what an in-process e2e launcher needs so a previous dev launch cannot
 * authenticate against a child that has a different token.
 */
export function bridgeToken(env = process.env, tokenFile = DEFAULT_TOKEN_FILE) {
  const fromEnv = env?.[BRIDGE_TOKEN_ENV] ?? '';
  const token = fromEnv.length > 0 ? fromEnv : (tokenFile ? readTokenFile(tokenFile) : '');
  if (token.length < MIN_TOKEN_LENGTH || /\s/.test(token)) throw tokenError();
  return token;
}

/** Authorization is applied last so `extra` cannot replace or duplicate the bearer. */
export function bridgeHeaders(extra = {}, env = process.env, tokenFile = DEFAULT_TOKEN_FILE) {
  const headers = {};
  for (const [key, value] of Object.entries(extra)) {
    if (key.toLowerCase() === 'authorization') continue;
    headers[key] = value;
  }
  headers.Authorization = `Bearer ${bridgeToken(env, tokenFile)}`;
  return headers;
}
