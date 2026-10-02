import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { SettingsSection } from './SettingsRow';

export type ProbePromptKind = 'issue' | 'pr';
export type ProbePromptDefaults = Record<ProbePromptKind, string> & { policy: string };

interface ProbeSpawnPromptsSectionProps {
  /** Stored custom templates (`null` = no override, the built-in default
   *  is active). Hydrated from `getAppPreferences` by the parent. */
  stored: Record<ProbePromptKind, string | null>;
  /** Built-in templates and review policy from the backend. `null` while the
   *  defaults load — the cards stay disabled until it resolves so a
   *  unloaded template can never be mistaken for an empty default. */
  defaults: ProbePromptDefaults | null;
  /** Persist the draft for `kind`. Blank drafts clear the override
   *  (backend collapses them to `None`). Returns `true` on success so
   *  the card can commit its snapshot; `false` rolls the draft back. */
  onSave: (kind: ProbePromptKind, value: string) => Promise<boolean>;
  /** Clear the stored override for `kind`, restoring the built-in
   *  wording. Same return contract as `onSave`. */
  onReset: (kind: ProbePromptKind) => Promise<boolean>;
  /** Mirror the aggregate dirty state to the modal so an Escape or
   *  backdrop click is intercepted by the discard banner (issue #730). */
  onDirtyChange?: (dirty: boolean) => void;
  /** Load failure for the built-in defaults. Rendered as an inline error
   *  with a Retry button — without it a failed defaults IPC would leave
   *  the cards permanently disabled with no explanation. */
  defaultsError?: string | null;
  /** Re-run the defaults fetch after a failure. */
  onRetryDefaults?: () => void;
  /** When `true`, every input / button is disabled. Used by the parent
   *  to gate the section on a failed preferences load (issue #1534). */
  disabled?: boolean;
}

interface PromptDraft {
  committed: string;
  draft: string;
  dirty: boolean;
  custom: boolean;
}

const EMPTY_DRAFT: PromptDraft = { committed: '', draft: '', dirty: false, custom: false };

const CARD_META: Record<
  ProbePromptKind,
  { title: string; summary: string; placeholders: string[]; testId: string }
> = {
  issue: {
    title: 'GitHub Issues probe',
    summary: 'Initial prompt for agents spawned from the Probe’s Issues tab.',
    placeholders: ['number', 'title', 'title_suffix', 'url', 'owner', 'repo'],
    testId: 'issue',
  },
  pr: {
    title: 'Pull Requests probe',
    summary: 'Initial prompt for agents spawned from the Probe’s Pull Requests tab.',
    placeholders: ['number', 'url', 'owner', 'repo', 'policy'],
    testId: 'pr',
  },
};

const KINDS: ProbePromptKind[] = ['issue', 'pr'];

const PLACEHOLDER_HELP: Record<string, string> = {
  number: 'Issue or pull request number, without #.',
  title: 'Issue title, or empty if no title is available.',
  title_suffix: 'Issue title preceded by “ — ”, or empty if no title is available.',
  url: 'Full GitHub link to the issue or pull request.',
  owner: 'GitHub user or organisation that owns the repository.',
  repo: 'Repository name, without the owner.',
  policy: 'Shared Buildmesh review instructions. Expand the example prompt below to read the full instructions.',
};

function exampleValues(kind: ProbePromptKind, policy: string): Record<string, string> {
  return {
    number: '42',
    url: `https://github.com/octocat/hello-world/${kind === 'issue' ? 'issues' : 'pull'}/42`,
    owner: 'octocat',
    repo: 'hello-world',
    ...(kind === 'issue'
      ? { title: 'Fix login timeout', title_suffix: ' — Fix login timeout' }
      : { policy }),
  };
}

function examplePrompt(template: string, values: Record<string, string>): string {
  // A single pass mirrors spawning: inserted values are never expanded again.
  return template.replace(/\{\{([\s\S]*?)\}\}/g, (token, name: string) => Object.prototype.hasOwnProperty.call(values, name) ? values[name] : token);
}

