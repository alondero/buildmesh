/**
 * RunProgressSummary — the at-a-glance progress of a Circuit Run, shared by
 * the agent node card's Overview tab and the Probe's expanded run card:
 *
 *  - the stage tracker (Prepare → Review → Finish for a review circuit, the
 *    visible steps otherwise), with the current stage marked;
 *  - for a review circuit, selectable round results and feedback, with the
 *    latest review selected by default and full reports opened on request.
 *
 * Every state is said in words (visible, or screen-reader text and a tooltip
 * beside a coloured mark), never by colour alone. The rules live in
 * `runProgressModel.ts`; this file only draws them.
 */

import { useEffect, useId, useState, type ReactNode } from 'react';
import type { CircuitRunOverview } from '../../types/generated/CircuitRunOverview';
import { circuitRunHistory } from '../../lib/tauri/circuitEvidence';
import { formatError } from '../../lib/errorUtils';
import { railRows } from './runOverviewModel';
import {
  reviewRounds,
  reviewRoundFeedback,
  reviewVerdictNode,
  roundStateLabel,
  runStages,
  type LatestReview,
  type ReviewRounds,
  type RoundState,
  type RunStage,
  type StageState,
} from './runProgressModel';
import { parseRunContext } from './runStepPresentation';

const STAGE_WORDS: Record<StageState, string> = {
  done: 'done',
  current: 'in progress',
  failed: 'failed',
  upcoming: 'not started',
  skipped: 'skipped',
};

const STAGE_MARK: Record<StageState, { glyph: string; className: string }> = {
  done: { glyph: '✓', className: 'bg-status-success text-text-inverse' },
  current: { glyph: '', className: 'border-2 border-accent-cyan bg-accent-cyan/15' },
  failed: { glyph: '✕', className: 'bg-status-error text-text-inverse' },
  upcoming: { glyph: '', className: 'border border-border-strong' },
  skipped: { glyph: '–', className: 'border border-dashed border-border-strong text-text-muted' },
};

/** Above this many stages, labels no longer fit side by side: show a bar and name the current one. */
const MAX_LABELLED_STAGES = 5;

export function RunStageTracker({ stages, detail }: {
  stages: readonly RunStage[];
  /** Extra words under a stage, e.g. the review round under Review. */
  detail?: Partial<Record<string, string>>;
}) {
  if (stages.length === 0) return null;
  if (stages.length > MAX_LABELLED_STAGES) return <CompactStageTracker stages={stages} />;
  return (
    <ol aria-label="Run stages" className="flex items-start" data-testid="run-stages">
      {stages.map((stage, index) => {
        const mark = STAGE_MARK[stage.state];
        const last = index === stages.length - 1;
        return (
          <li key={stage.key} aria-current={stage.state === 'current' ? 'step' : undefined}
            data-stage-state={stage.state} data-testid={`run-stage-${stage.key}`}
            className={`min-w-0 ${last ? 'shrink-0' : 'flex-1'}`}>
            <div className="flex items-center">
              <span aria-hidden className={`relative flex h-4 w-4 shrink-0 items-center justify-center rounded-pill text-micro font-bold leading-none ${mark.className}`}>
                {stage.state === 'current' ? <span className="h-1.5 w-1.5 rounded-pill bg-accent-cyan animate-pulse" /> : mark.glyph}
              </span>
              {!last && <span aria-hidden className={`mx-1 h-px flex-1 ${stage.state === 'done' ? 'bg-status-success/60' : 'bg-border-strong'}`} />}
            </div>
            <p className={`mt-1 pr-2 break-words text-meta ${stage.state === 'current' ? 'font-semibold text-text-primary' : stage.state === 'done' ? 'text-text-secondary' : 'text-text-muted'}`}>
              {stage.label}<span className="sr-only">: {STAGE_WORDS[stage.state]}</span>
            </p>
            {detail?.[stage.key] && <p className="pr-2 text-micro text-text-secondary break-words">{detail[stage.key]}</p>}
          </li>
        );
      })}
    </ol>
  );
}

const BAR_FILL: Record<StageState, string> = {
  done: 'bg-status-success/70',
  current: 'bg-accent-cyan animate-pulse',
  failed: 'bg-status-error',
  upcoming: 'bg-border-strong',
  skipped: 'bg-border-subtle',
};

function CompactStageTracker({ stages }: { stages: readonly RunStage[] }) {
  const index = stages.findIndex((stage) => stage.state === 'current' || stage.state === 'failed');
  const focus = index >= 0 ? stages[index] : null;
  const done = stages.filter((stage) => stage.state === 'done').length;
  return (
    <div data-testid="run-stages">
      <ol aria-label="Run stages" className="flex gap-0.5">
        {stages.map((stage) => (
          <li key={stage.key} title={`${stage.label}: ${STAGE_WORDS[stage.state]}`}
            aria-current={stage.state === 'current' ? 'step' : undefined} data-stage-state={stage.state}
            className={`h-1.5 min-w-1 flex-1 rounded-pill ${BAR_FILL[stage.state]}`}>
            <span className="sr-only">{stage.label}: {STAGE_WORDS[stage.state]}</span>
          </li>
        ))}
      </ol>
      <p aria-hidden className="mt-1 text-meta text-text-secondary break-words">
        {focus === null
          ? `${done} of ${stages.length} steps done`
          : <>Step {index + 1} of {stages.length} · <span className="font-semibold text-text-primary">{focus.label}</span></>}
      </p>
    </div>
  );
}

