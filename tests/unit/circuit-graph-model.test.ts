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
  toGraph,
  CIRCUIT_GRAPH_VERSION,
  fuzzyScore,
  fuzzyFilterSpecs,
  MUSTACHE_PATHS,
  MUSTACHE_GROUPS,
  groupForPath,
  getReachableContext,
  isReachablePath,
  upstreamSpawnTargets,
  insertMustache,
  sourceOutcomes,
  conditionLabel,
  edgeKey,
  traversedEdgeKeys,
  stepDurationMs,
  layoutPositions,
  sampleValueForPath,
  stableGraphJson,
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
  'close_agent_node',
  'notify',
  'llm_turn_classifier',
  'await_agent_turn',
  'review_verdict',
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
    expect(specFor('close_agent_node').category).toBe('action');
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
      provider: null,
      model: null,
      effort: null,
      extra_args: null,
      // #1219: v3 added `timeout_seconds`. Default to null ("inherit
      // orchestrator default") so a freshly spawned node from the
      // catalogue has no override.
      timeout_seconds: null,
    });
    expect(defaultKind('inject_pty')).toEqual({
      type: 'inject_pty',
      prompt: '',
      target_node_id: null,
    });
    expect(defaultKind('set_node_status')).toEqual({
      type: 'set_node_status',
      status: 'completed',
      target_node_id: null,
    });
    expect(defaultKind('close_agent_node')).toEqual({
      type: 'close_agent_node',
      target_node_id: null,
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
      provider: null,
      model: null,
      effort: null,
      extra_args: null,
    };
    expect(configSummary(emptySpawn)).toBe('(no prompt)');
    expect(configSummary({ ...emptySpawn, name: 'fix-it' })).toContain('fix-it');
    expect(
      configSummary({ type: 'inject_pty', prompt: 'wrap up', target_node_id: null })
    ).toContain('wrap up');
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
    expect(g.version).toBe(CIRCUIT_GRAPH_VERSION);
    expect(() => parseGraph('not json at all')).toThrow();
  });

  it('keeps the review blueprint marker when the author edits its prompt', () => {
    const g = parseGraph(
      JSON.stringify({
        version: 2,
        blueprint: 'issue_driven_autopilot_review',
        nodes: [
          { id: 'trigger', type: { type: 'github_issue_label', label: 'buildmesh:run' } },
          { id: 'reviewer', type: { type: 'spawn_agent_node', prompt: 'review' } },
          { id: 'review_prompt', type: { type: 'inject_pty', prompt: 'custom', target_node_id: 'reviewer' } },
          { id: 'close_reviewer', type: { type: 'close_agent_node', target_node_id: 'reviewer' } },
        ],
        edges: [],
      })
    );
    expect(g.blueprint).toBe('issue_driven_autopilot_review');

    const edited = toGraph(
      g.nodes.map((circuitNode) => ({ data: { circuitNode } })),
      [],
      g.blueprint
    );
    expect(edited.blueprint).toBe('issue_driven_autopilot_review');
  });

  it('infers the marker for a legacy review graph without reading prompt text', () => {
    const g = parseGraph(
      JSON.stringify({
        version: 2,
        nodes: [
          { id: 'trigger', type: { type: 'github_issue_label', label: 'buildmesh:run' } },
          { id: 'reviewer', type: { type: 'spawn_agent_node', prompt: 'custom reviewer setup' } },
          { id: 'review_prompt', type: { type: 'inject_pty', prompt: 'author changed this', target_node_id: 'reviewer' } },
          { id: 'close_reviewer', type: { type: 'close_agent_node', target_node_id: 'reviewer' } },
        ],
        edges: [],
      })
    );
    expect(g.blueprint).toBe('issue_driven_autopilot_review');
  });

  it('upgrades a v1 spawn/inject/status payload with the v2 optional fields', () => {
    const g = parseGraph(
      JSON.stringify({
        version: 1,
        nodes: [
          { id: 's', type: { type: 'spawn_agent_node', prompt: 'p', name: 'fix-it' } },
          { id: 'i', type: { type: 'inject_pty', prompt: 'hi' } },
          { id: 'st', type: { type: 'set_node_status', status: 'completed' } },
        ],
        edges: [],
      })
    );
    expect(g.version).toBe(CIRCUIT_GRAPH_VERSION);
    expect(g.nodes[0].type).toEqual({
      type: 'spawn_agent_node',
      prompt: 'p',
      name: 'fix-it',
      provider: null,
      model: null,
      effort: null,
      extra_args: null,
      // #1219: v3 added `timeout_seconds`; v1 graphs default it to null
      // through the upgrade path (`normalizeKind`).
      timeout_seconds: null,
    });
    expect(g.nodes[1].type).toEqual({
      type: 'inject_pty',
      prompt: 'hi',
      target_node_id: null,
    });
    expect(g.nodes[2].type).toEqual({
      type: 'set_node_status',
      status: 'completed',
      target_node_id: null,
    });
  });

  it('toGraph emits the current AST version', () => {
    const graph = toGraph(
      [{ data: { circuitNode: { id: 't', type: { type: 'manual' } } } }],
      [{ source: 't', target: 't' }]
    );
    expect(graph.version).toBe(CIRCUIT_GRAPH_VERSION);
    expect(graph.nodes).toHaveLength(1);
    expect(graph.edges[0].condition).toBe('always');
  });

  it('stableGraphJson is order-insensitive — an add+delete round-trip is not dirty', () => {
    const a = parseGraph(
      '{"version":1,"nodes":[{"id":"a","type":"manual"},{"id":"b","type":"notify","message":"m"}],"edges":[{"from":"a","to":"b"}]}'
    );
    const reordered = parseGraph(
      '{"version":1,"nodes":[{"id":"b","type":"notify","message":"m"},{"id":"a","type":"manual"}],"edges":[{"from":"a","to":"b"}]}'
    );
    expect(stableGraphJson(a)).toBe(stableGraphJson(reordered));
    // A real change still shows.
    const changed = parseGraph(
      '{"version":1,"nodes":[{"id":"a","type":"manual"},{"id":"b","type":"notify","message":"changed"}],"edges":[{"from":"a","to":"b"}]}'
    );
    expect(stableGraphJson(a)).not.toBe(stableGraphJson(changed));
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

  it('consumes a }} the user already typed past the caret (no dead text)', () => {
    // Common keyboard flow: user typed `{{ issue.number }}` then
    // realised they wanted `circuit.name`. They put the caret right
    // after `{{` (or any point inside) and Enter on a chip. The old
    // code left a stray `}}` after the insertion; the fix consumes it.
    const result = insertMustache('{{ issue.number }}', 2, 'circuit.name');
    expect(result.text).toBe('{{ circuit.name }}');
    expect(result.caret).toBe('{{ circuit.name }}'.length);
  });

  it('does not consume `}}` that is NOT immediately after the caret', () => {
    // `}}` further down the text is unrelated to the active placeholder.
    const result = insertMustache('{{ issue.number }} more', 18, 'circuit.name');
    expect(result.text).toBe('{{ circuit.name }} more');
  });

  it('groups paths by namespace for the autocomplete popup + inspector drawer', () => {
    // Each MUSTACHE_PATHS prefix is mapped to one of the catalogue
    // groups; spawn_output is the synthetic bucket for `node.<id>.output`.
    for (const ns of ['issue.', 'pr.', 'verification.', 'retry.', 'circuit.']) {
      const group = MUSTACHE_GROUPS.find((g) => ns.startsWith(g.namespace + '.'));
      expect(group, `namespace ${ns} must have a group`).toBeDefined();
    }
    expect(groupForPath('node.spawn_1.output')).toBe('spawn_output');
    expect(groupForPath('node.id')).toBe('node');
    expect(groupForPath('circuit.name')).toBe('circuit');
  });

  it('provides a sample value for every MUSTACHE_PATH and a fallback for spawn outputs', () => {
    for (const path of MUSTACHE_PATHS) {
      const sample = sampleValueForPath(path);
      expect(typeof sample).toBe('string');
      expect(sample.length, `sample for ${path}`).toBeGreaterThan(0);
    }
    // The fallback covers dynamic spawn-output chips.
    expect(sampleValueForPath('node.spawn_42.output')).toBe('<terminal text>');
  });
});

