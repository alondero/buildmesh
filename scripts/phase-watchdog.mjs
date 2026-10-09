// A child-side phase watchdog for long-running dev-tool scripts.
//
// The problem it replaces (issue #2168): a supervising wrapper had to know, in
// advance, the sum of every phase its child could spend, or a slow run got
// killed mid-flight and reported `ui-shot did not finish within 522000ms` —
// one number, no indication of which phase was stuck. Keeping that sum honest
// meant a second hand-maintained copy of the phase list in the test, which is
// precisely the copy that drifts.
//
// The child already knows what it is doing, so the child says so. When it
// enters a phase it appends the phase name to a file; the wrapper reads that
// file when it kills the child, and reports
//
//     ui-shot did not finish within 960000ms (killed while in phase: step script)
//
// One number for the deadline, one named phase for the diagnostic, and no
// second copy of the phase list anywhere.
//
// Why a file rather than a stderr marker: the child is killed with SIGKILL-like
// semantics on Windows, and its stdout/stderr buffers are lost with it, so a
// marker written to a pipe may never reach the wrapper. An appended file is
// already on disk when the child dies. The file is a single append-only line
// log, so a partial write can only ever cost the last entry, never corrupt an
// earlier one.

import { appendFileSync, mkdirSync, readFileSync } from 'node:fs';
import { dirname } from 'node:path';

/**
 * The env var a supervising wrapper sets to tell the child where to record
 * phases. Passed to `createPhaseRecorder()` by the caller; absent means "record
 * nothing", so a run that is not supervised pays nothing and writes no file.
 */
export const PHASE_FILE_ENV = 'BUILDMESH_PHASE_FILE';

/**
 * A recorder the child calls when it enters a phase.
 *
 * `record` appends the phase name and returns it, so a call site can read as
 * `await phase('navigation')` and get the name back for a log line. `phases()`
 * returns everything recorded so far in this process, which is what lets a
 * wrapper that never had to kill anything still know the run's shape.
 *
 * With no `file` the recorder is a no-op that still records in memory: an
 * unsupervised run keeps its phase list (useful in a `--steps` assertion) and
 * writes nothing to disk.
 */
export function createPhaseRecorder(file) {
  const seen = [];
  const record = (phase) => {
    seen.push(phase);
    if (file) {
      try {
        mkdirSync(dirname(file), { recursive: true });
        appendFileSync(file, `${phase}\n`);
      } catch {
        // Recording a phase must never fail the run it is describing: an
        // unwritable scratch path costs the diagnostic, not the screenshot.
      }
    }
    return phase;
  };
  return { record, phases: () => [...seen] };
}

/**
 * Read the phases a child recorded, oldest first.
 *
 * Returns an empty list for a missing or unreadable file: the wrapper's job is
 * to report the phase, and "no phase recorded" is a correct answer for a child
 * that died before entering one (a boot crash, an import error).
 */
export function readRecordedPhases(file) {
  if (!file) return [];
  try {
    return readFileSync(file, 'utf8').split('\n').filter(Boolean);
  } catch {
    return [];
  }
}

/**
 * The phase the child was in, or null if it never recorded one.
 *
 * The last entry wins because a child that is killed mid-phase has already
 * appended that phase: it appended on entry, so the newest name is the one it
 * was inside when it stopped making progress.
 */
export function activePhase(file) {
  const phases = readRecordedPhases(file);
  return phases.length > 0 ? phases[phases.length - 1] : null;
}

/**
 * The diagnostic a wrapper reports when it kills a child for exceeding its
 * deadline.
 *
 * The phase is what makes this actionable: a fixed deadline can say only that
 * time ran out, whereas "killed while in phase: step script" names the work
 * that was in flight and is the same information the child's own per-phase
 * timeout produces (#2063).
 */
export function killDiagnostic({ label, timeoutMs, phase }) {
  const where = phase ? ` (killed while in phase: ${phase})` : ' (killed before it recorded a phase)';
  return `${label} did not finish within ${timeoutMs}ms${where}`;
}