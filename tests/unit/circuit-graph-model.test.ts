/**
 * Tests for the circuit canvas editor's pure model helpers (issue #1209).
 *
 * These functions are the pre-agreed test seams of the editor: the node
 * catalogue vs. the generated AST union, config summaries, fuzzy search
 * for quick-connect, `{{` Mustache insertion, traversed-path highlighting
 * from a run's step ledger, and the Dagre auto-layout. Everything here is
 * pure — no React Flow rendering, no IPC.
 */

import { describe, it, expect } from 'vitest';
import type { CircuitNodeKind } from '../../src/types/generated/CircuitNodeKind';
import {
  NODE_SPECS,
  specFor,
  defaultKind,
  makeNodeId,
  configSummary,
  parseGraph,
  fuzzyScore,
  fuzzyFilterSpecs,
  MUSTACHE_PATHS,
  insertMustache,
  sourceOutcomes,
  conditionLabel,
  edgeKey,
  traversedEdgeKeys,
  stepDurationMs,
  layoutPositions,
} from '../../src/components/Circuits/circuitGraphModel';

const ALL_DISCRIMINATORS = [
  'manual',
  'interval',
  'github_issue_label',
  'github_pull_request_label',
  'spawn_agent_node',
  'inject_pty',
  'github_action',
  'set_node_status',
  'notify',
  'llm_turn_classifier',
  'deterministic_verification',
  'collaborator_check',
  'retry_limit',
  'all_completed',
  'any_completed',
] as const;

describe('node catalogue', () => {
  it('covers every discriminator of the generated AST union', () => {
    const covered = new Set(NODE_SPECS.map((s) => s.discriminator));
    for (const d of ALL_DISCRIMINATORS) {
      expect(covered.has(d), `catalogue must cover ${d}`).toBe(true);
    }
    expect(covered.size).toBe(ALL_DISCRIMINATORS.length);
  });

  it('groups kinds into the four palette categories', () => {
    expect(specFor('manual').category).toBe('trigger');
    expect(specFor('interval').category).toBe('trigger');
    expect(specFor('github_issue_label').category).toBe('trigger');
    expect(specFor('github_pull_request_label').category).toBe('trigger');
    expect(specFor('spawn_agent_node').category).toBe('action');
    expect(specFor('inject_pty').category).toBe('action');
    expect(specFor('notify').category).toBe('action');
    expect(specFor('github_action').category).toBe('action');
    expect(specFor('set_node_status').category).toBe('action');
    expect(specFor('llm_turn_classifier').category).toBe('gate');
    expect(specFor('deterministic_verification').category).toBe('gate');
    expect(specFor('collaborator_check').category).toBe('gate');
    expect(specFor('retry_limit').category).toBe('gate');
    expect(specFor('all_completed').category).toBe('join');
    expect(specFor('any_completed').category).toBe('join');
  });

  it('builds usable defaults for every kind', () => {
    expect(defaultKind('interval')).toEqual({ type: 'interval', interval_seconds: 300 });
    expect(defaultKind('spawn_agent_node')).toEqual({
      type: 'spawn_agent_node',
      prompt: '',
      name: null,
    });
    expect(defaultKind('collaborator_check')).toEqual({
      type: 'collaborator_check',
      require_approval: true,
    });
    // Every discriminator produces a valid AST value.
    for (const s of NODE_SPECS) {
      expect(typeof defaultKind(s.discriminator as never)).toBe('object');
    }
  });

  it('mints unique node ids per discriminator', () => {
    expect(makeNodeId('notify', [])).toBe('notify_1');
    expect(makeNodeId('notify', ['notify_1'])).toBe('notify_2');
    // First free slot wins — gaps are reused.
    expect(makeNodeId('notify', ['notify_1', 'other'])).toBe('notify_2');
    expect(makeNodeId('notify', ['notify_1', 'notify_2', 'other'])).toBe('notify_3');
  });
});

describe('config summaries', () => {
  it('summarises each kind in one line', () => {
    expect(configSummary({ type: 'interval', interval_seconds: 300 })).toBe('every 300s');
    expect(configSummary({ type: 'github_issue_label', label: 'buildmesh:run' })).toBe(
      'label "buildmesh:run"'
    );
    const emptySpawn: CircuitNodeKind = {
      type: 'spawn_agent_node',
      prompt: '',
      name: null,
    };
    expect(configSummary(emptySpawn)).toBe('(no prompt)');
    expect(configSummary({ ...emptySpawn, name: 'fix-it' })).toContain('fix-it');
    expect(configSummary({ type: 'inject_pty', prompt: 'wrap up' })).toContain('wrap up');
    expect(
      configSummary({ type: 'deterministic_verification', command: 'cargo test' })
    ).toContain('cargo test');
    expect(configSummary({ type: 'retry_limit', max_retries: 3 })).toBe('max 3 retries');
    expect(configSummary({ type: 'collaborator_check', require_approval: true })).toContain(
      'approval'
    );
    expect(configSummary({ type: 'notify', message: 'done!' })).toContain('done!');
    expect(configSummary({ type: 'manual' })).toBeTruthy();
    expect(configSummary({ type: 'all_completed' })).toBeTruthy();
  });
});

describe('parseGraph', () => {
  it('parses a stored graph_json payload and rejects garbage', () => {
    const g = parseGraph(
      '{"version":1,"nodes":[{"id":"t","type":"manual"}],"edges":[{"from":"t","to":"x"}]}'
    );
    expect(g.nodes).toHaveLength(1);
    expect(g.edges[0].condition).toBe('always');
    expect(() => parseGraph('not json at all')).toThrow();
  });
});