describe('upstream reachability (issue #1359)', () => {
  it('returns an all-false summary for an unknown node id (defensive)', () => {
    const r = getReachableContext('missing', {
      version: CIRCUIT_GRAPH_VERSION,
      nodes: [],
      edges: [],
    });
    expect(r.triggers).toEqual({ manual: false, interval: false, issue: false, pullRequest: false });
    expect(r.pullRequest).toBe(false);
    expect(r.nodeOutputIds).toEqual([]);
    expect(r.gates).toEqual({
      verification: false,
      retry: false,
      llmClassifier: false,
      collaborator: false,
    });
    // `metadata` (circuit.* / node.id) is unconditionally available —
    // the trigger wrapper sets them at run creation regardless of the
    // selected node, so reachability there is meaningless.
    expect(r.metadata).toBe(false);
  });

  it('a linear trigger → spawn → notify graph: triggers + spawn output are reachable', () => {
    const graph = {
      version: CIRCUIT_GRAPH_VERSION,
      nodes: [
        { id: 't', type: { type: 'manual' as const } },
        {
          id: 's',
          type: {
            type: 'spawn_agent_node' as const,
            prompt: 'go',
            name: null,
            provider: null,
            model: null,
            effort: null,
            extra_args: null,
          },
        },
        { id: 'n', type: { type: 'notify' as const, message: '' } },
      ],
      edges: [
        { from: 't', to: 's', condition: 'always' as const },
        { from: 's', to: 'n', condition: 'always' as const },
      ],
    };
    const r = getReachableContext('n', graph);
    expect(r.triggers.manual).toBe(true);
    expect(r.triggers.issue).toBe(false);
    expect(r.nodeOutputIds).toEqual(['s']);
    // PR / verification / retry producers absent.
    expect(r.pullRequest).toBe(false);
    expect(r.gates.verification).toBe(false);
    expect(r.gates.retry).toBe(false);
  });

  it('a spawn cannot reach its own output — the node is NOT upstream of itself', () => {
    // The spawn's initial prompt template runs before the agent
    // produces any output, so `{{ node.<id>.output }}` for its own id
    // must be unreachable. This is the temporal-paradox guard: a
    // node reading its own terminal output makes no sense.
    const graph = {
      version: CIRCUIT_GRAPH_VERSION,
      nodes: [
        { id: 't', type: { type: 'manual' as const } },
        {
          id: 's',
          type: {
            type: 'spawn_agent_node' as const,
            prompt: 'go',
            name: null,
            provider: null,
            model: null,
            effort: null,
            extra_args: null,
          },
        },
        { id: 'n', type: { type: 'notify' as const, message: '' } },
      ],
      edges: [
        { from: 't', to: 's', condition: 'always' as const },
        { from: 's', to: 'n', condition: 'always' as const },
      ],
    };
    const r = getReachableContext('s', graph);
    // The spawn itself is excluded; only the trigger upstream remains.
    expect(r.nodeOutputIds).toEqual([]);
    // Trigger still propagates.
    expect(r.triggers.manual).toBe(true);
  });

  it('an issue-label trigger propagates issue.* to a downstream notify', () => {
    const graph = {
      version: CIRCUIT_GRAPH_VERSION,
      nodes: [
        { id: 't', type: { type: 'github_issue_label' as const, label: 'buildmesh:run' } },
        { id: 'a', type: { type: 'github_action' as const, action: 'post_comment' as const, open_pr_policy: null, label: null, comment: null } },
      ],
      edges: [{ from: 't', to: 'a', condition: 'always' as const }],
    };
    const r = getReachableContext('a', graph);
    expect(r.triggers.issue).toBe(true);
    // `post_comment` is not an `open_pr` action — PR context stays empty.
    expect(r.pullRequest).toBe(false);
  });

  it('an open_pr action upstream makes pr.* reachable for downstream steps', () => {
    const graph = {
      version: CIRCUIT_GRAPH_VERSION,
      nodes: [
        { id: 't', type: { type: 'manual' as const } },
        { id: 'a', type: { type: 'github_action' as const, action: 'open_pr' as const, open_pr_policy: null, label: null, comment: null } },
        { id: 's', type: { type: 'spawn_agent_node' as const, prompt: 'go', name: null, provider: null, model: null, effort: null, extra_args: null } },
        { id: 'n', type: { type: 'notify' as const, message: '' } },
      ],
      edges: [
        { from: 't', to: 'a', condition: 'always' as const },
        { from: 'a', to: 's', condition: 'always' as const },
        { from: 's', to: 'n', condition: 'always' as const },
      ],
    };
    const r = getReachableContext('n', graph);
    expect(r.pullRequest).toBe(true);
    expect(r.nodeOutputIds).toEqual(['s']);
  });

  it('a diamond join merges multiple upstream namespaces without duplicating spawn outputs', () => {
    const graph = {
      version: CIRCUIT_GRAPH_VERSION,
      nodes: [
        { id: 't1', type: { type: 'github_issue_label' as const, label: 'l1' } },
        { id: 't2', type: { type: 'interval' as const, interval_seconds: 60 } },
        {
          id: 's1',
          type: { type: 'spawn_agent_node' as const, prompt: 'go', name: null, provider: null, model: null, effort: null, extra_args: null },
        },
        {
          id: 's2',
          type: { type: 'spawn_agent_node' as const, prompt: 'go', name: null, provider: null, model: null, effort: null, extra_args: null },
        },
        { id: 'j', type: { type: 'all_completed' as const } },
        { id: 'n', type: { type: 'notify' as const, message: '' } },
      ],
      edges: [
        { from: 't1', to: 's1', condition: 'always' as const },
        { from: 't2', to: 's2', condition: 'always' as const },
        { from: 's1', to: 'j', condition: 'always' as const },
        { from: 's2', to: 'j', condition: 'always' as const },
        { from: 'j', to: 'n', condition: 'always' as const },
      ],
    };
    const r = getReachableContext('n', graph);
    expect(r.triggers).toEqual({ manual: false, interval: true, issue: true, pullRequest: false });
    expect(r.nodeOutputIds).toEqual(['s1', 's2']);
    // upstreamSpawnTargets is the same view reused by the inspector picker.
    expect(upstreamSpawnTargets('n', graph)).toEqual(['s1', 's2']);
  });

  it('a bounded cycle (verifier ↔ retry_limit) terminates via visited-set and lists the gate once', () => {
    const graph = {
      version: CIRCUIT_GRAPH_VERSION,
      nodes: [
        { id: 't', type: { type: 'manual' as const } },
        { id: 's', type: { type: 'spawn_agent_node' as const, prompt: 'go', name: null, provider: null, model: null, effort: null, extra_args: null } },
        { id: 'v', type: { type: 'deterministic_verification' as const, command: 'cargo test' } },
        { id: 'r', type: { type: 'retry_limit' as const, max_retries: 3 } },
        { id: 'n', type: { type: 'notify' as const, message: '' } },
      ],
      edges: [
        { from: 't', to: 's', condition: 'always' as const },
        { from: 's', to: 'v', condition: 'always' as const },
        { from: 'v', to: 'r', condition: { on_outcome: 'red' as const } },
        { from: 'r', to: 's', condition: 'always' as const },
        { from: 'v', to: 'n', condition: { on_outcome: 'green' as const } },
      ],
    };
    // BFS from n walks back through v — and would loop through r→s→v
    // forever without a visited set. Termination is the contract.
    // `s` is upstream of `n` via `v→n` (after a green run the spawn's
    // terminal output IS available to the downstream notify), so it
    // appears in nodeOutputIds. In a second iteration the spawn
    // cannot read ITSELF, only its predecessors.
    const r = getReachableContext('n', graph);
    expect(r.triggers.manual).toBe(true);
    expect(r.nodeOutputIds).toEqual(['s']);
    expect(r.gates.verification).toBe(true);
    expect(r.gates.retry).toBe(true);
  });

  it('a spawn whose only upstream is itself (1-node cycle) cannot reach its own output', () => {
    // Pathological — the slice-1 validator rejects self-loops so
    // this graph shape never reaches the editor. Even if it did,
    // the BFS would mark `s` as visited on first hop and skip it.
    const graph = {
      version: CIRCUIT_GRAPH_VERSION,
      nodes: [
        { id: 's', type: { type: 'spawn_agent_node' as const, prompt: 'go', name: null, provider: null, model: null, effort: null, extra_args: null } },
      ],
      edges: [{ from: 's', to: 's', condition: 'always' as const }],
    };
    const r = getReachableContext('s', graph);
    expect(r.nodeOutputIds).toEqual([]);
  });

  it('verification + retry + llm_classifier + collaborator gates all surface as reachable booleans', () => {
    const graph = {
      version: CIRCUIT_GRAPH_VERSION,
      nodes: [
        { id: 't', type: { type: 'manual' as const } },
        { id: 'v', type: { type: 'deterministic_verification' as const, command: 'x' } },
        { id: 'r', type: { type: 'retry_limit' as const, max_retries: 3 } },
        { id: 'l', type: { type: 'llm_turn_classifier' as const, target_node_id: null } },
        { id: 'c', type: { type: 'collaborator_check' as const, require_approval: true } },
        { id: 'n', type: { type: 'notify' as const, message: '' } },
      ],
      edges: [
        { from: 't', to: 'v', condition: 'always' as const },
        { from: 'v', to: 'r', condition: 'always' as const },
        { from: 'r', to: 'l', condition: 'always' as const },
        { from: 'l', to: 'c', condition: 'always' as const },
        { from: 'c', to: 'n', condition: 'always' as const },
      ],
    };
    const r = getReachableContext('n', graph);
    expect(r.gates).toEqual({
      verification: true,
      retry: true,
      llmClassifier: true,
      collaborator: true,
    });
  });

  it('a self-loop with no upstream producer keeps the gate reachable but adds no spawn output', () => {
    // Self-loops are rejected by the validator — this guarantees the
    // BFS still terminates on a graph that contains one (defensive,
    // because the editor might briefly hold an invalid shape before
    // the user resolves the dangling edge).
    const graph = {
      version: CIRCUIT_GRAPH_VERSION,
      nodes: [
        { id: 'v', type: { type: 'deterministic_verification' as const, command: 'x' } },
      ],
      edges: [{ from: 'v', to: 'v', condition: 'always' as const }],
    };
    const r = getReachableContext('v', graph);
    expect(r.gates.verification).toBe(true);
    expect(r.nodeOutputIds).toEqual([]);
  });

  it('a PR-label trigger alone makes pr.* reachable (no open_pr action needed)', () => {
    const graph = {
      version: CIRCUIT_GRAPH_VERSION,
      nodes: [
        { id: 't', type: { type: 'github_pull_request_label' as const, label: 'l' } },
        { id: 'a', type: { type: 'github_action' as const, action: 'post_comment' as const, open_pr_policy: null, label: null, comment: null } },
      ],
      edges: [{ from: 't', to: 'a', condition: 'always' as const }],
    };
    const r = getReachableContext('a', graph);
    expect(r.triggers.pullRequest).toBe(true);
    expect(r.pullRequest).toBe(true);
  });
});

