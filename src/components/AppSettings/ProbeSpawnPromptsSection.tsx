import { useCallback, useEffect, useRef, useState } from 'react';
import { SettingsSection } from './SettingsRow';

export type ProbePromptKind = 'issue' | 'pr';

interface ProbeSpawnPromptsSectionProps {
  /** Stored custom templates (`null` = no override, the built-in default
   *  is active). Hydrated from `getAppPreferences` by the parent. */
  stored: Record<ProbePromptKind, string | null>;
  /** Built-in templates from `get_probe_spawn_prompt_defaults`, shown as
   *  the textarea placeholder and the reset target. `null` while the
   *  defaults load — the cards stay disabled until it resolves so a
   *  missing placeholder can never be mistaken for an empty default. */
  defaults: Record<ProbePromptKind, string> | null;
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
}

const EMPTY_DRAFT: PromptDraft = { committed: '', draft: '', dirty: false };

const CARD_META: Record<
  ProbePromptKind,
  { title: string; summary: string; placeholders: string; testId: string }
> = {
  issue: {
    title: 'GitHub Issues probe',
    summary: 'Initial prompt for agents spawned from the Probe’s Issues tab.',
    placeholders: '{{number}} {{title}} {{title_suffix}} {{url}} {{owner}} {{repo}}',
    testId: 'issue',
  },
  pr: {
    title: 'Pull Requests probe',
    summary: 'Initial prompt for agents spawned from the Probe’s Pull Requests tab.',
    placeholders: '{{number}} {{url}} {{owner}} {{repo}} {{policy}}',
    testId: 'pr',
  },
};

const KINDS: ProbePromptKind[] = ['issue', 'pr'];

/** Custom initial prompts for Probe-spawned agents. Each card edits one
 *  template with `{{placeholder}}` substitution; empty means "use the
 *  built-in default" (shown as the placeholder and restored by Reset).
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

  // Rebase drafts on the stored values. A draft the user never touched
  // follows hydration (blank on first mount, then the real stored
  // template when the parent's preferences load resolves); a draft the
  // user edited is preserved, with `dirty` recomputed against the new
  // committed value so a post-save refresh always clears the banner.
  useEffect(() => {
    setDrafts((prev) => {
      const next: Partial<Record<ProbePromptKind, PromptDraft>> = { ...prev };
      for (const kind of KINDS) {
        const committed = stored[kind] ?? '';
        const existing = prev[kind];
        if (!existing) {
          next[kind] = { committed, draft: committed, dirty: false };
        } else if (existing.draft === existing.committed) {
          next[kind] = { committed, draft: committed, dirty: false };
        } else {
          next[kind] = { ...existing, committed, dirty: existing.draft !== committed };
        }
      }
      return next;
    });
  }, [stored]);

  const dirtyRef = useRef(false);
  useEffect(() => {
    const anyDirty = KINDS.some((k) => drafts[k]?.dirty ?? false);
    if (dirtyRef.current === anyDirty) return;
    dirtyRef.current = anyDirty;
    onDirtyChange?.(anyDirty);
  }, [drafts, onDirtyChange]);

  const updateDraft = useCallback((kind: ProbePromptKind, value: string) => {
    setDrafts((prev) => {
      const current = prev[kind] ?? EMPTY_DRAFT;
      return { ...prev, [kind]: { ...current, draft: value, dirty: value !== current.committed } };
    });
  }, []);

  const commit = useCallback(
    async (kind: ProbePromptKind) => {
      const current = drafts[kind];
      if (!current || !current.dirty) return;
      const ok = await onSave(kind, current.draft);
      if (ok) {
        setDrafts((prev) => ({
          ...prev,
          [kind]: { ...current, committed: current.draft, dirty: false },
        }));
      } else {
        setDrafts((prev) => ({
          ...prev,
          [kind]: { ...current, draft: current.committed, dirty: false },
        }));
      }
    },
    [drafts, onSave],
  );

  const reset = useCallback(
    async (kind: ProbePromptKind) => {
      const current = drafts[kind];
      if (!current) return;
      const ok = await onReset(kind);
      if (ok) {
        setDrafts((prev) => ({
          ...prev,
          [kind]: { committed: '', draft: '', dirty: false },
        }));
      }
    },
    [drafts, onReset],
  );

  const inputsDisabled = disabled || defaults === null;

  return (
    <SettingsSection
      title="Probe spawn prompts"
      testId="probe-spawn-prompts-section"
      description={
        <>
          The initial prompt handed to agents spawned from the Probe’s GitHub
          Issues and Pull Requests tabs. Leave a prompt empty to keep the
          built-in wording; Reset restores it after a custom template is
          saved. Placeholders are substituted per spawn — unknown
          placeholders are left in place.
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
          const hasCustom = state.committed !== '';
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
                {hasCustom && (
                  <button
                    type="button"
                    onClick={() => void reset(kind)}
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
                value={state.draft}
                placeholder={defaults?.[kind] ?? ''}
                onChange={(e) => updateDraft(kind, e.target.value)}
                onBlur={() => void commit(kind)}
                disabled={inputsDisabled}
                rows={4}
                aria-label={`${meta.title} prompt template`}
                data-testid={`probe-prompt-input-${meta.testId}`}
                className="w-full bg-bg-card border border-border-subtle rounded-md px-3 py-2 font-mono text-sm text-text-primary placeholder:text-text-muted placeholder:font-sans focus:outline-none focus:border-accent-cyan disabled:opacity-50"
              />
              <p className="text-xs text-text-muted">
                Placeholders: <code>{meta.placeholders}</code>
                {kind === 'issue' && (
                  <> — <code>{'{{title_suffix}}'}</code> is “ — title”, or empty when the title is blank.</>
                )}
                {kind === 'pr' && (
                  <> — <code>{'{{policy}}'}</code> is the shared review policy.</>
                )}
              </p>
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
