import { withDeadline } from './ui-shot-deadline.mjs';
import { BROWSER_SETUP_TIMEOUT_MS } from './ui-shot-budgets.mjs';

const reactWarning = /Each child in a list should have a unique ["']key["'] prop|Encountered two children with the same key|React has detected a change in the order of Hooks|Invalid hook call|Rendered (?:fewer|more) hooks/i;

/** Collect before navigation; consult after mount, steps and screenshot. */
export function collectPageErrors(page) {
  const errors = [];
  const record = (surface, diagnostic) => {
    errors.push({ surface, diagnostic });
    console.error(`[${surface}]`, diagnostic);
  };
  page.on('pageerror', error => record('page error', error.message));
  page.on('console', message => {
    const text = message.text();
    if (message.type() === 'error') record('page console error', text);
    else if (message.type() === 'warning' && reactWarning.test(text)) record('React warning', text);
  });

  return {
    messages: () => errors.map(error => error.diagnostic),
    async assertHealthy() {
      // ErrorBoundary can catch a render failure without emitting pageerror.
      const fallback = await withDeadline(page.evaluate(() => {
        const heading = [...document.querySelectorAll('h1')]
          .find(element => element.textContent?.trim() === 'Buildmesh hit a render error');
        return heading ? heading.parentElement?.parentElement?.innerText ?? heading.textContent : null;
      }), BROWSER_SETUP_TIMEOUT_MS, 'Checking the UI error surface');
      if (fallback) record('Error Boundary', fallback);
      if (errors.length) {
        throw new Error(`UI errors prevented a successful screenshot:\n${errors.map(error => `[${error.surface}] ${error.diagnostic}`).join('\n')}`);
      }
    },
  };
}
