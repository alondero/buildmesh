import { useState } from 'react';

/** The same review continuation choice on the Overview and Probe run card. */
export function ReviewExtension({ runId, busy, onContinue, showHelp = true }: {
  runId: number;
  busy: boolean;
  onContinue: (additionalRounds: number) => void;
  showHelp?: boolean;
}) {
  const [additionalRounds, setAdditionalRounds] = useState(1);
  return (
    <div>
      <label className="mb-1 flex flex-wrap items-center gap-1 text-meta text-text-secondary">
        Additional review rounds
        <select aria-label="Additional review rounds" value={additionalRounds} disabled={busy}
          onChange={(event) => setAdditionalRounds(Number(event.target.value))}
          className="min-h-[24px] min-w-[24px] rounded-md border border-border-subtle bg-bg-input px-1.5 text-text-primary disabled:opacity-40">
          {Array.from({ length: 10 }, (_, index) => index + 1).map((rounds) => <option key={rounds} value={rounds}>{rounds}</option>)}
        </select>
      </label>
      <button type="button" disabled={busy} onClick={() => onContinue(additionalRounds)}
        data-testid={`run-continue-review-${runId}`}
        className="min-h-[24px] px-1.5 py-1 text-label rounded-md bg-accent-cyan/15 text-accent-cyan hover:bg-accent-cyan/25 disabled:opacity-40">
        Review again · +{additionalRounds} {additionalRounds === 1 ? 'round' : 'rounds'}
      </button>
      <p hidden={!showHelp} className="mt-1 text-meta text-text-secondary break-words">Adds the selected rounds to this run and continues with the retained reviewer. Earlier findings remain in Run History. Change Review rounds on the active run to adjust its remaining allowance.</p>
    </div>
  );
}
