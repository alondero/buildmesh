/**
 * InspectorPanel — the editor's right slide-over (issue #1209).
 *
 * One form per node kind for the fields this slice edits (the
 * blueprint stays canonical in Rust). Text fields that accept circuit
 * context use `MustacheTextarea` so `{{` opens autocomplete.
 *
 * Issue #1359 (Circuits v2 slice 4 — Canvas UX) adds three things:
 *
 *   1. `graph` prop so the panel can compute upstream reachability for
 *      the selected node and pass it into the textarea autocomplete
 *      (grouped, reachability-aware chips).
 *   2. A Context Variables reference drawer at the bottom of every
 *      template-bearing panel — shows every reachable namespace, the
 *      sample value the runtime will produce, and which ones would
 *      resolve empty in this branch.
 *   3. A `target_node_id` dropdown for `inject_pty` and
 *      `set_node_status` listing every upstream `spawn_agent_node`
 *      (the stepper dispatches the effect at the chosen agent).
 */

import { useMemo, useState } from 'react';
import type { CircuitGraph } from '../../types/generated/CircuitGraph';
import type { CircuitNode } from '../../types/generated/CircuitNode';
import type { CircuitNodeKind } from '../../types/generated/CircuitNodeKind';
import type { GithubActionKind } from '../../types/generated/GithubActionKind';
import type { SessionStatusKind } from '../../types/generated/SessionStatusKind';
import { MustacheTextarea } from './MustacheTextarea';
import {
  MUSTACHE_GROUPS,
  MUSTACHE_PATHS,
  categoryAccent,
  categoryOf,
  configSummary,
  getReachableContext,
  isReachablePath,
  sampleValueForPath,
  specFor,
  type ReachableContext,
} from './circuitGraphModel';

interface InspectorPanelProps {
  node: CircuitNode | null;
  onChange: (kind: CircuitNodeKind) => void;
  /** Full blueprint AST — required for reachability. Optional so the
   *  unit tests can mount the panel without a graph and ad-hoc reuse
   *  (e.g. embedding inside another form) still works. */
  graph?: CircuitGraph;
}

const inputClass =
  'w-full px-2 py-1 bg-bg-input border border-border-subtle rounded-md text-xs text-text-primary focus:outline-none focus:border-border-active';

/**
 * Number field that tolerates transient states (cleared input, "0",
 * partial typing): keeps a local draft while focused, commits any
 * finite number upward, and falls back to the committed value on blur.
 */