const ROUND_STYLE: Record<RoundState, string> = {
  changes_requested: 'bg-status-warning/15 text-status-warning ring-1 ring-inset ring-status-warning/40',
  approved: 'bg-status-success/15 text-status-success ring-1 ring-inset ring-status-success/40',
  needs_attention: 'bg-status-error/15 text-status-error ring-1 ring-inset ring-status-error/40',
  reviewing: 'bg-accent-cyan/15 text-accent-cyan ring-1 ring-inset ring-accent-cyan',
  fixing: 'border border-dashed border-accent-cyan/60 bg-accent-cyan/5 text-text-secondary',
  upcoming: 'text-text-muted ring-1 ring-inset ring-border-strong',
  unused: 'text-text-muted/70 border border-dashed border-border-default',
};

const ROUND_GLYPH: Partial<Record<RoundState, string>> = {
  changes_requested: '↺',
  approved: '✓',
  needs_attention: '!',
};

const TONE_CHIP = {
  success: 'bg-status-success/10 text-status-success ring-status-success/30',
  warning: 'bg-status-warning/10 text-status-warning ring-status-warning/30',
  error: 'bg-status-error/10 text-status-error ring-status-error/30',
} as const;

/** The opening lines of a report, without its markdown headings. */
export function reportExcerpt(report: string): string {
  return report.split('\n').map((line) => line.trim())
    .filter((line) => line !== '' && !line.startsWith('#') && !/^\*\*verdict\b/i.test(line))
    .join('\n');
}

/** `Round 1 of 5 · Changes requested, fixes in progress` — the same round the tab and stage name. */
export function roundsCaption(rounds: ReviewRounds): string {
  const current = rounds.rounds.find((round) => round.round === rounds.current);
  const fixing = rounds.rounds.some((round) => round.state === 'fixing');
  const of = rounds.limit === null ? ' of unlimited' : ` of ${rounds.limit}`;
  return `Round ${rounds.current}${of}${current === undefined ? '' : ` · ${roundStateLabel(current.state)}`}${fixing ? ', fixes in progress' : ''}`;
}

/**
 * Where a review stands: one mark per allowed round, the current round in
 * words, an optional round-limit control, and the selected round's feedback.
 */
export function ReviewRoundsPanel({ rounds, control, overview }: {
  rounds: ReviewRounds; control?: ReactNode; overview: CircuitRunOverview;
}) {
  const headingId = useId();
  const detailId = useId();
  const [selectedRound, setSelectedRound] = useState<number | null>(null);
  const selected = selectedRound ?? rounds.latest?.round;
  const round = rounds.rounds.find((item) => item.round === selected);
  const isLatest = rounds.latest !== null && selected === rounds.latest.round;
  const feedback: LatestReview | null = isLatest ? rounds.latest : round === undefined ? null : {
    round: round.round, label: roundStateLabel(round.state), report: null, byPerson: false,
    tone: round.state === 'approved' ? 'success' : round.state === 'changes_requested' ? 'warning' : 'error',
  };
  const verdictId = reviewVerdictNode(overview.graph)?.id;
  return (
    <section aria-labelledby={headingId} data-testid="run-review-rounds" className="space-y-2">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <SectionHeading id={headingId}>Review rounds</SectionHeading>
        <span className="text-meta text-text-secondary" data-testid="run-review-rounds-caption">{roundsCaption(rounds)}</span>
        {control !== undefined && <span className="ml-auto">{control}</span>}
      </div>
      <ol aria-label="Review round results" className="flex flex-wrap gap-1">
        {rounds.rounds.map(({ round, state }) => {
          const selectable = state === 'changes_requested' || state === 'approved' || state === 'needs_attention';
          const contents = <>
            <span aria-hidden>{round}</span>
            {ROUND_GLYPH[state] && <span aria-hidden>{ROUND_GLYPH[state]}</span>}
            {state === 'reviewing' && <span aria-hidden className="h-1.5 w-1.5 rounded-pill bg-accent-cyan animate-pulse" />}
            <span className="sr-only">Round {round}: {roundStateLabel(state)}</span>
          </>;
          const style = `flex min-h-6 min-w-8 items-center justify-center gap-1 rounded-md text-micro font-semibold tabular-nums ${ROUND_STYLE[state]}`;
          return (
            <li key={round} title={`Round ${round}: ${roundStateLabel(state)}`} data-round-state={state}
              className="min-w-8 flex-1">
              {selectable ? <button type="button" onClick={() => setSelectedRound(round)}
                aria-pressed={selected === round} aria-controls={detailId}
                className={`${style} w-full cursor-pointer hover:bg-bg-card-hover ${selected === round ? 'outline outline-1 outline-offset-1 outline-accent-cyan' : ''}`}>
                {contents}
              </button> : <span className={style}>{contents}</span>}
            </li>
          );
        })}
      </ol>
      <div id={detailId} aria-live="polite">
        {feedback !== null && verdictId !== undefined && <LatestReviewCard key={selected} latest={feedback}
          isLatest={isLatest} runId={overview.run.id} nodeId={verdictId} updatedAt={overview.run.updated_at}
          revision={overview.attention.revision} />}
      </div>
    </section>
  );
}

