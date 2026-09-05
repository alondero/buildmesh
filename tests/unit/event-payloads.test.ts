import { describe, it, expect } from 'vitest';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, resolve } from 'node:path';

/**
 * Pattern guard — every Tauri event payload type on the TS side must come
 * from `src/types/generated/`, not a hand-rolled inline literal
 * (issue #161, mirroring `ipc-contract.test.ts`'s pattern for `invoke()`
 * literals).
 *
 * History (per memory): `provider-error`, `resume-failed`, `node-renamed`,
 * `attention-needed`/`attention-cleared`, `serialize-terminal-request`,
 * `agent-output`, `autopilot-*`, `mesh-sync-warning`, `node-created`,
 * `node-spawn-*`, `worktree-cleanup-failed`, `agent-spawned`, and the
 * per-session `build-run-{output,exited}-*` events have all shipped with
 * inline `listen<EventName, { ... }>` literal types that drifted silently
 * from the Rust side: `provider-error`'s `session_id` was emitted in Rust
 * but unread in TS; `commands/attention.rs::clear_attention_node` emitted
 * with `node_id` while every other path (and TS) used `session_id` —
 * `agentNodeStore`'s auto-clear handler never actually flipped the manual
 * clear button; `commands/test.rs::inject_test_output` emitted with
 * `node_id` while production's `agent/spawn.rs` uses `session_id`, so the
 * e2e test injection path silently never reached the listener.
 *
 * The fix is the same shape as #359's `#[derive(TS)]` codegen: every Tauri
 * event payload struct on the Rust side derives `TS` and the TS half is
 * generated under `src/types/generated/`. This test prevents the
 * regression class by:
 *
 * 1. Walking `src/` for every `listen<T>(<event-name>, ...)` call.
 * 2. Asserting `T` is NOT an inline literal `{ ... }` — if it is, the
 *    payload would drift from the Rust side on the next refactor.
 * 3. Asserting `T` either:
 *    - Resolves to an imported type from `src/types/generated/`, OR
 *    - Is a built-in primitive (`string`, `number`, `boolean`, `unknown`),
 *      used for sentinel events that carry no payload.
 *
 * The companion Rust-side guard is the existing
 * `.github/workflows/build.yml` `git diff --exit-code src/types/generated`
 * step: a Rust struct change that isn't reflected in committed bindings
 * fails the build.
 */

const REPO_ROOT = resolve(__dirname, '..', '..');
const FRONTEND_SRC = join(REPO_ROOT, 'src');

function walkTsFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    const st = statSync(full);
    if (st.isDirectory()) {
      walkTsFiles(full, out);
    } else if (/\.(ts|tsx)$/.test(entry)) {
      out.push(full);
    }
  }
  return out;
}

interface ListenCall {
  eventName: string;
  payloadType: string;
  file: string;
  line: number;
}