function NumberField({
  value,
  min,
  ariaLabel,
  testId,
  onCommit,
}: {
  value: number;
  min?: number;
  ariaLabel: string;
  testId: string;
  onCommit: (n: number) => void;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const display = draft ?? String(value);
  return (
    <input
      type="number"
      min={min}
      value={display}
      aria-label={ariaLabel}
      data-testid={testId}
      onChange={(e) => {
        setDraft(e.target.value);
        if (e.target.value !== '') {
          const n = Number(e.target.value);
          if (Number.isFinite(n)) onCommit(n);
        }
      }}
      onFocus={() => setDraft(String(value))}
      onBlur={() => setDraft(null)}
      className={inputClass}
    />
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="block mb-2">
      <span className="block text-2xs uppercase tracking-wide text-text-muted mb-0.5">{label}</span>
      {children}
    </label>
  );
}

/** Dropdown for `inject_pty` / `set_node_status` to pick an upstream
 *  `spawn_agent_node` as the effect's target. The empty option lets
 *  the user fall back to the stepper's "nearest upstream spawn"
 *  runtime resolution; the explicit pick is what the stepper honours
 *  when set. */
function TargetNodeSelect({
  value,
  upstreamSpawns,
  onChange,
}: {
  value: string | null;
  upstreamSpawns: string[];
  onChange: (targetNodeId: string | null) => void;
}) {
  return (
    <select
      value={value ?? ''}
      aria-label="Target agent node"
      data-testid="inspector-target-node"
      onChange={(e) => onChange(e.target.value === '' ? null : e.target.value)}
      className={inputClass}
    >
      <option value="">
        {upstreamSpawns.length === 0
          ? '(no upstream spawn — will resolve at runtime)'
          : '(nearest upstream spawn at runtime)'}
      </option>
      {upstreamSpawns.map((id) => (
        <option key={id} value={id}>
          {id}
        </option>
      ))}
    </select>
  );
}

/** One row of the context reference drawer. Renders the canonical path
 *  alongside the sample value the runtime will resolve; rows whose
 *  namespace isn't reachable in this branch get an "empty" badge so
 *  the user can see why their template would interpolate blank. */
function ContextReferenceRow({
  path,
  reachable,
}: {
  path: string;
  reachable: ReachableContext | undefined;
}) {
  const live = isReachablePath(path, reachable);
  return (
    <li
      data-testid={`context-reference-${path}`}
      data-reachable={live ? 'true' : 'false'}
      className="flex items-center gap-2 py-0.5"
    >
      <code className={`font-mono text-2xs ${live ? 'text-text-secondary' : 'text-text-muted/60'}`}>
        {path}
      </code>
      <span className={`font-mono text-2xs italic ${live ? 'text-text-muted' : 'text-text-muted/50'}`}>
        {sampleValueForPath(path)}
      </span>
      {!live && (
        <span
          aria-label="unreachable in this branch"
          className="ml-auto text-2xs uppercase tracking-wide text-status-warning/80"
        >
          empty
        </span>
      )}
    </li>
  );
}

/** Drawer that lists every reachable context variable for the selected
 *  node. Hidden when the node has no template payload (triggers,
 *  gates, joins) so it never shows noise the user can't act on. */
function ContextReferenceDrawer({
  reachable,
  hasTemplate,
}: {
  reachable: ReachableContext | undefined;
  hasTemplate: boolean;
}) {
  if (!hasTemplate) return null;

  // Build the row list: static MUSTACHE_PATHS plus dynamic spawn-output
  // chips (`node.<id>.output`) for every reachable spawn upstream.
  const spawnOutputPaths = useMemo(
    () => (reachable?.nodeOutputIds ?? []).map((id) => `node.${id}.output`),
    [reachable]
  );
  const allPaths = useMemo(
    () => [...MUSTACHE_PATHS, ...spawnOutputPaths],
    [spawnOutputPaths]
  );
  const grouped = useMemo(() => {
    const buckets = new Map<string, string[]>();
    for (const spec of MUSTACHE_GROUPS) buckets.set(spec.namespace, []);
    for (const path of allPaths) {
      const ns = path.split('.', 1)[0];
      const list = buckets.get(ns);
      if (list !== undefined) list.push(path);
    }
    return MUSTACHE_GROUPS.flatMap((spec) => {
      const paths = buckets.get(spec.namespace) ?? [];
      if (paths.length === 0) return [];
      return [{ spec, paths }];
    });
  }, [allPaths]);

  return (
    <section
      data-testid="inspector-context-reference"
      className="mt-4 border-t border-border-subtle pt-3"
    >
      <header className="flex items-center justify-between mb-1">
        <span className="text-2xs uppercase tracking-wide text-text-muted">
          Context variables
        </span>
        {reachable === undefined && (
          <span className="text-2xs text-text-muted/70" data-testid="context-reference-no-graph">
            no graph
          </span>
        )}
      </header>
      <p className="text-2xs text-text-muted mb-2">
        Variables this template can interpolate at runtime.{" "}
        <span className="text-status-warning/80">empty</span> means a producer upstream is missing.
      </p>
      {grouped.map(({ spec, paths }) => (
        <div key={spec.namespace} className="mb-2" data-testid={`context-group-${spec.namespace}`}>
          <div className="text-2xs font-semibold text-text-secondary">{spec.label}</div>
          <ul className="pl-1">
            {paths.map((path) => (
              <ContextReferenceRow key={path} path={path} reachable={reachable} />
            ))}
          </ul>
        </div>
      ))}
    </section>
  );
}

export function InspectorPanel(props: InspectorPanelProps) {
  // Rules of Hooks: ALL hooks must run on every render, in the same
  // order. Compute reachability up front so the null-empty-state
  // branch doesn't change the hook count (which would crash React
  // with "Rendered more hooks than during the previous render" the
  // moment the user selects their first node after opening the
  // editor with nothing selected — issue #1359 review feedback).
  //
  // Dep on `props.node?.id` (not `props.node`) so the BFS only re-
  // walks when the selected node identity changes — not on every
  // parent re-render where the node object reference shifts but the
  // id is identical (review feedback round 2: a `props.node` dep
  // busts the memo on every keystroke because the canvas editor's
  // working copy mints a fresh React Flow node each render).
  const reachable = useMemo(
    () => (props.graph !== undefined && props.node !== null
      ? getReachableContext(props.node.id, props.graph)
      : undefined),
    [props.graph, props.node?.id]
  );

  if (props.node === null) {
    return (
      <aside
        data-testid="circuit-inspector"
        className="w-64 shrink-0 border-l border-border-subtle bg-bg-surface p-3 text-xs text-text-muted"
      >
        Select a node to edit its configuration.
      </aside>
    );
  }

  const { node, onChange } = props;
  const kind = node.type;
  const accent = categoryAccent(categoryOf(kind));
  // Upstream spawn nodes for the target dropdown — derived from the
  // same reachability memo, so no second BFS walk.
  const upstreamSpawns: string[] = reachable?.nodeOutputIds ?? [];

  // Whether the kind surfaces any template field (so the context
  // drawer is worth rendering). Joins / triggers / pure gates don't
  // interpolate placeholders; only action kinds do.
  const hasTemplate =
    kind.type === 'spawn_agent_node' ||
    kind.type === 'inject_pty' ||
    kind.type === 'github_action' ||
    kind.type === 'notify';

  return (
    <aside
      data-testid="circuit-inspector"
      className="w-64 shrink-0 border-l border-border-subtle bg-bg-surface p-3 overflow-y-auto"
    >
      <div className="flex items-center gap-2 mb-1">
        <span className={`inline-block w-2 h-2 rounded-full ${accent.bg}`} aria-hidden />
        <span className={`text-sm font-semibold ${accent.text}`}>{specFor(kind.type).label}</span>
      </div>
      <div className="text-2xs font-mono text-text-muted mb-3">
        id: <span data-testid="inspector-node-id">{node.id}</span>
      </div>

      {kind.type === 'spawn_agent_node' && (
        <>
          <Field label="Agent name">
            <input
              value={kind.name ?? ''}
              aria-label="Agent name"
              data-testid="inspector-agent-name"
              onChange={(e) => onChange({ ...kind, name: e.target.value || null })}
              className={inputClass}
            />
          </Field>
          <Field label="Initial prompt (via Inject PTY)">
            <MustacheTextarea
              value={kind.prompt}
              onChange={(prompt) => onChange({ ...kind, prompt })}
              rows={5}
              ariaLabel="Spawn prompt"
              testId="inspector-prompt"
              reachable={reachable}
            />
          </Field>
        </>
      )}

      {kind.type === 'inject_pty' && (
        <>
          <Field label="Target agent">
            <TargetNodeSelect
              value={kind.target_node_id}
              upstreamSpawns={upstreamSpawns}
              onChange={(target_node_id) => onChange({ ...kind, target_node_id })}
            />
          </Field>
          <Field label="PTY prompt">
            <MustacheTextarea
              value={kind.prompt}
              onChange={(prompt) => onChange({ ...kind, prompt })}
              rows={5}
              ariaLabel="Inject PTY prompt"
              testId="inspector-prompt"
              reachable={reachable}
            />
          </Field>
        </>
      )}

      {kind.type === 'notify' && (
        <Field label="Message">
          <MustacheTextarea
            value={kind.message}
            onChange={(message) => onChange({ ...kind, message })}
            rows={4}
            ariaLabel="Notify message"
            testId="inspector-message"
            reachable={reachable}
          />
        </Field>
      )}

      {kind.type === 'github_action' && (
        <>
          <Field label="Action">
            <select
              value={kind.action}
              aria-label="GitHub action"
              data-testid="inspector-github-action"
              onChange={(e) => onChange({ ...kind, action: e.target.value as GithubActionKind })}
              className={inputClass}
            >
              <option value="add_label">Add label</option>
              <option value="remove_label">Remove label</option>
              <option value="post_comment">Post comment</option>
              <option value="open_pr">Open PR</option>
              <option value="close_issue">Close issue</option>
            </select>
          </Field>
          {(kind.action === 'add_label' || kind.action === 'remove_label') && (
            <Field label="Label">
              <input
                value={kind.label ?? ''}
                aria-label="GitHub label"
                data-testid="inspector-github-label"
                onChange={(e) => onChange({ ...kind, label: e.target.value || null })}
                className={inputClass}
              />
            </Field>
          )}
          {kind.action === 'post_comment' && (
            <Field label="Comment">
              <MustacheTextarea
                value={kind.comment ?? ''}
                onChange={(comment) => onChange({ ...kind, comment: comment || null })}
                rows={4}
                ariaLabel="GitHub comment"
                testId="inspector-comment"
                reachable={reachable}
              />
            </Field>
          )}
        </>
      )}

      {kind.type === 'set_node_status' && (
        <>
          <Field label="Target agent">
            <TargetNodeSelect
              value={kind.target_node_id}
              upstreamSpawns={upstreamSpawns}
              onChange={(target_node_id) => onChange({ ...kind, target_node_id })}
            />
          </Field>
          <Field label="Status">
            <select
              value={kind.status}
              aria-label="Node status"
              data-testid="inspector-status-select"
              onChange={(e) => onChange({ ...kind, status: e.target.value as SessionStatusKind })}
              className={inputClass}
            >
              <option value="running">running</option>
              <option value="idle">idle</option>
              <option value="completed">completed</option>
            </select>
          </Field>
        </>
      )}

      {kind.type === 'interval' && (
        <Field label="Interval seconds">
          <NumberField
            value={kind.interval_seconds}
            min={60}
            ariaLabel="Interval seconds"
            testId="inspector-interval"
            onCommit={(interval_seconds) => onChange({ ...kind, interval_seconds })}
          />
        </Field>
      )}

      {(kind.type === 'github_issue_label' || kind.type === 'github_pull_request_label') && (
        <Field label="Trigger label">
          <input
            value={kind.label}
            aria-label="Trigger label"
            data-testid="inspector-trigger-label"
            onChange={(e) => onChange({ ...kind, label: e.target.value })}
            placeholder="buildmesh:run"
            className={inputClass}
          />
        </Field>
      )}

      {kind.type === 'deterministic_verification' && (
        <Field label="Verification command">
          <input
            value={kind.command}
            aria-label="Verification command"
            data-testid="inspector-command"
            onChange={(e) => onChange({ ...kind, command: e.target.value })}
            placeholder="cargo test"
            className={`${inputClass} font-mono`}
          />
        </Field>
      )}

      {kind.type === 'collaborator_check' && (
        <label className="flex items-center gap-2 text-xs text-text-primary" data-testid="inspector-require-approval-wrap">
          <input
            type="checkbox"
            checked={kind.require_approval}
            aria-label="Require approval"
            data-testid="inspector-require-approval"
            onChange={(e) => onChange({ ...kind, require_approval: e.target.checked })}
          />
          Require human approval
        </label>
      )}

      {kind.type === 'retry_limit' && (
        <Field label="Max retries">
          <NumberField
            value={kind.max_retries}
            min={0}
            ariaLabel="Max retries"
            testId="inspector-max-retries"
            onCommit={(max_retries) => onChange({ ...kind, max_retries })}
          />
        </Field>
      )}

      {/* Kinds with no configurable payload still get a readable line. */}
      {['manual', 'llm_turn_classifier', 'all_completed', 'any_completed'].includes(kind.type) && (
        <p className="text-xs text-text-secondary">{configSummary(kind)}</p>
      )}

      <ContextReferenceDrawer reachable={reachable} hasTemplate={hasTemplate} />
    </aside>
  );
}