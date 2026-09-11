import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { ObservedSessionTelemetry } from '../../src/components/AgentNodeView/ObservedSessionTelemetry';
import type { ObservedMuseSessionTelemetry } from '../../src/types/generated/ObservedMuseSessionTelemetry';

const SAMPLE: ObservedMuseSessionTelemetry = {
  kind: 'observed_session_telemetry',
  node_id: 7,
  session_id: 'sess-aaaa-1111',
  model_id: 'muse-spark-1.3-contributor',
  last_turn: {
    turn_id: 'turn-1',
    prompt_tokens: 20,
    output_tokens: 8,
    total_tokens: 28,
    input_tokens: 120,
    reasoning_tokens: 3,
    cached_tokens: 100,
    cache_read_tokens: 90,
    cache_write_tokens: 10,
  },
  cumulative: { prompt_tokens: 20, output_tokens: 8, total_tokens: 28 },
  context: {
    used_tokens: 4096,
    window_tokens: null,
    pressure: null,
    pressure_level: 'normal',
  },
};

describe('ObservedSessionTelemetry (issue #1680)', () => {
  it('keeps counted-once prompt distinct from raw input and cache', () => {
    render(<ObservedSessionTelemetry telemetry={SAMPLE} />);
    const panel = screen.getByRole('region', { name: 'Observed Session Telemetry' });
    expect(panel.textContent).toContain('Prompt (counted once) 20');
    expect(panel.textContent).toContain('Cache read 90');
    expect(panel.textContent).toContain('Cache write 10');
    expect(panel.textContent).not.toContain('Prompt (counted once) 120');
    expect(panel.textContent).not.toContain('Prompt (counted once) 220');
  });

  it('omits a window limit rather than inventing one', () => {
    render(<ObservedSessionTelemetry telemetry={SAMPLE} />);
    expect(screen.getByTestId('observed-session-telemetry').textContent).toContain(
      '4,096 used · window unknown',
    );
    expect(screen.getByTestId('observed-session-telemetry').textContent).not.toContain('/');
  });

  it('never labels observations as remaining quota or spend', () => {
    render(<ObservedSessionTelemetry telemetry={SAMPLE} />);
    const text = screen.getByTestId('observed-session-telemetry').textContent ?? '';
    expect(text).toContain('Session observations — not account quota');
    expect(text.toLowerCase()).not.toContain('remaining');
    expect(text.toLowerCase()).not.toContain('allowance');
    expect(text).not.toContain('$');
  });
});