describe('isReachablePath (shared reachability test)', () => {
  // The whole point of this helper is that BOTH the textarea popup
  // and the inspector drawer use it. If these tests pass, the two
  // call sites cannot disagree on which paths are live.
  const reachable = {
    triggers: { manual: true, interval: false, issue: true, pullRequest: false },
    pullRequest: true,
    nodeOutputIds: ['spawn_1'],
    gates: { verification: true, retry: true, llmClassifier: false, collaborator: false },
    metadata: true,
  };

  it('treats every path as live when reachability is undefined', () => {
    expect(isReachablePath('issue.number', undefined)).toBe(true);
    expect(isReachablePath('node.spawn_1.output', undefined)).toBe(true);
    expect(isReachablePath('circuit.name', undefined)).toBe(true);
  });

  it('circuit.* and node.id are always live', () => {
    expect(isReachablePath('circuit.id', reachable)).toBe(true);
    expect(isReachablePath('circuit.name', reachable)).toBe(true);
    expect(isReachablePath('circuit.run_id', reachable)).toBe(true);
    expect(isReachablePath('node.id', reachable)).toBe(true);
  });

  it('node.<id>.output is live iff the id is in nodeOutputIds', () => {
    expect(isReachablePath('node.spawn_1.output', reachable)).toBe(true);
    expect(isReachablePath('node.spawn_2.output', reachable)).toBe(false);
  });

  it('maps trigger / action / gate booleans 1:1', () => {
    expect(isReachablePath('issue.number', reachable)).toBe(true);
    expect(isReachablePath('pr.number', reachable)).toBe(true);
    expect(isReachablePath('verification.outcome', reachable)).toBe(true);
    expect(isReachablePath('retry.attempt', reachable)).toBe(true);
  });

  it('falls back to false for an unknown namespace', () => {
    expect(isReachablePath('totally.fake.path', reachable)).toBe(false);
  });
});

