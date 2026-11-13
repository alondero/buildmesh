#!/usr/bin/env node
// Gate the Circuit command layer's ownership (issue #2673).
//
// The desktop commands in `src-tauri/src/commands/circuit.rs` and the mobile
// routes in `src-tauri/src/http/routes/circuit.rs` are two adapters over one
// set of operations, and those operations live in
// `src-tauri/src/services/circuit_control/`. A route or a workflow module that
// calls a Circuit *command implementation* has stepped around that owner, and
// the step is invisible: the call compiles, and the operation it skipped (the
// worker wake, the evaluator registration, the update the windows need) is
// simply missing.
//
// The guard is narrow on purpose:
//
//   - It forbids **calls** to the `#[command]` functions of that one module.
//   - It **permits** the module's DTO declarations, so the wire shapes can
//     stay where ADR 0047 put them (`use crate::commands::circuit::…` on a
//     `struct` is fine).
//   - It says nothing about other command modules. The companion CLI calls
//     PR commands, and that is a different question.
//
// Usage:
//   node scripts/check-circuit-command-ownership.mjs
//
// Exit codes: 0 no route or workflow module calls a Circuit command
// implementation; 1 it does.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

/** The module whose command implementations the guard protects. */
const COMMAND_MODULE_PATH = 'src-tauri/src/commands/circuit.rs';

/**
 * Directories whose modules must reach the control module rather than the
 * command layer: the HTTP routes, the service (workflow) modules, and the
 * companion CLI's tool workflows.
 */
export const GUARDED_DIRS = [
  'src-tauri/src/http/routes',
  'src-tauri/src/services',
  'src-tauri/src/companion',
];

/** Replace comments with spaces so a doc comment does not read as a call. */
export function stripComments(source) {
  let out = '';
  let i = 0;
  while (i < source.length) {
    if (source.startsWith('//', i)) {
      while (i < source.length && source[i] !== '\n') {
        out += ' ';
        i += 1;
      }
      continue;
    }
    if (source.startsWith('/*', i)) {
      out += '  ';
      i += 2;
      let depth = 1;
      while (i < source.length && depth > 0) {
        if (source.startsWith('/*', i)) {
          depth += 1;
          out += '  ';
          i += 2;
        } else if (source.startsWith('*/', i)) {
          depth -= 1;
          out += '  ';
          i += 2;
        } else {
          out += source[i] === '\n' ? '\n' : ' ';
          i += 1;
        }
      }
      continue;
    }
    out += source[i];
    i += 1;
  }
  return out;
}

/**
 * The names of the `#[command]` functions a module declares. These are the
 * implementations the guard protects; the module's DTOs are not commands and
 * are not listed.
 */
export function circuitCommandNames(source) {
  const stripped = stripComments(source);
  const names = new Set();
  const attribute = /#\s*\[(?:tauri::)?command(?:\([^)]*\))?\]/g;
  const after = /(?:pub(?:\s*\(\s*crate\s*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)/;
  let match;
  while ((match = attribute.exec(stripped)) !== null) {
    const fn = after.exec(stripped.slice(match.index + match[0].length));
    if (fn) names.add(fn[1]);
    attribute.lastIndex = match.index + match[0].length;
  }
  return names;
}

/**
 * The item names a source file reaches through the Circuit command module,
 * whether by path, by alias, or by a braced import.
 */
export function circuitCommandReferences(source) {
  const stripped = stripComments(source);
  const references = [];

  const direct = /(?<![:\w])(?:crate::)?commands::circuit::\s*([A-Za-z_][A-Za-z0-9_]*)/g;
  let match;
  while ((match = direct.exec(stripped)) !== null) {
    references.push({ name: match[1], index: match.index });
  }

  const braced = /use\s+[^;{]*?commands::circuit::\s*\{([^}]*)\}\s*;/g;
  while ((match = braced.exec(stripped)) !== null) {
    for (const raw of match[1].split(',')) {
      const name = raw.trim().split(/\s+as\s+/)[0].trim();
      if (name) references.push({ name, index: match.index });
    }
  }

  // `use crate::commands::circuit;` and `use crate::commands::circuit as x;`
  // both bind a local name for the module; calls through it are the same
  // dependency.
  const bound = /use\s+(?:crate::)?commands::circuit\s*(?:as\s+([A-Za-z_][A-Za-z0-9_]*))?\s*;/g;
  const aliases = new Set();
  while ((match = bound.exec(stripped)) !== null) {
    aliases.add(match[1] ?? 'circuit');
  }
  for (const alias of aliases) {
    const call = new RegExp(`(?<![:\\w])${alias}::\\s*([A-Za-z_][A-Za-z0-9_]*)`, 'g');
    while ((match = call.exec(stripped)) !== null) {
      references.push({ name: match[1], index: match.index });
    }
  }

  return references;
}

function listRustFiles(dir) {
  const out = [];
  if (!fs.existsSync(dir)) return out;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...listRustFiles(full));
    else if (entry.name.endsWith('.rs')) out.push(full);
  }
  return out;
}

/**
 * The violations in the repository: a guarded module that calls a Circuit
 * command implementation.
 */
export function circuitCommandOwnershipViolations(root = repoRoot) {
  const commandPath = path.join(root, COMMAND_MODULE_PATH);
  if (!fs.existsSync(commandPath)) {
    return [`${COMMAND_MODULE_PATH}: the guarded command module is missing; the guard cannot prove anything`];
  }
  const commandNames = circuitCommandNames(fs.readFileSync(commandPath, 'utf8'));
  if (commandNames.size === 0) {
    return [`${COMMAND_MODULE_PATH}: no #[command] functions found; the guard would pass anything`];
  }

  const violations = [];
  for (const dir of GUARDED_DIRS) {
    for (const file of listRustFiles(path.join(root, dir))) {
      const source = fs.readFileSync(file, 'utf8');
      for (const reference of circuitCommandReferences(source)) {
        if (!commandNames.has(reference.name)) continue;
        const line = source.slice(0, reference.index).split('\n').length;
        const rel = path.relative(root, file).replaceAll('\\', '/');
        violations.push(
          `${rel}:${line}: ${reference.name} is a Circuit command implementation. ` +
            'Call the operation in src-tauri/src/services/circuit_control/ instead; ' +
            'the DTO declarations in commands/circuit.rs may still be imported.',
        );
      }
    }
  }
  return violations;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const violations = circuitCommandOwnershipViolations();
  if (violations.length > 0) {
    console.error('Circuit command ownership (issue #2673):');
    for (const violation of violations) console.error(`  ${violation}`);
    console.error('');
    console.error('A route or workflow module must reach the Circuit operations through');
    console.error('src-tauri/src/services/circuit_control/ — not through the command layer.');
    process.exit(1);
  }
  console.log('Circuit command ownership: no route or workflow module calls a Circuit command implementation.');
}
