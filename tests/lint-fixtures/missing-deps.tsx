// Issue #1542 — fixture: a useEffect with a missing dependency.
//
// `exhaustive-deps` must warn (or error, once we tighten it) that
// `external` is read inside the effect but absent from the deps
// array. Real-world analogue: stale-closure bug class. DO NOT add
// `eslint-disable` comments here — the fixture's whole purpose is
// to prove the rule is active.
//
// Excluded from `npm run lint` via `eslint.config.js`'s `ignores`
// block and verified separately by
// `scripts/check-eslint-fixtures.mjs` — see `npm run lint:fixtures`.

import { useEffect } from 'react';

type Props = {
  /** Read inside the effect without being listed in the deps. */
  external: number;
};

export function MissingDepsFixture({ external }: Props) {
  // Intentional violation: `external` is in the body but not in the
  // deps array. The rule must flag this.
  useEffect(() => {
    if (external > 0) {
      console.log('external is positive:', external);
    }
  }, []);

  return <div>{external}</div>;
}
