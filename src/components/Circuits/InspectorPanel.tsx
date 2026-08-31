/**
 * InspectorPanel — the editor's right slide-over (issue #1209).
 *
 * One form per node kind for the fields this slice edits (the
 * blueprint stays canonical in Rust). Text fields that accept circuit
 * context use `MustacheTextarea` so `{{` opens autocomplete.
 *
 * Issue #1358 (Circuits v2 slice 3 — Harness Integration) extends
 * `SpawnAgentNode` with a Provider selector + capability-gated
 * Model / Effort / Extra-Args overrides. The Inspector reads the
 * capability contract from a hardcoded `harnessCapabilities.ts`
 * table; the orchestrator applies the same contract via
 * `resolve_agent_config` so a user-authored override that the new
 * harness can't honour is dropped at spawn time. Issue #1359
 * (Circuits v2 slice 4 — Canvas UX) layers a Context Variables
 * reference drawer + `target_node_id` dropdowns on top.
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
 *   4. Provider / Model / Effort / Extra-Args controls for
 *      `SpawnAgentNode` (issue #1358). Switching the provider
 *      sanitises the other three fields so a stale value never
 *      serialises into a circuit the new harness can't honour.
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
  groupForPath,
  isReachablePath,
  sampleValueForPath,
  specFor,
  type ReachableContext,
} from './circuitGraphModel';
import {
  effortAllowedFor,
  getCapabilitiesFor,
  HARNESS_LABEL,
  type InspectorHarnessId,
} from './harnessCapabilities';

/**
 * The five harness ids offered in the Inspector's provider dropdown.
 * Mirrors the same surface that the Rust `BUILTIN_HARNESS_IDS`
 * list provides; the dropdown order is stable so the user's
 * selection persists across re-renders.
 */
const HARNESS_OPTIONS: { value: InspectorHarnessId; label: string }[] = [
  { value: 'anthropic', label: HARNESS_LABEL.anthropic },
  { value: 'codex', label: HARNESS_LABEL.codex },
  { value: 'agy', label: HARNESS_LABEL.agy },
  { value: 'opencode', label: HARNESS_LABEL.opencode },
  { value: 'grok', label: HARNESS_LABEL.grok },
  { value: 'cursor', label: HARNESS_LABEL.cursor },
  { value: 'kimi', label: HARNESS_LABEL.kimi },
  { value: 'mcode', label: HARNESS_LABEL.mcode },
  { value: 'dsh', label: HARNESS_LABEL.dsh },
  { value: 'commandcode', label: HARNESS_LABEL.commandcode },
  { value: 'freebuff', label: HARNESS_LABEL.freebuff },
  { value: 'terminal', label: HARNESS_LABEL.terminal },
];

interface InspectorPanelProps {
  node: CircuitNode | null;
  onChange: (kind: CircuitNodeKind) => void;
  /** Full blueprint AST — required for reachability. Optional so the
   *  unit tests can mount the panel without a graph and ad-hoc reuse
   *  (e.g. embedding inside another form) still works. */
  graph?: CircuitGraph;
}

/**
 * Compute the harness id string the Inspector uses to key into
 * `harnessCapabilities.ts`. Mirrors `Provider::from_db_str`'s default
 * fallback (`""` and unknown ids → `"anthropic"`) so a user-authored
 * `provider` that hasn't been normalised by the backend still resolves
 * to Anthropic's descriptor. `null` ⇒ no harness selected (use the
 * mesh's default at spawn).
 */
