// Never settles. Used to exercise the step-phase deadline in
// `tests/integration/ui-shot.test.ts` without waiting out the real
// STEP_SCRIPT_TIMEOUT_MS, which is deliberately long enough that ordinary CPU
// load must not trip it.
export default async function hangingSteps() {
  await new Promise(() => {});
}