/** Custom initial prompts for Probe-spawned agents. Each card edits one
 *  effective template, including the built-in wording when no override exists.
 *
 *  Save / reset semantics mirror `HarnessDefaultsSection`: optimistic
 *  save with rollback on failure, and the parent re-reads
 *  `getAppPreferences` after a successful write so a backend
 *  normalise-on-write (blank → override removed) is reflected without
 *  a stale local snapshot. */
export function ProbeSpawnPromptsSection({
  stored,
  defaults,
  onSave,
  onReset,
  onDirtyChange,
  defaultsError = null,
  onRetryDefaults,
  disabled = false,
}: ProbeSpawnPromptsSectionProps) {
  const [drafts, setDrafts] = useState<Partial<Record<ProbePromptKind, PromptDraft>>>({});
  const helpId = useId();
  const inputs = useRef<Partial<Record<ProbePromptKind, HTMLTextAreaElement>>>({});
  const focused = useRef<Record<ProbePromptKind, boolean>>({ issue: false, pr: false });
  const insertion = useRef<{ kind: ProbePromptKind; caret: number } | null>(null);
  const edits = useRef<Record<ProbePromptKind, number>>({ issue: 0, pr: 0 });
  const writes = useRef<Partial<Record<ProbePromptKind, { key: string; done: Promise<void> }>>>({});

  const queueWrite = useCallback((kind: ProbePromptKind, key: string, write: () => Promise<void>) => {
    const previous = writes.current[kind];
    if (previous?.key === key) return previous.done;
    // Queue writes as well as their settlements: a completion guard cannot
    // stop an older concurrent write from winning on disk.
    const done = previous ? previous.done.then(write, write) : write();
    const pending = { key, done };
    writes.current[kind] = pending;
    const clear = () => {
      if (writes.current[kind] === pending) delete writes.current[kind];
    };
    void done.then(clear, clear);
    return done;
  }, []);

  useLayoutEffect(() => {
    const pending = insertion.current;
    if (!pending) return;
    insertion.current = null;
    const input = inputs.current[pending.kind];
    input?.focus();
    input?.setSelectionRange(pending.caret, pending.caret);
  }, [drafts]);

  // Rebase drafts on the stored values. A draft the user never touched
  // follows hydration (blank on first mount, then the real stored
  // template when the parent's preferences load resolves); a draft the
  // user edited is preserved, with `dirty` recomputed against the new
  // committed value so a post-save refresh always clears the banner.
  useEffect(() => {
    setDrafts((prev) => {
      const next: Partial<Record<ProbePromptKind, PromptDraft>> = { ...prev };
      for (const kind of KINDS) {
        const committed = stored[kind] ?? defaults?.[kind] ?? '';
        const custom = stored[kind] !== null;
        const existing = prev[kind];
        if (!existing) {
          next[kind] = { committed, draft: committed, dirty: false, custom };
        } else if (existing.draft === existing.committed) {
          next[kind] = { committed, draft: committed, dirty: false, custom };
        } else {
          next[kind] = { ...existing, committed, custom, dirty: existing.draft !== committed };
        }
      }
      return next;
    });
  }, [stored, defaults]);

  const dirtyRef = useRef(false);
  useEffect(() => {
    const anyDirty = KINDS.some((k) => drafts[k]?.dirty ?? false);
    if (dirtyRef.current === anyDirty) return;
    dirtyRef.current = anyDirty;
    onDirtyChange?.(anyDirty);
  }, [drafts, onDirtyChange]);

  const updateDraft = useCallback((kind: ProbePromptKind, value: string) => {
    edits.current[kind]++;
    setDrafts((prev) => {
      const current = prev[kind] ?? EMPTY_DRAFT;
      return { ...prev, [kind]: { ...current, draft: value, dirty: value !== current.committed } };
    });
  }, []);

  const insertPlaceholder = (kind: ProbePromptKind, name: string) => {
    const input = inputs.current[kind];
    if (!input) return;
    const token = `{{${name}}}`;
    const { value } = input;
    const selectionStart = focused.current[kind] ? input.selectionStart : value.length;
    const selectionEnd = focused.current[kind] ? input.selectionEnd : value.length;
    insertion.current = { kind, caret: selectionStart + token.length };
    updateDraft(kind, value.slice(0, selectionStart) + token + value.slice(selectionEnd));
  };

  const commit = useCallback(
    (kind: ProbePromptKind) => {
      const current = drafts[kind];
      if (!current || !current.dirty) return;
      // Normalise at the commit boundary: the backend trims before
      // storing (blank collapses to "no override"), so the draft must be
      // trimmed too — otherwise a padded draft would compare dirty
      // against its own trimmed stored value forever.
      const saved = current.draft.trim();
      const committed = saved || defaults?.[kind] || '';
      const custom = saved !== '';
      const revision = edits.current[kind];
      // Refreshing preferences disables a focused input and can blur it.
      // An explicit reset already discards this revision; do not save it again.
      if (writes.current[kind]?.key === `reset:${revision}`) return;
      return queueWrite(kind, `save:${saved}`, async () => {
        const ok = await onSave(kind, saved).catch(() => false);
        setDrafts((prev) => {
          const cur = prev[kind];
          if (!cur) return prev;
          if (edits.current[kind] !== revision) {
            // Every serialized success moves the persisted baseline, including
            // when a newer queued write might subsequently fail.
            return ok
              ? { ...prev, [kind]: { ...cur, committed, custom, dirty: cur.draft.trim() !== committed } }
              : prev;
          }
          return { ...prev, [kind]: ok
            ? { committed, draft: committed, dirty: false, custom }
            : { ...cur, draft: cur.committed, dirty: false } };
        });
      });
    },
    [drafts, defaults, onSave, queueWrite],
  );

  const reset = useCallback(
    (kind: ProbePromptKind) => {
      const current = drafts[kind];
      if (!current) return;
      const revision = edits.current[kind];
      const committed = defaults?.[kind] ?? '';
      return queueWrite(kind, `reset:${revision}`, async () => {
        const ok = await onReset(kind).catch(() => false);
        if (!ok) return;
        setDrafts((prev) => {
          const cur = prev[kind];
          if (!cur) return prev;
          return { ...prev, [kind]: edits.current[kind] === revision
            ? { committed, draft: committed, dirty: false, custom: false }
            : { ...cur, committed, custom: false, dirty: cur.draft !== committed } };
        });
      });
    },
    [drafts, defaults, onReset, queueWrite],
  );

  const inputsDisabled = disabled || defaults === null;

  return (
    <SettingsSection
      title="Probe spawn prompts"
      testId="probe-spawn-prompts-section"
      description={
        <>
          The initial prompt handed to agents spawned from the Probe’s GitHub
          Issues and Pull Requests tabs. Edit the built-in wording directly;
          clearing a prompt or using Reset restores it. Click a placeholder
          to insert it at the cursor, replacing any selected text. Values are
          filled in when an agent starts; unknown placeholders stay unchanged.
        </>
      }
    >
      {defaults === null && defaultsError !== null && (
        <p className="text-sm text-status-error" role="alert" data-testid="probe-prompts-defaults-error">
          Could not load the built-in defaults: {defaultsError}{' '}
          <button
            type="button"
            onClick={() => onRetryDefaults?.()}
            className="underline hover:no-underline"
            data-testid="probe-prompts-defaults-retry"
          >
            Retry
          </button>
        </p>
      )}
      <div className="space-y-3">
        {KINDS.map((kind) => {
          const meta = CARD_META[kind];
          const state = drafts[kind] ?? EMPTY_DRAFT;
          const hasCustom = state.custom;
          const examples = exampleValues(kind, defaults?.policy ?? '');
          return (
            <div
              key={kind}
              className="border border-border-subtle rounded-lg px-4 py-3 space-y-2"
              data-testid={`probe-prompt-card-${meta.testId}`}
              data-has-custom={hasCustom ? 'true' : 'false'}
            >
              <div className="flex items-center gap-2">
                <span className="text-base font-medium text-text-primary">{meta.title}</span>
                <span
                  className={`text-xs px-2 py-0.5 rounded-full ${
                    hasCustom
                      ? 'bg-accent-cyan/15 text-accent-cyan'
                      : 'bg-bg-input text-text-muted'
                  }`}
                  data-testid={`probe-prompt-badge-${meta.testId}`}
                >
                  {hasCustom ? 'Custom' : 'Using default'}
                </span>
                {(hasCustom || state.dirty) && (
                  <button
                    type="button"
                    onClick={() => void reset(kind)}
                    onMouseDown={(event) => event.preventDefault()}
                    disabled={inputsDisabled}
                    className="ml-auto shrink-0 px-3 py-1.5 bg-status-error/15 text-status-error text-sm rounded-md hover:bg-status-error/25 disabled:opacity-50"
                    aria-label={`Reset ${meta.title} prompt to default`}
                    data-testid={`probe-prompt-reset-${meta.testId}`}
                  >
                    Reset to default
                  </button>
                )}
              </div>
              <p className="text-sm text-text-secondary">{meta.summary}</p>
              <textarea
                ref={(input) => { inputs.current[kind] = input ?? undefined; }}
                value={state.draft}
                placeholder={defaults === null ? 'Loading default prompt…' : ''}
                onChange={(e) => updateDraft(kind, e.target.value)}
                onFocus={() => { focused.current[kind] = true; }}
                onBlur={(event) => {
                  if (event.relatedTarget instanceof HTMLElement && event.relatedTarget.dataset.promptInsert === kind) return;
                  void commit(kind);
                }}
                disabled={inputsDisabled}
                rows={4}
                aria-label={`${meta.title} prompt template`}
                data-testid={`probe-prompt-input-${meta.testId}`}
                className="w-full bg-bg-card border border-border-subtle rounded-md px-3 py-2 font-mono text-sm text-text-primary placeholder:text-text-muted placeholder:font-sans focus:outline-none focus:border-accent-cyan disabled:opacity-50"
              />
              <p className="text-sm text-text-secondary">Insert a placeholder:</p>
              <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
                {meta.placeholders.map((name) => (
                  <button
                    key={name}
                    type="button"
                    disabled={inputsDisabled}
                    data-prompt-insert={kind}
                    aria-label={`Insert {{${name}}} into ${meta.title} prompt`}
                    aria-describedby={`${helpId}-${kind}-${name}-help ${helpId}-${kind}-${name}-example`}
                    onMouseDown={(event) => event.preventDefault()}
                    onClick={() => insertPlaceholder(kind, name)}
                    onBlur={(event) => {
                      const next = event.relatedTarget;
                      if (next === inputs.current[kind] || (next instanceof HTMLElement && next.dataset.promptInsert === kind)) return;
                      void commit(kind);
                    }}
                    className={`min-w-0 text-left border border-border-subtle rounded-md px-3 py-2 hover:bg-bg-card-hover focus-visible:outline-none focus-visible:border-accent-cyan disabled:opacity-50 ${name === 'policy' ? 'sm:col-span-2' : ''}`}
                  >
                    <code className="text-sm text-accent-cyan">{`{{${name}}}`}</code>
                    <span id={`${helpId}-${kind}-${name}-help`} className="block text-xs text-text-secondary mt-1">{PLACEHOLDER_HELP[name]}</span>
                    <span id={`${helpId}-${kind}-${name}-example`} className="block text-xs text-text-secondary mt-1 break-words">
                      Example: <code>{name === 'policy'
                        ? examples[name].match(/^[\s\S]*?[.!?](?:\s|$)/)?.[0].trim() ?? examples[name]
                        : examples[name]}</code>
                    </span>
                  </button>
                ))}
              </div>
              <details className="text-sm text-text-secondary">
                <summary className="cursor-pointer hover:text-text-primary">Example prompt</summary>
                <p className="my-2 text-xs">Live preview using fictional GitHub item #42 in octocat/hello-world.</p>
                <pre
                  data-testid={`probe-prompt-preview-${meta.testId}`}
                  className="whitespace-pre-wrap break-words bg-bg-input border border-border-subtle rounded-md p-3 font-mono text-sm text-text-primary"
                >
                  {examplePrompt(state.draft.trim() || defaults?.[kind] || '', examples)}
                </pre>
              </details>
              {state.dirty && (
                <span
                  className="text-sm text-status-warning"
                  data-testid={`probe-prompt-dirty-${meta.testId}`}
                >
                  Saves on blur
                </span>
              )}
            </div>
          );
        })}
      </div>
    </SettingsSection>
  );
}