function harnessIdFromProvider(provider: string | null | undefined): InspectorHarnessId | null {
  if (provider == null) return null;
  // Issue #1362 review: the backend's `non_empty_trim` collapses
  // whitespace-only and empty `provider` strings to `None` (cascade
  // falls through to the mesh's default autopilot). Mirror that
  // semantic here so an empty / whitespace / unknown id never gets
  // rendered as if the user had picked a harness — keep "no provider
  // selected" honest in the UI.
  const normalised = provider.trim().toLowerCase();
  if (normalised === '') return null;
  if (normalised === 'anthropic' || normalised === 'claude_code' || normalised === 'claude') {
    // Inspector's selector keys on `anthropic` to match the backend id.
    return 'anthropic';
  }
  if (normalised === 'antigravity') {
    return 'agy';
  }
  if (normalised === 'minimax-code' || normalised === 'minimax') {
    return 'mcode';
  }
  if (normalised === 'deepseek' || normalised === 'deepseek-harness') {
    return 'dsh';
  }
  if (normalised === 'command-code' || normalised === 'cmdc') {
    return 'commandcode';
  }
  if (
    normalised === 'codex' ||
    normalised === 'agy' ||
    normalised === 'opencode' ||
    normalised === 'grok' ||
    normalised === 'cursor' ||
    normalised === 'kimi' ||
    normalised === 'mcode' ||
    normalised === 'dsh' ||
    normalised === 'commandcode' ||
    normalised === 'freebuff' ||
    normalised === 'terminal'
  ) {
    return normalised as InspectorHarnessId;
  }
  // Unknown id: emit as a synthetic entry. We don't have a stable
  // capability descriptor for it (matches the backend's
  // `Provider::from_db_str` fallback to Anthropic, but the inspector
  // renders the resolved id so the user sees what they authored).
  // Return null to fall back to "no overrides".
  return null;
}

/**
 * Inverse of `harnessIdFromProvider` for the wire payload — keep the
 * user's id canonical. The backend's `Provider::from_db_str` accepts
 * both `codex` and `agy` (so does the inspector dropdown), so this
 * just maps back to the wire shape the AST serialises.
 */