describe('sampleValueForPath', () => {
  it('returns <terminal text> for spawn-output chips (no static sample)', () => {
    expect(sampleValueForPath('node.spawn_1.output')).toBe('<terminal text>');
    expect(sampleValueForPath('node.spawn_42.output')).toBe('<terminal text>');
  });

  it('returns the empty string for a truly unknown path (defensive default)', () => {
    // Default branch now distinguishes "spawn_output-shaped" from
    // "no case matched"; the latter is an empty placeholder rather
    // than the misleading "<terminal text>" used for spawn outputs.
    expect(sampleValueForPath('totally.unknown.path')).toBe('');
  });
});

describe('gate outcome ports', () => {
  it('exposes named outcome handles only on gates that route by outcome', () => {
    expect(sourceOutcomes({ type: 'llm_turn_classifier', target_node_id: null })).toEqual([
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

  it('queued steps light nothing — the path has not been walked yet', () => {
    const keys = traversedEdgeKeys(
      [{ node_id: 'next-turn', status: 'pending_slot', outcome: null }],
      edges
    );
    expect(keys.has(edgeKey(edges[2]))).toBe(false);
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

  it('tolerates ISO-8601 timestamps as well as SQLite datetime strings', () => {
    // Appending "Z" to an already-ISO string would yield "…ZZ" → NaN.
    expect(
      stepDurationMs({
        started_at: '2026-08-22T10:05:00Z',
        completed_at: '2026-08-22T10:05:03Z',
      })
    ).toBe(3000);
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
