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
/** Absolute profile directory. Read only by debug and dev-tools builds. */
export const PROFILE_DIR_ENV = 'BUILDMESH_PROFILE_DIR';
export const MIN_TOKEN_LENGTH = 32;
export const DEFAULT_TOKEN_FILE = path.join(root, '.tmp', 'test-bridge-token');
export const DEFAULT_INSTANCE_FILE = path.join(root, '.tmp', 'dev-instance.json');

/** Resolve the port belonging to the launch that wrote this worktree's token. */
export function bridgePort(explicitPort, instanceFile = DEFAULT_INSTANCE_FILE) {
  let port = explicitPort;
  if (port === undefined) {
    let record;
    try {
      record = JSON.parse(readFileSync(instanceFile, 'utf8'));
    } catch {
      throw new Error(`Cannot read dev instance record ${instanceFile}. Launch this worktree with -TestBridge or supply --invoke-port.`);
    }
    // The main-checkout launch records null when the bundle's default applies.
    const offset = record?.portOffset === null ? 1000 : record?.portOffset;
    if (!Number.isInteger(offset)) throw new Error(`Invalid port offset in dev instance record ${instanceFile}. Relaunch this worktree.`);
    port = 1991 + offset;
  }
  if ((typeof port !== 'number' && typeof port !== 'string') || String(port).trim() === '' || !Number.isInteger(Number(port)) || Number(port) < 1 || Number(port) > 65535) {
    throw new Error('Test bridge port must be an integer from 1 to 65535.');
  }
  return Number(port);
}

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
      'The release entry points do not compile the test bridge in.',
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
