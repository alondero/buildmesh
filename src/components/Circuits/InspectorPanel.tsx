/**
 * InspectorPanel — the editor's right slide-over (issue #1209).
 *
 * One form per node kind: only the fields the AST actually carries
 * (the blueprint stays canonical in Rust). Text fields that accept
 * circuit context use `MustacheTextarea` so `{{` opens autocomplete.
 */

import { useState } from 'react';
import type { CircuitNode } from '../../types/generated/CircuitNode';
import type { CircuitNodeKind } from '../../types/generated/CircuitNodeKind';
import type { GithubActionKind } from '../../types/generated/GithubActionKind';
import type { SessionStatusKind } from '../../types/generated/SessionStatusKind';
import { MustacheTextarea } from './MustacheTextarea';
import { categoryAccent, categoryOf, configSummary, specFor } from './circuitGraphModel';

interface InspectorPanelProps {
  node: CircuitNode | null;
  onChange: (kind: CircuitNodeKind) => void;
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

export function InspectorPanel({ node, onChange }: InspectorPanelProps) {
  if (node === null) {
    return (
      <aside
        data-testid="circuit-inspector"
        className="w-64 shrink-0 border-l border-border-subtle bg-bg-surface p-3 text-xs text-text-muted"
      >
        Select a node to edit its configuration.
      </aside>
    );
  }

  const kind = node.type;
  const accent = categoryAccent(categoryOf(kind));

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
            />
          </Field>
        </>
      )}

      {kind.type === 'inject_pty' && (
        <Field label="PTY prompt">
          <MustacheTextarea
            value={kind.prompt}
            onChange={(prompt) => onChange({ ...kind, prompt })}
            rows={5}
            ariaLabel="Inject PTY prompt"
            testId="inspector-prompt"
          />
        </Field>
      )}

      {kind.type === 'notify' && (
        <Field label="Message">
          <MustacheTextarea
            value={kind.message}
            onChange={(message) => onChange({ ...kind, message })}
            rows={4}
            ariaLabel="Notify message"
            testId="inspector-message"
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
              />
            </Field>
          )}
        </>
      )}

      {kind.type === 'set_node_status' && (
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
    </aside>
  );
}
