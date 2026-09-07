// Intentional ESLint fixture — every violation here MUST be caught by
// `npm run lint`. Used by `tests/agent-infra/eslint-config-gate.test.mjs`
// to assert the gate is wired (issue #1542 acceptance criterion:
// "Fixtures with conditional Hooks or missing effect dependencies must
// fail lint").
//
// DO NOT EDIT the violations away — the test file asserts these specific
// rule names appear in ESLint output. If you need to remove a violation,
// update the test's expected list first and explain why in the diff.

import { useState, useEffect } from 'react';

export function BadHooks({ shouldRender }: { shouldRender: boolean }) {
  if (shouldRender) {
    // Conditional hook call — must trigger react-hooks/rules-of-hooks.
    const [count] = useState(0);
    void count;
  }
  const [name] = useState('');
  useEffect(() => {
    // Reading `name` here without listing it in the dep array must
    // trigger react-hooks/exhaustive-deps.
    void name;
  }, []);
  return null;
}