describe('fuzzy search (quick-connect menu)', () => {
  it('scores subsequence matches, exact prefixes best', () => {
    expect(fuzzyScore('spawn', 'Spawn Agent Node')).not.toBeNull();
    expect(fuzzyScore('san', 'Spawn Agent Node')).not.toBeNull();
    expect(fuzzyScore('zzz', 'Spawn Agent Node')).toBeNull();
    const prefix = fuzzyScore('spawn', 'Spawn Agent Node')!;
    const scattered = fuzzyScore('san', 'Spawn Agent Node')!;
    expect(prefix).toBeGreaterThan(scattered);
  });

  it('filters the catalogue by label or discriminator', () => {
    const hits = fuzzyFilterSpecs('verif');
    expect(hits.map((h) => h.discriminator)).toContain('deterministic_verification');
    const gateHits = fuzzyFilterSpecs('retry');
    expect(gateHits[0].discriminator).toBe('retry_limit');
    expect(fuzzyFilterSpecs('')).toHaveLength(NODE_SPECS.length);
  });
});

describe('mustache autocomplete', () => {
  it('offers chips across every context namespace', () => {
    for (const ns of ['issue.', 'pr.', 'node.', 'verification.', 'retry.', 'circuit.']) {
      expect(MUSTACHE_PATHS.some((p) => p.startsWith(ns)), `namespace ${ns}`).toBe(true);
    }
  });

  it('inserts {{ path }} replacing the typed opening braces', () => {
    const result = insertMustache('Fix {{', 7, 'issue.number');
    expect(result.text).toBe('Fix {{ issue.number }}');
    expect(result.caret).toBe('Fix {{ issue.number }}'.length);
  });

  it('keeps text after the caret intact', () => {
    const result = insertMustache('{{ done-tail', 2, 'circuit.name');
    expect(result.text).toBe('{{ circuit.name }} done-tail');
  });
});

describe('gate outcome ports', () => {
  it('exposes named outcome handles only on gates that route by outcome', () => {
    expect(sourceOutcomes({ type: 'llm_turn_classifier' })).toEqual([
      'completed',
      'blocked',
      'working',
    ]);
    expect(sourceOutcomes({ type: 'deterministic_verification', command: 'x' })).toEqual([
      'green',
      'red',
    ]);
    expect(sourceOutcomes({ type: 'collaborator_check', require_approval: true })).toBeNull();
    expect(sourceOutcomes({ type: 'notify', message: '' })).toBeNull();
  });
});

describe('edge condition badges', () => {
  it('labels conditions readably', () => {
    expect(conditionLabel('always')).toBe('Always');
    expect(conditionLabel({ on_outcome: 'green' })).toBe('OnOutcome(green)');
  });
});

describe('traversed-path highlighting', () => {
  const edges = [
    { from: 'classify', to: 'approve', condition: { on_outcome: 'blocked' } },
    { from: 'classify', to: 'next-turn', condition: { on_outcome: 'completed' } },
    { from: 'next-turn', to: 'verify', condition: 'always' },
  ];

  it('marks only the edges matching each step outcome', () => {
    const steps = [
      { node_id: 'classify', status: 'completed', outcome: 'completed' },
      { node_id: 'next-turn', status: 'running', outcome: null },
    ];
    const keys = traversedEdgeKeys(steps, edges);
    expect(keys.has(edgeKey(edges[0]))).toBe(false); // blocked branch not taken
    expect(keys.has(edgeKey(edges[1]))).toBe(true);
    expect(keys.has(edgeKey(edges[2]))).toBe(true);
  });

  it('ignores steps without an outcome', () => {
    const keys = traversedEdgeKeys(
      [{ node_id: 'classify', status: 'running', outcome: null }],
      edges
    );
    expect(keys.size).toBe(0);
  });

  it('computes step durations from ledger timestamps', () => {
    expect(
      stepDurationMs({
        started_at: '2026-08-22 10:05:00',
        completed_at: '2026-08-22 10:05:02',
      })
    ).toBe(2000);
    expect(stepDurationMs({ started_at: null, completed_at: null })).toBeNull();
    expect(stepDurationMs({ started_at: '2026-08-22 10:05:00', completed_at: null })).toBeNull();
  });
});

describe('dagre auto-layout', () => {
  const nodes = [
    { id: 'a' },
    { id: 'b' },
    { id: 'c' },
    { id: 'orphan' }, // disconnected node must still get a position
  ];
  const edges = [
    { from: 'a', to: 'b' },
    { from: 'b', to: 'c' },
  ];

  it('arranges left-to-right so ranks increase along edges', () => {
    const pos = layoutPositions(
      { version: 1, nodes, edges },
      'LR'
    );
    expect(pos.get('a')!.x).toBeLessThan(pos.get('b')!.x);
    expect(pos.get('b')!.x).toBeLessThan(pos.get('c')!.x);
    expect(pos.has('orphan')).toBe(true);
  });

  it('arranges top-to-bottom so ranks increase downward', () => {
    const pos = layoutPositions(
      { version: 1, nodes, edges },
      'TB'
    );
    expect(pos.get('a')!.y).toBeLessThan(pos.get('b')!.y);
    expect(pos.get('b')!.y).toBeLessThan(pos.get('c')!.y);
  });

  it('produces finite coordinates for any graph', () => {
    const pos = layoutPositions(
      { version: 1, nodes: [{ id: 'lonely' }], edges: [] },
      'LR'
    );
    expect(Number.isFinite(pos.get('lonely')!.x)).toBe(true);
    expect(Number.isFinite(pos.get('lonely')!.y)).toBe(true);
  });
});
