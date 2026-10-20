import { useId } from 'react';

export function reviewRoundLabel(context: Record<string, string>): string {
  const limit = context['review.round_limit'] === '0' ? 'unlimited' : context['review.round_limit'] ?? context['retry.max_retries'];
  return `Round ${context['retry.attempt'] ?? '1'}${limit ? ` of ${limit}` : ''}`;
}

export function ReviewRoundLimit({ value, busy, onChange }: {
  value: number;
  busy: boolean;
  onChange: (value: number) => void;
}) {
  const id = useId();
  return (
    <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-2xs">
      <label htmlFor={id} className="text-text-secondary">Review rounds</label>
      <select id={id} value={value} disabled={busy} onChange={(event) => onChange(Number(event.target.value))}
        title="Maximum review rounds. Lowering this lets the current round finish. Unlimited keeps allowing another round until approval."
        className="min-h-6 max-w-full rounded-md border border-border-subtle bg-bg-input px-1.5 text-text-primary disabled:opacity-40">
        {Array.from({ length: 9 }, (_, index) => index + 1).map((rounds) => <option key={rounds} value={rounds}>{rounds}</option>)}
        {value > 9 && <option value={value}>{value}</option>}
        <option value={0}>Unlimited</option>
      </select>
    </div>
  );
}
