// Records that it ran, and what deps it received, by writing a marker file.
//
// Used two ways by `tests/integration/ui-shot.test.ts`:
//  - directly via `runSteps`, where `deps.markerPath` carries the destination;
//  - through the `ui-shot.mjs` CLI, where the CLI has no way to pass extra deps
//    to a steps file, so `UI_SHOT_MARKER_PATH` carries it instead.
//
// The CLI-level use is what proves the CLI still calls the step phase at all;
// asserting only on `runSteps` would stay green if that call site were dropped.
import { writeFileSync } from 'node:fs';

export default function recordingSteps({ page, invoke, mock, markerPath }) {
  const destination = markerPath ?? process.env.UI_SHOT_MARKER_PATH;
  writeFileSync(destination, JSON.stringify({
    hasPage: Boolean(page),
    hasInvoke: typeof invoke === 'function',
    hasMock: Boolean(mock),
    fromCli: markerPath === undefined,
  }));
}