function extractListenCalls(files: string[]): ListenCall[] {
  // Matches: listen<T>('event', ...), listen<T>("event", ...),
  //   listen<string | T>('event', ...) — union form used by
  //   BuildRunTerminalRegistry where the backend may send either a raw
  //   string OR a typed payload.
  // Captures the full generic expression up to the closing `>` so unions
  // are preserved, then the leading string literal as the event name.
  const re =
    /\blisten\s*<\s*([\s\S]*?)>\s*\(\s*['"]([a-zA-Z][a-zA-Z0-9_-]*)['"]/g;
  const calls: ListenCall[] = [];

  for (const file of files) {
    const source = readFileSync(file, 'utf8');
    // Strip block comments and line comments for clean matching. Naive
    // but sufficient — listen() inside comments isn't a real call.
    const cleaned = source
      .replace(/\/\*[\s\S]*?\*\//g, '')
      .replace(/\/\/.*$/gm, '');

    for (const m of cleaned.matchAll(re)) {
      const payloadType = m[1].trim();
      const eventName = m[2];
      const upto = cleaned.slice(0, m.index ?? 0);
      const line = upto.split('\n').length;
      calls.push({ eventName, payloadType, file, line });
    }
  }
  return calls;
}

/** True iff `payloadType` is a hand-rolled inline object literal. */
function isInlineObjectLiteral(payloadType: string): boolean {
  // An inline literal starts with `{`. Allow leading whitespace; reject
  // anything else (including bare identifiers and primitives).
  return /^\s*\{/.test(payloadType);
}

/** True iff `payloadType` is a built-in primitive the listener is allowed
 *  to consume without going through generated/ (sentinel events, raw text,
 *  fully-untyped payloads). */
function isPrimitiveAllowed(payloadType: string): boolean {
  const trimmed = payloadType.trim();
  if (trimmed === 'unknown') return true;
  if (trimmed === 'void') return true;
  // Primitive scalars (allow `| null` / `| undefined` unions of primitives).
  if (/^(string|number|boolean|null|undefined)(\s*\|\s*(string|number|boolean|null|undefined))*$/.test(trimmed)) {
    return true;
  }
  // Union of a primitive + a generated type (the BuildRunTerminalRegistry
  // shape — `string | BuildRunOutputPayload`).
  const parts = trimmed.split(/\s*\|\s*/);
  if (parts.length > 1 && parts.every((p) => isPrimitiveAllowed(p))) return true;
  return false;
}

function fileHasGeneratedImport(filePath: string, payloadType: string): boolean {
  // Strip any leading non-identifier segments of a union — we only need to
  // confirm at least one piece is an imported generated/ type.
  const candidates = payloadType
    .split(/\s*\|\s*/)
    .map((s) => s.trim())
    .filter((s) => /^[A-Z][A-Za-z0-9_]*$/.test(s));
  if (candidates.length === 0) return true; // Nothing to check.
  const source = readFileSync(filePath, 'utf8');
  // Each `candidates[i]` must appear as either:
  //   1. An `import type { ... } from '...types/generated/X'` declaration, OR
  //   2. A re-export `export type { ... } from '...types/generated/X'`, OR
  //   3. A type-aliased import (`import type X from '...generated/X'`).
  return candidates.every((name) => {
    const importRe = new RegExp(
      String.raw`from\s+['"][^'"]*types/generated/${name}['"]`,
    );
    const namedImportRe = new RegExp(
      String.raw`import\s+type\s*\{[^}]*\b${name}\b[^}]*\}\s+from\s+['"][^'"]*types/generated/[^'"]+['"]`,
    );
    return importRe.test(source) || namedImportRe.test(source);
  });
}

describe('Event payload type guard (issue #161)', () => {
  const files = walkTsFiles(FRONTEND_SRC);
  const listenCalls = extractListenCalls(files);

  it('finds listen<T>() calls in src/', () => {
    // Sanity: this guard is meaningful only if there are listeners to
    // check. The webview has 20+ Tauri event listeners as of #161.
    expect(listenCalls.length).toBeGreaterThanOrEqual(15);
  });

  it('no listen<T>() uses an inline literal payload type', () => {
    const violators = listenCalls.filter((c) => isInlineObjectLiteral(c.payloadType));
    const report = violators.map((c) => {
      const rel = c.file.slice(REPO_ROOT.length + 1).replace(/\\/g, '/');
      return `  listen<{ ... }>('${c.eventName}') at ${rel}:${c.line}`;
    });

    if (violators.length > 0) {
      const message =
        `Found ${violators.length} listen<T>() call(s) with an inline object-literal payload type.\n` +
        `Inline literals drift silently from the Rust side — issue #161 requires every event\n` +
        `payload to be a struct in src-tauri/src/ that derives #[derive(TS)] and is generated\n` +
        `under src/types/generated/. To fix:\n` +
        `  1. Add \`#[derive(TS, Serialize)]\` + \`#[ts(export, export_to = "X.ts")]\` to the Rust payload struct.\n` +
        `  2. Run \`cargo test\` in src-tauri/ to regenerate the .ts file.\n` +
        `  3. Import the generated type on the TS side and pass it as the generic to listen<>().\n\n` +
        `Violations:\n` +
        report.join('\n');
      throw new Error(message);
    }
  });

  it('every listen<T>() payload type is imported from src/types/generated/', () => {
    const violators = listenCalls
      .filter((c) => !isPrimitiveAllowed(c.payloadType))
      .filter((c) => !fileHasGeneratedImport(c.file, c.payloadType));
    const report = violators.map((c) => {
      const rel = c.file.slice(REPO_ROOT.length + 1).replace(/\\/g, '/');
      return `  listen<${c.payloadType}>('${c.eventName}') at ${rel}:${c.line}`;
    });

    if (violators.length > 0) {
      const message =
        `Found ${violators.length} listen<T>() call(s) whose payload type is not imported from\n` +
        `src/types/generated/. Either it is a hand-rolled type (not allowed — use a generated\n` +
        `Rust-derived type instead) or the import statement is missing/mis-typed.\n\n` +
        `Violations:\n` +
        report.join('\n');
      throw new Error(message);
    }
  });
});
