import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  circuitCommandNames,
  circuitCommandOwnershipViolations,
  circuitCommandReferences,
} from '../../scripts/check-circuit-command-ownership.mjs';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');

test('the guard reads the command implementations, not the DTOs', () => {
  const source = [
    '/// A wire shape, not a command.',
    'pub struct CircuitWithRuns { pub circuit: u8 }',
    '#[command]',
    'pub fn list_circuits(mesh_id: i64) -> u8 { mesh_id }',
    '#[command(async)]',
    'pub async fn record_circuit_outcome(app: u8) -> u8 { app }',
    '#[command]',
    'pub(crate) fn helper() {}',
  ].join('\n');
  const names = circuitCommandNames(source);
  assert.deepEqual([...names].sort(), ['helper', 'list_circuits', 'record_circuit_outcome']);
  assert.ok(!names.has('CircuitWithRuns'), 'a DTO declaration is not a command implementation');
});

test('the guard sees a call through a path, an alias or a braced import', () => {
  const names = new Set(['list_circuits']);
  for (const source of [
    'crate::commands::circuit::list_circuits(mesh_id);',
    'commands::circuit::list_circuits(mesh_id);',
    'use crate::commands::circuit as circuit_commands;\ncircuit_commands::list_circuits(mesh_id);',
    'use crate::commands::circuit::list_circuits;',
    'use crate::commands::circuit::{list_circuits};',
    'use crate::commands::circuit;\ncircuit::list_circuits(mesh_id);',
  ]) {
    const referenced = circuitCommandReferences(source).map((reference) => reference.name);
    assert.ok(
      referenced.includes('list_circuits'),
      `the guard must see the call in: ${source}`,
    );
    assert.ok(referenced.some((name) => names.has(name)));
  }
});

test('the guard permits the DTO declarations and ignores prose', () => {
  const source = [
    'use crate::commands::circuit::{CircuitAgentOwnership, CircuitWithRuns};',
    '// crate::commands::circuit::list_circuits(mesh_id) was the old path.',
    '/// See `commands::circuit::list_circuits` for the desktop route.',
    'pub fn use_dto(ownership: CircuitAgentOwnership) -> CircuitWithRuns { todo!() }',
  ].join('\n');
  const referenced = circuitCommandReferences(source).map((reference) => reference.name);
  assert.ok(referenced.includes('CircuitAgentOwnership'));
  assert.ok(referenced.includes('CircuitWithRuns'));
  assert.ok(!referenced.includes('list_circuits'), 'a comment is not a call');
});

test('no route or workflow module calls a Circuit command implementation', () => {
  const violations = circuitCommandOwnershipViolations(repoRoot);
  assert.deepEqual(violations, [], violations.join('\n'));
});

test('the guarded command module still declares the commands the guard protects', () => {
  const source = readFileSync(
    path.join(repoRoot, 'src-tauri/src/commands/circuit.rs'),
    'utf8',
  );
  const names = circuitCommandNames(source);
  for (const expected of [
    'list_circuits',
    'list_circuits_with_runs',
    'record_circuit_outcome',
    'recover_failed_circuit_run',
    'cancel_circuit_run',
    'cancel_circuit_runs',
    'delete_circuit',
    'approve_circuit_step',
    'pause_circuit_run',
    'resume_circuit_run',
    'continue_circuit_review',
    'set_circuit_review_round_limit',
    'trigger_circuit_from_node',
  ]) {
    assert.ok(names.has(expected), `commands/circuit.rs no longer declares ${expected}`);
  }
});
