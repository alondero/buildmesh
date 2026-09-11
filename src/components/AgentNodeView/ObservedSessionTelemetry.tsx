import type { ObservedMuseSessionTelemetry } from '../../types/generated/ObservedMuseSessionTelemetry';

/**
 * Node-detail readout for observed Muse MSP telemetry (issue #1680).
 *
 * Labelled "Observed Session Telemetry" so it cannot be mistaken for a
 * Usage Meter, remaining quota, or dollar spend. Cache counters stay
 * separate from counted-once prompt tokens.
 */
export function ObservedSessionTelemetry({
  telemetry,
}: {
  telemetry: ObservedMuseSessionTelemetry;
}) {
  const turn = telemetry.last_turn;
  const context = telemetry.context;
  const pressurePercent =
    context?.pressure != null ? Math.round(context.pressure * 100) : null;

  return (
    <section
      data-testid="observed-session-telemetry"
      aria-label="Observed Session Telemetry"
      className="mt-2 border-t border-border-subtle pt-2 text-text-muted"
    >
      <div className="font-medium text-text-secondary">Observed Session Telemetry</div>
      <p className="mt-0.5 text-2xs text-text-muted/80">
        Session observations — not account quota
      </p>
      {telemetry.model_id && (
        <div className="mt-1 truncate" title={telemetry.model_id}>
          Model {telemetry.model_id}
        </div>
      )}
      {turn && (
        <div className="mt-1">
          <div className="text-text-secondary">Last turn</div>
          <div>Prompt (counted once) {formatTokens(turn.prompt_tokens)}</div>
          <div>Raw input {formatTokens(turn.input_tokens)}</div>
          <div>Output {formatTokens(turn.output_tokens)}</div>
          {turn.reasoning_tokens > 0 && (
            <div>Reasoning {formatTokens(turn.reasoning_tokens)}</div>
          )}
          {turn.cache_read_tokens != null && (
            <div>Cache read {formatTokens(turn.cache_read_tokens)}</div>
          )}
          {turn.cache_write_tokens != null && (
            <div>Cache write {formatTokens(turn.cache_write_tokens)}</div>
          )}
          {turn.cache_read_tokens == null && turn.cache_write_tokens == null && turn.cached_tokens > 0 && (
            <div>Cached {formatTokens(turn.cached_tokens)}</div>
          )}
        </div>
      )}
      <div className="mt-1">
        <div className="text-text-secondary">Session total</div>
        <div>Prompt {formatTokens(telemetry.cumulative.prompt_tokens)}</div>
        <div>Output {formatTokens(telemetry.cumulative.output_tokens)}</div>
        <div>Total {formatTokens(telemetry.cumulative.total_tokens)}</div>
      </div>
      {context && (
        <div className="mt-1">
          <div className="text-text-secondary">Context window</div>
          <div>
            {context.window_tokens != null
              ? `${formatTokens(context.used_tokens)} / ${formatTokens(context.window_tokens)}${
                  pressurePercent != null ? ` (${pressurePercent}%)` : ''
                }`
              : `${formatTokens(context.used_tokens)} used · window unknown`}
          </div>
          <div className="capitalize">{context.pressure_level}</div>
        </div>
      )}
    </section>
  );
}

function formatTokens(n: number): string {
  return Math.trunc(n).toLocaleString('en-US');
}