function LatestReviewCard({ latest: fallback, isLatest, runId, nodeId, updatedAt, revision }: {
  latest: LatestReview; isLatest: boolean; runId: number; nodeId: string; updatedAt: string; revision: number;
}) {
  const [open, setOpen] = useState(false);
  const [history, setHistory] = useState<{ review: LatestReview | null; error: string | null } | null>(null);
  const [retry, setRetry] = useState(0);
  const round = fallback.round;
  useEffect(() => {
    if (isLatest) return;
    let current = true;
    setHistory(null);
    circuitRunHistory(runId).then(
      (value) => { if (current) setHistory({ review: reviewRoundFeedback(value.entries, nodeId, round), error: null }); },
      (cause: unknown) => { if (current) setHistory({ review: null, error: formatError(cause) }); },
    );
    return () => { current = false; };
  }, [isLatest, runId, nodeId, round, updatedAt, revision, retry]);
  const latest = isLatest ? fallback : history?.review ?? fallback;
  const reportId = useId();
  const excerpt = latest.report === null ? '' : reportExcerpt(latest.report);
  return (
    <div className="rounded-md border border-border-subtle bg-bg-base/50 p-2" data-testid="run-latest-review">
      <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-meta">
        <span className="text-text-secondary">{isLatest ? 'Latest review' : 'Review'} · Round {latest.round}</span>
        <span className={`rounded-md px-1.5 py-px text-micro font-semibold ring-1 ring-inset ${TONE_CHIP[latest.tone]}`}>{latest.label}</span>
        {latest.byPerson && <span className="text-text-muted">recorded by a person</span>}
      </p>
      {!isLatest && history === null && <p className="mt-1 text-meta text-text-secondary">Loading feedback…</p>}
      {!isLatest && history?.error && <div className="mt-1 text-meta">
        <p role="alert" className="break-words text-status-error">Could not load feedback: {history.error}</p>
        <button type="button" onClick={() => setRetry((value) => value + 1)}
          className="inline-flex min-h-6 items-center rounded-md px-1 text-accent-cyan hover:bg-accent-cyan/10">Retry loading feedback</button>
      </div>}
      {latest.report === null && (isLatest || (history !== null && history.error === null))
        && <p className="mt-1 text-meta text-text-secondary">No feedback was retained for this round.</p>}
      {latest.report !== null && (
        <>
          {!open && excerpt !== '' && (
            <p className="mt-1 line-clamp-3 whitespace-pre-line break-words text-meta text-text-secondary">{excerpt}</p>
          )}
          {open && (
            <pre id={reportId} className="mt-1 max-h-80 overflow-y-auto whitespace-pre-wrap break-words font-mono text-meta text-text-secondary">
              {latest.report}
            </pre>
          )}
          <button type="button" onClick={() => setOpen(!open)} aria-expanded={open} aria-controls={open ? reportId : undefined}
            className="mt-1 inline-flex h-6 items-center rounded-md px-1 text-meta text-accent-cyan hover:bg-accent-cyan/10">
            {open ? 'Hide full review' : latest.byPerson ? 'Read the full note' : 'Read the full review'}
          </button>
        </>
      )}
    </div>
  );
}

/** Small uppercase section label, as menus and panels use (DESIGN.md). */
export function SectionHeading({ id, children }: { id?: string; children: ReactNode }) {
  return <h3 id={id} className="text-micro font-semibold uppercase tracking-wide text-text-muted">{children}</h3>;
}

/** The stage tracker and, for a review circuit, the rounds panel — for the Probe's run card. */
export function RunProgressSummary({ overview, roleLabel }: {
  overview: CircuitRunOverview;
  roleLabel: (nodeId: string) => string;
}) {
  const context = parseRunContext(overview.run.context_json);
  const rows = railRows(overview.graph, overview.steps, overview.run.state);
  const stages = runStages(overview.graph, rows, roleLabel);
  const rounds = reviewRounds(overview, context);
  return (
    <div className="space-y-3" data-testid={`run-progress-summary-${overview.run.id}`}>
      <RunStageTracker stages={stages} />
      {rounds !== null && <ReviewRoundsPanel key={overview.run.id} rounds={rounds} overview={overview} />}
    </div>
  );
}
