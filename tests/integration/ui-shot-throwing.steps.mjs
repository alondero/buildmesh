// Throws, so the CLI's exit code and diagnostic can be asserted: a failing step
// must fail the run rather than reporting green.
export default async function throwingSteps() {
  throw new Error('step assertion exploded');
}