function providerStringFromHarnessId(id: InspectorHarnessId): string {
  if (id === 'agy') return 'agy';
  return id;
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

function TargetAgentField({
  value,
  upstreamSpawns,
  onChange,
}: {
  value: string | null;
  upstreamSpawns: string[];
  onChange: (targetNodeId: string | null) => void;
}) {
  return (
    <Field label="Target agent">
      <TargetNodeSelect value={value} upstreamSpawns={upstreamSpawns} onChange={onChange} />
    </Field>
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
  // Build the row list from static paths plus dynamic spawn-output chips,
  // then keep only values with an upstream producer for this node.
  const spawnOutputPaths = useMemo(
    () => (reachable?.nodeOutputIds ?? []).map((id) => `node.${id}.output`),
    [reachable]
  );
  const allPaths = useMemo(
    () =>
      [...MUSTACHE_PATHS, ...spawnOutputPaths].filter((path) =>
        isReachablePath(path, reachable)
      ),
    [reachable, spawnOutputPaths]
  );
  const grouped = useMemo(() => {
    const buckets = new Map<string, string[]>();
    for (const spec of MUSTACHE_GROUPS) buckets.set(spec.namespace, []);
    for (const path of allPaths) {
      // groupForPath routes `node.<id>.output` into the spawn_output
      // bucket so spawn-output chips end up under the Node Outputs
      // header, not under "node.id" (issue #1359 round-3 review).
      const ns = groupForPath(path);
      const list = buckets.get(ns);
      if (list !== undefined) list.push(path);
    }
    return MUSTACHE_GROUPS.flatMap((spec) => {
      const paths = buckets.get(spec.namespace) ?? [];
      if (paths.length === 0) return [];
      return [{ spec, paths }];
    });
  }, [allPaths]);

  // Keep hooks above this branch: the selected node can change from a gate
  // to an action without unmounting this child component.
  if (!hasTemplate) return null;

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
        Variables this template can interpolate at runtime.
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
        <SpawnAgentNodeFields kind={kind} onChange={onChange} reachable={reachable} />
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

      {kind.type === 'close_agent_node' && (
        <TargetAgentField
          value={kind.target_node_id}
          upstreamSpawns={upstreamSpawns}
          onChange={(target_node_id) => onChange({ ...kind, target_node_id })}
        />
      )}

      {kind.type === 'llm_turn_classifier' && (
        <TargetAgentField
          value={kind.target_node_id}
          upstreamSpawns={upstreamSpawns}
          onChange={(target_node_id) => onChange({ ...kind, target_node_id })}
        />
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
      {['manual', 'all_completed', 'any_completed'].includes(kind.type) && (
        <p className="text-xs text-text-secondary">{configSummary(kind)}</p>
      )}

      <ContextReferenceDrawer reachable={reachable} hasTemplate={hasTemplate} />
    </aside>
  );
}
/**
 * The SpawnAgentNode inspector (issue #1358). Renders the existing
 * agent-name / prompt fields plus the four capability-gated v2
 * overrides: provider, model, effort, extra-args. When the user
 * hasn't selected a provider (`kind.provider == null`) we hide the
 * model / effort / extra-args inputs entirely — the resolver falls
 * through to the mesh / application defaults in that mode, and
 * showing the inputs would suggest an actionable control the spec
 * doesn't honour.
 */
function SpawnAgentNodeFields({
  kind,
  onChange,
  reachable,
}: {
  kind: Extract<CircuitNodeKind, { type: 'spawn_agent_node' }>;
  onChange: (kind: CircuitNodeKind) => void;
  reachable: ReachableContext | undefined;
}) {
  const harnessId = harnessIdFromProvider(kind.provider);
  const caps = getCapabilitiesFor(harnessId);

  return (
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

      <Field label="Provider (default = mesh autopilot)">
        <select
          value={harnessId ?? ''}
          aria-label="Provider"
          data-testid="inspector-provider-select"
          onChange={(e) => {
            const v = e.target.value;
            // Issue #1362 review (PR #1362): when the user switches
            // provider, the previously-authored `model` / `effort` /
            // `extra_args` may no longer be honoured by the new
            // harness's capability contract. Clear them so the AST
            // doesn't serialise dangling configuration that the
            // resolver would silently drop at spawn time — the next
            // edit re-populates them. The `provider` itself is the
            // one field that legitimately persists across switches.
            const next: Partial<typeof kind> =
              v === ''
                ? { provider: null }
                : { provider: providerStringFromHarnessId(v as InspectorHarnessId) };
            onChange({
              ...kind,
              ...next,
              model: null,
              effort: null,
              extra_args: null,
            });
          }}
          className={inputClass}
        >
          <option value="">Default (mesh autopilot)</option>
          {HARNESS_OPTIONS.map((opt) => (
            <option key={opt.value} value={opt.value}>
              {opt.label}
            </option>
          ))}
        </select>
      </Field>

      {/* Capability-gated overrides — rendered only when a harness is
          selected AND the descriptor advertises the control. Capability
          contract is the authoritative source — see
          `src-tauri/src/agent/capabilities.rs::inventory_matches_research_matrix`. */}
      {caps?.supports_model_override && (
        <Field label="Model override">
          <input
            value={kind.model ?? ''}
            aria-label="Model override"
            data-testid="inspector-model-input"
            placeholder="e.g. opus-4-1"
            onChange={(e) => onChange({ ...kind, model: e.target.value || null })}
            className={inputClass}
          />
        </Field>
      )}

      {caps && caps.effort_control.kind !== 'none' && (
        <Field label="Reasoning effort">
          <select
            value={kind.effort ?? ''}
            aria-label="Effort override"
            data-testid="inspector-effort-select"
            onChange={(e) => onChange({ ...kind, effort: e.target.value || null })}
            className={inputClass}
          >
            <option value="">— inherit default —</option>
            {effortAllowedFor(caps).map((level) => (
              <option key={level} value={level}>
                {level}
              </option>
            ))}
          </select>
        </Field>
      )}

      {caps?.supports_extra_args && (
        <Field label="Extra CLI args (whitespace-separated)">
          <input
            value={kind.extra_args ?? ''}
            aria-label="Extra CLI args"
            data-testid="inspector-extra-args-input"
            placeholder="e.g. --dangerously-skip-permissions"
            onChange={(e) => onChange({ ...kind, extra_args: e.target.value || null })}
            className={`${inputClass} font-mono`}
          />
        </Field>
      )}

      {!caps && harnessId == null && (
        <p className="text-2xs text-text-muted">
          No provider override selected — model, effort, and extra args
          fall through to the mesh / application defaults at spawn.
        </p>
      )}
    </>
  );
}
