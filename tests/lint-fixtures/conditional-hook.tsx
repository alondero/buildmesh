// Issue #1542 — fixture: a conditional Hook call.
//
// `rules-of-hooks` must trip on the early-return-before-useState
// pattern. Real-world analogue: closed issue #1242 (conditional
// hook crash). DO NOT add `eslint-disable` comments here — the
// fixture's whole purpose is to prove the rule is active.
//
// Excluded from `npm run lint` via `eslint.config.js`'s `ignores`
// block and verified separately by
// `scripts/check-eslint-fixtures.mjs` — see `npm run lint:fixtures`.

import { useEffect, useState } from 'react';

type Props = {
  /** Set to `false` to license the early-return branch and trip `rules-of-hooks`. */
  ready: boolean;
};

export function ConditionalHookFixture({ ready }: Props) {
  // Intentional violation: a Hook after an early return. The rule
  // must produce a "Rules of Hooks" error here.
  if (!ready) {
    return null;
  }
  const [count, setCount] = useState(0);

  useEffect(() => {
    setCount((c) => c + 1);
  }, []);

  return <button onClick={() => setCount(0)}>count: {count}</button>;
}
